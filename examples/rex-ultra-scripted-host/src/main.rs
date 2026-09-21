//! No-network reference host for REX over stdio MCP.
//!
//! This is the whole contract a host agent needs: spawn `rex-mcp`, speak
//! newline-delimited JSON-RPC, and drive the custody and Ultra loops with
//! real evidence. It makes no network calls; inference stands in as
//! deterministic scripted content so the full pipeline can be qualified
//! offline (spec section K).
//!
//! Usage: cargo run -p rex-ultra-scripted-host [-- smoke|ultra]
//! The rex-mcp binary is located via REX_MCP_BIN, then workspace target
//! dirs (debug first, then release).

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};

/// Minimal newline-delimited JSON-RPC session with a spawned rex-mcp child.
struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    next_id: u64,
}

impl Session {
    fn start(bin: &Path, state: &Path, workspace: &Path) -> Result<Self, String> {
        let mut child = Command::new(bin)
            .env("REX_STATE_DIR", state)
            .env("REX_WORKSPACE", workspace)
            .env("REX_APPROVE_TASK_MUTATIONS", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot spawn {}: {e}", bin.display()))?;
        let stdin = child.stdin.take().ok_or("rex-mcp stdin unavailable")?;
        let stdout = BufReader::new(child.stdout.take().ok_or("rex-mcp stdout unavailable")?);
        Ok(Self { child, stdin, stdout, next_id: 0 })
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next_id += 1;
        let msg = json!({ "jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params });
        serde_json::to_writer(&mut self.stdin, &msg).map_err(|e| e.to_string())?;
        self.stdin.write_all(b"\n").map_err(|e| e.to_string())?;
        self.stdin.flush().map_err(|e| e.to_string())?;
        let mut line = String::new();
        self.stdout.read_line(&mut line).map_err(|e| e.to_string())?;
        serde_json::from_str(&line).map_err(|e| format!("invalid JSON-RPC response ({e}): {line}"))
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        let msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        serde_json::to_writer(&mut self.stdin, &msg).map_err(|e| e.to_string())?;
        self.stdin.write_all(b"\n").map_err(|e| e.to_string())?;
        self.stdin.flush().map_err(|e| e.to_string())
    }

    fn tool(&mut self, name: &str, args: Value) -> Result<Value, String> {
        let r = self.request("tools/call", json!({ "name": name, "arguments": args }))?;
        if let Some(err) = r.get("error") {
            return Err(format!("rex-mcp error: {err}"));
        }
        let result = r["result"].clone();
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            return Err(format!("tool {name} failed: {result}"));
        }
        Ok(result["structuredContent"].clone())
    }

    fn shutdown(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn rex_mcp_bin() -> Result<PathBuf, String> {
    if let Ok(p) = std::env::var("REX_MCP_BIN") {
        return Ok(PathBuf::from(p));
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    for profile in ["debug", "release"] {
        let bin = root.join("target").join(profile).join("rex-mcp");
        if bin.exists() {
            return Ok(bin);
        }
    }
    Err("rex-mcp binary not found; build it or set REX_MCP_BIN".into())
}

fn temp_dir(tag: &str) -> Result<PathBuf, String> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("rex-scripted-{tag}-{nanos:x}"));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "smoke".into());
    let result = match mode.as_str() {
        "smoke" => smoke(),
        other => Err(format!("unknown mode {other}; expected smoke")),
    };
    if let Err(e) = result {
        eprintln!("FAIL: {e}");
        std::process::exit(1);
    }
}

/// Handshake, tool surface, and one custody task lifecycle.
fn smoke() -> Result<(), String> {
    let bin = rex_mcp_bin()?;
    let state = temp_dir("state")?;
    let ws = temp_dir("ws")?;
    let mut s = Session::start(&bin, &state, &ws)?;

    let init = s.request(
        "initialize",
        json!({ "protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": { "name": "rex-ultra-scripted-host", "version": env!("CARGO_PKG_VERSION") } }),
    )?;
    if init["result"]["protocolVersion"] != "2025-11-25" {
        return Err(format!("handshake mismatch: {init}"));
    }
    s.notify("notifications/initialized", json!({}))?;
    println!("ok handshake: protocol 2025-11-25");

    let list = s.request("tools/list", json!({}))?;
    let tools = list["result"]["tools"].as_array().ok_or("tools/list missing tools")?;
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    if names.len() != 16 {
        return Err(format!("expected 16 tools, got {}: {names:?}", names.len()));
    }
    for t in ["rex_execute", "rex_ultra_open", "rex_ultra_submit", "rex_ultra_promote", "rex_proof"] {
        if !names.contains(&t) {
            return Err(format!("missing tool {t}"));
        }
    }
    println!("ok tools: 16 exposed, Ultra surface present");

    let ex = s.tool(
        "rex_execute",
        json!({ "request_id": "scripted-smoke", "task": "smoke the host loop",
            "host": "generic_agent", "operator_is_agent": true,
            "plan": [{ "instructions": "open", "acceptance": "task opens" }] }),
    )?;
    let task_id = ex["task_id"].as_str().ok_or("rex_execute returned no task id")?.to_string();
    let status = s.tool("rex_status", json!({ "task_id": task_id }))?;
    if status["state"] != "active" {
        return Err(format!("unexpected task state: {status}"));
    }
    println!("ok execute+status: task {task_id} active");

    let done = s.tool("rex_cancel", json!({ "task_id": task_id, "reason": "smoke complete" }))?;
    if done["state"] != "cancelled" {
        return Err(format!("unexpected cancel state: {done}"));
    }
    println!("ok cancel: durable terminal state");

    s.shutdown();
    std::fs::remove_dir_all(&state).ok();
    std::fs::remove_dir_all(&ws).ok();
    println!("PASS scripted-host smoke");
    Ok(())
}
