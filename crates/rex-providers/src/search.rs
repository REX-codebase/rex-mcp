use crate::{ProviderError, SecretStore, Transport};
use rex_search::{
    Coverage, DiscoveryKind, Evidence, FetchState, SearchEngine, SearchRequest, SearchResponse,
};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub const EXA_DOCS: &str = "https://exa.ai/docs/reference/search";
pub const TINYFISH_DOCS: &str = "https://docs.tinyfish.ai/search-api/reference";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchProvider {
    Rex,
    Exa,
    Tinyfish,
}
impl SearchProvider {
    pub fn id(self) -> &'static str {
        match self {
            Self::Rex => "rex",
            Self::Exa => "exa",
            Self::Tinyfish => "tinyfish",
        }
    }
    pub fn credential_id(self) -> Option<&'static str> {
        match self {
            Self::Rex => None,
            Self::Exa => Some("search:exa"),
            Self::Tinyfish => Some("search:tinyfish"),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchProviderSummary {
    pub id: String,
    pub name: String,
    pub active: bool,
    pub built_in: bool,
    pub has_key: bool,
    pub endpoint: String,
    pub docs_url: String,
}

pub struct SearchRouter<S: SecretStore, T: Transport> {
    secrets: S,
    transport: T,
    active: Mutex<SearchProvider>,
}
impl<S: SecretStore, T: Transport> SearchRouter<S, T> {
    pub fn new(secrets: S, transport: T) -> Self {
        Self {
            secrets,
            transport,
            active: Mutex::new(SearchProvider::Rex),
        }
    }
    pub fn summaries(&self) -> Vec<SearchProviderSummary> {
        let active = self.active();
        [
            (
                SearchProvider::Rex,
                "REX-search",
                "local bounded retrieval",
                "",
            ),
            (
                SearchProvider::Exa,
                "Exa",
                "https://api.exa.ai/search",
                EXA_DOCS,
            ),
            (
                SearchProvider::Tinyfish,
                "TinyFish",
                "https://api.search.tinyfish.ai",
                TINYFISH_DOCS,
            ),
        ]
        .into_iter()
        .map(|(p, name, endpoint, docs)| SearchProviderSummary {
            id: p.id().into(),
            name: name.into(),
            active: p == active,
            built_in: p == SearchProvider::Rex,
            has_key: p
                .credential_id()
                .map(|id| self.secrets.has_key(id))
                .unwrap_or(true),
            endpoint: endpoint.into(),
            docs_url: docs.into(),
        })
        .collect()
    }
    pub fn active(&self) -> SearchProvider {
        self.active
            .lock()
            .map(|v| *v)
            .unwrap_or(SearchProvider::Rex)
    }
    pub fn select(&self, provider: SearchProvider) -> Result<(), ProviderError> {
        if let Some(id) = provider.credential_id() {
            if !self.secrets.has_key(id) {
                return Err(ProviderError::NotConfigured);
            }
        }
        *self
            .active
            .lock()
            .map_err(|e| ProviderError::Store(e.to_string()))? = provider;
        Ok(())
    }
    pub fn set_key(&self, provider: SearchProvider, key: &str) -> Result<(), ProviderError> {
        let id = provider
            .credential_id()
            .ok_or_else(|| ProviderError::Unsupported("REX-search needs no API key".into()))?;
        if key.trim().is_empty() {
            return Err(ProviderError::NotConfigured);
        }
        self.secrets.set_key(id, key.trim())
    }
    pub fn clear_key(&self, provider: SearchProvider) -> Result<(), ProviderError> {
        let id = provider
            .credential_id()
            .ok_or_else(|| ProviderError::Unsupported("REX-search needs no API key".into()))?;
        self.secrets.clear_key(id)?;
        if self.active() == provider {
            *self
                .active
                .lock()
                .map_err(|e| ProviderError::Store(e.to_string()))? = SearchProvider::Rex;
        }
        Ok(())
    }
    pub fn search(&self, request: SearchRequest) -> Result<SearchResponse, ProviderError> {
        match self.active() {
            SearchProvider::Rex => Ok(SearchEngine::default().search(request)),
            SearchProvider::Exa => self.search_exa(request),
            SearchProvider::Tinyfish => self.search_tinyfish(request),
        }
    }
    fn key(&self, p: SearchProvider) -> Result<String, ProviderError> {
        self.secrets
            .get_key(p.credential_id().unwrap())?
            .ok_or(ProviderError::NotConfigured)
    }
    fn search_exa(&self, request: SearchRequest) -> Result<SearchResponse, ProviderError> {
        let body=serde_json::json!({"query":request.query,"numResults":request.max_results,"contents":{"text":{"maxCharacters":24000},"highlights":{"numSentences":3}}}).to_string();
        let headers = vec![
            ("x-api-key".into(), self.key(SearchProvider::Exa)?),
            ("content-type".into(), "application/json".into()),
        ];
        let (status, body) = self
            .transport
            .post("https://api.exa.ai/search", &headers, &body)?;
        normalize(status, &body, &request, "exa")
    }
    fn search_tinyfish(&self, request: SearchRequest) -> Result<SearchResponse, ProviderError> {
        let mut url = url::Url::parse("https://api.search.tinyfish.ai").unwrap();
        url.query_pairs_mut().append_pair("query", &request.query);
        let headers = vec![("X-API-Key".into(), self.key(SearchProvider::Tinyfish)?)];
        let (status, body) = self.transport.get(url.as_str(), &headers)?;
        normalize(status, &body, &request, "tinyfish")
    }
}
fn normalize(
    status: u16,
    body: &str,
    request: &SearchRequest,
    source: &str,
) -> Result<SearchResponse, ProviderError> {
    match status {
        401 | 403 => return Err(ProviderError::AuthFailed),
        429 => return Err(ProviderError::RateLimited),
        200..=299 => {}
        s => return Err(ProviderError::InvalidResponse(format!("HTTP {s}"))),
    }
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| ProviderError::InvalidResponse(format!("invalid JSON: {e}")))?;
    let list = value
        .get("results")
        .or_else(|| value.get("data"))
        .and_then(|v| v.as_array())
        .ok_or_else(|| ProviderError::InvalidResponse("missing results array".into()))?;
    let mut evidence = Vec::new();
    for (i, item) in list.iter().take(request.max_results).enumerate() {
        let url = item.get("url").and_then(|v| v.as_str()).unwrap_or("");
        if url.is_empty() {
            continue;
        }
        let title = item
            .get("title")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let content = item
            .get("text")
            .or_else(|| item.get("content"))
            .or_else(|| item.get("snippet"))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let excerpt = item
            .get("snippet")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| {
                item.get("highlights").and_then(|v| v.as_array()).map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
            });
        evidence.push(Evidence {
            url: url.into(),
            final_url: url.into(),
            redirect_chain: vec![url.into()],
            title,
            excerpt,
            content,
            retrieved_at_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
            http_status: Some(200),
            content_type: Some("application/json".into()),
            discovery: DiscoveryKind::Seed,
            discovered_from: Some(source.into()),
            state: FetchState::Fetched,
            error: None,
            score: item
                .get("score")
                .and_then(|v| v.as_f64())
                .map(|v| v as f32)
                .unwrap_or(1.0 / (i + 1) as f32),
            robots_allowed: None,
        });
    }
    if evidence.is_empty() {
        return Err(ProviderError::EmptyCatalog);
    }
    Ok(SearchResponse {
        query: request.query.clone(),
        coverage: Coverage {
            model: format!("hosted:{source}"),
            seeds_received: request.seeds.len(),
            pages_attempted: evidence.len(),
            pages_fetched: evidence.len(),
            pages_denied: 0,
            redirects_followed: 0,
            sitemap_urls_discovered: 0,
            feed_urls_discovered: 0,
            truncated_by_budget: list.len() > request.max_results,
            disclaimer: format!(
                "Results supplied by {source}; provider ranking and crawl policy apply."
            ),
        },
        evidence,
        failures: vec![],
    })
}
