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

    fn handshake(&mut self) -> Result<(), String> {
        let init = self.request(
            "initialize",
            json!({ "protocolVersion": "2025-11-25", "capabilities": {},
                "clientInfo": { "name": "rex-ultra-scripted-host", "version": env!("CARGO_PKG_VERSION") } }),
        )?;
        if init["result"]["protocolVersion"] != "2025-11-25" {
            return Err(format!("handshake mismatch: {init}"));
        }
        self.notify("notifications/initialized", json!({}))
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

    /// Call one tool. Tool-level failures (scope, gates, invalid evidence)
    /// come back as structured content carrying `data.code`; transport and
    /// envelope failures become Err.
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

    /// Call one tool and require success (no protocol error payload).
    fn tool_ok(&mut self, name: &str, args: Value) -> Result<Value, String> {
        let v = self.tool(name, args)?;
        if let Some(code) = v.pointer("/data/code") {
            return Err(format!("tool {name} refused ({code}): {v}"));
        }
        Ok(v)
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

fn hash(content: &str) -> Result<String, String> {
    rex_protocol::schema::canonical_hash(&content.to_string())
        .map_err(|e| format!("canonical hash failed: {e}"))
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "smoke".into());
    let result = match mode.as_str() {
        "smoke" => smoke(),
        "ultra" => ultra(),
        other => Err(format!("unknown mode {other}; expected smoke|ultra")),
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
    s.handshake()?;
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

    let ex = s.tool_ok(
        "rex_execute",
        json!({ "request_id": "scripted-smoke", "task": "smoke the host loop",
            "host": "generic_agent", "operator_is_agent": true,
            "plan": [{ "instructions": "open", "acceptance": "task opens" }] }),
    )?;
    let task_id = ex["task_id"].as_str().ok_or("rex_execute returned no task id")?.to_string();
    let status = s.tool_ok("rex_status", json!({ "task_id": task_id }))?;
    if status["state"] != "active" {
        return Err(format!("unexpected task state: {status}"));
    }
    println!("ok execute+status: task {task_id} active");

    let done = s.tool_ok("rex_cancel", json!({ "task_id": task_id, "reason": "smoke complete" }))?;
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

/// The full Ultra qualification path, offline and deterministic:
/// candidates, adversary/verifier/visual evidence, a rejection, a visual
/// critic rejection repaired by the host, two daemon restarts with
/// replayed calls, promotion, terminal result, and the proof bundle.
fn ultra() -> Result<(), String> {
    let bin = rex_mcp_bin()?;
    let state = temp_dir("state")?;
    let ws = temp_dir("ws")?;

    let mut s = Session::start(&bin, &state, &ws)?;
    s.handshake()?;

    let ex = s.tool_ok(
        "rex_execute",
        json!({ "request_id": "scripted-ultra", "task": "Build a visual landing page hero with motion",
            "host": "generic_agent", "operator_is_agent": true,
            "plan": [
                { "instructions": "hero layout", "acceptance": "hero renders at desktop and phone" },
                { "instructions": "motion detail", "acceptance": "interaction replays cleanly" }
            ] }),
    )?;
    if ex["state"] != "active" {
        return Err(format!("unexpected execute state: {ex}"));
    }
    let task_id = ex["task_id"].as_str().ok_or("no task id")?.to_string();
    let epoch = ex["lease"]["epoch"].as_u64().ok_or("no lease epoch")?;
    let mut handle = ex["host_resume_handle"]
        .as_str()
        .ok_or("no host resume handle")?
        .to_string();
    let action1 = ex["next"]["action_id"].as_str().ok_or("no first action")?.to_string();
    println!("ok execute: task {task_id}, resume handle issued");

    let open = s.tool_ok("rex_ultra_open", json!({ "task_id": task_id, "lease_epoch": epoch }))?;
    if open["status"] != "collecting" {
        return Err(format!("unexpected ultra_open status: {open}"));
    }
    let candidates: Vec<String> = open["candidate_requests"]
        .as_array()
        .ok_or("no candidate requests")?
        .iter()
        .map(|c| {
            if c["work_kind"] != "visual" {
                return Err(format!("expected visual contract, got {c}"));
            }
            c["candidate_id"]
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| "candidate id missing".to_string())
        })
        .collect::<Result<_, _>>()?;
    if candidates.len() < 2 {
        return Err(format!("expected at least 2 candidates, got {candidates:?}"));
    }
    println!("ok ultra_open: {} candidate theses requested (visual contract)", candidates.len());

    // Candidate theses: each answers with an isolated file bundle.
    let mut view = Value::Null;
    for (i, candidate_id) in candidates.iter().enumerate() {
        let content = json!({ "files": [{ "path": format!("hero-{i}.html"),
            "content": format!("<html><body><h1>candidate {i}</h1></body></html>") }] })
        .to_string();
        view = s.tool_ok(
            "rex_ultra_submit",
            json!({ "task_id": task_id, "lease_epoch": epoch, "kind": "candidate",
                "request_id": candidate_id, "candidate_id": candidate_id,
                "response_hash": hash(&content)?, "content": content }),
        )?;
    }
    if view["kernel_state"] != "awaiting_evidence" {
        return Err(format!("expected awaiting_evidence, got {view}"));
    }
    let evidence: Vec<Value> = view["evidence_requests"]
        .as_array()
        .ok_or("no evidence requests")?
        .clone();
    if evidence.len() != candidates.len() * 3 {
        return Err(format!("expected adversary+verifier+visual per candidate, got {evidence:?}"));
    }
    println!("ok candidates: {} evidence requests issued", evidence.len());

    let loser = candidates[0].clone();
    let winner = candidates[1].clone();

    // The loser is rejected by adversary evidence: the hostile pass grounds
    // a real defect (the deterministic kernel's mutation stand-in).
    let adversary_req = evidence
        .iter()
        .find(|r| r["candidate_id"] == loser && r["kind"] == "adversary")
        .ok_or("no adversary request for loser")?;
    let defect = json!({ "defects": [{ "title": "hero collapses under mutation",
        "detail": "removing the media query breaks the layout at phone width" }] })
    .to_string();
    s.tool_ok(
        "rex_ultra_submit",
        json!({ "task_id": task_id, "lease_epoch": epoch, "kind": "adversary",
            "request_id": adversary_req["request_id"], "candidate_id": loser,
            "response_hash": hash(&defect)?, "content": defect }),
    )?;
    println!("ok adversary: candidate {loser} rejected on grounded defects");

    // The winner: clean adversary and fully proven verifier outcomes.
    for r in evidence.iter().filter(|r| r["candidate_id"] == winner) {
        let (kind, content) = match r["kind"].as_str().unwrap_or("") {
            "adversary" => ("adversary", json!({ "defects": [] }).to_string()),
            "verifier" => (
                "verifier",
                json!({ "outcomes": [
                    { "obligation_id": "step-1", "status": "proven" },
                    { "obligation_id": "step-2", "status": "proven" }
                ] })
                .to_string(),
            ),
            _ => continue, // visual handled below: first rejected, then repaired
        };
        s.tool_ok(
            "rex_ultra_submit",
            json!({ "task_id": task_id, "lease_epoch": epoch, "kind": kind,
                "request_id": r["request_id"], "candidate_id": winner,
                "response_hash": hash(&content)?, "content": content }),
        )?;
    }
    println!("ok evidence: winner clean adversary + proven verifier");

    // The visual critic rejects the winner's first visual evidence: the
    // critic pass is not clean, so the gate refuses it outright.
    let visual_req = evidence
        .iter()
        .find(|r| r["candidate_id"] == winner && r["kind"] == "visual")
        .ok_or("no visual request for winner")?;
    let shot = |class: &str, seed: usize| json!({ "viewport": class, "artifact_hash": format!("{seed:064x}") });
    let bad_visual = json!({ "thesis_id": "thesis-winner",
        "screenshots": [shot("desktop", 1), shot("phone", 2)],
        "interaction_replay_hash": format!("{:064x}", 3),
        "forbidden_patterns_hit": [], "critic_clean": false })
    .to_string();
    let rejected = s.tool(
        "rex_ultra_submit",
        json!({ "task_id": task_id, "lease_epoch": epoch, "kind": "visual",
            "request_id": visual_req["request_id"], "candidate_id": winner,
            "response_hash": hash(&bad_visual)?, "content": bad_visual }),
    );
    match rejected {
        Err(e) if e.contains("critic") => {
            println!("ok visual critic: first winner submission refused (critic not clean)")
        }
        other => return Err(format!("visual critic accepted a dirty pass: {other:?}")),
    }

    // Restart 1: the daemon process dies; a new server over the same state
    // dir resumes the task only with the rotated host resume handle.
    s.shutdown();
    let mut s = Session::start(&bin, &state, &ws)?;
    s.handshake()?;
    let replay = s.tool_ok(
        "rex_execute",
        json!({ "request_id": "scripted-ultra", "task": "Build a visual landing page hero with motion",
            "host": "generic_agent", "operator_is_agent": true,
            "resume_handle": handle,
            "plan": [
                { "instructions": "hero layout", "acceptance": "hero renders at desktop and phone" },
                { "instructions": "motion detail", "acceptance": "interaction replays cleanly" }
            ] }),
    )?;
    if replay["resumed"] != true || replay["task_id"] != task_id {
        return Err(format!("replay did not resume the same task: {replay}"));
    }
    handle = replay["host_resume_handle"].as_str().ok_or("no rotated handle")?.to_string();
    println!("ok restart 1: replayed call resumed {task_id}, handle rotated");

    // The host repairs the winner: a clean critic pass with hash-bound
    // desktop and phone screenshots and a hash-bound interaction replay.
    let good_visual = json!({ "thesis_id": "thesis-winner",
        "screenshots": [shot("desktop", 11), shot("phone", 12)],
        "interaction_replay_hash": format!("{:064x}", 13),
        "forbidden_patterns_hit": [], "critic_clean": true })
    .to_string();
    view = s.tool_ok(
        "rex_ultra_submit",
        json!({ "task_id": task_id, "lease_epoch": epoch, "kind": "visual",
            "request_id": visual_req["request_id"], "candidate_id": winner,
            "response_hash": hash(&good_visual)?, "content": good_visual }),
    )?;
    if view["kernel_state"] != "completed" {
        return Err(format!("expected completed kernel, got {view}"));
    }
    println!("ok repair: clean visual evidence accepted, kernel completed");

    // Restart 2 + second replayed call: completed state survives.
    s.shutdown();
    let mut s = Session::start(&bin, &state, &ws)?;
    s.handshake()?;
    let replay2 = s.tool_ok(
        "rex_execute",
        json!({ "request_id": "scripted-ultra", "task": "Build a visual landing page hero with motion",
            "host": "generic_agent", "operator_is_agent": true,
            "resume_handle": handle,
            "plan": [
                { "instructions": "hero layout", "acceptance": "hero renders at desktop and phone" },
                { "instructions": "motion detail", "acceptance": "interaction replays cleanly" }
            ] }),
    )?;
    if replay2["resumed"] != true {
        return Err(format!("second replay failed: {replay2}"));
    }
    let open2 = s.tool_ok("rex_ultra_open", json!({ "task_id": task_id, "lease_epoch": epoch }))?;
    if open2["kernel_state"] != "completed" || !open2["evidence_requests"].as_array().map(|e| e.is_empty()).unwrap_or(false) {
        return Err(format!("completed kernel not durable across restart: {open2}"));
    }
    println!("ok restart 2: completed kernel durable, no open requests");

    // Atomic promotion: exactly the winning bundle lands in the workspace.
    let receipt = s.tool_ok("rex_ultra_promote", json!({ "task_id": task_id, "lease_epoch": epoch }))?;
    if receipt["state"] != "committed" || receipt["candidate_id"] != winner {
        return Err(format!("unexpected promotion receipt: {receipt}"));
    }
    let promoted = std::fs::read_to_string(ws.join("hero-1.html"))
        .map_err(|e| format!("promoted artifact missing from workspace: {e}"))?;
    if !promoted.contains("candidate 1") {
        return Err(format!("workspace holds the wrong bundle: {promoted}"));
    }
    println!("ok promotion: winner committed, artifact in workspace");

    // Terminal result: the host closes the frozen plan citing the receipts
    // the harness registered, never its own narration.
    let run1 = s.tool_ok("rex_run", json!({ "task_id": task_id, "lease_epoch": epoch,
        "argv": ["ls"] }))?;
    let receipt1 = run1["receipt"].as_str().ok_or("no run receipt")?.to_string();
    let sub1 = s.tool_ok("rex_submit", json!({ "task_id": task_id, "lease_epoch": epoch,
        "action_id": action1, "narrative": "winner promoted into the workspace" }))?;
    if sub1["accepted"] != true {
        return Err(format!("first plan submit refused: {sub1}"));
    }
    let action2 = sub1["next"]["action_id"].as_str().ok_or("no second action")?.to_string();
    let run2 = s.tool_ok("rex_run", json!({ "task_id": task_id, "lease_epoch": epoch,
        "argv": ["cat", "hero-1.html"] }))?;
    let receipt2 = run2["receipt"].as_str().ok_or("no second receipt")?.to_string();
    let sub2 = s.tool_ok("rex_submit", json!({ "task_id": task_id, "lease_epoch": epoch,
        "action_id": action2, "narrative": "promotion verified against the workspace",
        "evidence": { "ls": receipt1, "cat": receipt2 } }))?;
    if sub2["state"] != "completed" {
        return Err(format!("final submit did not complete: {sub2}"));
    }
    let result = s.tool_ok("rex_result", json!({ "task_id": task_id }))?;
    if result["output"] != "promotion verified against the workspace" {
        return Err(format!("unexpected terminal result: {result}"));
    }
    println!("ok terminal: plan completed, result carries host output");

    // The proof bundle is deterministic and records the whole journey.
    let p1 = s.tool_ok("rex_proof", json!({ "task_id": task_id }))?;
    let p2 = s.tool_ok("rex_proof", json!({ "task_id": task_id }))?;
    if p1["bundle_hash"] != p2["bundle_hash"] {
        return Err("proof bundle hash is not deterministic".into());
    }
    if p1["kernel_state"] != "completed" || p1["promotion_state"] != "committed" {
        return Err(format!("proof bundle misses the verified journey: {}", p1["bundle_hash"]));
    }
    if p1["qualified_candidate"] != winner {
        return Err(format!("proof bundle names the wrong winner: {}", p1["qualified_candidate"]));
    }
    let persisted = state.join("proofs").join(format!("{task_id}.json"));
    if !persisted.exists() {
        return Err(format!("proof bundle not persisted at {}", persisted.display()));
    }
    println!("ok proof: deterministic bundle {} persisted", p1["bundle_hash"]);

    s.shutdown();
    println!("PASS scripted-host ultra");
    println!("state dir kept for inspection: {}", state.display());
    println!("workspace kept for inspection: {}", ws.display());
    Ok(())
}
