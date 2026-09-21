//! REX-search: bounded live-web discovery and evidence retrieval for agents.
//!
//! This is deliberately not a browser and not a web-scale search index. An
//! agent supplies public seed URLs; REX-search checks robots.txt, fetches live
//! HTML, follows a small number of same-origin links, extracts readable text,
//! ranks it against the query, and returns evidence with explicit provenance.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io;
use std::net::{IpAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::DefaultConnector;
use ureq::Error as UreqError;
use url::Url;

pub const USER_AGENT: &str = "REX-search/0.2 (+https://github.com/REX-codebase/rex-harness)";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    pub seeds: Vec<String>,
    #[serde(default = "default_max_pages")]
    pub max_pages: usize,
    #[serde(default = "default_max_results")]
    pub max_results: usize,
    #[serde(default)]
    pub allow_subdomains: bool,
    #[serde(default = "default_true")]
    pub discover_sitemaps: bool,
    #[serde(default = "default_true")]
    pub discover_feeds: bool,
}
fn default_max_pages() -> usize {
    24
}
fn default_max_results() -> usize {
    8
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryKind {
    Seed,
    Link,
    Sitemap,
    Feed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FetchState {
    Fetched,
    RobotsDenied,
    InvalidUrl,
    UnsafeAddress,
    OutOfScopeRedirect,
    RedirectMissingLocation,
    RedirectLimit,
    UnsupportedContent,
    HttpError,
    RateLimited,
    NetworkError,
    TooLarge,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub url: String,
    pub final_url: String,
    pub redirect_chain: Vec<String>,
    pub title: Option<String>,
    pub excerpt: Option<String>,
    pub content: Option<String>,
    pub retrieved_at_unix_ms: u128,
    pub http_status: Option<u16>,
    pub content_type: Option<String>,
    pub discovery: DiscoveryKind,
    pub discovered_from: Option<String>,
    pub state: FetchState,
    pub error: Option<String>,
    pub score: f32,
    pub robots_allowed: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Coverage {
    pub model: String,
    pub seeds_received: usize,
    pub pages_attempted: usize,
    pub pages_fetched: usize,
    pub pages_denied: usize,
    pub redirects_followed: usize,
    pub sitemap_urls_discovered: usize,
    pub feed_urls_discovered: usize,
    pub truncated_by_budget: bool,
    pub disclaimer: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    pub query: String,
    pub evidence: Vec<Evidence>,
    pub failures: Vec<Evidence>,
    pub coverage: Coverage,
}

#[derive(Debug, Clone)]
pub struct SearchConfig {
    pub timeout: Duration,
    pub per_origin_delay: Duration,
    pub max_body_bytes: usize,
    pub max_content_chars: usize,
    pub max_links_per_page: usize,
    pub max_redirects: usize,
    pub max_discovery_documents: usize,
    pub max_discovered_urls: usize,
}
impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(15),
            per_origin_delay: Duration::from_millis(750),
            max_body_bytes: 2 * 1024 * 1024,
            max_content_chars: 24_000,
            max_links_per_page: 32,
            max_redirects: 5,
            max_discovery_documents: 8,
            max_discovered_urls: 128,
        }
    }
}

pub struct SearchEngine {
    agent: ureq::Agent,
    config: SearchConfig,
    /// Test-only escape hatch: adversarial HTTP fixtures bind loopback, so the
    /// crate's own unit tests may permit literal loopback destinations. This
    /// field exists only in `cfg(test)` builds; production code paths can
    /// never enable it.
    #[cfg(test)]
    allow_test_loopback: bool,
}

impl SearchEngine {
    pub fn new(config: SearchConfig) -> Self {
        let http_config = ureq::Agent::config_builder()
            .timeout_global(Some(config.timeout))
            .max_redirects(0)
            .http_status_as_error(false)
            .user_agent(USER_AGENT)
            .build();
        let agent = ureq::Agent::with_parts(
            http_config,
            DefaultConnector::new(),
            PublicResolver::default(),
        );
        Self {
            agent,
            config,
            #[cfg(test)]
            allow_test_loopback: false,
        }
    }

    #[cfg(test)]
    fn new_for_tests(config: SearchConfig) -> Self {
        let http_config = ureq::Agent::config_builder()
            .timeout_global(Some(config.timeout))
            .max_redirects(0)
            .http_status_as_error(false)
            .user_agent(USER_AGENT)
            .build();
        let agent = ureq::Agent::with_parts(
            http_config,
            DefaultConnector::new(),
            PublicResolver::allowing_loopback(),
        );
        Self {
            agent,
            config,
            allow_test_loopback: true,
        }
    }

    /// Destination check applied to seeds and robots fetches. In test builds
    /// with the loopback fixture flag, literal loopback hosts are permitted;
    /// every other rule still applies.
    fn check_destination(&self, url: &Url) -> Result<(), String> {
        #[cfg(test)]
        if self.allow_test_loopback && is_loopback_host(url) {
            return Ok(());
        }
        reject_private_destination(url)
    }

    /// Per-hop local-address check. Same test-only loopback carve-out.
    fn check_local(&self, url: &Url) -> Result<(), String> {
        #[cfg(test)]
        if self.allow_test_loopback && is_loopback_host(url) {
            return Ok(());
        }
        reject_local_hostname(url)
    }

    fn normalize_seed(&self, raw: &str) -> Result<Url, (FetchState, String)> {
        let url = Url::parse(raw).map_err(|e| (FetchState::InvalidUrl, e.to_string()))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err((
                FetchState::InvalidUrl,
                "only http and https are supported".into(),
            ));
        }
        self.check_destination(&url)
            .map_err(|e| (FetchState::UnsafeAddress, e))?;
        Ok(url)
    }

    pub fn search(&self, mut request: SearchRequest) -> SearchResponse {
        request.max_pages = request.max_pages.clamp(1, 64);
        request.max_results = request.max_results.clamp(1, 20);
        let seed_count = request.seeds.len();
        let query_terms = terms(&request.query);
        let mut queue = VecDeque::new();
        let mut seen = HashSet::new();
        let mut discovery_seen = HashSet::new();
        let mut roots = Vec::new();
        let mut evidence = Vec::new();
        let mut failures = Vec::new();
        let mut robots: HashMap<String, RobotsRules> = HashMap::new();
        let mut last_request: HashMap<String, Instant> = HashMap::new();
        let mut redirects_followed = 0;
        let mut sitemap_urls_discovered = 0;
        let mut feed_urls_discovered = 0;

        for seed in &request.seeds {
            match self.normalize_seed(seed) {
                Ok(url) => {
                    if !roots
                        .iter()
                        .any(|r: &Url| same_scope(r, &url, request.allow_subdomains))
                    {
                        roots.push(url.clone());
                    }
                    queue.push_back((url, DiscoveryKind::Seed, None));
                }
                Err((state, detail)) => {
                    failures.push(failure(seed, state, detail, DiscoveryKind::Seed, None))
                }
            }
        }

        if request.discover_sitemaps {
            let initial_roots = roots.clone();
            for root in initial_roots {
                if discovery_seen.len() >= self.config.max_discovery_documents {
                    break;
                }
                for sitemap in self.sitemap_candidates(&root, &mut robots) {
                    if discovery_seen.len() >= self.config.max_discovery_documents {
                        break;
                    }
                    if !same_scope(&root, &sitemap, request.allow_subdomains)
                        || !discovery_seen.insert(canonical_key(&sitemap))
                    {
                        continue;
                    }
                    match self.fetch_discovery(
                        &sitemap,
                        &roots,
                        request.allow_subdomains,
                        &mut robots,
                        &mut last_request,
                    ) {
                        Ok((final_url, body, hops)) => {
                            redirects_followed += hops;
                            for discovered in parse_sitemap_urls(&body, &final_url)
                                .into_iter()
                                .take(self.config.max_discovered_urls)
                            {
                                if roots
                                    .iter()
                                    .any(|r| same_scope(r, &discovered, request.allow_subdomains))
                                {
                                    sitemap_urls_discovered += 1;
                                    queue.push_back((
                                        discovered,
                                        DiscoveryKind::Sitemap,
                                        Some(final_url.to_string()),
                                    ));
                                }
                            }
                        }
                        Err(item) => failures.push(item),
                    }
                }
            }
        }

        while seen.len() < request.max_pages {
            let Some((url, discovery, from)) = queue.pop_front() else {
                break;
            };
            if !seen.insert(canonical_key(&url)) {
                continue;
            }
            match self.fetch_html(
                &url,
                discovery.clone(),
                from.clone(),
                &roots,
                request.allow_subdomains,
                &mut robots,
                &mut last_request,
            ) {
                Ok((mut item, links, feeds, hops)) => {
                    redirects_followed += hops;
                    item.score = rank(item.title.as_deref(), item.content.as_deref(), &query_terms);
                    item.excerpt = item
                        .content
                        .as_deref()
                        .and_then(|c| make_excerpt(c, &query_terms));
                    let parent = item.final_url.clone();
                    evidence.push(item);
                    for link in links.into_iter().take(self.config.max_links_per_page) {
                        if roots
                            .iter()
                            .any(|root| same_scope(root, &link, request.allow_subdomains))
                        {
                            queue.push_back((link, DiscoveryKind::Link, Some(parent.clone())));
                        }
                    }
                    if request.discover_feeds {
                        for feed in feeds.into_iter().take(4) {
                            if discovery_seen.len() >= self.config.max_discovery_documents {
                                break;
                            }
                            if !roots
                                .iter()
                                .any(|r| same_scope(r, &feed, request.allow_subdomains))
                                || !discovery_seen.insert(canonical_key(&feed))
                            {
                                continue;
                            }
                            match self.fetch_discovery(
                                &feed,
                                &roots,
                                request.allow_subdomains,
                                &mut robots,
                                &mut last_request,
                            ) {
                                Ok((final_url, body, feed_hops)) => {
                                    redirects_followed += feed_hops;
                                    for discovered in parse_feed_urls(&body, &final_url)
                                        .into_iter()
                                        .take(self.config.max_discovered_urls)
                                    {
                                        if roots.iter().any(|r| {
                                            same_scope(r, &discovered, request.allow_subdomains)
                                        }) {
                                            feed_urls_discovered += 1;
                                            queue.push_back((
                                                discovered,
                                                DiscoveryKind::Feed,
                                                Some(final_url.to_string()),
                                            ));
                                        }
                                    }
                                }
                                Err(item) => failures.push(item),
                            }
                        }
                    }
                }
                Err(item) => failures.push(item),
            }
        }

        let attempted = seen.len()
            + failures
                .iter()
                .filter(|e| e.state == FetchState::InvalidUrl)
                .count();
        let pages_denied = failures
            .iter()
            .filter(|e| e.state == FetchState::RobotsDenied)
            .count();
        let pages_fetched = evidence.len();
        let truncated = !queue.is_empty() && seen.len() >= request.max_pages;
        evidence.sort_by(|a, b| b.score.total_cmp(&a.score));
        evidence.truncate(request.max_results);
        SearchResponse {
            query: request.query, evidence, failures,
            coverage: Coverage {
                model: "bounded_seed_discovery".into(), seeds_received: seed_count,
                pages_attempted: attempted, pages_fetched, pages_denied, redirects_followed,
                sitemap_urls_discovered, feed_urls_discovered, truncated_by_budget: truncated,
                disclaimer: "REX-search searched supplied public seeds plus bounded, in-scope sitemap/feed/link discovery. It is not a complete index of the web.".into(),
            },
        }
    }

    fn sitemap_candidates(
        &self,
        root: &Url,
        robots: &mut HashMap<String, RobotsRules>,
    ) -> Vec<Url> {
        let rules = robots
            .entry(origin_key(root))
            .or_insert_with(|| self.load_robots(root));
        let mut out = rules.sitemaps.clone();
        // A robots failure is fail-closed for the origin; do not probe a
        // conventional sitemap when robots itself could not be read safely.
        if !rules.deny_all {
            if let Ok(url) = root.join("/sitemap.xml") {
                out.push(url);
            }
        }
        dedupe_urls(out)
    }

    fn load_robots(&self, page: &Url) -> RobotsRules {
        let Ok(robots_url) = page.join("/robots.txt") else {
            return RobotsRules::allow_all();
        };
        if self.check_destination(&robots_url).is_err() {
            return RobotsRules::deny_all();
        }
        match self
            .agent
            .get(robots_url.as_str())
            .header("Accept", "text/plain")
            .call()
        {
            Ok(response) if response.status().as_u16() == 200 => {
                let body = response
                    .into_body()
                    .with_config()
                    .limit(512 * 1024)
                    .read_to_string()
                    .unwrap_or_default();
                RobotsRules::parse_with_base(&body, &robots_url)
            }
            Ok(response) if response.status().as_u16() >= 500 => RobotsRules::deny_all(),
            Ok(_) => RobotsRules::allow_all(),
            Err(_) => RobotsRules::deny_all(),
        }
    }

    fn policy_for(
        &self,
        url: &Url,
        roots: &[Url],
        allow_subdomains: bool,
        robots: &mut HashMap<String, RobotsRules>,
    ) -> Result<(), (FetchState, String)> {
        self.check_local(url)
            .map_err(|e| (FetchState::UnsafeAddress, e))?;
        if !roots.iter().any(|r| same_scope(r, url, allow_subdomains)) {
            return Err((
                FetchState::OutOfScopeRedirect,
                "redirect leaves operator-approved crawl scope".into(),
            ));
        }
        let rules = robots
            .entry(origin_key(url))
            .or_insert_with(|| self.load_robots(url));
        if !rules.allowed(&url_path_query(url)) {
            return Err((
                FetchState::RobotsDenied,
                "robots.txt disallows this path".into(),
            ));
        }
        Ok(())
    }

    // Redirect walking takes the crawl context as separate borrow scopes;
    // the Evidence error type is the audit record, not a hot-path payload.
    #[allow(clippy::too_many_arguments, clippy::result_large_err)]
    fn request_following_redirects(
        &self,
        start: &Url,
        roots: &[Url],
        allow_subdomains: bool,
        accept: &str,
        robots: &mut HashMap<String, RobotsRules>,
        last: &mut HashMap<String, Instant>,
        discovery: DiscoveryKind,
        from: Option<String>,
    ) -> Result<(ureq::http::Response<ureq::Body>, Url, Vec<String>), Evidence> {
        let mut current = start.clone();
        let mut chain = vec![current.to_string()];
        for hop in 0..=self.config.max_redirects {
            if let Err((state, detail)) = self.policy_for(&current, roots, allow_subdomains, robots)
            {
                return Err(failure_with_chain(
                    start.as_str(),
                    current.as_str(),
                    chain,
                    state,
                    detail,
                    discovery,
                    from,
                ));
            }
            let origin = origin_key(&current);
            throttle(last, &origin, self.config.per_origin_delay);
            let response = self
                .agent
                .get(current.as_str())
                .header("Accept", accept)
                .call()
                .map_err(|e| {
                    failure_with_chain(
                        start.as_str(),
                        current.as_str(),
                        chain.clone(),
                        FetchState::NetworkError,
                        e.to_string(),
                        discovery.clone(),
                        from.clone(),
                    )
                })?;
            let status = response.status().as_u16();
            if (300..400).contains(&status) {
                if hop == self.config.max_redirects {
                    return Err(failure_with_chain(
                        start.as_str(),
                        current.as_str(),
                        chain,
                        FetchState::RedirectLimit,
                        format!("redirect limit {} exceeded", self.config.max_redirects),
                        discovery,
                        from,
                    )
                    .with_status(status));
                }
                let location = response
                    .headers()
                    .get("location")
                    .and_then(|h| h.to_str().ok())
                    .ok_or_else(|| {
                        failure_with_chain(
                            start.as_str(),
                            current.as_str(),
                            chain.clone(),
                            FetchState::RedirectMissingLocation,
                            "redirect response has no valid Location header".into(),
                            discovery.clone(),
                            from.clone(),
                        )
                        .with_status(status)
                    })?;
                let next = current.join(location).map_err(|e| {
                    failure_with_chain(
                        start.as_str(),
                        current.as_str(),
                        chain.clone(),
                        FetchState::InvalidUrl,
                        format!("invalid redirect location: {e}"),
                        discovery.clone(),
                        from.clone(),
                    )
                    .with_status(status)
                })?;
                chain.push(next.to_string());
                current = next;
                continue;
            }
            if status >= 400 {
                let state = if status == 429 {
                    FetchState::RateLimited
                } else {
                    FetchState::HttpError
                };
                return Err(failure_with_chain(
                    start.as_str(),
                    current.as_str(),
                    chain,
                    state,
                    format!("HTTP {status}"),
                    discovery,
                    from,
                )
                .with_status(status));
            }
            return Ok((response, current, chain));
        }
        unreachable!()
    }

    // Same crawl-context shape as request_following_redirects.
    #[allow(clippy::too_many_arguments, clippy::result_large_err)]
    fn fetch_discovery(
        &self,
        url: &Url,
        roots: &[Url],
        allow_subdomains: bool,
        robots: &mut HashMap<String, RobotsRules>,
        last: &mut HashMap<String, Instant>,
    ) -> Result<(Url, String, usize), Evidence> {
        let (response, final_url, chain) = self.request_following_redirects(
            url,
            roots,
            allow_subdomains,
            "application/xml,text/xml,application/rss+xml,application/atom+xml;q=0.9",
            robots,
            last,
            DiscoveryKind::Sitemap,
            None,
        )?;
        if response
            .headers()
            .get("content-length")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.parse::<usize>().ok())
            .is_some_and(|n| n > self.config.max_body_bytes)
        {
            return Err(failure_with_chain(
                url.as_str(),
                final_url.as_str(),
                chain,
                FetchState::TooLarge,
                "declared discovery document exceeds body limit".into(),
                DiscoveryKind::Sitemap,
                None,
            ));
        }
        let body = response
            .into_body()
            .with_config()
            .limit(self.config.max_body_bytes as u64)
            .read_to_string()
            .map_err(|e| {
                failure(
                    url.as_str(),
                    FetchState::NetworkError,
                    format!("discovery read failed: {e}"),
                    DiscoveryKind::Sitemap,
                    None,
                )
            })?;
        let hops = chain.len().saturating_sub(1);
        Ok((final_url, body, hops))
    }

    // Same crawl-context shape as request_following_redirects.
    #[allow(clippy::too_many_arguments, clippy::result_large_err)]
    fn fetch_html(
        &self,
        url: &Url,
        discovery: DiscoveryKind,
        from: Option<String>,
        roots: &[Url],
        allow_subdomains: bool,
        robots: &mut HashMap<String, RobotsRules>,
        last: &mut HashMap<String, Instant>,
    ) -> Result<(Evidence, Vec<Url>, Vec<Url>, usize), Evidence> {
        let now = now_ms();
        let (response, effective, chain) = self.request_following_redirects(
            url,
            roots,
            allow_subdomains,
            "text/html,application/xhtml+xml;q=0.9,text/plain;q=0.5",
            robots,
            last,
            discovery.clone(),
            from.clone(),
        )?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !(content_type.contains("text/html")
            || content_type.contains("application/xhtml+xml")
            || content_type.contains("text/plain"))
        {
            return Err(failure_with_chain(
                url.as_str(),
                effective.as_str(),
                chain,
                FetchState::UnsupportedContent,
                format!("unsupported content type: {content_type}"),
                discovery,
                from,
            )
            .with_status(status));
        }
        if response
            .headers()
            .get("content-length")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.parse::<usize>().ok())
            .is_some_and(|n| n > self.config.max_body_bytes)
        {
            return Err(failure_with_chain(
                url.as_str(),
                effective.as_str(),
                chain,
                FetchState::TooLarge,
                "declared response exceeds body limit".into(),
                discovery,
                from,
            )
            .with_status(status));
        }
        let body = response
            .into_body()
            .with_config()
            .limit(self.config.max_body_bytes as u64)
            .read_to_string()
            .map_err(|e| {
                failure(
                    url.as_str(),
                    FetchState::NetworkError,
                    format!("body read failed: {e}"),
                    discovery.clone(),
                    from.clone(),
                )
                .with_status(status)
            })?;
        let title = extract_title(&body);
        let mut content = html_to_text(&body);
        content.truncate(char_boundary(&content, self.config.max_content_chars));
        let links = extract_links(&body, &effective);
        let feeds = extract_feed_links(&body, &effective);
        let hops = chain.len().saturating_sub(1);
        Ok((
            Evidence {
                url: url.to_string(),
                final_url: effective.to_string(),
                redirect_chain: chain,
                title,
                excerpt: None,
                content: Some(content),
                retrieved_at_unix_ms: now,
                http_status: Some(status),
                content_type: Some(content_type),
                discovery,
                discovered_from: from,
                state: FetchState::Fetched,
                error: None,
                score: 0.0,
                robots_allowed: Some(true),
            },
            links,
            feeds,
            hops,
        ))
    }
}
impl Default for SearchEngine {
    fn default() -> Self {
        Self::new(SearchConfig::default())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexedDocument {
    pub id: String,
    pub url: String,
    pub title: Option<String>,
    pub content: String,
    pub indexed_at_unix_ms: u128,
    pub provenance: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexHit {
    pub document: IndexedDocument,
    pub score: f32,
    pub excerpt: Option<String>,
}
#[derive(Debug, Clone)]
pub struct LocalIndex {
    path: PathBuf,
}
impl LocalIndex {
    pub fn open(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
    pub fn upsert(&self, mut document: IndexedDocument) -> io::Result<()> {
        if document.id.trim().is_empty() || document.url.trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "index id and URL are required",
            ));
        }
        document.indexed_at_unix_ms = now_ms();
        let mut documents = self.load()?;
        if let Some(slot) = documents.iter_mut().find(|d| d.id == document.id) {
            *slot = document;
        } else {
            documents.push(document);
        }
        self.store(&documents)
    }
    pub fn remove(&self, id: &str) -> io::Result<bool> {
        let mut documents = self.load()?;
        let before = documents.len();
        documents.retain(|d| d.id != id);
        if before != documents.len() {
            self.store(&documents)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
    pub fn query(&self, query: &str, limit: usize) -> io::Result<Vec<IndexHit>> {
        let terms = terms(query);
        let mut hits: Vec<_> = self
            .load()?
            .into_iter()
            .filter_map(|d| {
                let score = rank(d.title.as_deref(), Some(&d.content), &terms);
                (score > 0.0 || terms.is_empty()).then(|| {
                    let excerpt = make_excerpt(&d.content, &terms);
                    IndexHit {
                        document: d,
                        score,
                        excerpt,
                    }
                })
            })
            .collect();
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        hits.truncate(limit.clamp(1, 50));
        Ok(hits)
    }
    fn load(&self) -> io::Result<Vec<IndexedDocument>> {
        if let Ok(meta) = fs::symlink_metadata(&self.path) {
            if meta.file_type().is_symlink() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "local index must not be a symlink",
                ));
            }
        }
        match fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(vec![]),
            Err(e) => Err(e),
        }
    }
    fn store(&self, documents: &[IndexedDocument]) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(documents).map_err(io::Error::other)?;
        if let Ok(meta) = fs::symlink_metadata(&self.path) {
            if meta.file_type().is_symlink() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "local index must not be a symlink",
                ));
            }
        }
        fs::write(&tmp, bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
        }
        fs::rename(tmp, &self.path)
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RobotsRule {
    allow: bool,
    pattern: String,
    specificity: usize,
}

#[derive(Debug, Clone)]
struct RobotsGroup {
    agents: Vec<String>,
    rules: Vec<RobotsRule>,
}

#[derive(Debug, Clone)]
struct RobotsRules {
    rules: Vec<RobotsRule>,
    sitemaps: Vec<Url>,
    deny_all: bool,
}
impl RobotsRules {
    fn allow_all() -> Self {
        Self {
            rules: vec![],
            sitemaps: vec![],
            deny_all: false,
        }
    }
    fn deny_all() -> Self {
        Self {
            rules: vec![RobotsRule {
                allow: false,
                pattern: "/".into(),
                specificity: 1,
            }],
            sitemaps: vec![],
            deny_all: true,
        }
    }

    #[cfg(test)]
    fn parse(text: &str) -> Self {
        let base = Url::parse("https://invalid.example/robots.txt").unwrap();
        Self::parse_with_base(text, &base)
    }

    fn parse_with_base(text: &str, base: &Url) -> Self {
        let mut groups: Vec<RobotsGroup> = Vec::new();
        let mut agents: Vec<String> = Vec::new();
        let mut rules: Vec<RobotsRule> = Vec::new();
        let mut saw_rule = false;
        let mut sitemaps = Vec::new();

        let flush = |groups: &mut Vec<RobotsGroup>,
                     agents: &mut Vec<String>,
                     rules: &mut Vec<RobotsRule>| {
            if !agents.is_empty() {
                groups.push(RobotsGroup {
                    agents: std::mem::take(agents),
                    rules: std::mem::take(rules),
                });
            }
        };

        for raw in text.lines() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            let name = name.trim();
            let value = value.trim();
            if name.eq_ignore_ascii_case("sitemap") {
                if let Ok(url) = base.join(value) {
                    if matches!(url.scheme(), "http" | "https") {
                        sitemaps.push(url);
                    }
                }
            } else if name.eq_ignore_ascii_case("user-agent") {
                if saw_rule {
                    flush(&mut groups, &mut agents, &mut rules);
                    saw_rule = false;
                }
                agents.push(value.to_ascii_lowercase());
            } else if name.eq_ignore_ascii_case("allow") || name.eq_ignore_ascii_case("disallow") {
                if agents.is_empty() {
                    continue;
                }
                saw_rule = true;
                if value.is_empty() {
                    continue;
                }
                rules.push(RobotsRule {
                    allow: name.eq_ignore_ascii_case("allow"),
                    specificity: value.chars().filter(|c| *c != '*' && *c != '$').count(),
                    pattern: value.to_string(),
                });
            }
        }
        flush(&mut groups, &mut agents, &mut rules);

        let token = USER_AGENT
            .split('/')
            .next()
            .unwrap_or(USER_AGENT)
            .to_ascii_lowercase();
        let best = groups
            .iter()
            .filter_map(|g| {
                g.agents
                    .iter()
                    .filter(|a| *a != "*" && token.starts_with(a.as_str()))
                    .map(String::len)
                    .max()
            })
            .max();
        let selected = groups.into_iter().filter(|g| match best {
            Some(n) => g
                .agents
                .iter()
                .any(|a| a != "*" && a.len() == n && token.starts_with(a.as_str())),
            None => g.agents.iter().any(|a| a == "*"),
        });
        Self {
            rules: selected.flat_map(|g| g.rules).collect(),
            sitemaps: dedupe_urls(sitemaps),
            deny_all: false,
        }
    }

    fn allowed(&self, path: &str) -> bool {
        self.rules
            .iter()
            .filter(|rule| robots_pattern_matches(&rule.pattern, path))
            .max_by(|a, b| {
                a.specificity
                    .cmp(&b.specificity)
                    .then_with(|| a.allow.cmp(&b.allow))
            })
            .map(|rule| rule.allow)
            .unwrap_or(true)
    }
}

fn robots_pattern_matches(pattern: &str, path: &str) -> bool {
    let anchored = pattern.ends_with('$');
    let pattern = pattern.strip_suffix('$').unwrap_or(pattern);
    let mut cursor = 0usize;
    for (index, part) in pattern.split('*').enumerate() {
        if part.is_empty() {
            continue;
        }
        if index == 0 {
            if !path[cursor..].starts_with(part) {
                return false;
            }
            cursor += part.len();
        } else if let Some(offset) = path[cursor..].find(part) {
            cursor += offset + part.len();
        } else {
            return false;
        }
    }
    !anchored || cursor == path.len()
}
#[cfg(test)]
fn normalize_public_url(raw: &str) -> Result<Url, (FetchState, String)> {
    let url = Url::parse(raw).map_err(|e| (FetchState::InvalidUrl, e.to_string()))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err((
            FetchState::InvalidUrl,
            "only http and https are supported".into(),
        ));
    }
    reject_private_destination(&url).map_err(|e| (FetchState::UnsafeAddress, e))?;
    Ok(url)
}

#[derive(Debug, Default)]
struct PublicResolver {
    inner: DefaultResolver,
    #[cfg(test)]
    allow_loopback: bool,
}

#[cfg(test)]
impl PublicResolver {
    fn allowing_loopback() -> Self {
        Self {
            inner: DefaultResolver::default(),
            allow_loopback: true,
        }
    }
}

impl Resolver for PublicResolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &ureq::config::Config,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ResolvedSocketAddrs, UreqError> {
        let resolved = self.inner.resolve(uri, config, timeout)?;
        // Return only the exact public addresses validated here. The connector
        // consumes this list directly, so there is no second DNS lookup between
        // policy validation and connect. TLS still receives the original URI
        // hostname and verifies that name, not the numeric address.
        let mut public = self.empty();
        for addr in resolved
            .iter()
            .copied()
            .filter(|addr| self.permits(addr.ip()))
        {
            public.push(addr);
        }
        if public.is_empty() {
            Err(UreqError::HostNotFound)
        } else {
            Ok(public)
        }
    }
}

impl PublicResolver {
    #[cfg(test)]
    fn permits(&self, ip: IpAddr) -> bool {
        is_public_ip(ip) || (self.allow_loopback && ip.is_loopback())
    }

    #[cfg(not(test))]
    fn permits(&self, ip: IpAddr) -> bool {
        is_public_ip(ip)
    }
}

#[cfg(test)]
fn is_loopback_host(url: &Url) -> bool {
    url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<IpAddr>()
                .map(|ip| ip.is_loopback())
                .unwrap_or(false)
    })
}

fn reject_local_hostname(url: &Url) -> Result<(), String> {
    let host = url
        .host_str()
        .ok_or_else(|| "URL has no host".to_string())?;
    if host.eq_ignore_ascii_case("localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
    {
        return Err("local hostnames are blocked".into());
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        if !is_public_ip(ip) {
            return Err(format!("non-public destination {ip} is blocked"));
        }
    }
    Ok(())
}
fn reject_private_destination(url: &Url) -> Result<(), String> {
    reject_local_hostname(url)?;
    let host = url
        .host_str()
        .ok_or_else(|| "URL has no host".to_string())?;
    let port = url.port_or_known_default().unwrap_or(80);
    let addrs = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("host resolution failed: {e}"))?;
    let mut resolved = false;
    for addr in addrs {
        resolved = true;
        if !is_public_ip(addr.ip()) {
            return Err(format!("non-public destination {} is blocked", addr.ip()));
        }
    }
    if !resolved {
        return Err("host resolved to no addresses".into());
    }
    Ok(())
}
fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => is_public_ipv4(v),
        IpAddr::V6(v) => {
            // An IPv4-mapped IPv6 address must satisfy the IPv4 rules:
            // ::ffff:127.0.0.1 is loopback, not a public destination.
            if let Some(mapped) = v.to_ipv4_mapped() {
                return is_public_ipv4(mapped);
            }
            let segments = v.segments();
            !(v.is_loopback()
                || v.is_unspecified()
                || v.is_unique_local()
                || v.is_unicast_link_local()
                || v.is_multicast()
                // 2001:db8::/32 documentation range
                || (segments[0] == 0x2001 && segments[1] == 0x0db8))
        }
    }
}
fn is_public_ipv4(v: std::net::Ipv4Addr) -> bool {
    let o = v.octets();
    !(v.is_private()
        || v.is_loopback()
        || v.is_link_local()
        || v.is_broadcast()
        || v.is_unspecified()
        || v.is_multicast()
        || o[0] == 0
        // 100.64.0.0/10 shared address space (CGNAT)
        || (o[0] == 100 && (o[1] & 0xC0) == 64)
        // 198.18.0.0/15 benchmarking
        || (o[0] == 198 && (o[1] & 0xFE) == 18)
        // 240.0.0.0/4 reserved
        || o[0] >= 240
        // documentation ranges 192.0.2.0/24, 198.51.100.0/24, 203.0.113.0/24
        || (o[0] == 192 && o[1] == 0 && o[2] == 2)
        || (o[0] == 198 && o[1] == 51 && o[2] == 100)
        || (o[0] == 203 && o[1] == 0 && o[2] == 113))
}
fn same_scope(root: &Url, candidate: &Url, subdomains: bool) -> bool {
    let (Some(a), Some(b)) = (root.host_str(), candidate.host_str()) else {
        return false;
    };
    a.eq_ignore_ascii_case(b)
        || (subdomains
            && b.to_ascii_lowercase()
                .ends_with(&format!(".{}", a.to_ascii_lowercase())))
}
fn origin_key(url: &Url) -> String {
    format!(
        "{}://{}:{}",
        url.scheme(),
        url.host_str().unwrap_or(""),
        url.port_or_known_default().unwrap_or(0)
    )
}
fn canonical_key(url: &Url) -> String {
    let mut u = url.clone();
    u.set_fragment(None);
    u.to_string()
}
fn throttle(last: &mut HashMap<String, Instant>, origin: &str, delay: Duration) {
    if let Some(previous) = last.get(origin) {
        let elapsed = previous.elapsed();
        if elapsed < delay {
            std::thread::sleep(delay - elapsed);
        }
    }
    last.insert(origin.to_string(), Instant::now());
}
fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn terms(query: &str) -> Vec<String> {
    query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| s.len() > 1)
        .map(str::to_ascii_lowercase)
        .collect()
}
fn rank(title: Option<&str>, body: Option<&str>, query: &[String]) -> f32 {
    if query.is_empty() {
        return 0.0;
    }
    let title = title.unwrap_or("").to_ascii_lowercase();
    let body = body.unwrap_or("").to_ascii_lowercase();
    query
        .iter()
        .map(|t| {
            (if title.contains(t) { 4.0 } else { 0.0 }) + body.matches(t).take(8).count() as f32
        })
        .sum::<f32>()
        / query.len() as f32
}
fn extract_title(html: &str) -> Option<String> {
    extract_between_ci(html, "<title", "</title>")
        .and_then(|s| {
            s.split_once('>')
                .map(|(_, v)| decode_entities(v).trim().to_string())
        })
        .filter(|s| !s.is_empty())
}
fn extract_between_ci<'a>(s: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let lower = s.to_ascii_lowercase();
    let a = lower.find(start)?;
    let b = lower[a..].find(end)? + a;
    Some(&s[a..b])
}
fn extract_links(html: &str, base: &Url) -> Vec<Url> {
    let lower = html.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(pos) = lower[at..].find("href=") {
        let i = at + pos + 5;
        let bytes = html.as_bytes();
        if i >= bytes.len() {
            break;
        };
        let quote = bytes[i] as char;
        let (start, end) = if quote == '\'' || quote == '"' {
            let start = i + 1;
            let Some(n) = html[start..].find(quote) else {
                break;
            };
            (start, start + n)
        } else {
            let end = html[i..]
                .find(|c: char| c.is_whitespace() || c == '>')
                .map(|n| i + n)
                .unwrap_or(html.len());
            (i, end)
        };
        if let Ok(mut u) = base.join(html[start..end].trim()) {
            u.set_fragment(None);
            if matches!(u.scheme(), "http" | "https") {
                out.push(u);
            }
        }
        at = end.saturating_add(1);
        if out.len() >= 128 {
            break;
        }
    }
    out
}
fn url_path_query(url: &Url) -> String {
    let mut value = url.path().to_string();
    if let Some(query) = url.query() {
        value.push('?');
        value.push_str(query);
    }
    value
}
fn dedupe_urls(urls: Vec<Url>) -> Vec<Url> {
    let mut seen = HashSet::new();
    urls.into_iter()
        .filter(|u| seen.insert(canonical_key(u)))
        .collect()
}
fn extract_xml_values(text: &str, tag: &str) -> Vec<String> {
    let lower = text.to_ascii_lowercase();
    let open = format!("<{}", tag);
    let close = format!("</{}>", tag);
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(p) = lower[at..].find(&open) {
        let a = at + p;
        let Some(gt) = lower[a..].find('>') else {
            break;
        };
        let start = a + gt + 1;
        let Some(end_rel) = lower[start..].find(&close) else {
            break;
        };
        let end = start + end_rel;
        let value = decode_entities(text[start..end].trim());
        if !value.is_empty() {
            out.push(value)
        }
        at = end + close.len();
    }
    out
}
fn parse_sitemap_urls(xml: &str, base: &Url) -> Vec<Url> {
    dedupe_urls(
        extract_xml_values(xml, "loc")
            .into_iter()
            .filter_map(|v| base.join(&v).ok())
            .filter(|u| matches!(u.scheme(), "http" | "https"))
            .collect(),
    )
}
fn parse_feed_urls(xml: &str, base: &Url) -> Vec<Url> {
    let mut out: Vec<Url> = extract_xml_values(xml, "link")
        .into_iter()
        .filter_map(|v| base.join(&v).ok())
        .collect();
    let lower = xml.to_ascii_lowercase();
    let mut at = 0;
    while let Some(p) = lower[at..].find("href=") {
        let i = at + p + 5;
        let bytes = xml.as_bytes();
        if i >= bytes.len() {
            break;
        };
        let q = bytes[i] as char;
        let (a, b) = if q == '\'' || q == '"' {
            let a = i + 1;
            let Some(n) = xml[a..].find(q) else { break };
            (a, a + n)
        } else {
            let b = xml[i..]
                .find(|c: char| c.is_whitespace() || c == '>')
                .map(|n| i + n)
                .unwrap_or(xml.len());
            (i, b)
        };
        if let Ok(u) = base.join(xml[a..b].trim()) {
            out.push(u)
        }
        at = b.saturating_add(1);
    }
    dedupe_urls(
        out.into_iter()
            .filter(|u| matches!(u.scheme(), "http" | "https"))
            .collect(),
    )
}
fn extract_feed_links(html: &str, base: &Url) -> Vec<Url> {
    let lower = html.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(p) = lower[at..].find("<link") {
        let a = at + p;
        let end = lower[a..].find('>').map(|n| a + n).unwrap_or(html.len());
        let tag = &html[a..end];
        let tl = tag.to_ascii_lowercase();
        if (tl.contains("application/rss+xml") || tl.contains("application/atom+xml"))
            && tl.contains("rel=")
        {
            for u in extract_links(tag, base) {
                out.push(u)
            }
        }
        at = end.saturating_add(1);
    }
    dedupe_urls(out)
}
fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len().min(32_000));
    let mut tag = false;
    let mut skip: Option<&str> = None;
    let lower = html.to_ascii_lowercase();
    let bytes = html.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if skip.is_some() {
            let needle = format!("</{}", skip.unwrap());
            if let Some(n) = lower[i..].find(&needle) {
                i += n;
                skip = None;
                continue;
            } else {
                break;
            }
        }
        if bytes[i] == b'<' {
            let tail = &lower[i..];
            if tail.starts_with("<script") {
                skip = Some("script")
            } else if tail.starts_with("<style") {
                skip = Some("style")
            } else if tail.starts_with("<noscript") {
                skip = Some("noscript")
            }
            tag = true;
            if !out.ends_with(' ') {
                out.push(' ')
            }
        } else if bytes[i] == b'>' {
            tag = false
        } else if !tag {
            out.push(bytes[i] as char)
        }
        i += 1
    }
    decode_entities(&out)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}
fn floor_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1
    }
    i
}
fn char_boundary(s: &str, max: usize) -> usize {
    if s.len() <= max {
        return s.len();
    }
    let mut i = max;
    while !s.is_char_boundary(i) {
        i -= 1
    }
    i
}
fn make_excerpt(content: &str, query: &[String]) -> Option<String> {
    if content.is_empty() {
        return None;
    }
    let lower = content.to_ascii_lowercase();
    let pos = query
        .iter()
        .filter_map(|t| lower.find(t))
        .min()
        .unwrap_or(0);
    let start = pos.saturating_sub(180);
    let start = floor_boundary(content, start);
    let end = floor_boundary(content, (start + 520).min(content.len()));
    Some(content[start..end].to_string())
}

fn failure(
    url: &str,
    state: FetchState,
    detail: String,
    discovery: DiscoveryKind,
    from: Option<String>,
) -> Evidence {
    Evidence {
        url: url.into(),
        final_url: url.into(),
        redirect_chain: vec![url.into()],
        title: None,
        excerpt: None,
        content: None,
        retrieved_at_unix_ms: now_ms(),
        http_status: None,
        content_type: None,
        discovery,
        discovered_from: from,
        state,
        error: Some(detail),
        score: 0.0,
        robots_allowed: None,
    }
}
fn failure_with_chain(
    url: &str,
    final_url: &str,
    redirect_chain: Vec<String>,
    state: FetchState,
    detail: String,
    discovery: DiscoveryKind,
    from: Option<String>,
) -> Evidence {
    let mut item = failure(url, state, detail, discovery, from);
    item.final_url = final_url.into();
    item.redirect_chain = redirect_chain;
    item
}
impl Evidence {
    fn with_status(mut self, status: u16) -> Self {
        self.http_status = Some(status);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn robots_longest_match_wins() {
        let r = RobotsRules::parse("User-agent: *\nDisallow: /private\nAllow: /private/public\n");
        assert!(!r.allowed("/private/a"));
        assert!(r.allowed("/private/public/a"));
    }
    #[test]
    fn named_agent_rules_apply() {
        let r = RobotsRules::parse("User-agent: REX-search\nDisallow: /no\n");
        assert!(!r.allowed("/no/a"));
    }
    #[test]
    fn robots_allow_wins_equal_specificity() {
        let r = RobotsRules::parse("User-agent: *\nDisallow: /same\nAllow: /same\n");
        assert!(r.allowed("/same"));
    }
    #[test]
    fn robots_wildcard_and_anchor() {
        let r = RobotsRules::parse("User-agent: *\nDisallow: /*.pdf$\n");
        assert!(!r.allowed("/docs/a.pdf"));
        assert!(r.allowed("/docs/a.pdf?download=1"));
    }
    #[test]
    fn robots_prefers_specific_agent_and_merges_matching_groups() {
        let r=RobotsRules::parse("User-agent: *\nDisallow: /global\n\nUser-agent: REX-search\nDisallow: /one\n\nUser-agent: rex-search\nAllow: /one/public\n");
        assert!(r.allowed("/global"));
        assert!(!r.allowed("/one/x"));
        assert!(r.allowed("/one/public"));
    }
    #[test]
    fn robots_groups_support_consecutive_agents_and_case_insensitive_fields() {
        let r = RobotsRules::parse(
            "USER-AGENT: other\nUser-Agent: REX-search\nDISALLOW: /secret # comment\n",
        );
        assert!(!r.allowed("/secret/x"));
    }
    #[test]
    fn private_targets_are_rejected() {
        assert!(normalize_public_url("http://127.0.0.1/x").is_err());
        assert!(normalize_public_url("file:///etc/passwd").is_err());
    }
    #[test]
    fn scope_is_exact_by_default() {
        let a = Url::parse("https://example.com/a").unwrap();
        assert!(same_scope(
            &a,
            &Url::parse("https://example.com/b").unwrap(),
            false
        ));
        assert!(!same_scope(
            &a,
            &Url::parse("https://evil-example.com/b").unwrap(),
            true
        ));
        assert!(same_scope(
            &a,
            &Url::parse("https://docs.example.com/b").unwrap(),
            true
        ));
    }
    #[test]
    fn extraction_removes_scripts() {
        let h="<html><head><title>A &amp; B</title><style>x</style></head><body>Hello <b>world</b><script>secret</script></body></html>";
        assert_eq!(extract_title(h).as_deref(), Some("A & B"));
        let t = html_to_text(h);
        assert!(t.contains("Hello world"));
        assert!(!t.contains("secret"));
    }
    #[test]
    fn ranking_favors_title() {
        let q = terms("rust agents");
        assert!(
            rank(Some("Rust agents"), Some("x"), &q) > rank(Some("x"), Some("rust agents"), &q)
        );
    }
    #[test]
    fn robots_sitemap_directives_are_collected() {
        let base = Url::parse("https://example.com/robots.txt").unwrap();
        let r = RobotsRules::parse_with_base("User-agent: *\nAllow: /\nSitemap: /map.xml\n", &base);
        assert_eq!(r.sitemaps[0].as_str(), "https://example.com/map.xml");
    }
    #[test]
    fn sitemap_and_feed_discovery_are_bounded_to_parsed_urls() {
        let base = Url::parse("https://example.com/feed.xml").unwrap();
        let map = parse_sitemap_urls(
            "<urlset><url><loc>https://example.com/a</loc></url><url><loc>/b</loc></url></urlset>",
            &base,
        );
        assert_eq!(map.len(), 2);
        let feed = parse_feed_urls("<feed><entry><link href=\"/post\"/></entry></feed>", &base);
        assert_eq!(feed[0].as_str(), "https://example.com/post");
    }
    #[test]
    fn robots_matching_includes_query() {
        let r = RobotsRules::parse("User-agent: *\nDisallow: /*?secret$\n");
        let u = Url::parse("https://example.com/path?secret").unwrap();
        assert!(!r.allowed(&url_path_query(&u)));
    }
    #[test]
    fn local_index_is_operator_controlled_and_deletable() {
        let path = std::env::temp_dir().join(format!("rex-index-test-{}.json", std::process::id()));
        let index = LocalIndex::open(&path);
        index
            .upsert(IndexedDocument {
                id: "one".into(),
                url: "https://example.com/one".into(),
                title: Some("Rust agents".into()),
                content: "bounded local evidence".into(),
                indexed_at_unix_ms: 0,
                provenance: "operator".into(),
            })
            .unwrap();
        let hits = index.query("rust", 5).unwrap();
        assert_eq!(hits[0].document.id, "one");
        assert!(index.remove("one").unwrap());
        assert!(index.query("rust", 5).unwrap().is_empty());
        let _ = std::fs::remove_file(path);
    }
}

/// Adversarial local HTTP fixtures: scripted loopback servers that exercise
/// every redirect and destination-safety branch end to end. These run only in
/// `cfg(test)` through `SearchEngine::new_for_tests`, which permits literal
/// loopback destinations while leaving every other production protection
/// (scope, per-origin robots, redirect limits, private-IP rejection, DNS
/// pinning) fully active.
#[cfg(test)]
mod http_fixtures {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[derive(Clone)]
    struct Route {
        status: u16,
        reason: &'static str,
        headers: Vec<(String, String)>,
        body: String,
    }
    fn redirect_to(location: &str) -> Route {
        Route {
            status: 302,
            reason: "Found",
            headers: vec![("Location".into(), location.into())],
            body: String::new(),
        }
    }
    fn redirect_without_location() -> Route {
        Route {
            status: 302,
            reason: "Found",
            headers: vec![],
            body: String::new(),
        }
    }
    fn html_page(body: &str) -> Route {
        Route {
            status: 200,
            reason: "OK",
            headers: vec![("Content-Type".into(), "text/html".into())],
            body: body.into(),
        }
    }
    fn plain(status: u16, reason: &'static str, body: &str) -> Route {
        Route {
            status,
            reason,
            headers: vec![("Content-Type".into(), "text/plain".into())],
            body: body.into(),
        }
    }

    struct FixtureServer {
        base: String,
        hits: Arc<AtomicUsize>,
    }
    impl FixtureServer {
        fn url(&self, path: &str) -> String {
            format!("{}{}", self.base, path)
        }
    }

    fn serve(routes: Vec<(&'static str, Route)>) -> FixtureServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let thread_hits = hits.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
                let mut buf: Vec<u8> = Vec::new();
                let mut chunk = [0u8; 2048];
                let mut target = String::new();
                loop {
                    match stream.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&chunk[..n]);
                            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                                let head = String::from_utf8_lossy(&buf[..pos]);
                                target = head
                                    .lines()
                                    .next()
                                    .unwrap_or("")
                                    .split_whitespace()
                                    .nth(1)
                                    .unwrap_or("")
                                    .to_string();
                                break;
                            }
                            if buf.len() > 64 * 1024 {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                thread_hits.fetch_add(1, Ordering::SeqCst);
                let response = match routes.iter().find(|(path, _)| *path == target) {
                    Some((_, route)) => {
                        let mut out = format!("HTTP/1.1 {} {}\r\n", route.status, route.reason);
                        for (name, value) in &route.headers {
                            out.push_str(&format!("{name}: {value}\r\n"));
                        }
                        out.push_str(&format!(
                            "Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                            route.body.len(),
                            route.body
                        ));
                        out
                    }
                    None => {
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_string()
                    }
                };
                stream.write_all(response.as_bytes()).ok();
                stream.flush().ok();
            }
        });
        FixtureServer {
            base: format!("http://127.0.0.1:{port}"),
            hits,
        }
    }

    fn fixture_engine(max_redirects: usize) -> SearchEngine {
        SearchEngine::new_for_tests(SearchConfig {
            timeout: Duration::from_secs(5),
            per_origin_delay: Duration::from_millis(0),
            max_redirects,
            ..SearchConfig::default()
        })
    }
    fn seed_request(seed: &str) -> SearchRequest {
        SearchRequest {
            query: "evidence marker".into(),
            seeds: vec![seed.into()],
            max_pages: 8,
            max_results: 8,
            allow_subdomains: false,
            discover_sitemaps: false,
            discover_feeds: false,
        }
    }

    #[test]
    fn multi_hop_redirect_chain_is_followed_and_recorded() {
        let server = serve(vec![
            ("/start", redirect_to("/hop-two")),
            ("/hop-two", redirect_to("/hop-three")),
            (
                "/hop-three",
                redirect_to("/final"),
            ),
            (
                "/final",
                html_page("<html><head><title>Final page</title></head><body>evidence marker content</body></html>"),
            ),
        ]);
        let response = fixture_engine(5).search(seed_request(&server.url("/start")));
        assert_eq!(
            response.evidence.len(),
            1,
            "failures: {:?}",
            response.failures
        );
        let item = &response.evidence[0];
        assert_eq!(item.state, FetchState::Fetched);
        assert_eq!(item.final_url, server.url("/final"));
        assert_eq!(
            item.redirect_chain,
            vec![
                server.url("/start"),
                server.url("/hop-two"),
                server.url("/hop-three"),
                server.url("/final")
            ]
        );
        assert!(item
            .content
            .as_deref()
            .unwrap_or("")
            .contains("evidence marker"));
        assert_eq!(response.coverage.redirects_followed, 3);
    }

    #[test]
    fn redirect_to_other_origin_applies_that_origins_robots() {
        let allowed_origin = serve(vec![
            (
                "/robots.txt",
                plain(200, "OK", "User-agent: *\nDisallow: /private\n"),
            ),
            (
                "/public",
                html_page("<html><body>evidence marker public</body></html>"),
            ),
        ]);
        let source = serve(vec![
            ("/open", redirect_to(&allowed_origin.url("/public"))),
            ("/closed", redirect_to(&allowed_origin.url("/private"))),
        ]);
        let engine = fixture_engine(5);

        // The source origin allows everything, yet the destination origin's
        // robots must win: /private is denied, /public is fetched.
        let denied = engine.search(seed_request(&source.url("/closed")));
        assert_eq!(denied.evidence.len(), 0);
        assert_eq!(denied.failures.len(), 1);
        assert_eq!(denied.failures[0].state, FetchState::RobotsDenied);
        assert_eq!(denied.failures[0].final_url, allowed_origin.url("/private"));
        assert_eq!(
            denied.failures[0].redirect_chain,
            vec![source.url("/closed"), allowed_origin.url("/private")]
        );
        // Only the destination's robots.txt was read; the denied page itself
        // was never requested.
        assert_eq!(allowed_origin.hits.load(Ordering::SeqCst), 1);

        let fetched = engine.search(seed_request(&source.url("/open")));
        assert_eq!(
            fetched.evidence.len(),
            1,
            "failures: {:?}",
            fetched.failures
        );
        assert_eq!(fetched.evidence[0].final_url, allowed_origin.url("/public"));
        // Second search reloads robots.txt, then fetches the page; /private
        // was still never requested.
        assert_eq!(allowed_origin.hits.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn cross_host_redirect_out_of_scope_is_refused_before_fetch() {
        let server = serve(vec![("/start", redirect_to("http://example.com/escape"))]);
        let response = fixture_engine(5).search(seed_request(&server.url("/start")));
        assert_eq!(response.evidence.len(), 0);
        assert_eq!(response.failures.len(), 1);
        assert_eq!(response.failures[0].state, FetchState::OutOfScopeRedirect);
        assert_eq!(response.failures[0].final_url, "http://example.com/escape");
    }

    #[test]
    fn redirect_without_location_is_reported() {
        let server = serve(vec![("/start", redirect_without_location())]);
        let response = fixture_engine(5).search(seed_request(&server.url("/start")));
        assert_eq!(response.evidence.len(), 0);
        assert_eq!(response.failures.len(), 1);
        assert_eq!(
            response.failures[0].state,
            FetchState::RedirectMissingLocation
        );
        assert_eq!(response.failures[0].http_status, Some(302));
    }

    #[test]
    fn malformed_redirect_location_is_reported() {
        let server = serve(vec![("/start", redirect_to("http://[::1"))]);
        let response = fixture_engine(5).search(seed_request(&server.url("/start")));
        assert_eq!(response.evidence.len(), 0);
        assert_eq!(response.failures.len(), 1);
        assert_eq!(response.failures[0].state, FetchState::InvalidUrl);
        assert_eq!(response.failures[0].http_status, Some(302));
    }

    #[test]
    fn redirect_to_a_different_loopback_host_is_stopped_by_scope() {
        // Fixture mode permits loopback destinations, so a redirect to a
        // *different* loopback host is stopped by the scope rule rather than
        // the address rule. Production mode rejects [::1] outright.
        let server = serve(vec![("/v6loop", redirect_to("http://[::1]:9/x"))]);
        let response = fixture_engine(5).search(seed_request(&server.url("/v6loop")));
        assert_eq!(response.evidence.len(), 0);
        assert_eq!(response.failures.len(), 1);
        assert_eq!(response.failures[0].state, FetchState::OutOfScopeRedirect);
    }

    #[test]
    fn redirect_loop_stops_at_the_hop_limit() {
        let server = serve(vec![
            ("/loop-a", redirect_to("/loop-b")),
            ("/loop-b", redirect_to("/loop-a")),
        ]);
        let engine = fixture_engine(3);
        let response = engine.search(seed_request(&server.url("/loop-a")));
        assert_eq!(response.evidence.len(), 0);
        assert_eq!(response.failures.len(), 1);
        let failure = &response.failures[0];
        assert_eq!(failure.state, FetchState::RedirectLimit);
        assert_eq!(failure.http_status, Some(302));
        assert_eq!(failure.redirect_chain.len(), 4); // start plus 3 followed hops
                                                     // robots fetch plus 4 page requests, no unbounded spinning.
        assert_eq!(server.hits.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn redirect_to_private_or_loopback_destination_is_rejected() {
        let server = serve(vec![
            (
                "/metadata",
                redirect_to("http://169.254.169.254/latest/meta-data"),
            ),
            ("/internal-net", redirect_to("http://10.9.8.7/x")),
            ("/cg-nat", redirect_to("http://100.64.0.1/x")),
        ]);
        let engine = fixture_engine(5);
        for path in ["/metadata", "/internal-net", "/cg-nat"] {
            let response = engine.search(seed_request(&server.url(path)));
            assert_eq!(response.evidence.len(), 0, "{path}");
            assert_eq!(response.failures.len(), 1, "{path}");
            assert_eq!(
                response.failures[0].state,
                FetchState::UnsafeAddress,
                "{path}: {:?}",
                response.failures[0].error
            );
        }
    }

    #[test]
    fn robots_denied_seed_is_not_fetched() {
        let server = serve(vec![
            (
                "/robots.txt",
                plain(200, "OK", "User-agent: *\nDisallow: /blocked\n"),
            ),
            (
                "/blocked",
                html_page("<html><body>should never be read</body></html>"),
            ),
        ]);
        let response = fixture_engine(5).search(seed_request(&server.url("/blocked")));
        assert_eq!(response.evidence.len(), 0);
        assert_eq!(response.failures.len(), 1);
        assert_eq!(response.failures[0].state, FetchState::RobotsDenied);
        // Only the robots fetch happened; the denied page was never requested.
        assert_eq!(server.hits.load(Ordering::SeqCst), 1);
        assert_eq!(response.coverage.pages_denied, 1);
    }

    #[test]
    fn production_engine_still_refuses_loopback_seeds() {
        // The production constructor has no loopback carve-out: even with a
        // live loopback server answering, seeds must fail closed.
        let server = serve(vec![(
            "/",
            html_page("<html><body>evidence marker</body></html>"),
        )]);
        let engine = SearchEngine::new(SearchConfig {
            timeout: Duration::from_secs(2),
            per_origin_delay: Duration::from_millis(0),
            ..SearchConfig::default()
        });
        for seed in [
            server.url("/"),
            format!(
                "http://localhost:{}/",
                server.base.rsplit(':').next().unwrap()
            ),
        ] {
            let response = engine.search(seed_request(&seed));
            assert_eq!(response.evidence.len(), 0, "{seed}");
            assert_eq!(response.failures.len(), 1, "{seed}");
            assert_eq!(
                response.failures[0].state,
                FetchState::UnsafeAddress,
                "{seed}"
            );
        }
        assert_eq!(server.hits.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn dns_safety_rules_cover_special_use_ranges() {
        // Literal-IP checks need no network: these are the DNS outcomes the
        // resolver and destination checks must reject after any lookup.
        let blocked_v4 = [
            "10.0.0.9",
            "172.16.0.9",
            "192.168.0.9",
            "127.0.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "100.127.255.254",
            "0.0.0.0",
            "198.18.0.1",
            "198.19.255.1",
            "192.0.2.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
        ];
        for ip in blocked_v4 {
            assert!(
                !is_public_ip(ip.parse().unwrap()),
                "{ip} must not be treated as public"
            );
        }
        let blocked_v6 = ["::1", "::", "fc00::1", "fe80::1", "ff02::1", "2001:db8::1"];
        for ip in blocked_v6 {
            assert!(
                !is_public_ip(ip.parse().unwrap()),
                "{ip} must not be treated as public"
            );
        }
        // IPv4-mapped IPv6 inherits the IPv4 verdict in both directions.
        assert!(!is_public_ip("::ffff:127.0.0.1".parse().unwrap()));
        assert!(!is_public_ip("::ffff:169.254.169.254".parse().unwrap()));
        assert!(is_public_ip("::ffff:8.8.8.8".parse().unwrap()));
        assert!(is_public_ip("8.8.8.8".parse().unwrap()));
        assert!(is_public_ip("2606:4700:4700::1111".parse().unwrap()));

        // localhost resolves through the system resolver (hosts file), so the
        // full DNS path can be checked offline.
        let localhost = Url::parse("http://localhost/").unwrap();
        assert!(reject_private_destination(&localhost).is_err());
        let v6_loopback = Url::parse("http://[::1]/").unwrap();
        assert!(reject_private_destination(&v6_loopback).is_err());
        let unspecified = Url::parse("http://0.0.0.0/").unwrap();
        assert!(reject_private_destination(&unspecified).is_err());
    }
}
