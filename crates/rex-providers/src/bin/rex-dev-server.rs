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
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::Arc;

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
        Some(SearchRouter::new(make_store(&store_dir), UreqTransport::new())),
        agent_runs_root,
    ));

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
                std::thread::spawn(move || {
                    let _ = handle(stream, live, agent);
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

fn handle(stream: std::net::TcpStream, live: Arc<Live>, agent: Arc<Agent>) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut parts = request_line.trim().split_whitespace();
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

    let response = route(&method, &path, &body, &live, &agent);
    let mut stream = reader.into_inner();
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

fn route(method: &str, path: &str, body: &str, live: &Arc<Live>, agent: &Arc<Agent>) -> String {
    let service = live.service();
    if method == "OPTIONS" {
        return json_response(200, "{}");
    }
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
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
            match agent.begin(task, "gemini", budgets) {
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
        _ => json_response(
            404,
            "{\"error\":{\"kind\":\"invalid_response\",\"detail\":\"unknown route\"}}",
        ),
    }
}
