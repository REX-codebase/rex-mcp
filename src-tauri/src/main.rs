// REX Harness desktop shell with the first real backend slice: provider
// connectivity. The Rust core (`rex-providers`) owns credentials, provider
// HTTP, and model catalogs. The frontend receives summaries and catalogs
// only - key material never crosses the bridge.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use rex_providers::{
    FileSecretStore, ModelCatalog, ProviderError, ProviderService, ProviderSummary, SearchProvider,
    SearchProviderSummary, SearchRouter, UreqTransport,
};
use rex_search::{IndexHit, IndexedDocument, LocalIndex, SearchRequest, SearchResponse};
use rex_tools::{CallState, PreparedCall, ToolRequest, ToolResult, ToolRuntime};
use rex_preview::{detect_project, BrowserAction, LaunchPlan, PreviewError, PreviewRecipe, PreviewSession, SessionState, PORT_MIN, PORT_MAX};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::collections::{BTreeMap, HashMap};
use std::time::SystemTime;
use tauri::State;

type Service = ProviderService<FileSecretStore, UreqTransport>;
type SearchService = SearchRouter<FileSecretStore, UreqTransport>;
type LocalTools = ToolRuntime;
type Previews = Mutex<HashMap<String, PreviewSession>>;

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


fn preview_manifest_snapshot(workspace: &std::path::Path, project_dir: &std::path::Path) -> Result<BTreeMap<String, String>, PreviewError> {
    let root = workspace.join(project_dir);
    let mut files = BTreeMap::new();
    for name in ["package.json", "index.html", "vite.config.ts", "vite.config.js", "next.config.js", "next.config.mjs", "next.config.ts"] {
        let path = root.join(name);
        if path.is_file() {
            let text = std::fs::read_to_string(path).map_err(|_| PreviewError::UnsupportedProject)?;
            if text.len() <= 1024 * 1024 { files.insert(name.to_string(), text); }
        }
    }
    Ok(files)
}

#[tauri::command]
fn preview_detect(project_dir: String) -> Result<PreviewRecipe, String> {
    let workspace = std::env::var("REX_WORKSPACE_ROOT").map(PathBuf::from).unwrap_or_else(|_| config_dir().join("workspace"));
    let project = PathBuf::from(project_dir);
    let files = preview_manifest_snapshot(&workspace, &project).map_err(|e| e.to_string())?;
    detect_project(&workspace, &project, &files).map_err(|e| e.to_string())
}

#[derive(serde::Serialize)]
struct PreviewSessionSummary { id: String, state: SessionState, url: String, framework: rex_preview::Framework, iteration: u8, cancellation_requested: bool }

fn preview_summary(session: &PreviewSession, framework: rex_preview::Framework) -> Result<PreviewSessionSummary, String> {
    Ok(PreviewSessionSummary { id: session.id.clone(), state: session.state.clone(), url: session.launch.url("/").map_err(|e| e.to_string())?.to_string(), framework, iteration: session.iteration, cancellation_requested: session.cancellation_requested })
}

#[tauri::command]
fn preview_start(previews: State<'_, Arc<Previews>>, project_dir: String) -> Result<PreviewSessionSummary, String> {
    let recipe = preview_detect(project_dir)?;
    let port = PORT_MIN + (std::process::id() as u16 % (PORT_MAX - PORT_MIN));
    let launch = LaunchPlan::from_recipe(&recipe, port).map_err(|e| e.to_string())?;
    let id = format!("preview-{}-{}", std::process::id(), port);
    let mut session = PreviewSession::new(id.clone(), launch, SystemTime::now());
    session.start().map_err(|e| e.to_string())?;
    // Process spawning belongs to the native supervisor increment. Until then the
    // session remains `starting`; the UI must never claim it is live.
    let summary = preview_summary(&session, recipe.framework)?;
    previews.lock().map_err(|_| "preview registry poisoned")?.insert(id, session);
    Ok(summary)
}

#[tauri::command]
fn preview_action(previews: State<'_, Arc<Previews>>, session_id: String, action: BrowserAction) -> Result<(), String> {
    action.validate().map_err(|e| e.to_string())?;
    let previews = previews.lock().map_err(|_| "preview registry poisoned")?;
    let session = previews.get(&session_id).ok_or("preview session not found")?;
    if session.terminal() { return Err("preview session is terminal".into()); }
    Ok(())
}

#[tauri::command]
fn preview_cancel(previews: State<'_, Arc<Previews>>, session_id: String) -> Result<(), String> {
    let mut previews = previews.lock().map_err(|_| "preview registry poisoned")?;
    previews.get_mut(&session_id).ok_or("preview session not found")?.cancel();
    Ok(())
}

#[tauri::command]
fn preview_teardown(previews: State<'_, Arc<Previews>>, session_id: String) -> Result<(), String> {
    let mut previews = previews.lock().map_err(|_| "preview registry poisoned")?;
    if let Some(mut session) = previews.remove(&session_id) { session.cancel(); }
    Ok(())
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
    let tools = Arc::new(ToolRuntime::new(workspace).expect("could not open local tool workspace"));
    let previews: Arc<Previews> = Arc::new(Mutex::new(HashMap::new()));

    tauri::Builder::default()
        .manage(service)
        .manage(search_service)
        .manage(tools)
        .manage(previews)
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
            preview_detect,
            preview_start,
            preview_action,
            preview_cancel,
            preview_teardown,
        ])
        .run(tauri::generate_context!())
        .expect("error while running REX Harness");
}
