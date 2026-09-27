use crate::{
    BrowserAction, ConsoleLevel, Evidence, KeyState, PointerButton, PreviewError, SafeKey,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::net::TcpStream;
use std::{
    collections::VecDeque,
    fs,
    net::{Ipv4Addr, TcpListener},
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tungstenite::{connect, stream::MaybeTlsStream, WebSocket};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BrowserEvidence {
    pub items: Vec<Evidence>,
    pub screenshot_data_url: Option<String>,
    pub dom_text: String,
    pub accessibility_text: String,
}

pub struct BrowserRuntime {
    child: Child,
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
    next_id: u64,
    events: VecDeque<Value>,
    profile: PathBuf,
    width: u16,
    height: u16,
    scale: f32,
    cursor_x: f32,
    cursor_y: f32,
}

impl BrowserRuntime {
    pub fn launch(url: &str) -> Result<Self, PreviewError> {
        let port = reserve_debug_port()?;
        let profile = std::env::temp_dir().join(format!(
            "rex-preview-browser-{}-{}",
            std::process::id(),
            now()
        ));
        fs::create_dir_all(&profile).map_err(|_| PreviewError::CommandDenied)?;
        let chrome = [
            "google-chrome-stable",
            "google-chrome",
            "chromium",
            "chromium-browser",
        ]
        .into_iter()
        .find(|name| {
            Command::new(name)
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok()
        })
        .ok_or(PreviewError::CommandDenied)?;
        let child = Command::new(chrome)
            .args([
                "--headless=new",
                "--no-sandbox",
                "--disable-gpu",
                "--disable-dev-shm-usage",
                "--disable-background-networking",
                "--disable-component-update",
                "--disable-sync",
                "--metrics-recording-only",
                "--no-first-run",
                "--hide-scrollbars",
            ])
            .arg(format!("--remote-debugging-port={port}"))
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg("about:blank")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| PreviewError::CommandDenied)?;
        let deadline = Instant::now() + Duration::from_secs(10);
        let ws_url = loop {
            if Instant::now() >= deadline {
                return Err(PreviewError::NotRunning);
            }
            if let Ok(mut r) = ureq::get(&format!("http://127.0.0.1:{port}/json/list")).call() {
                if let Ok(v) = r.body_mut().read_json::<Value>() {
                    if let Some(s) = v.as_array().and_then(|a| {
                        a.iter()
                            .find(|t| {
                                t.get("type").and_then(Value::as_str) == Some("page")
                                    && t.get("url").and_then(Value::as_str) == Some("about:blank")
                            })
                            .and_then(|t| t.get("webSocketDebuggerUrl").and_then(Value::as_str))
                    }) {
                        break s.to_string();
                    }
                }
            }
            thread::sleep(Duration::from_millis(100));
        };
        let (socket, _) = connect(&ws_url).map_err(|_| PreviewError::NotRunning)?;
        let mut b = Self {
            child,
            socket,
            next_id: 1,
            events: VecDeque::new(),
            profile,
            width: 1280,
            height: 800,
            scale: 1.0,
            cursor_x: 0.0,
            cursor_y: 0.0,
        };
        b.command("Page.enable", json!({}))?;
        b.command("Runtime.enable", json!({}))?;
        b.command("Network.enable", json!({"maxTotalBufferSize": 1048576}))?;
        // Preview is local-only. Prevent project JS and assets from reaching
        // remote hosts through the user's machine; loopback serves the app.
        b.command(
            "Network.setBlockedURLs",
            json!({"urls":["http://*","https://*"]}),
        )?;

        b.command("Log.enable", json!({}))?;
        b.command("Page.navigate", json!({"url": url}))?;
        b.wait_loaded()?;
        Ok(b)
    }

    fn wait_loaded(&mut self) -> Result<(), PreviewError> {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let v = self.command(
                "Runtime.evaluate",
                json!({"expression":"document.readyState","returnByValue":true}),
            )?;
            if v.pointer("/result/result/value")
                .and_then(Value::as_str)
                .is_some_and(|s| s == "complete" || s == "interactive")
            {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(50));
        }
        Err(PreviewError::NotRunning)
    }

    fn command(&mut self, method: &str, params: Value) -> Result<Value, PreviewError> {
        let id = self.next_id;
        self.next_id += 1;
        self.socket
            .send(tungstenite::Message::Text(
                json!({"id":id,"method":method,"params":params})
                    .to_string()
                    .into(),
            ))
            .map_err(|_| PreviewError::NotRunning)?;
        loop {
            let msg = self.socket.read().map_err(|_| PreviewError::NotRunning)?;
            if let tungstenite::Message::Text(t) = msg {
                let v: Value = serde_json::from_str(&t).map_err(|_| PreviewError::NotRunning)?;
                if v.get("id").and_then(Value::as_u64) == Some(id) {
                    if v.get("error").is_some() {
                        return Err(PreviewError::CommandDenied);
                    }
                    return Ok(v);
                }
                if v.get("method").is_some() {
                    self.events.push_back(v);
                    while self.events.len() > 1024 {
                        self.events.pop_front();
                    }
                }
            }
        }
    }

    pub fn action(&mut self, action: &BrowserAction, base_url: &str) -> Result<(), PreviewError> {
        action.validate()?;
        match action {
            BrowserAction::Navigate { path } => {
                let base = url::Url::parse(base_url).map_err(|_| PreviewError::UrlDenied)?;
                let target = base.join(path).map_err(|_| PreviewError::UrlDenied)?;
                self.command("Page.navigate", json!({"url":target.as_str()}))?;
                self.wait_loaded()?;
            }
            BrowserAction::PointerMove { x, y } => {
                self.cursor_x = *x;
                self.cursor_y = *y;
                self.command(
                    "Input.dispatchMouseEvent",
                    json!({"type":"mouseMoved","x":x,"y":y}),
                )?;
            }
            BrowserAction::PointerDown { button } => {
                self.command("Input.dispatchMouseEvent", json!({"type":"mousePressed","x":self.cursor_x,"y":self.cursor_y,"button":button_name(*button),"clickCount":1}))?;
            }
            BrowserAction::PointerUp { button } => {
                self.command("Input.dispatchMouseEvent", json!({"type":"mouseReleased","x":self.cursor_x,"y":self.cursor_y,"button":button_name(*button),"clickCount":1}))?;
            }
            BrowserAction::Key { key, state } => {
                self.command("Input.dispatchKeyEvent", json!({"type": if *state == KeyState::Down {"keyDown"} else {"keyUp"},"key":key_name(*key)}))?;
            }
            BrowserAction::Text { value } => {
                self.command("Input.insertText", json!({"text":value}))?;
            }
            BrowserAction::Scroll { delta_x, delta_y } => {
                self.command("Input.dispatchMouseEvent", json!({"type":"mouseWheel","x":self.cursor_x,"y":self.cursor_y,"deltaX":delta_x,"deltaY":delta_y}))?;
            }
            BrowserAction::SetViewport {
                width,
                height,
                scale,
            } => {
                self.width = *width;
                self.height = *height;
                self.scale = *scale;
                self.command(
                    "Emulation.setDeviceMetricsOverride",
                    json!({"width":width,"height":height,"deviceScaleFactor":scale,"mobile":false}),
                )?;
            }
        }
        Ok(())
    }

    pub fn capture(&mut self) -> Result<BrowserEvidence, PreviewError> {
        let shot = self.command(
            "Page.captureScreenshot",
            json!({"format":"png","captureBeyondViewport":false,"fromSurface":true}),
        )?;
        let encoded = shot
            .pointer("/result/data")
            .and_then(Value::as_str)
            .ok_or(PreviewError::NotRunning)?
            .to_string();
        let bytes = STANDARD
            .decode(&encoded)
            .map_err(|_| PreviewError::EvidenceLimit)?;
        if bytes.len() > crate::MAX_SCREENSHOT_BYTES {
            return Err(PreviewError::EvidenceLimit);
        }
        let hash = format!("{:x}", Sha256::digest(&bytes));
        let id = format!("shot-{}", &hash[..16]);
        let dom = self.command(
            "Runtime.evaluate",
            json!({"expression":"document.documentElement.outerHTML","returnByValue":true}),
        )?;
        let dom_text = dom
            .pointer("/result/result/value")
            .and_then(Value::as_str)
            .unwrap_or("")
            .chars()
            .take(512 * 1024)
            .collect::<String>();
        let dom_hash = format!("{:x}", Sha256::digest(dom_text.as_bytes()));
        let ax = self.command("Accessibility.getFullAXTree", json!({}))?;
        let accessibility_text =
            serde_json::to_string(ax.pointer("/result/nodes").unwrap_or(&Value::Null))
                .unwrap_or_default();
        let node_count = ax
            .pointer("/result/nodes")
            .and_then(Value::as_array)
            .map_or(0, |v| v.len() as u32);
        let mut items = vec![
            Evidence::Viewport {
                width: self.width,
                height: self.height,
                scale_milli: (self.scale * 1000.0) as u16,
            },
            Evidence::Screenshot {
                id: id.clone(),
                width: self.width,
                height: self.height,
                byte_len: bytes.len(),
                sha256: hash,
            },
            Evidence::DomSnapshot {
                id: format!("dom-{}", &dom_hash[..16]),
                byte_len: dom_text.len(),
                sha256: dom_hash,
            },
            Evidence::Accessibility {
                id: format!("ax-{}", now()),
                node_count,
                text: accessibility_text.chars().take(256 * 1024).collect(),
            },
        ];
        while let Some(event) = self.events.pop_front() {
            let method = event.get("method").and_then(Value::as_str).unwrap_or("");
            if matches!(
                method,
                "Runtime.consoleAPICalled" | "Runtime.exceptionThrown" | "Log.entryAdded"
            ) {
                let message = event.to_string();
                items.push(Evidence::Console {
                    level: if method == "Runtime.consoleAPICalled" {
                        ConsoleLevel::Info
                    } else {
                        ConsoleLevel::Error
                    },
                    message: message.chars().take(4096).collect(),
                });
            } else if method == "Network.loadingFailed"
                || (method == "Network.responseReceived"
                    && event
                        .pointer("/params/response/status")
                        .and_then(Value::as_u64)
                        .is_some_and(|s| s >= 400))
            {
                items.push(Evidence::NetworkFailure {
                    method: "GET".into(),
                    path: event
                        .pointer("/params/response/url")
                        .or_else(|| event.pointer("/params/requestId"))
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .chars()
                        .take(2048)
                        .collect(),
                    status: event
                        .pointer("/params/response/status")
                        .and_then(Value::as_u64)
                        .map(|v| v as u16),
                    error: event
                        .pointer("/params/errorText")
                        .and_then(Value::as_str)
                        .unwrap_or("request failed")
                        .into(),
                });
            }
        }
        if items.len() > crate::MAX_EVIDENCE_ITEMS {
            items.truncate(crate::MAX_EVIDENCE_ITEMS);
        }
        Ok(BrowserEvidence {
            items,
            screenshot_data_url: Some(format!("data:image/png;base64,{encoded}")),
            dom_text,
            accessibility_text,
        })
    }
}

impl Drop for BrowserRuntime {
    fn drop(&mut self) {
        let _ = self.command("Browser.close", json!({}));
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.profile);
    }
}
fn reserve_debug_port() -> Result<u16, PreviewError> {
    let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|_| PreviewError::PortDenied)?;
    Ok(l.local_addr().map_err(|_| PreviewError::PortDenied)?.port())
}
fn now() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
fn button_name(b: PointerButton) -> &'static str {
    match b {
        PointerButton::Primary => "left",
        PointerButton::Auxiliary => "middle",
        PointerButton::Secondary => "right",
    }
}
fn key_name(k: SafeKey) -> &'static str {
    match k {
        SafeKey::Enter => "Enter",
        SafeKey::Tab => "Tab",
        SafeKey::Escape => "Escape",
        SafeKey::Backspace => "Backspace",
        SafeKey::ArrowUp => "ArrowUp",
        SafeKey::ArrowDown => "ArrowDown",
        SafeKey::ArrowLeft => "ArrowLeft",
        SafeKey::ArrowRight => "ArrowRight",
        SafeKey::Home => "Home",
        SafeKey::End => "End",
        SafeKey::PageUp => "PageUp",
        SafeKey::PageDown => "PageDown",
        SafeKey::Space => " ",
    }
}
