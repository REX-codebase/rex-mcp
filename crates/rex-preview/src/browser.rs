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
    /// Canonical hash of the captured project's supported local source tree.
    pub source_sha256: String,
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
    reduced_motion: Option<bool>,
    javascript_enabled: Option<bool>,
    script_probe_id: Option<String>,
    cursor_x: f32,
    cursor_y: f32,
    allowed_port: u16,
    allowed_host: String,
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
            reduced_motion: None,
            javascript_enabled: None,
            script_probe_id: None,
            cursor_x: 0.0,
            cursor_y: 0.0,
            allowed_port: url::Url::parse(url)
                .map_err(|_| PreviewError::UrlDenied)?
                .port()
                .ok_or(PreviewError::UrlDenied)?,
            allowed_host: url::Url::parse(url)
                .map_err(|_| PreviewError::UrlDenied)?
                .host_str()
                .ok_or(PreviewError::UrlDenied)?
                .to_string(),
        };
        b.command(
            "Emulation.setDeviceMetricsOverride",
            json!({
                "width":b.width,"height":b.height,"deviceScaleFactor":b.scale,"mobile":false
            }),
        )?;
        b.command("Page.enable", json!({}))?;
        b.command("Runtime.enable", json!({}))?;
        b.command("Network.enable", json!({"maxTotalBufferSize": 1048576}))?;
        // Intercept subresources instead of a wildcard network block: the
        // wildcard also blocks the local JS/CSS that the preview must run.
        b.command(
            "Fetch.enable",
            json!({"patterns":[{"urlPattern":"*","requestStage":"Request"}]}),
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
                if v.get("method").and_then(Value::as_str) == Some("Fetch.requestPaused") {
                    let request_id = v
                        .pointer("/params/requestId")
                        .and_then(Value::as_str)
                        .ok_or(PreviewError::CommandDenied)?;
                    let request_url = v
                        .pointer("/params/request/url")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let allowed = crate::validate_local_url(request_url, self.allowed_port)
                        .is_ok_and(|u| u.host_str() == Some(self.allowed_host.as_str()));
                    let request = if allowed {
                        "Fetch.continueRequest"
                    } else {
                        "Fetch.failRequest"
                    };
                    let params = if allowed {
                        json!({"requestId":request_id})
                    } else {
                        json!({"requestId":request_id,"errorReason":"BlockedByClient"})
                    };
                    let sub_id = self.next_id;
                    self.next_id += 1;
                    self.socket
                        .send(tungstenite::Message::Text(
                            json!({"id":sub_id,"method":request,"params":params})
                                .to_string()
                                .into(),
                        ))
                        .map_err(|_| PreviewError::NotRunning)?;
                    continue;
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
                // A slash-prefixed path is not enough: URL parsing may treat
                // backslashes or encoded authority separators as a host change.
                crate::validate_local_url(
                    target.as_str(),
                    base.port().ok_or(PreviewError::UrlDenied)?,
                )?;
                self.command("Page.navigate", json!({"url":target.as_str()}))?;
                self.wait_loaded()?;
                self.verify_conditions()?;
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
            BrowserAction::WaitForAnimations { timeout_ms } => {
                self.wait_for_animations(*timeout_ms)?;
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
                self.verify_conditions()?;
            }
            BrowserAction::SetReducedMotion { enabled } => {
                self.command(
                    "Emulation.setEmulatedMedia",
                    json!({
                        "features":[{"name":"prefers-reduced-motion",
                        "value":if *enabled {"reduce"} else {"no-preference"}}]
                    }),
                )?;
                self.reduced_motion = Some(*enabled);
                self.verify_conditions()?;
            }
            BrowserAction::SetJavaScript { enabled } => {
                // Re-enabling in an already disabled target can return CDP
                // success while the page remains inert. Fail closed and ask
                // for a fresh preview rather than claiming the on state.
                if *enabled && self.javascript_enabled == Some(false) {
                    return Err(PreviewError::ConditionNotApplied);
                }
                if self.script_probe_id.is_none() {
                    let installed = self.command("Page.addScriptToEvaluateOnNewDocument", json!({
                        "source":"addEventListener('DOMContentLoaded',()=>document.documentElement.setAttribute('data-rex-script-probe','ran'))"
                    }))?;
                    self.script_probe_id = Some(
                        installed
                            .pointer("/result/identifier")
                            .and_then(Value::as_str)
                            .ok_or(PreviewError::ConditionNotApplied)?
                            .to_string(),
                    );
                }
                self.command(
                    "Emulation.setScriptExecutionDisabled",
                    json!({"value": !enabled}),
                )?;
                self.javascript_enabled = Some(*enabled);
                // Fresh local document, not the already-executed page.
                self.events.clear();
                let mut fresh = url::Url::parse(base_url).map_err(|_| PreviewError::UrlDenied)?;
                fresh
                    .query_pairs_mut()
                    .append_pair("rex_script_condition", if *enabled { "on" } else { "off" });
                self.command("Page.navigate", json!({"url":fresh.as_str()}))?;
                self.wait_loaded()?;
                self.verify_conditions()?;
            }
        }
        Ok(())
    }

    fn wait_for_animations(&mut self, timeout_ms: u32) -> Result<(), PreviewError> {
        // The host requests this explicitly after an input, rather than all
        // captures sleeping or mislabeling an in-flight frame as settled.
        // Finite Web Animations (including CSS transitions) are observable;
        // arbitrary JS drawing and infinite ambient loops are not certified.
        let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
        loop {
            let result = self.command("Runtime.evaluate", json!({
                "expression":"document.getAnimations({subtree:true}).filter(a=>a.playState==='running' && Number.isFinite(a.effect?.getComputedTiming().endTime)).length",
                "returnByValue":true,
            }))?;
            let count = result
                .pointer("/result/result/value")
                .and_then(Value::as_u64)
                .ok_or(PreviewError::ConditionNotApplied)?;
            if count == 0 {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(PreviewError::AnimationNotSettled);
            }
            thread::sleep(Duration::from_millis(40));
        }
    }

    fn conditions_match(
        probe: &Value,
        width: u16,
        height: u16,
        scale: f32,
        reduced_motion: Option<bool>,
    ) -> bool {
        probe["width"].as_u64() == Some(width as u64)
            && probe["height"].as_u64() == Some(height as u64)
            && probe["dpr"]
                .as_f64()
                .is_some_and(|dpr| (dpr - scale as f64).abs() < 0.01)
            && reduced_motion.is_none_or(|wanted| probe["reduced"].as_bool() == Some(wanted))
    }

    fn verify_conditions(&mut self) -> Result<(), PreviewError> {
        // A successful CDP command only confirms receipt, not adoption by the
        // page. Probe the browsing context that will supply the pixels.
        let result = self.command("Runtime.evaluate", json!({
            "expression":"JSON.stringify({width:innerWidth,height:innerHeight,dpr:devicePixelRatio,reduced:matchMedia('(prefers-reduced-motion: reduce)').matches})",
            "returnByValue":true
        }))?;
        let value = result
            .pointer("/result/result/value")
            .and_then(Value::as_str)
            .ok_or(PreviewError::ConditionNotApplied)?;
        let probe: Value =
            serde_json::from_str(value).map_err(|_| PreviewError::ConditionNotApplied)?;
        if !Self::conditions_match(
            &probe,
            self.width,
            self.height,
            self.scale,
            self.reduced_motion,
        ) {
            return Err(PreviewError::ConditionNotApplied);
        }
        if let Some(enabled) = self.javascript_enabled {
            // Runtime.evaluate can execute even with page script execution
            // disabled. The marker is installed for *new documents* and must
            // only appear when ordinary page scripts were allowed to start.
            let observed = self.command(
                "Runtime.evaluate",
                json!({
                    "expression":"document.documentElement.getAttribute('data-rex-script-probe')",
                    "returnByValue":true
                }),
            )?;
            let ran = observed
                .pointer("/result/result/value")
                .and_then(Value::as_str)
                == Some("ran");
            if ran != enabled {
                return Err(PreviewError::ConditionNotApplied);
            }
        }
        Ok(())
    }

    pub fn capture(&mut self, base_url: &str) -> Result<BrowserEvidence, PreviewError> {
        // Page controls can navigate without going through rex_preview_action.
        // A capture must not register pixels from an escaped preview origin.
        let base = url::Url::parse(base_url).map_err(|_| PreviewError::UrlDenied)?;
        let current = self.command(
            "Runtime.evaluate",
            json!({
                "expression":"location.href", "returnByValue":true
            }),
        )?;
        let current_url = current
            .pointer("/result/result/value")
            .and_then(Value::as_str)
            .ok_or(PreviewError::UrlDenied)?;
        let current_origin =
            crate::validate_local_url(current_url, base.port().ok_or(PreviewError::UrlDenied)?)?;
        if current_origin.host_str() != base.host_str() {
            return Err(PreviewError::UrlDenied);
        }
        self.verify_conditions()?;
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
        let decoder = png::Decoder::new(std::io::Cursor::new(&bytes));
        let info = decoder
            .read_info()
            .map_err(|_| PreviewError::EvidenceLimit)?;
        // The viewport evidence must describe the actual raster, not a
        // requested CDP size. A cropped capture is not desktop proof.
        let expected_width = (self.width as f32 * self.scale).round() as u32;
        let expected_height = (self.height as f32 * self.scale).round() as u32;
        if info.info().width != expected_width || info.info().height != expected_height {
            return Err(PreviewError::EvidenceLimit);
        }
        if near_uniform_png(&bytes)? {
            return Err(PreviewError::BlankFrame);
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
        // Browsers repair some malformed HTML; this checks observable failure,
        // not the author's intended structure.
        let body = self.command(
            "Runtime.evaluate",
            json!({
                "expression":"document.body?.innerText?.trim().length||0", "returnByValue":true
            }),
        )?;
        if body
            .pointer("/result/result/value")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            == 0
        {
            return Err(PreviewError::BrokenPage);
        }
        let dom_hash = format!("{:x}", Sha256::digest(dom_text.as_bytes()));
        let ax = self.command("Accessibility.getFullAXTree", json!({}))?;
        let accessibility_text =
            serde_json::to_string(ax.pointer("/result/nodes").unwrap_or(&Value::Null))
                .unwrap_or_default();
        let node_count = ax
            .pointer("/result/nodes")
            .and_then(Value::as_array)
            .map_or(0, |v| v.len() as u32);
        if self.events.iter().any(|event| {
            let local_resource_failed = event.get("method").and_then(Value::as_str)
                == Some("Network.responseReceived")
                && event
                    .pointer("/params/response/status")
                    .and_then(Value::as_u64)
                    .is_some_and(|s| s >= 400)
                && event
                    .pointer("/params/response/url")
                    .and_then(Value::as_str)
                    .is_some_and(|url| {
                        crate::validate_local_url(url, self.allowed_port).is_ok_and(|u| {
                            u.host_str() == Some(self.allowed_host.as_str())
                                && !u.path().ends_with("/favicon.ico")
                        })
                    });
            local_resource_failed
                || event.get("method").and_then(Value::as_str) == Some("Runtime.exceptionThrown")
                || (event.get("method").and_then(Value::as_str) == Some("Log.entryAdded")
                    && event.pointer("/params/entry/level").and_then(Value::as_str)
                        == Some("error")
                    && event
                        .pointer("/params/entry/source")
                        .and_then(Value::as_str)
                        != Some("network"))
        }) {
            return Err(PreviewError::BrokenPage);
        }
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
            source_sha256: String::new(), // supervisor binds a source tree around this capture
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
fn near_uniform_png(bytes: &[u8]) -> Result<bool, PreviewError> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .map_err(|_| PreviewError::EvidenceLimit)?;
    let mut buffer = vec![
        0;
        reader
            .output_buffer_size()
            .ok_or(PreviewError::EvidenceLimit)?
    ];
    let frame = reader
        .next_frame(&mut buffer)
        .map_err(|_| PreviewError::EvidenceLimit)?;
    let channels = match frame.color_type {
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        _ => return Ok(false),
    };
    let data = &buffer[..frame.buffer_size()];
    let stride = frame.width as usize * channels;
    let mut bins = std::collections::HashMap::new();
    let mut total = 0usize;
    // A regular grid rejects almost solid loading/error states, not varied
    // but irrelevant pages. Judgment must still inspect the rendered pixels.
    for y in (0..frame.height as usize).step_by((frame.height as usize / 50).max(1)) {
        for x in (0..frame.width as usize).step_by((frame.width as usize / 50).max(1)) {
            let i = y * stride + x * channels;
            if i + 2 < data.len() {
                *bins
                    .entry((data[i] / 32, data[i + 1] / 32, data[i + 2] / 32))
                    .or_insert(0usize) += 1;
                total += 1;
            }
        }
    }
    let dominant = bins.values().copied().max().unwrap_or(0);
    Ok(total > 0 && (bins.len() == 1 || dominant as f64 / total as f64 > 0.9995))
}
#[cfg(test)]
mod evidence_quality_tests {
    use super::*;
    fn png_of(pixels: Vec<u8>, width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, width, height);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&pixels).unwrap();
        }
        bytes
    }
    #[test]
    fn rejects_uniform_and_near_uniform_but_keeps_visible_design() {
        let w = 100;
        let h = 100;
        let mut flat = vec![235u8; w * h * 3];
        assert!(near_uniform_png(&png_of(flat.clone(), w as u32, h as u32)).unwrap());
        flat[0..3].copy_from_slice(&[20, 30, 40]);
        assert!(near_uniform_png(&png_of(flat.clone(), w as u32, h as u32)).unwrap());
        for y in 0..h {
            for x in 0..w / 3 {
                let i = (y * w + x) * 3;
                flat[i..i + 3].copy_from_slice(&[20, 30, 40]);
            }
        }
        assert!(!near_uniform_png(&png_of(flat, w as u32, h as u32)).unwrap());
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

#[cfg(test)]
mod condition_tests {
    use super::BrowserRuntime;
    use serde_json::json;

    #[test]
    fn accepts_only_effective_viewport_and_motion_probe() {
        let actual = json!({"width":390,"height":844,"dpr":2.0,"reduced":true});
        assert!(BrowserRuntime::conditions_match(
            &actual,
            390,
            844,
            2.0,
            Some(true)
        ));
        assert!(!BrowserRuntime::conditions_match(
            &actual,
            390,
            844,
            2.0,
            Some(false)
        ));
        assert!(!BrowserRuntime::conditions_match(
            &actual,
            390,
            800,
            2.0,
            Some(true)
        ));
        assert!(!BrowserRuntime::conditions_match(
            &actual,
            390,
            844,
            1.0,
            Some(true)
        ));
        assert!(!BrowserRuntime::conditions_match(
            &json!({"width":390,"height":844}),
            390,
            844,
            1.0,
            Some(true)
        ));
    }
}
