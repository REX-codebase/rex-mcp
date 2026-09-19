// REX Harness desktop shell with the first real backend slice: provider
// connectivity. The Rust core (`rex-providers`) owns credentials, provider
// HTTP, and model catalogs. The frontend receives summaries and catalogs
// only - key material never crosses the bridge.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

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
use std::path::PathBuf;
use std::sync::Arc;
use tauri::State;

type Service = ProviderService<FileSecretStore, UreqTransport>;
type Live = LiveRunService<FileSecretStore, UreqTransport>;
type Agent = AutonomousRunService<FileSecretStore, UreqTransport>;
type SearchService = SearchRouter<FileSecretStore, UreqTransport>;
type LocalTools = ToolRuntime;
type NativePreview = PreviewSupervisor;

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

    tauri::Builder::default()
        .manage(service)
        .manage(search_service)
        .manage(tools)
        .manage(preview)
        .manage(live)
        .manage(agent)
        .invoke_handler(tauri::generate_handler![
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
            agent_begin,
            agent_snapshot,
            agent_decide,
            agent_cancel,
            agent_resume,
            agent_preview_action,
            agent_capture,
            agent_teardown,
        ])
        .run(tauri::generate_context!())
        .expect("error while running REX Harness");
}
