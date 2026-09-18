use serde::{Deserialize, Serialize};

/// Where the catalog data came from. `Live` means the provider API answered
/// during this fetch. `Replay` means a previously recorded live response was
/// loaded from disk (development only, and labeled as such).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogSource {
    Live,
    Replay,
}

/// One available model, normalized across provider protocols.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Exact provider model ID, usable in API calls (e.g. "gemini-2.5-flash").
    pub id: String,
    /// Human label when the provider supplies one; falls back to the ID.
    pub label: String,
    /// Registry provider ID this model belongs to.
    pub provider: String,
}

/// A fetched model catalog for one provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCatalog {
    pub provider: String,
    /// Unix seconds when the provider (or the recording) produced this data.
    pub fetched_at: u64,
    pub source: CatalogSource,
    pub models: Vec<ModelInfo>,
}

impl ModelCatalog {
    pub fn live(provider: &str, fetched_at: u64, models: Vec<ModelInfo>) -> Self {
        Self { provider: provider.to_string(), fetched_at, source: CatalogSource::Live, models }
    }
}
