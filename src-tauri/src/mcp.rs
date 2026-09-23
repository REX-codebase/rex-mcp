//! MCP server management: attach external MCP servers, probe them, and
//! toggle individual tools per server.
//!
//! Servers are configured by the user (name + command). The store persists
//! them under the REX config dir (`mcp_servers.json`). Probing spawns the
//! server, performs the MCP initialize handshake, and lists tools — with a
//! real timeout so a hung server cannot hang the UI.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServer {
    pub id: String,
    pub name: String,
    pub command: String,
    pub enabled: bool,
    pub tools: Vec<McpTool>,
    /// Last probe result: None = never probed.
    pub last_probe_ok: Option<bool>,
    pub last_probe_error: Option<String>,
    pub last_probe_at_ms: Option<u64>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoreData {
    servers: Vec<McpServer>,
}

pub struct McpStore {
    path: PathBuf,
    inner: Mutex<StoreData>,
}

impl McpStore {
    pub fn new(config_dir: &Path) -> Result<Self, String> {
        let path = config_dir.join("mcp_servers.json");
        let data = if path.exists() {
            let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            serde_json::from_str(&raw).map_err(|e| e.to_string())?
        } else {
            StoreData::default()
        };
        Ok(McpStore {
            path,
            inner: Mutex::new(data),
        })
    }

    fn save(&self) -> Result<(), String> {
        let data = self.inner.lock().map_err(|e| e.to_string())?;
        let raw = serde_json::to_string_pretty(&*data).map_err(|e| e.to_string())?;
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, raw).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<McpServer>, String> {
        let data = self.inner.lock().map_err(|e| e.to_string())?;
        Ok(data.servers.clone())
    }

    pub fn add(&self, name: &str, command: &str) -> Result<McpServer, String> {
        let name = name.trim();
        let command = command.trim();
        if name.is_empty() {
            return Err("server name is required".to_string());
        }
        if command.is_empty() {
            return Err("server command is required".to_string());
        }
        let id = format!(
            "mcp-{}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_millis(),
            std::process::id()
        );
        let server = McpServer {
            id: id.clone(),
            name: name.to_string(),
            command: command.to_string(),
            enabled: true,
            tools: Vec::new(),
            last_probe_ok: None,
            last_probe_error: None,
            last_probe_at_ms: None,
        };
        {
            let mut data = self.inner.lock().map_err(|e| e.to_string())?;
            data.servers.push(server.clone());
        }
        self.save()?;
        Ok(server)
    }

    pub fn remove(&self, id: &str) -> Result<(), String> {
        {
            let mut data = self.inner.lock().map_err(|e| e.to_string())?;
            let before = data.servers.len();
            data.servers.retain(|s| s.id != id);
            if data.servers.len() == before {
                return Err("server not found".to_string());
            }
        }
        self.save()?;
        Ok(())
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), String> {
        {
            let mut data = self.inner.lock().map_err(|e| e.to_string())?;
            let s = data
                .servers
                .iter_mut()
                .find(|s| s.id == id)
                .ok_or("server not found")?;
            s.enabled = enabled;
        }
        self.save()?;
        Ok(())
    }

    pub fn set_tool_enabled(
        &self,
        server_id: &str,
        tool_name: &str,
        enabled: bool,
    ) -> Result<(), String> {
        {
            let mut data = self.inner.lock().map_err(|e| e.to_string())?;
            let s = data
                .servers
                .iter_mut()
                .find(|s| s.id == server_id)
                .ok_or("server not found")?;
            let t = s
                .tools
                .iter_mut()
                .find(|t| t.name == tool_name)
                .ok_or("tool not found")?;
            t.enabled = enabled;
        }
        self.save()?;
        Ok(())
    }

    /// Probe a server: spawn it, handshake, list tools. Updates the server's
    /// tool list (preserving per-tool enabled flags) and probe status.
    pub fn probe(&self, id: &str) -> Result<McpServer, String> {
        let command = {
            let data = self.inner.lock().map_err(|e| e.to_string())?;
            data.servers
                .iter()
                .find(|s| s.id == id)
                .ok_or("server not found")?
                .command
                .clone()
        };
        let result = probe_server(&command);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_millis() as u64;
        {
            let mut data = self.inner.lock().map_err(|e| e.to_string())?;
            let s = data
                .servers
                .iter_mut()
                .find(|s| s.id == id)
                .ok_or("server not found")?;
            match result {
                Ok(tools) => {
                    // Preserve enabled flags for tools we've seen before.
                    let prev: std::collections::HashMap<String, bool> = s
                        .tools
                        .iter()
                        .map(|t| (t.name.clone(), t.enabled))
                        .collect();
                    s.tools = tools
                        .into_iter()
                        .map(|(name, description)| McpTool {
                            enabled: prev.get(&name).copied().unwrap_or(true),
                            name,
                            description,
                        })
                        .collect();
                    s.last_probe_ok = Some(true);
                    s.last_probe_error = None;
                }
                Err(e) => {
                    s.last_probe_ok = Some(false);
                    s.last_probe_error = Some(e);
                }
            }
            s.last_probe_at_ms = Some(now_ms);
        }
        self.save()?;
        let data = self.inner.lock().map_err(|e| e.to_string())?;
        data.servers
            .iter()
            .find(|s| s.id == id)
            .cloned()
            .ok_or("server not found".to_string())
    }
}

/// Probe an MCP server over stdio with a real timeout.
/// Returns the list of (tool_name, description).
fn probe_server(server_command: &str) -> Result<Vec<(String, String)>, String> {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::Duration;

    let mut parts = server_command.split_whitespace();
    let bin = parts
        .next()
        .ok_or("server command is empty")?
        .to_string();
    let args: Vec<String> = parts.map(|s| s.to_string()).collect();

    let mut child = Command::new(&bin)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot spawn {bin}: {e}"))?;

    let mut stdin = child.stdin.take().ok_or("server stdin unavailable")?;
    let stdout = child.stdout.take().ok_or("server stdout unavailable")?;

    // Read responses on a helper thread so we can enforce a timeout.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break, // EOF
                Ok(_) if !line.trim().is_empty() => {
                    if tx.send(line.clone()).is_err() {
                        break;
                    }
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
    });

    let mut next_id = 0u64;
    // Helper to send a JSON-RPC message (notification or request).
    macro_rules! send_msg {
        ($msg:expr) => {{
            writeln!(stdin, "{}", $msg).map_err(|e| format!("write failed: {e}"))?;
            stdin.flush().map_err(|e| format!("flush failed: {e}"))?;
        }};
    }
    // Helper to send a request and wait for the response with timeout.
    macro_rules! request {
        ($method:expr, $params:expr) => {{
            next_id += 1;
            let msg = serde_json::json!({
                "jsonrpc": "2.0",
                "id": next_id,
                "method": $method,
                "params": $params,
            });
            send_msg!(msg);
            // Real timeout: the reader thread blocks, we don't.
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(line) => {
                    let v: serde_json::Value = serde_json::from_str(&line)
                        .map_err(|e| format!("bad JSON from server: {e}"))?;
                    // Verify the response ID matches.
                    if v.get("id").and_then(|i| i.as_u64()) != Some(next_id) {
                        Err("mismatched response ID".to_string())
                    } else if let Some(err) = v.get("error") {
                        Err(format!("server error: {err}"))
                    } else {
                        Ok(v)
                    }
                }
                Err(_) => Err("server did not answer within 10s".to_string()),
            }
        }};
    }

    let result = (|| -> Result<Vec<(String, String)>, String> {
        // 1. Initialize.
        let hello = request!(
            "initialize",
            serde_json::json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "rex-harness", "version": "0.1.0" },
            })
        )?;
        // 2. Send initialized notification (no response expected).
        let notif = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
        });
        send_msg!(notif);

        // 3. List tools.
        let tools_resp = request!("tools/list", serde_json::json!({}))?;
        let tools = tools_resp
            .get("result")
            .and_then(|r| r.get("tools"))
            .and_then(|t| t.as_array())
            .ok_or("bad tools/list response")?;

        let mut out = Vec::new();
        for t in tools {
            let name = t
                .get("name")
                .and_then(|n| n.as_str())
                .ok_or("tool missing name")?
                .to_string();
            let description = t
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_string();
            out.push((name, description));
        }
        let _ = hello;
        Ok(out)
    })();

    // Always kill the child; this is a probe, not a session.
    let _ = child.kill();
    let _ = child.wait();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rex-mcp-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn add_remove_toggle() {
        let dir = temp();
        let store = McpStore::new(&dir).unwrap();

        let s = store.add("Test Server", "python -m test_server").unwrap();
        assert!(s.enabled);
        assert!(s.tools.is_empty());

        let list = store.list().unwrap();
        assert_eq!(list.len(), 1);

        store.set_enabled(&s.id, false).unwrap();
        let list = store.list().unwrap();
        assert!(!list[0].enabled);

        store.remove(&s.id).unwrap();
        let list = store.list().unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn rejects_empty() {
        let dir = temp();
        let store = McpStore::new(&dir).unwrap();
        assert!(store.add("", "cmd").is_err());
        assert!(store.add("name", "").is_err());
    }
}
