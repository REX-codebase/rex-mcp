//! External MCP client: handshake, tools/list and tools/call against a fake
//! MCP server. Skipped when python3 is unavailable (the fake server is a
//! small Python script).

use std::process::Command;

const FAKE_SERVER: &str = r#"#!/usr/bin/env python3
import json, sys
def reply(req_id, result):
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": req_id, "result": result}) + "\n")
    sys.stdout.flush()
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        msg = json.loads(line)
    except json.JSONDecodeError:
        continue
    method = msg.get("method")
    req_id = msg.get("id")
    if method == "initialize":
        reply(req_id, {"protocolVersion": "2025-11-25", "capabilities": {"tools": {}},
                       "serverInfo": {"name": "fake-mcp", "version": "0.1"}})
    elif method == "notifications/initialized":
        continue
    elif method == "tools/list":
        reply(req_id, {"tools": [{"name": "echo", "description": "Echo the input text back.",
                                  "inputSchema": {"type": "object"}}]})
    elif method == "tools/call":
        text = msg.get("params", {}).get("arguments", {}).get("text", "")
        reply(req_id, {"content": [{"type": "text", "text": "echo:" + str(text)}]})
"#;

fn python3_available() -> bool {
    Command::new("python3")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

struct Harness {
    dir: std::path::PathBuf,
}

impl Harness {
    fn setup() -> Option<Self> {
        if !python3_available() {
            return None;
        }
        let dir = std::env::temp_dir().join(format!(
            "rex-mcp-ext-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".rex")).unwrap();
        std::fs::write(dir.join("fake_mcp_server.py"), FAKE_SERVER).unwrap();
        std::fs::write(
            dir.join(".rex").join("mcp.json"),
            r#"{"servers": {"fake": {"command": "python3", "args": ["fake_mcp_server.py"]}}}"#,
        )
        .unwrap();
        Some(Self { dir })
    }

    fn rex(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_rex"))
            .args(args)
            .current_dir(&self.dir)
            .env("REX_STATE_DIR", self.dir.join("state"))
            .output()
            .expect("failed to run rex")
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn mcp_tools_lists_fake_server_tools() {
    let Some(h) = Harness::setup() else {
        eprintln!("skipping: python3 not available");
        return;
    };
    let out = h.rex(&["mcp", "tools", "fake"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "mcp tools failed: stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("echo"), "expected echo tool in: {stdout}");
}

#[test]
fn mcp_call_round_trips_arguments() {
    let Some(h) = Harness::setup() else {
        eprintln!("skipping: python3 not available");
        return;
    };
    let out = h.rex(&["mcp", "call", "fake", "echo", "--args", r#"{"text":"hi"}"#]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "mcp call failed: stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("echo:hi"), "expected echo:hi in: {stdout}");
}

#[test]
fn mcp_tools_reports_broken_server_honestly() {
    let Some(h) = Harness::setup() else {
        eprintln!("skipping: python3 not available");
        return;
    };
    std::fs::write(
        h.dir.join(".rex").join("mcp.json"),
        r#"{"servers": {"broken": {"command": "/nonexistent/binary-xyz"}}}"#,
    )
    .unwrap();
    let out = h.rex(&["mcp", "tools"]);
    assert!(!out.status.success(), "broken server should fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("broken"),
        "expected server name in error: {stderr}"
    );
}
