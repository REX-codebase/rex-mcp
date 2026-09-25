//! Minimal MCP stdio client for the desktop shell.
//!
//! The UI supervises host-driven REX tasks through the exact protocol hosts
//! use - a spawned `rex-mcp` child over newline-delimited JSON-RPC - never
//! through a private side channel into the daemon. A host-opened task and a
//! UI-opened task therefore share one authority path, one audit chain and
//! one human-stop fence.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Wire protocol this client speaks. Must match the server's negotiated
/// version; a mismatch is a hard error, never a silent downgrade.
pub const CLIENT_PROTOCOL_VERSION: &str = "2025-11-25";

/// A spawned `rex-mcp` child with the handshake completed.
pub struct McpStdioClient {
    child: Child,
    stdin: ChildStdin,
    replies: Receiver<Result<String, String>>,
    reader: Option<JoinHandle<()>>,
    next_id: u64,
}

impl McpStdioClient {
    /// Spawn the server and complete initialize + initialized. The state
    /// dir and workspace mirror the host environment variables exactly.
    pub fn spawn(bin: &Path, state_dir: &Path, workspace: &Path) -> Result<Self, String> {
        let mut child = Command::new(bin)
            .env("REX_STATE_DIR", state_dir)
            .env("REX_WORKSPACE", workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot spawn rex-mcp at {}: {e}", bin.display()))?;
        let stdin = child.stdin.take().ok_or("rex-mcp stdin unavailable")?;
        let stdout = child.stdout.take().ok_or("rex-mcp stdout unavailable")?;
        let (tx, replies) = mpsc::channel();
        let reader = thread::spawn(move || {
            let mut stdout = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                match stdout.read_line(&mut line) {
                    Ok(0) => {
                        let _ = tx.send(Err("rex-mcp closed stdout unexpectedly".into()));
                        break;
                    }
                    Ok(_) => {
                        if tx.send(Ok(line)).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(format!("rex-mcp read: {e}")));
                        break;
                    }
                }
            }
        });
        let mut client = Self {
            child,
            stdin,
            replies,
            reader: Some(reader),
            next_id: 0,
        };
        let hello = client.request(
            json!({
                "protocolVersion": CLIENT_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "rex-shell", "version": env!("CARGO_PKG_VERSION") },
            }),
            "initialize",
        )?;
        if hello.get("result").is_none() {
            let _ = client.child.kill();
            return Err(format!("rex-mcp refused initialize: {hello}"));
        }
        let negotiated = hello["result"]["protocolVersion"].as_str();
        if negotiated != Some(CLIENT_PROTOCOL_VERSION) {
            let _ = client.child.kill();
            return Err(format!(
                "rex-mcp negotiated unsupported protocol version {negotiated:?}; expected {CLIENT_PROTOCOL_VERSION}"
            ));
        }
        client.notify("notifications/initialized", json!({}))?;
        Ok(client)
    }

    /// Call one MCP tool and return its structured content. Protocol and
    /// tool errors surface as Err with the server's own code and message;
    /// nothing is swallowed or retried silently.
    pub fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value, String> {
        let response = self.request(
            json!({ "name": name, "arguments": arguments }),
            "tools/call",
        )?;
        if let Some(err) = response.get("error") {
            return Err(format!("rex-mcp error: {err}"));
        }
        let result = response
            .get("result")
            .cloned()
            .ok_or("rex-mcp reply missing result")?;
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            return Err(format!("tool {name} failed: {result}"));
        }
        result
            .get("structuredContent")
            .cloned()
            .ok_or_else(|| format!("tool {name} returned no structured content"))
    }

    fn request(&mut self, params: Value, method: &str) -> Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        // Read until the matching id; notifications from the server are skipped.
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let line = match self.replies.recv_timeout(remaining) {
                Ok(Ok(line)) => line,
                Ok(Err(error)) => return Err(error),
                Err(RecvTimeoutError::Timeout) => {
                    return Err(format!("rex-mcp reply timed out for {method}"));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("rex-mcp stdout reader stopped unexpectedly".into());
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            let msg: Value =
                serde_json::from_str(&line).map_err(|e| format!("rex-mcp bad json: {e}"))?;
            if msg.get("id").and_then(Value::as_u64) == Some(id) {
                return Ok(msg);
            }
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }

    fn send(&mut self, msg: &Value) -> Result<(), String> {
        serde_json::to_writer(&mut self.stdin, msg).map_err(|e| format!("rex-mcp write: {e}"))?;
        self.stdin
            .write_all(b"\n")
            .map_err(|e| format!("rex-mcp write: {e}"))?;
        self.stdin
            .flush()
            .map_err(|e| format!("rex-mcp flush: {e}"))
    }
}

impl Drop for McpStdioClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// One-shot tool call: spawn, handshake, call, drop. Supervision polling is
/// low-frequency, so a fresh process per call keeps no protocol state alive
/// between requests and cannot strand a half-open session.
pub fn call_tool_once(
    bin: &Path,
    state_dir: &Path,
    workspace: &Path,
    name: &str,
    arguments: Value,
) -> Result<Value, String> {
    let mut client = McpStdioClient::spawn(bin, state_dir, workspace)?;
    client.call_tool(name, arguments)
}

/// Same default the `rex-mcp` binary uses, so the shell supervises the same
/// durable store hosts write to.
pub fn default_state_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".rex")
        .join("harness")
}

/// Locate the server binary: explicit override first, then the sibling of
/// the current executable (cargo target dir or an installed bundle).
pub fn rex_mcp_bin() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("REX_MCP_BIN") {
        return Ok(PathBuf::from(path));
    }
    let exe = std::env::current_exe().map_err(|e| format!("current exe unknown: {e}"))?;
    let sibling = exe
        .parent()
        .ok_or("current exe has no parent dir")?
        .join(if cfg!(windows) {
            "rex-mcp.exe"
        } else {
            "rex-mcp"
        });
    if sibling.exists() {
        Ok(sibling)
    } else {
        Err(format!(
            "rex-mcp binary not found next to {}; set REX_MCP_BIN",
            exe.display()
        ))
    }
}
