//! `rex serve`: localhost HTTP API for IDE integrations (Phase 2 parity).
//!
//! Binds 127.0.0.1 only — never 0.0.0.0. This is a local developer tool,
//! not a network service; there is no auth because loopback is the trust
//! boundary.
//!
//! The first line on stdout is `{"port": N}` (machine-readable, so an IDE
//! extension can spawn `rex serve` and discover the port); everything else
//! goes to stderr.
//!
//! Endpoints:
//!   POST /v1/runs             {task, provider?, model?, skills[]?,
//!                            auto_approve?, max_steps?, workspace?}
//!                            -> 202 {run_id}  (4xx when the run cannot start)
//!   GET  /v1/runs             -> {runs: [{run_id, task, status, started_at}]}
//!   GET  /v1/runs/:id         -> live snapshot, or the finished receipt
//!   POST /v1/runs/:id/approve {approve: bool} -> {}
//!   POST /v1/runs/:id/cancel  -> {}
//!
//! Runs are interactive by default: tool and plan approvals park until
//! POST /approve resolves them. Pass `auto_approve: true` for CI-style
//! runs. A minimal std-only HTTP/1.1 implementation — no new dependencies.

use crate::exec::{Agent, BeginHook, ExecError, ExecOptions};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

struct LiveRun {
    agent: Arc<Agent>,
    slot: Arc<Mutex<Option<bool>>>,
    task: String,
    started_at: String,
}

struct Registry {
    live: HashMap<String, LiveRun>,
    /// Finished receipts (and terminal errors) by run id.
    done: HashMap<String, Value>,
}

fn snapshot_json(run_id: &str, live: &LiveRun) -> Result<Value, String> {
    let snap = live
        .agent
        .snapshot(run_id)
        .ok_or_else(|| "run vanished from the registry".to_string())?;
    let status = format!("{:?}", snap.status);
    Ok(serde_json::json!({
        "run_id": snap.id,
        "live": true,
        "status": status.to_lowercase(),
        "task": live.task,
        "started_at": live.started_at,
        "step": snap.step,
        "max_steps": snap.max_steps,
        "tool_calls": snap.tool_calls,
        "tokens_used": snap.tokens_used,
        "elapsed_ms": snap.elapsed_ms,
        "awaiting_plan": status == "AwaitingPlan",
        "pending_approval": snap.pending_approval,
        "error": snap.error,
    }))
}

struct Request {
    method: String,
    path: String,
    body: Vec<u8>,
}

fn read_request(stream: &mut TcpStream) -> Result<Request, String> {
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .map_err(|e| format!("cannot read request: {e}"))?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("/").to_string();
    if method.is_empty() {
        return Err("empty request line".to_string());
    }
    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .map_err(|e| format!("cannot read headers: {e}"))?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("Content-Length:") {
            content_length = v.trim().parse().unwrap_or(0).min(4 * 1024 * 1024);
        } else if let Some(v) = line.strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0).min(4 * 1024 * 1024);
        }
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader
            .read_exact(&mut body)
            .map_err(|e| format!("cannot read body: {e}"))?;
    }
    Ok(Request { method, path, body })
}

fn respond(stream: &mut TcpStream, code: u16, reason: &str, body: &Value) {
    let text = serde_json::to_string(body).unwrap_or_else(|_| "{\"error\":\"encode\"}".into());
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        text.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(text.as_bytes());
}

fn ok(stream: &mut TcpStream, body: &Value) {
    respond(stream, 200, "OK", body);
}

fn err(stream: &mut TcpStream, code: u16, reason: &str, msg: impl Into<String>) {
    respond(
        stream,
        code,
        reason,
        &serde_json::json!({"error": msg.into()}),
    );
}

fn handle(req: Request, stream: &mut TcpStream, reg: &Arc<Mutex<Registry>>) {
    let segs: Vec<&str> = req.path.split('/').collect();
    // segs: ["", "v1", "runs", ...]
    match (req.method.as_str(), segs.as_slice()) {
        ("GET", ["", "v1", "runs"]) => {
            let reg = reg.lock().unwrap_or_else(|p| p.into_inner());
            let mut runs: Vec<Value> = Vec::new();
            for (id, live) in &reg.live {
                let status = live
                    .agent
                    .snapshot(id)
                    .map(|s| format!("{:?}", s.status).to_lowercase())
                    .unwrap_or_else(|| "unknown".into());
                runs.push(serde_json::json!({
                    "run_id": id, "task": live.task,
                    "status": status, "started_at": live.started_at,
                    "live": true,
                }));
            }
            for (id, receipt) in &reg.done {
                runs.push(serde_json::json!({
                    "run_id": id,
                    "task": receipt.get("task"),
                    "status": receipt.get("status"),
                    "live": false,
                }));
            }
            ok(stream, &serde_json::json!({"runs": runs}));
        }
        ("POST", ["", "v1", "runs"]) => {
            let body: Value = match serde_json::from_slice(&req.body) {
                Ok(v) => v,
                Err(e) => {
                    err(stream, 400, "Bad Request", format!("invalid JSON: {e}"));
                    return;
                }
            };
            let task = body
                .get("task")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if task.trim().is_empty() {
                err(stream, 400, "Bad Request", "task is required");
                return;
            }
            let (tx, rx) = mpsc::channel::<Result<String, ExecError>>();
            let reg2 = Arc::clone(reg);
            let slot = Arc::new(Mutex::new(None::<bool>));
            let slot2 = Arc::clone(&slot);
            let task2 = task.clone();
            let auto = body
                .get("auto_approve")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let started_at = chrono::Utc::now().to_rfc3339();
            let started_at2 = started_at.clone();
            let task3 = task2.clone();
            let tx2 = tx.clone();
            let reg3 = Arc::clone(&reg2);
            let opts = ExecOptions {
                task,
                provider: body
                    .get("provider")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                model: body
                    .get("model")
                    .and_then(Value::as_str)
                    .map(|s| s.to_string()),
                workspace: body
                    .get("workspace")
                    .and_then(Value::as_str)
                    .map(std::path::PathBuf::from),
                max_steps: body
                    .get("max_steps")
                    .and_then(Value::as_u64)
                    .map(|n| n as usize),
                skills: body
                    .get("skills")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(|s| s.to_string())
                            .collect()
                    })
                    .unwrap_or_default(),
                yes: auto,
                json: true, // keep run output machine-shaped on stdout
                interactive: if auto { None } else { Some(slot2) },
                on_begin: Some(BeginHook(Arc::new(move |agent, run_id| {
                    {
                        let mut reg = reg2.lock().unwrap_or_else(|p| p.into_inner());
                        reg.live.insert(
                            run_id.to_string(),
                            LiveRun {
                                agent: Arc::clone(&agent),
                                slot: Arc::clone(&slot),
                                task: task3.clone(),
                                started_at: started_at2.clone(),
                            },
                        );
                    }
                    let _ = tx2.send(Ok(run_id.to_string()));
                }))),
                ..ExecOptions::default()
            };
            std::thread::spawn(move || {
                // If execute() fails before on_begin fires, report the
                // failure to the waiting POST handler.
                let out = crate::exec::execute(opts);
                match out {
                    Ok(o) => {
                        let id = o
                            .receipt
                            .get("run_id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let mut reg = reg3.lock().unwrap_or_else(|p| p.into_inner());
                        reg.live.remove(&id);
                        reg.done.insert(id, o.receipt);
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e));
                    }
                }
            });
            match rx.recv_timeout(Duration::from_secs(120)) {
                Ok(Ok(run_id)) => respond(
                    stream,
                    202,
                    "Accepted",
                    &serde_json::json!({"run_id": run_id}),
                ),
                Ok(Err(e)) => err(stream, 400, "Bad Request", e.message),
                Err(_) => err(stream, 504, "Gateway Timeout", "run did not start in time"),
            }
        }
        ("GET", ["", "v1", "runs", id]) => {
            let reg = reg.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(live) = reg.live.get(*id) {
                match snapshot_json(id, live) {
                    Ok(v) => ok(stream, &v),
                    Err(e) => err(stream, 500, "Internal Server Error", e),
                }
            } else if let Some(receipt) = reg.done.get(*id) {
                ok(stream, receipt)
            } else {
                err(stream, 404, "Not Found", "unknown run id");
            }
        }
        ("POST", ["", "v1", "runs", id, "approve"]) => {
            let approve = serde_json::from_slice::<Value>(&req.body)
                .ok()
                .and_then(|v| v.get("approve").and_then(Value::as_bool));
            let approve = match approve {
                Some(a) => a,
                None => {
                    err(stream, 400, "Bad Request", "body needs {\"approve\": bool}");
                    return;
                }
            };
            let reg = reg.lock().unwrap_or_else(|p| p.into_inner());
            match reg.live.get(*id) {
                Some(live) => {
                    *live.slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(approve);
                    ok(stream, &serde_json::json!({"ok": true}));
                }
                None => err(stream, 404, "Not Found", "unknown or finished run id"),
            }
        }
        ("POST", ["", "v1", "runs", id, "cancel"]) => {
            let reg = reg.lock().unwrap_or_else(|p| p.into_inner());
            match reg.live.get(*id) {
                Some(live) => match live.agent.cancel(id) {
                    Ok(_) => ok(stream, &serde_json::json!({"ok": true})),
                    Err(e) => err(stream, 500, "Internal Server Error", e),
                },
                None => err(stream, 404, "Not Found", "unknown or finished run id"),
            }
        }
        _ => err(stream, 404, "Not Found", "unknown endpoint"),
    }
}

pub fn run_serve(port: u16) -> Result<i32, ExecError> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .map_err(|e| ExecError::internal(format!("cannot bind 127.0.0.1:{port}: {e}")))?;
    let actual = listener.local_addr().map(|a| a.port()).unwrap_or(port);
    // Machine-readable first line: the IDE extension spawns `rex serve`
    // and reads the port from stdout.
    println!("{}", serde_json::json!({"port": actual}));
    eprintln!("rex: serve listening on 127.0.0.1:{actual} (loopback only)");
    let reg = Arc::new(Mutex::new(Registry {
        live: HashMap::new(),
        done: HashMap::new(),
    }));
    for conn in listener.incoming() {
        match conn {
            Ok(mut stream) => {
                let reg = Arc::clone(&reg);
                std::thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
                    match read_request(&mut stream) {
                        Ok(req) => handle(req, &mut stream, &reg),
                        Err(e) => err(&mut stream, 400, "Bad Request", e),
                    }
                });
            }
            Err(e) => eprintln!("rex: serve accept error: {e}"),
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_request_line_headers_and_body() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            s.write_all(b"POST /v1/runs HTTP/1.1\r\nContent-Length: 11\r\n\r\nhello world")
                .unwrap();
        });
        let (mut stream, _) = listener.accept().unwrap();
        let req = read_request(&mut stream).unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/v1/runs");
        assert_eq!(req.body, b"hello world");
    }

    #[test]
    fn rejects_empty_request() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            s.write_all(b"\r\n").unwrap();
        });
        let (mut stream, _) = listener.accept().unwrap();
        assert!(read_request(&mut stream).is_err());
    }
}
