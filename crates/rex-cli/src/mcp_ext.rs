//! External MCP servers: connect REX to third-party MCP servers over stdio.
//!
//! Config lives in `.rex/mcp.json` inside the workspace (next to
//! `.rex/policy.json`), falling back to `$REX_STATE_DIR/mcp.json`:
//!
//! ```json
//! { "servers": {
//!     "fetch": { "command": "uvx", "args": ["mcp-server-fetch"] },
//!     "gh":    { "command": "npx", "args": ["-y", "my-mcp-server"],
//!                "env": { "API_TOKEN": "secret-in-cleartext" } }
//! } }
//! ```
//!
//! `env` values are stored in cleartext in the config file: prefer passing
//! real secrets through the parent process environment and only use `env`
//! for non-secret settings. A server is spawned per command; nothing is
//! cached between invocations.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// Protocol version this client speaks (matches the bundled server).
pub const CLIENT_PROTOCOL_VERSION: &str = "2025-11-25";

/// How long to wait for any single server response before giving up.
const RPC_TIMEOUT: Duration = Duration::from_secs(30);

/// One configured external server.
#[derive(Debug, Clone)]
pub struct ExtServer {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
}

/// A tool advertised by an external server.
#[derive(Debug, Clone)]
pub struct ExtTool {
    pub server: String,
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Load the external-server config. Workspace config wins; the state-dir
/// config is the fallback. Returns the config path used and the servers.
pub fn load_config(
    workspace: Option<&Path>,
    state_dir: &Path,
) -> Result<(PathBuf, Vec<ExtServer>), String> {
    let mut candidates = Vec::new();
    if let Some(ws) = workspace {
        candidates.push(ws.join(".rex").join("mcp.json"));
    } else if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join(".rex").join("mcp.json"));
    }
    candidates.push(state_dir.join("mcp.json"));

    for path in &candidates {
        if path.exists() {
            return Ok((path.clone(), parse_config(path)?));
        }
    }
    Err(format!(
        "no MCP config found; create one at {} or {}",
        candidates
            .first()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| ".rex/mcp.json".to_string()),
        state_dir.join("mcp.json").display()
    ))
}

fn parse_config(path: &Path) -> Result<Vec<ExtServer>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| format!("{} is not valid JSON: {e}", path.display()))?;
    let servers = v
        .get("servers")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{} needs a top-level \"servers\" object", path.display()))?;
    let mut out = Vec::with_capacity(servers.len());
    for (name, spec) in servers {
        if name.trim().is_empty() || name.contains(char::is_whitespace) {
            return Err(format!("bad server name {name:?} in {}", path.display()));
        }
        let command = spec
            .get("command")
            .and_then(Value::as_str)
            .filter(|c| !c.trim().is_empty())
            .ok_or_else(|| format!("server {name:?} needs a \"command\" string"))?;
        let args = match spec.get("args") {
            None => Vec::new(),
            Some(a) => a
                .as_array()
                .ok_or_else(|| format!("server {name:?}: \"args\" must be an array of strings"))?
                .iter()
                .map(|x| {
                    x.as_str().map(str::to_string).ok_or_else(|| {
                        format!("server {name:?}: \"args\" must be an array of strings")
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
        };
        let env = match spec.get("env") {
            None => HashMap::new(),
            Some(e) => e
                .as_object()
                .ok_or_else(|| format!("server {name:?}: \"env\" must be an object"))?
                .iter()
                .map(|(k, val)| {
                    val.as_str()
                        .map(|s| (k.clone(), s.to_string()))
                        .ok_or_else(|| {
                            format!("server {name:?}: env value for {k:?} must be a string")
                        })
                })
                .collect::<Result<HashMap<_, _>, _>>()?,
        };
        out.push(ExtServer {
            name: name.clone(),
            command: command.to_string(),
            args,
            env,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// A live stdio session with one external MCP server.
pub struct McpExtClient {
    server_name: String,
    child: Child,
    stdin: ChildStdin,
    reader: std::sync::Arc<std::sync::Mutex<BufReader<std::process::ChildStdout>>>,
    next_id: u64,
    negotiated_version: String,
}

impl McpExtClient {
    /// Spawn the server and complete the initialize handshake. A version
    /// mismatch is reported on stderr but not fatal: the negotiated version
    /// is recorded and every call carries it back for inspection.
    pub fn spawn(srv: &ExtServer) -> Result<Self, String> {
        let mut child = Command::new(&srv.command)
            .args(&srv.args)
            .envs(&srv.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                format!(
                    "cannot spawn MCP server {:?} ({}): {e}",
                    srv.name, srv.command
                )
            })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| format!("MCP server {:?}: stdin unavailable", srv.name))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| format!("MCP server {:?}: stdout unavailable", srv.name))?;
        let mut client = Self {
            server_name: srv.name.clone(),
            child,
            stdin,
            reader: std::sync::Arc::new(std::sync::Mutex::new(BufReader::new(stdout))),
            next_id: 1,
            negotiated_version: String::new(),
        };
        let hello = client.request(
            "initialize",
            json!({
                "protocolVersion": CLIENT_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "rex", "version": env!("CARGO_PKG_VERSION") },
            }),
        )?;
        let version = hello
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if version != CLIENT_PROTOCOL_VERSION {
            eprintln!(
                "rex: warning: MCP server {:?} negotiated protocol version {version:?} (client speaks {CLIENT_PROTOCOL_VERSION})",
                srv.name
            );
        }
        client.negotiated_version = version;
        client.notify("notifications/initialized", json!({}))?;
        Ok(client)
    }

    pub fn negotiated_version(&self) -> &str {
        &self.negotiated_version
    }

    /// List the server's tools (first page; pagination cursors are followed
    /// up to a bounded depth).
    pub fn list_tools(&mut self) -> Result<Vec<ExtTool>, String> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..8 {
            let mut params = json!({});
            if let Some(c) = &cursor {
                params = json!({ "cursor": c });
            }
            let result = self.request("tools/list", params)?;
            let page = result
                .get("tools")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    format!(
                        "MCP server {:?}: tools/list returned no tools array",
                        self.server_name
                    )
                })?;
            for t in page {
                tools.push(ExtTool {
                    server: self.server_name.clone(),
                    name: t
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                        .to_string(),
                    description: t
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    input_schema: t
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({"type": "object"})),
                });
            }
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
        Ok(tools)
    }

    /// Call one tool. The raw result object is returned; `isError` stays
    /// visible to the caller instead of being hidden.
    pub fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value, String> {
        self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        let line = json!({"jsonrpc": "2.0", "method": method, "params": params});
        writeln!(self.stdin, "{line}").map_err(|e| {
            format!(
                "MCP server {:?}: cannot write {method}: {e}",
                self.server_name
            )
        })?;
        self.stdin
            .flush()
            .map_err(|e| format!("MCP server {:?}: flush failed: {e}", self.server_name))?;
        Ok(())
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{line}").map_err(|e| {
            format!(
                "MCP server {:?}: cannot write {method}: {e}",
                self.server_name
            )
        })?;
        self.stdin
            .flush()
            .map_err(|e| format!("MCP server {:?}: flush failed: {e}", self.server_name))?;
        let mut buf = String::new();
        read_line_timeout(&self.reader, &mut buf, RPC_TIMEOUT).map_err(|e| {
            let _ = self.child.kill();
            format!(
                "MCP server {:?}: no response to {method}: {e}",
                self.server_name
            )
        })?;
        let v: Value = serde_json::from_str(&buf).map_err(|e| {
            format!(
                "MCP server {:?}: bad JSON response to {method}: {e}",
                self.server_name
            )
        })?;
        if v.get("id") != Some(&json!(id)) {
            return Err(format!(
                "MCP server {:?}: response id mismatch for {method}",
                self.server_name
            ));
        }
        if let Some(err) = v.get("error") {
            return Err(format!(
                "MCP server {:?}: {method} failed: {err}",
                self.server_name
            ));
        }
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    }
}

impl Drop for McpExtClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `BufRead::read_line` has no timeout; run it on a thread and bound the
/// wait. On timeout the caller kills the child, which EOFs the pipe and
/// lets the stranded thread exit on its own.
fn read_line_timeout(
    reader: &std::sync::Arc<std::sync::Mutex<BufReader<std::process::ChildStdout>>>,
    buf: &mut String,
    timeout: Duration,
) -> Result<(), String> {
    let reader = std::sync::Arc::clone(reader);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let res = reader
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .read_line(&mut line)
            .map(|_| line);
        let _ = tx.send(res);
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(line)) => {
            if line.is_empty() {
                return Err("server closed stdout".to_string());
            }
            buf.push_str(&line);
            Ok(())
        }
        Ok(Err(e)) => Err(format!("read error: {e}")),
        Err(_) => Err(format!("timed out after {}s", timeout.as_secs())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_requires_servers_object() {
        let dir = std::env::temp_dir().join("rex-mcp-cfg-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bad = dir.join("mcp.json");
        std::fs::write(&bad, "{}").unwrap();
        assert!(parse_config(&bad).is_err());
        std::fs::write(&bad, "{\"servers\": {\"\": {\"command\": \"x\"}}}").unwrap();
        assert!(parse_config(&bad).is_err());
        std::fs::write(&bad, "{\"servers\": {\"a\": {}}}").unwrap();
        assert!(parse_config(&bad).is_err());
        std::fs::write(
            &bad,
            "{\"servers\": {\"b\": {\"command\": \"uvx\", \"args\": [\"s\"], \"env\": {\"K\": \"V\"}}}}",
        )
        .unwrap();
        let servers = parse_config(&bad).unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].command, "uvx");
        assert_eq!(servers[0].args, vec!["s".to_string()]);
        assert_eq!(servers[0].env.get("K").map(String::as_str), Some("V"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
