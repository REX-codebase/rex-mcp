// REX Harness desktop shell with the first real backend slice: provider
// connectivity. The Rust core (`rex-providers`) owns credentials, provider
// HTTP, and model catalogs. The frontend receives summaries and catalogs
// only - key material never crosses the bridge.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use rex_providers::{FileSecretStore, ModelCatalog, ProviderError, ProviderService, ProviderSummary, UreqTransport};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::State;

type Service = ProviderService<FileSecretStore, UreqTransport>;

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
fn provider_clear_key(service: State<'_, Arc<Service>>, provider: String) -> Result<(), ProviderError> {
    service.clear_key(&provider)
}

#[tauri::command]
fn provider_refresh(service: State<'_, Arc<Service>>, provider: String) -> Result<ModelCatalog, ProviderError> {
    service.refresh(&provider)
}

#[tauri::command]
fn provider_catalog(service: State<'_, Arc<Service>>, provider: String) -> Option<ModelCatalog> {
    service.catalog(&provider)
}

fn provider_bridge_smoke() -> bool {
    if std::env::args().any(|arg| arg == "--verify-provider-bridge") {
        let dir = std::env::temp_dir().join(format!("rex-harness-smoke-{}", std::process::id()));
        let store = FileSecretStore::new(dir.clone()).expect("could not open smoke credential store");
        let service = ProviderService::new(store, UreqTransport::new());
        let summaries = service.summaries();
        assert!(!summaries.is_empty(), "provider registry must not be empty");
        println!("{}", serde_json::to_string(&summaries).expect("provider summaries must serialize"));
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

    tauri::Builder::default()
        .manage(service)
        .invoke_handler(tauri::generate_handler![
            provider_summaries,
            provider_set_key,
            provider_clear_key,
            provider_refresh,
            provider_catalog,
        ])
        .run(tauri::generate_context!())
        .expect("error while running REX Harness");
}
