//! End-to-end proof: a clean machine can build rex-mcp, launch it over
//! stdio like a real MCP host, and drive a full task lifecycle.

use serde_json::{json, Value};
use std::io::Write;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct Session { child: Child, stdin: ChildStdin, stdout: std::io::BufReader<ChildStdout>, next_id: i64 }
impl Session {
    fn start(state: &std::path::Path, workspace: &std::path::Path, approve: bool) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_rex-mcp"))
            .env("REX_STATE_DIR", state).env("REX_WORKSPACE", workspace)
            .env("REX_APPROVE_TASK_MUTATIONS", if approve { "1" } else { "0" })
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
            .spawn().expect("spawn rex-mcp");
        Self { stdin: child.stdin.take().unwrap(),
            stdout: std::io::BufReader::new(child.stdout.take().unwrap()),
            next_id: 0, child }
    }
    fn call(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let msg = json!({"jsonrpc":"2.0","id":self.next_id,"method":method,"params":params});
        serde_json::to_writer(&mut self.stdin, &msg).unwrap();
        self.stdin.write_all(b"\n").unwrap(); self.stdin.flush().unwrap();
        let mut line = String::new();
        use std::io::BufRead;
        self.stdout.read_line(&mut line).unwrap();
        serde_json::from_str(&line).expect("valid JSON-RPC response")
    }
    fn tool(&mut self, name: &str, args: Value) -> Value {
        let r = self.call("tools/call", json!({"name":name,"arguments":args}));
        if r.get("error").is_some() { return r["error"].clone(); }
        r["result"]["structuredContent"].clone()
    }
}

#[test]
fn full_caller_driven_lifecycle_over_stdio() {
    let d = tempfile::tempdir().unwrap();
    let ws = d.path().join("ws"); std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("note.txt"), "hello rex").unwrap();
    let mut s = Session::start(&d.path().join("state"), &ws, true);

    // Handshake: MCP negotiation + capability discovery.
    let init = s.call("initialize", json!({"protocolVersion":"2025-11-25",
        "capabilities":{},"clientInfo":{"name":"e2e","version":"0"}}));
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
    let list = s.call("tools/list", json!({}));
    let names: Vec<&str> = list["result"]["tools"].as_array().unwrap()
        .iter().filter_map(|t| t["name"].as_str()).collect();
    assert_eq!(names.len(), 12);
    assert!(names.contains(&"rex_execute") && names.contains(&"rex_submit"));

    // Execute: durable task, frozen plan, first action issued.
    let ex = s.tool("rex_execute", json!({"request_id":"e2e-1","task":"prove the loop",
        "host":"claude_code","operator_is_agent":true,
        "plan":[{"instructions":"read note.txt","acceptance":"content known"},
                {"instructions":"append line","acceptance":"file updated"}]}));
    assert_eq!(ex["state"], "active");
    let task_id = ex["task_id"].as_str().unwrap().to_string();
    let epoch = ex["lease"]["epoch"].as_u64().unwrap();
    let action1 = ex["next"]["action_id"].as_str().unwrap().to_string();

    // Work under custody: read, edit, run.
    let read = s.tool("rex_read", json!({"task_id":task_id,"lease_epoch":epoch,"path":"note.txt"}));
    assert_eq!(read["content"], "hello rex");
    let sub1 = s.tool("rex_submit", json!({"task_id":task_id,"lease_epoch":epoch,
        "action_id":action1,"narrative":"read it"}));
    assert_eq!(sub1["accepted"], true);
    let action2 = sub1["next"]["action_id"].as_str().unwrap().to_string();
    let edit = s.tool("rex_edit", json!({"task_id":task_id,"lease_epoch":epoch,
        "path":"note.txt","expected":"hello rex","replacement":"hello rex\nworld"}));
    assert!(edit["bytes_written"].as_u64().unwrap() > 0);
    let run = s.tool("rex_run", json!({"task_id":task_id,"lease_epoch":epoch,
        "argv":["cat","note.txt"]}));
    assert!(run["stdout"].as_str().unwrap().contains("world"));

    // Complete: final submit passes custody gates; result carries output.
    let sub2 = s.tool("rex_submit", json!({"task_id":task_id,"lease_epoch":epoch,
        "action_id":action2,"narrative":"appended and verified"}));
    assert_eq!(sub2["state"], "completed");
    let res = s.tool("rex_result", json!({"task_id":task_id}));
    assert_eq!(res["output"], "appended and verified");

    // Events are durable and ordered.
    let events = s.tool("rex_events", json!({"task_id":task_id}));
    let seqs: Vec<u64> = events["events"].as_array().unwrap()
        .iter().filter_map(|e| e["seq"].as_u64()).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]));

    // Recovery: a brand-new server process sees the completed task.
    drop(s);
    let mut s2 = Session::start(&d.path().join("state"), &ws, true);
    s2.call("initialize", json!({"protocolVersion":"2025-11-25"}));
    let status = s2.tool("rex_status", json!({"task_id":task_id}));
    assert_eq!(status["state"], "completed");

    // A scope escape on a fresh task fails closed AND seizes custody: the
    // grant is quarantined, so even the matching submit is refused.
    let ex3 = s2.tool("rex_execute", json!({"request_id":"e2e-3","task":"escape attempt",
        "host":"generic_agent","operator_is_agent":true}));
    let t3 = ex3["task_id"].as_str().unwrap().to_string();
    let e3 = ex3["lease"]["epoch"].as_u64().unwrap();
    let denied = s2.tool("rex_read", json!({"task_id":t3,"lease_epoch":e3,"path":"../outside"}));
    assert_eq!(denied["data"]["code"], "scope_denied");
    let after = s2.tool("rex_submit", json!({"task_id":t3,"lease_epoch":e3,
        "action_id":ex3["next"]["action_id"],"narrative":"try anyway"}));
    assert_eq!(after["data"]["code"], "unauthorized");
}

#[test]
fn mutations_require_trusted_launcher_approval_over_stdio() {
    let d = tempfile::tempdir().unwrap();
    let ws = d.path().join("ws"); std::fs::create_dir_all(&ws).unwrap();
    let mut s = Session::start(&d.path().join("state"), &ws, false);
    s.call("initialize", json!({"protocolVersion":"2025-11-25"}));
    let ex = s.tool("rex_execute", json!({"request_id":"e2e-2","task":"try write",
        "host":"generic_agent","operator_is_agent":true}));
    let task_id = ex["task_id"].as_str().unwrap();
    let epoch = ex["lease"]["epoch"].as_u64().unwrap();
    let err = s.tool("rex_edit", json!({"task_id":task_id,"lease_epoch":epoch,
        "path":"x.txt","replacement":"nope","create":true}));
    assert_eq!(err["data"]["code"], "approval_required");
}
