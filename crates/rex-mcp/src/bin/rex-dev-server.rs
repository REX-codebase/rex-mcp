//! Development sidecar for the REX Harness frontend.
//!
//! The desktop app talks to the provider core through Tauri commands. While
//! developing the frontend in a plain browser (vite dev), that bridge does
//! not exist, so this binary exposes the same `ProviderService` over a
//! localhost-only HTTP API. It is a development tool, not the product:
//!
//! - binds 127.0.0.1 only (port 8787 by default)
//! - holds credentials in memory or in a 0600 file under the config dir;
//!   keys are never logged or included in any response
//! - `--replay <provider>=<fixture.json>` seeds a recorded live catalog,
//!   labeled `source: "replay"` so the UI cannot mistake it for a fresh fetch
//!
//! Usage:
//!   rex-dev-server [--port 8787] [--store-file <dir>] [--replay gemini=path.json]...

use rex_providers::{
    AutonomousRunService, Budgets, FileSecretStore, LiveRunService, MemorySecretStore,
    ModelCatalog, ProviderService, SearchRouter, SecretStore, UreqTransport,
};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

enum Store {
    Memory(MemorySecretStore),
    File(FileSecretStore),
}

impl SecretStore for Store {
    fn get_key(&self, provider: &str) -> Result<Option<String>, rex_providers::ProviderError> {
        match self {
            Store::Memory(s) => s.get_key(provider),
            Store::File(s) => s.get_key(provider),
        }
    }
    fn set_key(&self, provider: &str, key: &str) -> Result<(), rex_providers::ProviderError> {
        match self {
            Store::Memory(s) => s.set_key(provider, key),
            Store::File(s) => s.set_key(provider, key),
        }
    }
    fn clear_key(&self, provider: &str) -> Result<(), rex_providers::ProviderError> {
        match self {
            Store::Memory(s) => s.clear_key(provider),
            Store::File(s) => s.clear_key(provider),
        }
    }
}

type Live = LiveRunService<Store, UreqTransport>;
type Agent = AutonomousRunService<Store, UreqTransport>;
type CustodyRuns = rex_providers::custody_link::CustodyRunService<Store, UreqTransport>;

/// Agent-mode supervision context: where the shared rex-mcp state lives and
/// which workspace shell-spawned server children confine themselves to.
struct RexShell {
    state_dir: std::path::PathBuf,
    workspace: std::path::PathBuf,
}

fn rex_call(
    shell: &RexShell,
    tool: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let bin = rex_mcp::client::rex_mcp_bin()?;
    std::fs::create_dir_all(&shell.workspace).map_err(|e| e.to_string())?;
    rex_mcp::client::call_tool_once(&bin, &shell.state_dir, &shell.workspace, tool, arguments)
}

fn main() {
    let mut port: u16 = 8787;
    let mut store_dir: Option<String> = None;
    let mut replays: Vec<(String, String)> = Vec::new();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--port" => {
                i += 1;
                port = args[i].parse().expect("--port needs a number");
            }
            "--store-file" => {
                i += 1;
                store_dir = Some(args[i].clone());
            }
            "--replay" => {
                i += 1;
                let spec = args[i].clone();
                let (provider, path) = spec
                    .split_once('=')
                    .expect("--replay needs provider=path.json");
                replays.push((provider.to_string(), path.to_string()));
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }

    let runs_root = match &store_dir {
        Some(dir) => std::path::PathBuf::from(dir).join("runs"),
        None => std::env::temp_dir().join("rex-dev-runs"),
    };
    let make_store = |dir: &Option<String>| match dir {
        Some(dir) => {
            Store::File(FileSecretStore::new(dir.into()).expect("could not open dev secret store"))
        }
        None => Store::Memory(MemorySecretStore::new()),
    };
    let live = Arc::new(LiveRunService::new(
        ProviderService::new(make_store(&store_dir), UreqTransport::new()),
        runs_root,
    ));
    let agent_runs_root = match &store_dir {
        Some(dir) => std::path::PathBuf::from(dir).join("agent-runs"),
        None => std::env::temp_dir().join("rex-dev-agent-runs"),
    };
    let agent = Arc::new(Agent::new(
        ProviderService::new(make_store(&store_dir), UreqTransport::new()),
        Some(SearchRouter::new(
            make_store(&store_dir),
            UreqTransport::new(),
        )),
        agent_runs_root.clone(),
    ));

    let custody_root = match &store_dir {
        Some(dir) => std::path::PathBuf::from(dir).join("custody"),
        None => std::env::temp_dir().join("rex-dev-custody"),
    };
    let custody = Arc::new(std::sync::Mutex::new(
        rex_custody::CustodyRegistry::open(custody_root).expect("could not open custody store"),
    ));
    let custody_runs = Arc::new(CustodyRuns::new(Arc::clone(&agent), custody));

    // Agent-mode supervision bridge: the shell reaches host-driven REX tasks
    // through the same rex-mcp stdio protocol hosts use. With --store-file
    // the state dir is scoped to that store (a development sandbox); without
    // it, the default host state dir is shared so the UI supervises the real
    // tasks Claude Code or Antigravity open on this machine.
    let rex_state_dir = match &store_dir {
        Some(dir) => std::path::PathBuf::from(dir).join("rex-harness"),
        None => rex_mcp::client::default_state_dir(),
    };
    let rex_shell = Arc::new(RexShell {
        state_dir: rex_state_dir,
        workspace: agent_runs_root.join("rex-shell"),
    });
    let resume_handles = Arc::new(Mutex::new(HashMap::<String, String>::new()));

    for (provider, path) in &replays {
        let text = std::fs::read_to_string(path).expect("replay fixture unreadable");
        // Replay fixtures hold the raw provider list response; run them
        // through a one-shot scripted fetch so normalization stays identical.
        let body: serde_json::Value = serde_json::from_str(&text).expect("replay fixture invalid");
        let models = normalize_replay(provider, &body);
        let catalog = ModelCatalog::live(provider, now_unix(), models);
        live.service().seed_replay(provider, catalog);
    }

    let listener = TcpListener::bind(("127.0.0.1", port)).expect("could not bind dev server");
    eprintln!("rex-dev-server listening on http://127.0.0.1:{port} (localhost only)");
    if !replays.is_empty() {
        let names: Vec<String> = replays.iter().map(|(p, _)| p.clone()).collect();
        eprintln!(
            "replayed catalogs (recorded live data): {}",
            names.join(", ")
        );
    }
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let live = Arc::clone(&live);
                let agent = Arc::clone(&agent);
                let custody_runs = Arc::clone(&custody_runs);
                let runs_root = agent_runs_root.clone();
                let rex_shell = Arc::clone(&rex_shell);
                let resume_handles = Arc::clone(&resume_handles);
                std::thread::spawn(move || {
                    let _ = handle(
                        stream,
                        live,
                        agent,
                        custody_runs,
                        runs_root,
                        rex_shell,
                        resume_handles,
                    );
                });
            }
            Err(_) => continue,
        }
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn normalize_replay(provider: &str, body: &serde_json::Value) -> Vec<rex_providers::ModelInfo> {
    // Replay files store the provider-native list response. Gemini shape:
    // {models:[{name, displayName, supportedGenerationMethods}]}.
    let mut out = Vec::new();
    if let Some(list) = body.get("models").and_then(|m| m.as_array()) {
        for entry in list {
            let name = entry.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let id = name.strip_prefix("models/").unwrap_or(name);
            let generates = entry
                .get("supportedGenerationMethods")
                .and_then(|m| m.as_array())
                .map(|ms| ms.iter().any(|m| m.as_str() == Some("generateContent")))
                .unwrap_or(false);
            if id.is_empty() || !generates {
                continue;
            }
            let label = entry
                .get("displayName")
                .and_then(|d| d.as_str())
                .filter(|d| !d.is_empty())
                .unwrap_or(id)
                .to_string();
            out.push(rex_providers::ModelInfo {
                id: id.to_string(),
                label,
                provider: provider.to_string(),
            });
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

fn json_response(status: u16, body: &str) -> String {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Status",
    };
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, DELETE, OPTIONS\r\nAccess-Control-Allow-Headers: content-type\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn handle(
    stream: std::net::TcpStream,
    live: Arc<Live>,
    agent: Arc<Agent>,
    custody_runs: Arc<CustodyRuns>,
    agent_runs_root: std::path::PathBuf,
    rex_shell: Arc<RexShell>,
    resume_handles: Arc<Mutex<HashMap<String, String>>>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();

    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut body = vec![0u8; content_length.min(64 * 1024)];
    reader.read_exact(&mut body)?;
    let body = String::from_utf8_lossy(&body).to_string();

    let response = route(
        &method,
        &path,
        &body,
        &live,
        &agent,
        &custody_runs,
        &agent_runs_root,
        &rex_shell,
        &resume_handles,
    );
    let mut stream = reader.into_inner();
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

// The router carries every service handle; grouping them behind a context
// struct would only rename the dependencies.
#[allow(clippy::too_many_arguments)]
fn route(
    method: &str,
    path: &str,
    body: &str,
    live: &Arc<Live>,
    agent: &Arc<Agent>,
    custody_runs: &Arc<CustodyRuns>,
    agent_runs_root: &std::path::Path,
    rex_shell: &Arc<RexShell>,
    resume_handles: &Arc<Mutex<HashMap<String, String>>>,
) -> String {
    let service = live.service();
    if method == "OPTIONS" {
        return json_response(200, "{}");
    }
    let (path_only, query) = path.split_once('?').unwrap_or((path, ""));
    let segments: Vec<&str> = path_only.trim_start_matches('/').split('/').collect();
    let query_param = |key: &str| -> Option<String> {
        query.split('&').find_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            if k == key {
                Some(v.to_string())
            } else {
                None
            }
        })
    };
    match (method, segments.as_slice()) {
        ("GET", ["api", "status"]) => json_response(
            200,
            "{\"name\":\"rex-dev-server\",\"kind\":\"sidecar\",\"version\":\"0.1.0\"}",
        ),
        ("GET", ["api", "providers"]) => {
            json_response(200, &serde_json::to_string(&service.summaries()).unwrap())
        }
        ("POST", ["api", "providers", id, "key"]) => {
            let parsed: serde_json::Value =
                serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
            let key = parsed.get("key").and_then(|k| k.as_str()).unwrap_or("");
            let base_url = parsed.get("base_url").and_then(|b| b.as_str());
            match service.set_key(id, key, base_url) {
                Ok(()) => json_response(200, "{\"ok\":true}"),
                Err(e) => json_response(
                    200,
                    &format!(
                        "{{\"ok\":false,\"error\":{}}}",
                        serde_json::to_string(&e).unwrap()
                    ),
                ),
            }
        }
        ("DELETE", ["api", "providers", id, "key"]) => match service.clear_key(id) {
            Ok(()) => json_response(200, "{\"ok\":true}"),
            Err(e) => json_response(
                200,
                &format!(
                    "{{\"ok\":false,\"error\":{}}}",
                    serde_json::to_string(&e).unwrap()
                ),
            ),
        },
        ("POST", ["api", "providers", id, "refresh"]) => match service.refresh(id) {
            Ok(catalog) => json_response(
                200,
                &format!(
                    "{{\"ok\":true,\"catalog\":{}}}",
                    serde_json::to_string(&catalog).unwrap()
                ),
            ),
            Err(e) => json_response(
                200,
                &format!(
                    "{{\"ok\":false,\"error\":{}}}",
                    serde_json::to_string(&e).unwrap()
                ),
            ),
        },
        ("GET", ["api", "providers", id, "catalog"]) => match service.catalog(id) {
            Some(catalog) => json_response(200, &serde_json::to_string(&catalog).unwrap()),
            None => json_response(404, "{\"error\":{\"kind\":\"not_configured\"}}"),
        },
        ("POST", ["api", "runs"]) => {
            let parsed: serde_json::Value =
                serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
            let task = parsed.get("task").and_then(|t| t.as_str()).unwrap_or("");
            let provider = parsed
                .get("provider")
                .and_then(|p| p.as_str())
                .unwrap_or("gemini");
            match live.begin(task, provider) {
                Ok(snapshot) => json_response(200, &serde_json::to_string(&snapshot).unwrap()),
                Err(detail) => json_response(
                    200,
                    &format!("{{\"error\":{}}}", serde_json::to_string(&detail).unwrap()),
                ),
            }
        }
        ("GET", ["api", "runs", id]) => match live.snapshot(id) {
            Some(snapshot) => json_response(200, &serde_json::to_string(&snapshot).unwrap()),
            None => json_response(404, "{\"error\":\"unknown run\"}"),
        },
        ("POST", ["api", "runs", id, "decision"]) => {
            let parsed: serde_json::Value =
                serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
            let approved = parsed
                .get("approved")
                .and_then(|a| a.as_bool())
                .unwrap_or(false);
            match live.decide(id, approved) {
                Ok(snapshot) => json_response(200, &serde_json::to_string(&snapshot).unwrap()),
                Err(detail) => json_response(
                    200,
                    &format!("{{\"error\":{}}}", serde_json::to_string(&detail).unwrap()),
                ),
            }
        }
        ("POST", ["api", "runs", id, "action"]) => {
            let parsed: Result<rex_preview::BrowserAction, _> = serde_json::from_str(body);
            match parsed {
                Ok(action) => match live.preview_action(id, &action) {
                    Ok(()) => json_response(200, "{\"ok\":true}"),
                    Err(detail) => json_response(
                        200,
                        &format!("{{\"error\":{}}}", serde_json::to_string(&detail).unwrap()),
                    ),
                },
                Err(_) => json_response(200, "{\"error\":\"invalid browser action\"}"),
            }
        }
        ("POST", ["api", "runs", id, "capture"]) => match live.capture(id) {
            Ok(evidence) => json_response(200, &serde_json::to_string(&evidence).unwrap()),
            Err(detail) => json_response(
                200,
                &format!("{{\"error\":{}}}", serde_json::to_string(&detail).unwrap()),
            ),
        },
        ("POST", ["api", "runs", id, "teardown"]) => match live.teardown(id) {
            Ok(()) => json_response(200, "{\"ok\":true}"),
            Err(detail) => json_response(
                200,
                &format!("{{\"error\":{}}}", serde_json::to_string(&detail).unwrap()),
            ),
        },
        ("POST", ["api", "agent", "runs"]) => {
            let parsed: serde_json::Value =
                serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
            let task = parsed.get("task").and_then(|t| t.as_str()).unwrap_or("");
            let budgets: Option<Budgets> = parsed
                .get("budgets")
                .and_then(|b| serde_json::from_value(b.clone()).ok());
            let provider = parsed
                .get("provider")
                .and_then(|p| p.as_str())
                .unwrap_or("gemini");
            let model = parsed.get("model").and_then(|m| m.as_str());
            match agent.begin_with_model(task, provider, model, budgets) {
                Ok(snapshot) => json_response(200, &serde_json::to_string(&snapshot).unwrap()),
                Err(detail) => json_response(
                    200,
                    &format!("{{\"error\":{}}}", serde_json::to_string(&detail).unwrap()),
                ),
            }
        }
        ("GET", ["api", "agent", "runs", id]) => match agent.snapshot(id) {
            Some(snapshot) => json_response(200, &serde_json::to_string(&snapshot).unwrap()),
            None => json_response(404, "{\"error\":\"unknown run\"}"),
        },
        ("POST", ["api", "agent", "runs", id, "decision"]) => {
            let parsed: serde_json::Value =
                serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
            let approved = parsed
                .get("approved")
                .and_then(|a| a.as_bool())
                .unwrap_or(false);
            match agent.decide(id, approved) {
                Ok(snapshot) => json_response(200, &serde_json::to_string(&snapshot).unwrap()),
                Err(detail) => json_response(
                    200,
                    &format!("{{\"error\":{}}}", serde_json::to_string(&detail).unwrap()),
                ),
            }
        }
        ("POST", ["api", "agent", "runs", id, "cancel"]) => match agent.cancel(id) {
            Ok(snapshot) => json_response(200, &serde_json::to_string(&snapshot).unwrap()),
            Err(detail) => json_response(
                200,
                &format!("{{\"error\":{}}}", serde_json::to_string(&detail).unwrap()),
            ),
        },
        ("POST", ["api", "agent", "runs", id, "resume"]) => match agent.resume(id) {
            Ok(snapshot) => json_response(200, &serde_json::to_string(&snapshot).unwrap()),
            Err(detail) => json_response(
                200,
                &format!("{{\"error\":{}}}", serde_json::to_string(&detail).unwrap()),
            ),
        },
        ("POST", ["api", "agent", "runs", id, "action"]) => {
            let parsed: Result<rex_preview::BrowserAction, _> = serde_json::from_str(body);
            match parsed {
                Ok(action) => match agent.preview_action(id, &action) {
                    Ok(()) => json_response(200, "{\"ok\":true}"),
                    Err(detail) => json_response(
                        200,
                        &format!("{{\"error\":{}}}", serde_json::to_string(&detail).unwrap()),
                    ),
                },
                Err(_) => json_response(200, "{\"error\":\"invalid browser action\"}"),
            }
        }
        ("POST", ["api", "agent", "runs", id, "capture"]) => match agent.capture(id) {
            Ok(evidence) => json_response(200, &serde_json::to_string(&evidence).unwrap()),
            Err(detail) => json_response(
                200,
                &format!("{{\"error\":{}}}", serde_json::to_string(&detail).unwrap()),
            ),
        },
        ("POST", ["api", "agent", "runs", id, "teardown"]) => match agent.teardown(id) {
            Ok(()) => json_response(200, "{\"ok\":true}"),
            Err(detail) => json_response(
                200,
                &format!("{{\"error\":{}}}", serde_json::to_string(&detail).unwrap()),
            ),
        },
        ("POST", ["api", "agent", "custody", "runs"]) => {
            let parsed: serde_json::Value =
                serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
            let task = parsed.get("task").and_then(|t| t.as_str()).unwrap_or("");
            let provider = parsed
                .get("provider")
                .and_then(|p| p.as_str())
                .unwrap_or("gemini");
            let model = parsed
                .get("model")
                .and_then(|m| m.as_str())
                .map(|s| s.to_string());
            // The operator identity is the caller's declaration, recorded
            // for audit; custody decisions never trust the string.
            let operator = match parsed.get("operator").and_then(|o| o.as_str()) {
                Some("agent") => rex_custody::OperatorIdentity::Agent(
                    rex_custody::CustodyRegistry::register_agent(
                        parsed
                            .get("agent_name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("rex-ui-agent"),
                        rex_custody::AgentProtocol::Mcp {
                            client: "rex-ui".into(),
                            version: "ui-supervision".into(),
                        },
                    ),
                ),
                _ => rex_custody::OperatorIdentity::Human,
            };
            let task_id = format!(
                "task-ui-{:x}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0)
            );
            // Custodied workspaces must sit under the agent runs root: the
            // autonomous service refuses explicit workspaces outside it.
            let workspace = agent_runs_root.join(&task_id);
            let store_workspace = match &std::fs::create_dir_all(&workspace) {
                Ok(()) => Ok(workspace),
                Err(e) => Err(e.to_string()),
            };
            match store_workspace {
                Err(detail) => json_response(
                    200,
                    &format!("{{\"error\":{}}}", serde_json::to_string(&detail).unwrap()),
                ),
                Ok(ws) => {
                    let req = rex_providers::custody_link::ui_managed_request(
                        task_id,
                        task.to_string(),
                        operator,
                        provider.to_string(),
                        model,
                        ws,
                    );
                    match custody_runs.begin_managed_task(req) {
                        Ok(run) => json_response(
                            200,
                            &serde_json::to_string(&custody_runs.view_of(&run)).unwrap(),
                        ),
                        Err(detail) => json_response(
                            200,
                            &format!("{{\"error\":{}}}", serde_json::to_string(&detail).unwrap()),
                        ),
                    }
                }
            }
        }
        ("GET", ["api", "agent", "custody", "grants", gid]) => match custody_runs.phase_of(gid) {
            Some(phase) => json_response(
                200,
                &serde_json::to_string(&serde_json::json!({"grant_id": gid, "phase": phase}))
                    .unwrap(),
            ),
            None => json_response(404, "{\"error\":\"unknown grant\"}"),
        },
        ("POST", ["api", "agent", "custody", "runs", id, "stop"]) => {
            let parsed: serde_json::Value =
                serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
            let grant_id = parsed
                .get("grant_id")
                .and_then(|g| g.as_str())
                .unwrap_or("");
            match custody_runs.human_stop(grant_id, id) {
                Ok(reason) => json_response(
                    200,
                    &serde_json::to_string(&serde_json::json!({"ok": true, "reason": reason}))
                        .unwrap(),
                ),
                Err(detail) => json_response(
                    200,
                    &format!("{{\"error\":{}}}", serde_json::to_string(&detail).unwrap()),
                ),
            }
        }
        ("GET", ["api", "rex", "tasks"]) => {
            // Read-only scan of the shared task store; no daemon lock taken.
            let mut tasks = Vec::new();
            if let Ok(entries) = std::fs::read_dir(rex_shell.state_dir.join("tasks")) {
                for ent in entries.flatten() {
                    if let Ok(bytes) = std::fs::read(ent.path().join("task.json")) {
                        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                            tasks.push(serde_json::json!({
                                "task_id": v.get("task_id").cloned().unwrap_or(serde_json::Value::Null),
                                "task": v.get("task").cloned().unwrap_or(serde_json::Value::Null),
                                "state": v.get("state").cloned().unwrap_or(serde_json::Value::Null),
                                "host": v.get("host").cloned().unwrap_or(serde_json::Value::Null),
                                "operator_is_agent": v.get("operator_is_agent").cloned().unwrap_or(serde_json::Value::Null),
                            }));
                        }
                    }
                }
            }
            tasks.sort_by(|a, b| {
                let key = |v: &serde_json::Value| {
                    v.get("task_id")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .to_string()
                };
                key(a).cmp(&key(b))
            });
            json_response(
                200,
                &serde_json::to_string(&serde_json::json!({"tasks": tasks})).unwrap(),
            )
        }
        ("POST", ["api", "rex", "tasks"]) => {
            let parsed: serde_json::Value =
                serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
            let task = parsed.get("task").and_then(|t| t.as_str()).unwrap_or("");
            let request_id = format!(
                "ui-{:x}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0)
            );
            // A task opened in agent mode waits for an external host to
            // drive it; custody records an agent operator from the start.
            let args = serde_json::json!({
                "request_id": request_id,
                "task": task,
                "host": "generic_agent",
                "operator_is_agent": true,
            });
            match rex_call(rex_shell, "rex_execute", args) {
                Ok(v) => {
                    remember_resume_handle(resume_handles, &v);
                    json_response(200, &task_response(&v))
                }
                Err(detail) => json_response(
                    200,
                    &format!("{{\"error\":{}}}", serde_json::to_string(&detail).unwrap()),
                ),
            }
        }
        ("POST", ["api", "rex", "tasks", id, "follow-up"]) => {
            let parsed: serde_json::Value =
                serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
            let follow_up = parsed
                .get("task")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .trim();
            let handle = resume_handles
                .lock()
                .ok()
                .and_then(|handles| handles.get(*id).cloned());
            let Some(handle) = handle else {
                return json_response(200, &serde_json::json!({
                    "error": "resume handle unavailable; reopen this task through its trusted host"
                }).to_string());
            };
            let status = rex_call(rex_shell, "rex_status", serde_json::json!({"task_id": id}));
            let task = match status.and_then(|value| {
                value
                    .get("task")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
                    .ok_or_else(|| "REX status returned no task text".to_string())
            }) {
                Ok(task) => task,
                Err(detail) => {
                    return json_response(200, &serde_json::json!({"error": detail}).to_string())
                }
            };
            let args = serde_json::json!({
                "request_id": format!("ui-follow-up-{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)),
                "task": task,
                "task_id": id,
                "resume_handle": handle,
                "follow_up": follow_up,
                "host": "generic_agent",
                "operator_is_agent": true,
            });
            match rex_call(rex_shell, "rex_execute", args) {
                Ok(v) => {
                    remember_resume_handle(resume_handles, &v);
                    json_response(200, &task_response(&v))
                }
                Err(detail) => json_response(
                    200,
                    &format!("{{\"error\":{}}}", serde_json::to_string(&detail).unwrap()),
                ),
            }
        }
        ("GET", ["api", "rex", "tasks", id, "status"]) => {
            match rex_call(rex_shell, "rex_status", serde_json::json!({"task_id": id})) {
                Ok(v) => json_response(200, &serde_json::to_string(&v).unwrap()),
                Err(detail) => json_response(
                    200,
                    &format!("{{\"error\":{}}}", serde_json::to_string(&detail).unwrap()),
                ),
            }
        }
        ("GET", ["api", "rex", "tasks", id, "events"]) => {
            let after = query_param("since")
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            match rex_call(
                rex_shell,
                "rex_events",
                serde_json::json!({"task_id": id, "after_seq": after, "limit": 200}),
            ) {
                Ok(v) => json_response(200, &serde_json::to_string(&v).unwrap()),
                Err(detail) => json_response(
                    200,
                    &format!("{{\"error\":{}}}", serde_json::to_string(&detail).unwrap()),
                ),
            }
        }
        ("GET", ["api", "rex", "tasks", id, "proof"]) => {
            // Read-only load of the persisted proof bundle; no daemon lock
            // taken, matching the task scan above.
            match read_proof_bundle(&rex_shell.state_dir, id) {
                Ok(bundle) => json_response(200, &bundle),
                Err(detail) => json_response(
                    404,
                    &format!("{{\"error\":{}}}", serde_json::to_string(&detail).unwrap()),
                ),
            }
        }
        ("POST", ["api", "rex", "tasks", id, "stop"]) => {
            // The permanent human Stop: cancel fences the lease and is final
            // even against an agent operator.
            match rex_call(
                rex_shell,
                "rex_cancel",
                serde_json::json!({"task_id": id, "reason": "human stop from REX UI"}),
            ) {
                Ok(v) => json_response(200, &serde_json::to_string(&v).unwrap()),
                Err(detail) => json_response(
                    200,
                    &format!("{{\"error\":{}}}", serde_json::to_string(&detail).unwrap()),
                ),
            }
        }
        _ => json_response(
            404,
            "{\"error\":{\"kind\":\"invalid_response\",\"detail\":\"unknown route\"}}",
        ),
    }
}

fn remember_resume_handle(handles: &Mutex<HashMap<String, String>>, response: &serde_json::Value) {
    if let (Some(task_id), Some(handle)) = (
        response.get("task_id").and_then(|v| v.as_str()),
        response.get("host_resume_handle").and_then(|v| v.as_str()),
    ) {
        if let Ok(mut stored) = handles.lock() {
            stored.insert(task_id.to_string(), handle.to_string());
        }
    }
}

fn task_response(response: &serde_json::Value) -> String {
    serde_json::json!({
        "task_id": response.get("task_id"),
        "state": response.get("state"),
        "resumed": response.get("resumed").unwrap_or(&serde_json::Value::Bool(false)),
    })
    .to_string()
}

/// Load the persisted proof bundle for a task. Task ids are confined to a
/// filename-safe alphabet so a request path can never escape the proofs
/// directory.
fn read_proof_bundle(state_dir: &std::path::Path, task_id: &str) -> Result<String, String> {
    if task_id.is_empty()
        || !task_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err("invalid task id".to_string());
    }
    let path = state_dir.join("proofs").join(format!("{task_id}.json"));
    std::fs::read_to_string(&path).map_err(|_| format!("no proof bundle for task {task_id}"))
}

#[cfg(test)]
mod tests {
    use super::read_proof_bundle;

    #[test]
    fn proof_bundle_reads_persisted_bundle_and_refuses_traversal() {
        let dir = std::env::temp_dir().join(format!(
            "rex-dev-proof-{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("proofs")).unwrap();
        std::fs::write(
            dir.join("proofs").join("task-1.json"),
            "{\"bundle_hash\":\"abc\"}",
        )
        .unwrap();

        let found = read_proof_bundle(&dir, "task-1").unwrap();
        assert!(found.contains("abc"));
        assert!(read_proof_bundle(&dir, "task-9").is_err());
        assert!(read_proof_bundle(&dir, "../task-1").is_err());
        assert!(read_proof_bundle(&dir, "task-1/../../proofs/task-1").is_err());
        assert!(read_proof_bundle(&dir, "").is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
