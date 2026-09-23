// REX Harness desktop shell with the first real backend slice: provider
// connectivity. The Rust core (`rex-providers`) owns credentials, provider
// HTTP, and model catalogs. The frontend receives summaries and catalogs
// only - key material never crosses the bridge.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod terminal;

use rex_installed_agents::{
    discover as discover_installed_agents, run as run_installed_agent, InstalledAgentId,
    InstalledAgentRun, InstalledAgentSummary, RunManager as InstalledAgentManager, RunOptions,
    RunSnapshot as InstalledRunSnapshot,
};
use rex_preview::{
    BrowserAction, BrowserEvidence, IterationReceipt, PreviewRecipe, PreviewSupervisor,
    ProductionReport, SupervisorSummary,
};
use rex_providers::{
    AgentSnapshot, AutonomousRunService, Budgets, FileSecretStore, LiveRunService, ModelCatalog,
    ProviderError, ProviderService, ProviderSummary, RunSnapshot, SearchProvider,
    SearchProviderSummary, SearchRouter, UreqTransport,
};
use rex_search::{IndexHit, IndexedDocument, LocalIndex, SearchRequest, SearchResponse};
use rex_tools::{CallState, PreparedCall, ToolRequest, ToolResult, ToolRuntime};
use rex_ultra::orchestrator::{UltraOptions, UltraRunService, UltraSnapshot};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use tauri::State;

type Service = ProviderService<FileSecretStore, UreqTransport>;
type Live = LiveRunService<FileSecretStore, UreqTransport>;
type Agent = AutonomousRunService<FileSecretStore, UreqTransport>;
type CustodyRuns = rex_providers::CustodyRunService<FileSecretStore, UreqTransport>;
type Ultra = UltraRunService<FileSecretStore, UreqTransport>;
type SearchService = SearchRouter<FileSecretStore, UreqTransport>;
type LocalTools = ToolRuntime;
type NativePreview = PreviewSupervisor;
type InstalledRuns = InstalledAgentManager;

static REX_RESUME_HANDLES: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

fn rex_resume_handles() -> &'static Mutex<HashMap<String, String>> {
    REX_RESUME_HANDLES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn search_index() -> LocalIndex {
    LocalIndex::open(config_dir().join("search-index.json"))
}

fn config_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("rex-harness");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".config").join("rex-harness");
    }
    std::env::temp_dir().join("rex-harness")
}

#[tauri::command]
fn installed_agent_summaries() -> Vec<InstalledAgentSummary> {
    discover_installed_agents()
}

#[tauri::command]
fn installed_agent_run(
    backend: InstalledAgentId,
    prompt: String,
    workspace: String,
) -> Result<InstalledAgentRun, rex_installed_agents::InstalledAgentError> {
    run_installed_agent(backend, &prompt, std::path::Path::new(&workspace))
}

/// Begin a real installed-agent run. Returns immediately; every child event
/// becomes visible through `installed_agent_snapshot` while the child works.
/// An empty workspace creates a fresh task workspace the child owns outright;
/// a real workspace is staged and its diff waits for `installed_agent_decide`.
#[tauri::command]
fn installed_agent_begin(
    runs: State<'_, Arc<InstalledRuns>>,
    backend: InstalledAgentId,
    prompt: String,
    workspace: Option<String>,
    model: Option<String>,
    effort: Option<String>,
) -> Result<InstalledRunSnapshot, rex_installed_agents::InstalledAgentError> {
    runs.begin(
        backend,
        &prompt,
        &workspace.unwrap_or_default(),
        RunOptions { model, effort },
    )
}

#[tauri::command]
fn installed_agent_snapshot(
    runs: State<'_, Arc<InstalledRuns>>,
    run_id: String,
) -> Result<InstalledRunSnapshot, String> {
    runs.snapshot(&run_id)
        .ok_or_else(|| "unknown run".to_string())
}

/// Trusted operator decision on a staged diff. The model and the child can
/// never reach this path; only the desktop UI invokes it.
#[tauri::command]
fn installed_agent_decide(
    runs: State<'_, Arc<InstalledRuns>>,
    run_id: String,
    approved: bool,
) -> Result<InstalledRunSnapshot, String> {
    runs.decide(&run_id, approved).map_err(|e| e.to_string())
}

#[tauri::command]
fn installed_agent_cancel(
    runs: State<'_, Arc<InstalledRuns>>,
    run_id: String,
) -> Result<InstalledRunSnapshot, String> {
    runs.cancel(&run_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn provider_summaries(service: State<'_, Arc<Service>>) -> Vec<ProviderSummary> {
    service.summaries()
}

#[tauri::command]
fn provider_set_key(
    service: State<'_, Arc<Service>>,
    provider: String,
    key: String,
    base_url: Option<String>,
) -> Result<(), ProviderError> {
    service.set_key(&provider, &key, base_url.as_deref())
}

#[tauri::command]
fn provider_clear_key(
    service: State<'_, Arc<Service>>,
    provider: String,
) -> Result<(), ProviderError> {
    service.clear_key(&provider)
}

#[tauri::command]
fn provider_refresh(
    service: State<'_, Arc<Service>>,
    provider: String,
) -> Result<ModelCatalog, ProviderError> {
    service.refresh(&provider)
}

#[tauri::command]
fn provider_catalog(service: State<'_, Arc<Service>>, provider: String) -> Option<ModelCatalog> {
    service.catalog(&provider)
}

/// Run a bounded, robots-aware live evidence search over explicit public seeds.
/// This is intentionally synchronous in 0.1; Tauri executes commands off the UI
/// call site and the request budgets cap work. No provider key or browser state
/// is involved.
#[tauri::command]
fn search_live(
    search: State<'_, Arc<SearchService>>,
    request: SearchRequest,
) -> Result<SearchResponse, ProviderError> {
    search.search(request)
}

#[tauri::command]
fn search_provider_summaries(search: State<'_, Arc<SearchService>>) -> Vec<SearchProviderSummary> {
    search.summaries()
}
#[tauri::command]
fn search_provider_set_key(
    search: State<'_, Arc<SearchService>>,
    provider: SearchProvider,
    key: String,
) -> Result<(), ProviderError> {
    search.set_key(provider, &key)
}
#[tauri::command]
fn search_provider_clear_key(
    search: State<'_, Arc<SearchService>>,
    provider: SearchProvider,
) -> Result<(), ProviderError> {
    search.clear_key(provider)
}
#[tauri::command]
fn search_provider_select(
    search: State<'_, Arc<SearchService>>,
    provider: SearchProvider,
) -> Result<(), ProviderError> {
    search.select(provider)
}

/// Add or replace one operator-selected document in the private local index.
#[tauri::command]
fn search_index_upsert(document: IndexedDocument) -> Result<(), String> {
    search_index().upsert(document).map_err(|e| e.to_string())
}

/// Delete one document from the local index. There is no implicit retention.
#[tauri::command]
fn search_index_remove(id: String) -> Result<bool, String> {
    search_index().remove(&id).map_err(|e| e.to_string())
}

/// Query only the local documents the operator explicitly indexed.
#[tauri::command]
fn search_index_query(query: String, limit: usize) -> Result<Vec<IndexHit>, String> {
    search_index()
        .query(&query, limit)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn tool_prepare(
    tools: State<'_, Arc<LocalTools>>,
    request: ToolRequest,
) -> Result<PreparedCall, rex_tools::ToolError> {
    tools.prepare(request)
}

/// Trusted UI decision. Model tool payloads cannot call this through the agent dispatcher.
#[tauri::command]
fn tool_resolve_approval(
    tools: State<'_, Arc<LocalTools>>,
    call_id: String,
    approved: bool,
) -> Result<CallState, rex_tools::ToolError> {
    tools.resolve_approval(&call_id, approved)
}

#[tauri::command]
fn tool_execute(tools: State<'_, Arc<LocalTools>>, call_id: String) -> ToolResult {
    tools.execute(&call_id)
}

#[tauri::command]
fn tool_cancel(
    tools: State<'_, Arc<LocalTools>>,
    call_id: String,
) -> Result<CallState, rex_tools::ToolError> {
    tools.cancel(&call_id)
}

#[tauri::command]
fn preview_detect(
    preview: State<'_, Arc<NativePreview>>,
    project_dir: String,
) -> Result<PreviewRecipe, String> {
    preview
        .detect(std::path::Path::new(&project_dir))
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn preview_start(
    preview: State<'_, Arc<NativePreview>>,
    project_dir: String,
) -> Result<SupervisorSummary, String> {
    preview
        .start(std::path::Path::new(&project_dir))
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn preview_status(
    preview: State<'_, Arc<NativePreview>>,
    session_id: String,
) -> Result<SupervisorSummary, String> {
    preview.summary(&session_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn preview_action(
    preview: State<'_, Arc<NativePreview>>,
    session_id: String,
    action: BrowserAction,
) -> Result<(), String> {
    preview
        .action(&session_id, &action)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn preview_capture(
    preview: State<'_, Arc<NativePreview>>,
    session_id: String,
) -> Result<BrowserEvidence, String> {
    preview.capture(&session_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn preview_begin_iteration(
    preview: State<'_, Arc<NativePreview>>,
    session_id: String,
) -> Result<u8, String> {
    preview
        .begin_iteration(&session_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn preview_record_iteration(
    preview: State<'_, Arc<NativePreview>>,
    session_id: String,
    receipt: IterationReceipt,
) -> Result<(), String> {
    preview
        .record_iteration(&session_id, receipt)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn preview_production_report(
    preview: State<'_, Arc<NativePreview>>,
    session_id: String,
) -> Result<ProductionReport, String> {
    preview
        .production_report(&session_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn preview_finish(
    preview: State<'_, Arc<NativePreview>>,
    session_id: String,
) -> Result<(), String> {
    preview.finish(&session_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn preview_cancel(
    preview: State<'_, Arc<NativePreview>>,
    session_id: String,
) -> Result<SupervisorSummary, String> {
    preview.cancel(&session_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn preview_teardown(
    preview: State<'_, Arc<NativePreview>>,
    session_id: String,
) -> Result<(), String> {
    preview.teardown(&session_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn run_begin(live: State<'_, Arc<Live>>, task: String) -> Result<RunSnapshot, String> {
    live.begin(&task, "gemini")
}

/// Trusted UI decision for a live run. Model output cannot invoke this path.
#[tauri::command]
fn run_decide(
    live: State<'_, Arc<Live>>,
    run_id: String,
    approved: bool,
) -> Result<RunSnapshot, String> {
    live.decide(&run_id, approved)
}

#[tauri::command]
fn run_snapshot(live: State<'_, Arc<Live>>, run_id: String) -> Result<RunSnapshot, String> {
    live.snapshot(&run_id)
        .ok_or_else(|| "unknown run".to_string())
}

#[tauri::command]
fn run_preview_action(
    live: State<'_, Arc<Live>>,
    run_id: String,
    action: BrowserAction,
) -> Result<(), String> {
    live.preview_action(&run_id, &action)
}

#[tauri::command]
fn run_capture(live: State<'_, Arc<Live>>, run_id: String) -> Result<BrowserEvidence, String> {
    live.capture(&run_id)
}

#[tauri::command]
fn run_teardown(live: State<'_, Arc<Live>>, run_id: String) -> Result<(), String> {
    live.teardown(&run_id)
}

// ---- custody-wired runs (human mode) --------------------------------------

fn now_ms_u128() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Begin a Simple-mode run under custody. The operator identity chosen at
/// the Human/Agent gate is recorded in the grant, the run's budgets are
/// clamped to the grant, and completion must pass the grant's evidence
/// gates. The capability token never leaves the backend.
///
/// When `plan_mode` is set, the run first produces a plan via a single
/// plan-only model turn and parks in `awaiting_plan` until the UI approves
/// it through `custody_decide_plan`. No tool executes before approval.
#[tauri::command]
fn custody_begin(
    custody_runs: State<'_, Arc<CustodyRuns>>,
    task: String,
    provider: Option<String>,
    model: Option<String>,
    plan_mode: Option<bool>,
) -> Result<rex_providers::CustodiedRunView, String> {
    let task_id = format!("task-ui-{:x}", now_ms_u128());
    // The custodied workspace sits under the agent runs root: the run
    // service refuses explicit workspaces outside it.
    let workspace = config_dir().join("agent-runs").join(&task_id);
    std::fs::create_dir_all(&workspace).map_err(|e| e.to_string())?;
    let req = rex_providers::ui_managed_request(
        task_id,
        task,
        rex_custody::OperatorIdentity::Human,
        provider.unwrap_or_else(|| "gemini".into()),
        model,
        workspace,
        plan_mode.unwrap_or(false),
    );
    let run = custody_runs.begin_managed_task(req)?;
    Ok(custody_runs.view_of(&run))
}

/// Trusted UI decision on a plan-gated run's proposed plan. Approval
/// continues the run into the normal loop; denial ends it as denied with
/// no tool having executed.
#[tauri::command]
fn custody_decide_plan(
    custody_runs: State<'_, Arc<CustodyRuns>>,
    run_id: String,
    approved: bool,
) -> Result<rex_providers::AgentSnapshot, String> {
    custody_runs.decide_plan(&run_id, approved)
}

// ---------------------------------------------------------------------------
// Fable gate: native THINK → PROVE → ATTACK → WRITE sessions.
// ---------------------------------------------------------------------------

fn fable_dir() -> std::path::PathBuf {
    config_dir().join("fable-sessions")
}

/// Serializable view of a Fable session for the UI countdown.
#[derive(serde::Serialize)]
struct FableStatusView {
    name: String,
    objective: String,
    phase: String,
    unlocked: bool,
    timer_remaining_ms: u64,
    timer_remaining_human: String,
    timer_elapsed: bool,
    proven_count: usize,
    invariant_count: usize,
    unlock_ready: bool,
}

fn fable_view_of(s: &rex_fable::FableSession) -> FableStatusView {
    let timer = s.timer();
    FableStatusView {
        name: s.name().to_string(),
        objective: s.objective().to_string(),
        phase: s.phase().to_string(),
        unlocked: s.unlocked(),
        timer_remaining_ms: timer.remaining_ms(),
        timer_remaining_human: timer.remaining_human(),
        timer_elapsed: timer.elapsed(),
        proven_count: s.ledger().proven_count(),
        invariant_count: s.ledger().invariants().len(),
        unlock_ready: s.ledger().prerequisites_met() && timer.elapsed(),
    }
}

/// Create a Fable session in THINK. The authority timer starts immediately.
#[tauri::command]
fn fable_create_session(
    name: String,
    objective: String,
    time_budget_minutes: Option<u32>,
) -> Result<FableStatusView, String> {
    let dir = fable_dir();
    let session =
        rex_fable::FableSession::create(name, objective, time_budget_minutes).map_err(|e| e.to_string())?;
    session.save(&dir).map_err(|e| e.to_string())?;
    Ok(fable_view_of(&session))
}

/// Current status of a Fable session, including the live countdown.
#[tauri::command]
fn fable_session_status(name: String) -> Result<FableStatusView, String> {
    let session =
        rex_fable::FableSession::load(&fable_dir(), &name).map_err(|e| e.to_string())?;
    Ok(fable_view_of(&session))
}

/// Attempt the PROVE → ATTACK unlock. Fails honestly when prerequisites or
/// the authority timer are unmet.
#[tauri::command]
fn fable_unlock_session(name: String, rationale: String) -> Result<FableStatusView, String> {
    let dir = fable_dir();
    let mut session =
        rex_fable::FableSession::load(&dir, &name).map_err(|e| e.to_string())?;
    session.unlock_execution(rationale).map_err(|e| e.to_string())?;
    session.save(&dir).map_err(|e| e.to_string())?;
    Ok(fable_view_of(&session))
}

// ---------------------------------------------------------------------------
// Fable-mode MCP link: probe an external Fable Engine MCP server.
// ---------------------------------------------------------------------------

/// Result of probing a fable-mode MCP server over stdio.
#[derive(serde::Serialize)]
struct FableMcpLink {
    available: bool,
    server_command: String,
    tools: Vec<String>,
    has_fable_session_tool: bool,
    error: Option<String>,
}

/// Probe an external fable-mode MCP server. Spawns `server_command` (split
/// on whitespace), performs the MCP initialize handshake, and lists tools.
/// The child is killed afterwards; this is a link check, not a session.
#[tauri::command]
fn fable_mcp_probe(server_command: String) -> Result<FableMcpLink, String> {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};
    use std::time::Duration;

    let mut parts = server_command.split_whitespace();
    let bin = parts.next().ok_or("server command is empty")?.to_string();
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
    let mut reader = BufReader::new(stdout);

    let mut next_id = 0u64;
    let mut request = |method: &str, params: serde_json::Value| -> Result<serde_json::Value, String> {
        next_id += 1;
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": next_id,
            "method": method,
            "params": params,
        });
        writeln!(stdin, "{}", msg).map_err(|e| format!("write failed: {e}"))?;
        stdin.flush().map_err(|e| format!("flush failed: {e}"))?;
        // Read with a timeout so a hung server cannot hang the UI.
        let mut line = String::new();
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_secs(10) {
            line.clear();
            // Non-blocking check: try to read; BufRead::read_line blocks,
            // so we rely on the overall spawn timeout via a helper thread
            // in production. Here we read one line; a well-behaved MCP
            // server answers initialize promptly.
            match reader.read_line(&mut line) {
                Ok(0) => break, // EOF
                Ok(_) if !line.trim().is_empty() => break,
                Ok(_) => continue,
                Err(e) => return Err(format!("read failed: {e}")),
            }
            if start.elapsed() >= Duration::from_secs(10) {
                break;
            }
        }
        if line.trim().is_empty() {
            return Err("server did not answer within 10s".to_string());
        }
        serde_json::from_str(&line).map_err(|e| format!("bad JSON from server: {e}"))
    };

    let link = (|| -> Result<FableMcpLink, String> {
        let hello = request(
            "initialize",
            serde_json::json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "rex-harness", "version": env!("CARGO_PKG_VERSION") },
            }),
        )?;
        if hello.get("result").is_none() {
            return Err(format!("server refused initialize: {hello}"));
        }
        let tools_resp = request("tools/list", serde_json::json!({}))?;
        let tools: Vec<String> = tools_resp
            .pointer("/result/tools")
            .and_then(|t| t.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        Ok(FableMcpLink {
            available: true,
            server_command: server_command.clone(),
            has_fable_session_tool: tools.iter().any(|t| t == "fable_session"),
            tools,
            error: None,
        })
    })();

    let _ = child.kill();
    let _ = child.wait();

    match link {
        Ok(mut l) => {
            if !l.has_fable_session_tool {
                l.available = false;
                l.error = Some(format!(
                    "server answered but has no fable_session tool (has: {})",
                    if l.tools.is_empty() {
                        "none".to_string()
                    } else {
                        l.tools.join(", ")
                    }
                ));
            }
            Ok(l)
        }
        Err(e) => Ok(FableMcpLink {
            available: false,
            server_command: server_command.clone(),
            tools: vec![],
            has_fable_session_tool: false,
            error: Some(e),
        }),
    }
}

// ---------------------------------------------------------------------------
// Interactive terminal: PTY sessions bound to the workspace.
// ---------------------------------------------------------------------------

/// Spawn a terminal in `workspace` (must sit under the agent runs root).
/// Returns the terminal ID. Output arrives as `terminal-output-{id}` events.
#[tauri::command]
fn terminal_spawn(
    app: tauri::AppHandle,
    terminals: State<'_, terminal::SharedTerminals>,
    workspace: String,
    cols: Option<u16>,
    rows: Option<u16>,
) -> Result<String, String> {
    let runs_root = config_dir().join("agent-runs");
    let id = terminals.spawn(
        PathBuf::from(workspace),
        &runs_root,
        cols.unwrap_or(80),
        rows.unwrap_or(24),
    )?;

    // Drain the PTY in a background thread; forward bytes as events.
    // The event name embeds the terminal ID so the UI routes correctly.
    let event_name = format!("terminal-output-{id}");
    let terminals_clone = terminals.inner().clone();
    let id_clone = id.clone();
    std::thread::spawn(move || {
        let mut reader = match terminals_clone.clone_reader(&id_clone) {
            Ok(r) => r,
            Err(_) => return,
        };
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break, // EOF: child exited
                Ok(n) => {
                    // Lossy conversion is fine for terminal output; the
                    // alternative is dropping undecodable bytes entirely.
                    let text = String::from_utf8_lossy(&buf[..n]).to_string();
                    let _ = app.emit(&event_name, text);
                }
                Err(_) => break,
            }
        }
        let _ = app.emit(&format!("terminal-exit-{id_clone}"), ());
    });

    Ok(id)
}

/// Write bytes (UTF-8) to the terminal's stdin.
#[tauri::command]
fn terminal_write(
    terminals: State<'_, terminal::SharedTerminals>,
    id: String,
    data: String,
) -> Result<(), String> {
    terminals.write(&id, data.as_bytes())
}

/// Resize the PTY.
#[tauri::command]
fn terminal_resize(
    terminals: State<'_, terminal::SharedTerminals>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    terminals.resize(&id, cols, rows)
}

/// Kill the terminal and its child.
#[tauri::command]
fn terminal_kill(
    terminals: State<'_, terminal::SharedTerminals>,
    id: String,
) -> Result<(), String> {
    terminals.kill(&id)
}

/// The default workspace for new terminals: the agent runs root.
/// The UI passes this to `terminal_spawn`; the backend re-validates it.
#[tauri::command]
fn terminal_default_workspace() -> Result<String, String> {
    let root = config_dir().join("agent-runs");
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    root.to_str()
        .map(|s| s.to_string())
        .ok_or("workspace path is not UTF-8".to_string())
}

/// Current custody phase for a grant (active, verifying, released, ...).
#[tauri::command]
fn custody_phase(
    custody_runs: State<'_, Arc<CustodyRuns>>,
    grant_id: String,
) -> Option<rex_custody::CustodyPhase> {
    custody_runs.phase_of(&grant_id)
}

/// The permanent human Stop for a custodied run: terminal fence first,
/// then the run loop is cancelled. Late success cannot win.
#[tauri::command]
fn custody_stop(
    custody_runs: State<'_, Arc<CustodyRuns>>,
    grant_id: String,
    run_id: String,
) -> Result<String, String> {
    custody_runs
        .human_stop(&grant_id, &run_id)
        .map(|reason| format!("{reason:?}"))
}

// ---- agent-mode supervision through rex-mcp (the host protocol) ---------

/// The shell reaches host-driven REX tasks through the same rex-mcp stdio
/// protocol hosts use, against the shared default state dir - never through
/// a private side channel into the daemon.
fn rex_shell_call(tool: &str, arguments: serde_json::Value) -> Result<serde_json::Value, String> {
    let bin = rex_mcp::client::rex_mcp_bin()?;
    let state = rex_mcp::client::default_state_dir();
    let workspace = config_dir().join("agent-runs").join("rex-shell");
    std::fs::create_dir_all(&workspace).map_err(|e| e.to_string())?;
    rex_mcp::client::call_tool_once(&bin, &state, &workspace, tool, arguments)
}

/// Open a durable REX task for an external host agent to drive. Custody
/// records an agent operator from the start; the task waits active until a
/// host session resumes it by id through rex-mcp.
#[tauri::command]
fn rex_task_begin(task: String, ultra: Option<bool>) -> Result<serde_json::Value, String> {
    let response = rex_shell_call(
        "rex_execute",
        serde_json::json!({
            "request_id": format!("ui-{:x}", now_ms_u128()),
            "task": task,
            "host": "generic_agent",
            "operator_is_agent": true,
            "ultra": ultra.unwrap_or(false),
        }),
    )?;
    remember_rex_resume_handle(&response)?;
    Ok(task_response(&response))
}

#[tauri::command]
fn rex_task_follow_up(task_id: String, task: String) -> Result<serde_json::Value, String> {
    let handle = rex_resume_handles()
        .lock()
        .map_err(|_| "resume handle store unavailable".to_string())?
        .get(&task_id)
        .cloned()
        .ok_or_else(|| {
            "resume handle unavailable; reopen this task through its trusted host".to_string()
        })?;
    let original = rex_shell_call("rex_status", serde_json::json!({ "task_id": task_id }))?;
    let original_task = original
        .get("task")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "REX status returned no task text".to_string())?;
    let response = rex_shell_call(
        "rex_execute",
        serde_json::json!({
            "request_id": format!("ui-follow-up-{:x}", now_ms_u128()),
            "task": original_task,
            "task_id": task_id,
            "resume_handle": handle,
            "follow_up": task,
            "host": "generic_agent",
            "operator_is_agent": true,
        }),
    )?;
    remember_rex_resume_handle(&response)?;
    Ok(task_response(&response))
}

fn remember_rex_resume_handle(response: &serde_json::Value) -> Result<(), String> {
    let task_id = response
        .get("task_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "rex-mcp returned no task id".to_string())?;
    let handle = response
        .get("host_resume_handle")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "rex-mcp returned no resume handle".to_string())?;
    rex_resume_handles()
        .lock()
        .map_err(|_| "resume handle store unavailable".to_string())?
        .insert(task_id.to_string(), handle.to_string());
    Ok(())
}

fn task_response(response: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "task_id": response.get("task_id"),
        "state": response.get("state"),
        "resumed": response.get("resumed").unwrap_or(&serde_json::Value::Bool(false)),
    })
}

/// Compact view of every durable REX task in the shared store: read-only
/// directory scan, no daemon lock taken.
#[tauri::command]
fn rex_tasks() -> serde_json::Value {
    let mut tasks = Vec::new();
    if let Ok(entries) = std::fs::read_dir(rex_mcp::client::default_state_dir().join("tasks")) {
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
    serde_json::json!({ "tasks": tasks })
}

#[tauri::command]
fn rex_task_status(task_id: String) -> Result<serde_json::Value, String> {
    rex_shell_call("rex_status", serde_json::json!({ "task_id": task_id }))
}

#[tauri::command]
fn rex_task_events(task_id: String, after_seq: Option<u64>) -> Result<serde_json::Value, String> {
    rex_shell_call(
        "rex_events",
        serde_json::json!({
            "task_id": task_id,
            "after_seq": after_seq.unwrap_or(0),
            "limit": 200,
        }),
    )
}

/// The deterministic per-task proof bundle the proof journey renders.
#[tauri::command]
fn rex_task_proof(task_id: String) -> Result<serde_json::Value, String> {
    rex_shell_call("rex_proof", serde_json::json!({ "task_id": task_id }))
}

/// The permanent human Stop for an agent-driven task: a distinct authority
/// from operator cancel. The trusted local launcher reads the daemon's
/// human-stop token (0600 in the shared state dir); an MCP host cannot mint
/// it. Final in every phase, even against an agent operator.
#[tauri::command]
fn rex_task_stop(task_id: String) -> Result<serde_json::Value, String> {
    let token_path = rex_mcp::client::default_state_dir().join("human-stop-token");
    let token = std::fs::read_to_string(&token_path)
        .map_err(|e| format!("human-stop token unavailable: {e}"))?;
    rex_shell_call(
        "rex_human_stop",
        serde_json::json!({
            "task_id": task_id,
            "human_token": token.trim(),
            "reason": "human stop from REX UI",
        }),
    )
}

/// Autonomous Simple Mode run: many bounded model turns, visible plan,
/// trusted approvals, concrete completion gates, truthful terminal reasons.
#[tauri::command]
fn agent_begin(
    agent: State<'_, Arc<Agent>>,
    task: String,
    provider: Option<String>,
    model: Option<String>,
    budgets: Option<Budgets>,
) -> Result<AgentSnapshot, String> {
    let provider = provider.as_deref().unwrap_or("gemini");
    agent.begin_with_model(&task, provider, model.as_deref(), budgets)
}

#[tauri::command]
fn agent_snapshot(agent: State<'_, Arc<Agent>>, run_id: String) -> Result<AgentSnapshot, String> {
    agent
        .snapshot(&run_id)
        .ok_or_else(|| "unknown run".to_string())
}

/// Trusted UI decision. Only this command can release a prepared write.
#[tauri::command]
fn agent_decide(
    agent: State<'_, Arc<Agent>>,
    run_id: String,
    approved: bool,
) -> Result<AgentSnapshot, String> {
    agent.decide(&run_id, approved)
}

#[tauri::command]
fn agent_cancel(agent: State<'_, Arc<Agent>>, run_id: String) -> Result<AgentSnapshot, String> {
    agent.cancel(&run_id)
}

#[tauri::command]
fn agent_resume(agent: State<'_, Arc<Agent>>, run_id: String) -> Result<AgentSnapshot, String> {
    agent.resume(&run_id)
}

#[tauri::command]
fn agent_preview_action(
    agent: State<'_, Arc<Agent>>,
    run_id: String,
    action: BrowserAction,
) -> Result<(), String> {
    agent.preview_action(&run_id, &action)
}

#[tauri::command]
fn agent_capture(agent: State<'_, Arc<Agent>>, run_id: String) -> Result<BrowserEvidence, String> {
    agent.capture(&run_id)
}

#[tauri::command]
fn agent_teardown(agent: State<'_, Arc<Agent>>, run_id: String) -> Result<(), String> {
    agent.teardown(&run_id)
}

/// Ultra run: the same worker model wrapped in contract compilation,
/// deterministic re-verification, an adversary pass and a clean-room judge.
/// The judge/adversary default to the worker identity unless configured by
/// REX_ULTRA_JUDGE_PROVIDER / REX_ULTRA_JUDGE_MODEL (and _ADVERSARY_).
#[tauri::command]
fn ultra_begin(
    ultra: State<'_, Arc<Ultra>>,
    task: String,
    provider: Option<String>,
    model: Option<String>,
) -> Result<UltraSnapshot, String> {
    let provider = provider.as_deref().unwrap_or("gemini");
    let options = UltraOptions {
        judge_provider: std::env::var("REX_ULTRA_JUDGE_PROVIDER").ok(),
        judge_model: std::env::var("REX_ULTRA_JUDGE_MODEL").ok(),
        adversary_provider: std::env::var("REX_ULTRA_ADVERSARY_PROVIDER").ok(),
        adversary_model: std::env::var("REX_ULTRA_ADVERSARY_MODEL").ok(),
        adversary_enabled: None,
    };
    ultra.begin(&task, provider, model.as_deref(), options)
}

#[tauri::command]
fn ultra_snapshot(ultra: State<'_, Arc<Ultra>>, run_id: String) -> Result<UltraSnapshot, String> {
    ultra
        .snapshot(&run_id)
        .ok_or_else(|| "unknown run".to_string())
}

/// Trusted UI decision for the currently active builder/adversary sub-run.
#[tauri::command]
fn ultra_decide(
    ultra: State<'_, Arc<Ultra>>,
    run_id: String,
    approved: bool,
) -> Result<UltraSnapshot, String> {
    ultra.decide(&run_id, approved)
}

#[tauri::command]
fn ultra_cancel(ultra: State<'_, Arc<Ultra>>, run_id: String) -> Result<UltraSnapshot, String> {
    ultra.cancel(&run_id)
}

fn provider_bridge_smoke() -> bool {
    if std::env::args().any(|arg| arg == "--verify-provider-bridge") {
        let dir = std::env::temp_dir().join(format!("rex-harness-smoke-{}", std::process::id()));
        let store =
            FileSecretStore::new(dir.clone()).expect("could not open smoke credential store");
        let service = ProviderService::new(store, UreqTransport::new());
        let summaries = service.summaries();
        assert!(!summaries.is_empty(), "provider registry must not be empty");
        println!(
            "{}",
            serde_json::to_string(&summaries).expect("provider summaries must serialize")
        );
        let _ = std::fs::remove_dir_all(dir);
        return true;
    }
    false
}

fn main() {
    // A native-binary smoke path verifies that this exact desktop executable
    // contains and can initialize the provider core, without opening a window
    // or requiring credentials. It never reads the normal credential store.
    if provider_bridge_smoke() {
        return;
    }

    let store = FileSecretStore::new(config_dir()).expect("could not open the credential store");
    let service = Arc::new(ProviderService::new(store, UreqTransport::new()));
    let search_store =
        FileSecretStore::new(config_dir()).expect("could not open search credential store");
    let search_service = Arc::new(SearchRouter::new(search_store, UreqTransport::new()));
    let workspace = std::env::var("REX_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| config_dir().join("workspace"));
    let tools =
        Arc::new(ToolRuntime::new(workspace.clone()).expect("could not open local tool workspace"));
    let preview =
        Arc::new(PreviewSupervisor::new(workspace).expect("could not open preview workspace"));
    let live = Arc::new(LiveRunService::new(
        ProviderService::new(
            FileSecretStore::new(config_dir()).expect("could not open the credential store"),
            UreqTransport::new(),
        ),
        config_dir().join("runs"),
    ));
    let installed_runs = Arc::new(
        InstalledAgentManager::new(config_dir().join("installed-agent-runs"))
            .expect("could not open installed-agent run store"),
    );
    let agent = Arc::new(AutonomousRunService::new(
        ProviderService::new(
            FileSecretStore::new(config_dir()).expect("could not open the credential store"),
            UreqTransport::new(),
        ),
        Some(SearchRouter::new(
            FileSecretStore::new(config_dir()).expect("could not open search credential store"),
            UreqTransport::new(),
        )),
        config_dir().join("agent-runs"),
    ));
    // Ultra shares the Simple run service: one provider core, one approval
    // channel, one runs root. Ultra adds its verification phases on top.
    let ultra = Arc::new(UltraRunService::new(
        agent.clone(),
        config_dir().join("agent-runs"),
    ));
    // Custody wraps the same run service: human-mode runs carry an operator
    // grant, grant-clamped budgets and gated completion.
    let custody_runs = Arc::new(CustodyRuns::new(
        agent.clone(),
        Arc::new(std::sync::Mutex::new(
            rex_custody::CustodyRegistry::open(config_dir().join("custody"))
                .expect("could not open the custody store"),
        )),
    ));

    tauri::Builder::default()
        .manage(service)
        .manage(search_service)
        .manage(tools)
        .manage(preview)
        .manage(live)
        .manage(agent)
        .manage(ultra)
        .manage(custody_runs)
        .manage(installed_runs)
        .manage(terminal::SharedTerminals::default())
        .invoke_handler(tauri::generate_handler![
            installed_agent_summaries,
            installed_agent_run,
            installed_agent_begin,
            installed_agent_snapshot,
            installed_agent_decide,
            installed_agent_cancel,
            provider_summaries,
            provider_set_key,
            provider_clear_key,
            provider_refresh,
            provider_catalog,
            search_live,
            search_provider_summaries,
            search_provider_set_key,
            search_provider_clear_key,
            search_provider_select,
            search_index_upsert,
            search_index_remove,
            search_index_query,
            tool_prepare,
            tool_resolve_approval,
            tool_execute,
            tool_cancel,
            preview_detect,
            preview_start,
            preview_status,
            preview_action,
            preview_capture,
            preview_begin_iteration,
            preview_record_iteration,
            preview_production_report,
            preview_finish,
            preview_cancel,
            preview_teardown,
            run_begin,
            run_decide,
            run_snapshot,
            run_preview_action,
            run_capture,
            run_teardown,
            custody_begin,
            custody_phase,
            custody_stop,
            custody_decide_plan,
            fable_create_session,
            fable_session_status,
            fable_unlock_session,
            fable_mcp_probe,
            terminal_spawn,
            terminal_write,
            terminal_resize,
            terminal_kill,
            terminal_default_workspace,
            rex_task_begin,
            rex_task_follow_up,
            rex_tasks,
            rex_task_status,
            rex_task_events,
            rex_task_proof,
            rex_task_stop,
            agent_begin,
            agent_snapshot,
            agent_decide,
            agent_cancel,
            agent_resume,
            agent_preview_action,
            agent_capture,
            agent_teardown,
            ultra_begin,
            ultra_snapshot,
            ultra_decide,
            ultra_cancel,
        ])
        .run(tauri::generate_context!())
        .expect("error while running REX Harness");
}
