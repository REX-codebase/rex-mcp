//! REX-search: bounded live-web discovery and evidence retrieval for agents.
//!
//! This is deliberately not a browser and not a web-scale search index. An
//! agent supplies public seed URLs; REX-search checks robots.txt, fetches live
//! HTML, follows a small number of same-origin links, extracts readable text,
//! ranks it against the query, and returns evidence with explicit provenance.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, ToSocketAddrs};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use url::Url;

pub const USER_AGENT: &str = "REX-search/0.1 (+https://github.com/REX-codebase/rex-harness)";

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
}
fn default_max_pages() -> usize { 24 }
fn default_max_results() -> usize { 8 }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryKind { Seed, Link }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FetchState {
    Fetched,
    RobotsDenied,
    InvalidUrl,
    UnsafeAddress,
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
    pub model: &'static str,
    pub seeds_received: usize,
    pub pages_attempted: usize,
    pub pages_fetched: usize,
    pub pages_denied: usize,
    pub truncated_by_budget: bool,
    pub disclaimer: &'static str,
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
}
impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(15),
            per_origin_delay: Duration::from_millis(750),
            max_body_bytes: 2 * 1024 * 1024,
            max_content_chars: 24_000,
            max_links_per_page: 32,
        }
    }
}

pub struct SearchEngine {
    agent: ureq::Agent,
    config: SearchConfig,
}

impl SearchEngine {
    pub fn new(config: SearchConfig) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(config.timeout))
            .max_redirects(0)
            .user_agent(USER_AGENT)
            .build()
            .into();
        Self { agent, config }
    }

    pub fn search(&self, mut request: SearchRequest) -> SearchResponse {
        request.max_pages = request.max_pages.clamp(1, 64);
        request.max_results = request.max_results.clamp(1, 20);
        let seed_count = request.seeds.len();
        let query_terms = terms(&request.query);
        let mut queue = VecDeque::new();
        let mut seen = HashSet::new();
        let mut roots = Vec::new();
        let mut evidence = Vec::new();
        let mut failures = Vec::new();
        let mut robots: HashMap<String, RobotsRules> = HashMap::new();
        let mut last_request: HashMap<String, Instant> = HashMap::new();

        for seed in &request.seeds {
            match normalize_public_url(seed) {
                Ok(url) => {
                    if !roots.iter().any(|r: &Url| same_scope(r, &url, request.allow_subdomains)) {
                        roots.push(url.clone());
                    }
                    queue.push_back((url, DiscoveryKind::Seed, None));
                }
                Err((state, detail)) => failures.push(failure(seed, state, detail, DiscoveryKind::Seed, None)),
            }
        }

        while evidence.len() + failures.len() < request.max_pages {
            let Some((url, discovery, from)) = queue.pop_front() else { break };
            let key = canonical_key(&url);
            if !seen.insert(key) { continue; }
            let origin = origin_key(&url);

            let rules = robots.entry(origin.clone()).or_insert_with(|| self.load_robots(&url));
            if !rules.allowed(url.path()) {
                failures.push(failure(url.as_str(), FetchState::RobotsDenied,
                    "robots.txt disallows this path".into(), discovery, from).with_robots(false));
                continue;
            }

            throttle(&mut last_request, &origin, self.config.per_origin_delay);
            match self.fetch_html(&url, discovery.clone(), from.clone()) {
                Ok((mut item, links)) => {
                    item.robots_allowed = Some(true);
                    item.score = rank(item.title.as_deref(), item.content.as_deref(), &query_terms);
                    let parent = item.final_url.clone();
                    evidence.push(item);
                    for link in links.into_iter().take(self.config.max_links_per_page) {
                        if roots.iter().any(|root| same_scope(root, &link, request.allow_subdomains)) {
                            queue.push_back((link, DiscoveryKind::Link, Some(parent.clone())));
                        }
                    }
                }
                Err(mut item) => {
                    item.robots_allowed = Some(true);
                    failures.push(item);
                }
            }
        }

        evidence.sort_by(|a, b| b.score.total_cmp(&a.score));
        evidence.truncate(request.max_results);
        let pages_fetched = evidence.len();
        let pages_denied = failures.iter().filter(|e| e.state == FetchState::RobotsDenied).count();
        let attempted = seen.len() + failures.iter().filter(|e| e.state == FetchState::InvalidUrl).count();
        let truncated = !queue.is_empty() && attempted >= request.max_pages;
        SearchResponse {
            query: request.query,
            evidence,
            failures,
            coverage: Coverage {
                model: "bounded_seed_crawl",
                seeds_received: seed_count,
                pages_attempted: attempted,
                pages_fetched,
                pages_denied,
                truncated_by_budget: truncated,
                disclaimer: "REX-search searched only the supplied public seeds and a bounded set of allowed links. It is live evidence retrieval, not a complete index of the web.",
            },
        }
    }

    fn load_robots(&self, page: &Url) -> RobotsRules {
        let Ok(robots_url) = page.join("/robots.txt") else { return RobotsRules::allow_all() };
        if reject_private_destination(&robots_url).is_err() { return RobotsRules::deny_all() }
        match self.agent.get(robots_url.as_str()).header("Accept", "text/plain").call() {
            Ok(response) if response.status().as_u16() == 200 => {
                let body = response.into_body().read_to_string().unwrap_or_default();
                RobotsRules::parse(&body)
            }
            Ok(response) if response.status().as_u16() >= 500 => RobotsRules::deny_all(),
            Ok(_) => RobotsRules::allow_all(),
            Err(ureq::Error::StatusCode(code)) if code >= 500 => RobotsRules::deny_all(),
            Err(_) => RobotsRules::deny_all(),
        }
    }

    fn fetch_html(&self, url: &Url, discovery: DiscoveryKind, from: Option<String>) -> Result<(Evidence, Vec<Url>), Evidence> {
        if let Err(detail) = reject_private_destination(url) {
            return Err(failure(url.as_str(), FetchState::UnsafeAddress, detail, discovery, from));
        }
        let now = now_ms();
        let response = match self.agent.get(url.as_str()).header("Accept", "text/html,application/xhtml+xml;q=0.9,text/plain;q=0.5").call() {
            Ok(r) => r,
            Err(ureq::Error::StatusCode(code)) => {
                let state = if code == 429 { FetchState::RateLimited } else { FetchState::HttpError };
                return Err(failure(url.as_str(), state, format!("HTTP {code}"), discovery, from).with_status(code));
            }
            Err(e) => return Err(failure(url.as_str(), FetchState::NetworkError, e.to_string(), discovery, from)),
        };
        let status = response.status().as_u16();
        let final_url = url.to_string();
        let content_type = response.headers().get("content-type").and_then(|h| h.to_str().ok()).unwrap_or("").to_ascii_lowercase();
        if !(content_type.contains("text/html") || content_type.contains("application/xhtml+xml") || content_type.contains("text/plain")) {
            return Err(failure(url.as_str(), FetchState::UnsupportedContent, format!("unsupported content type: {content_type}"), discovery, from).with_status(status));
        }
        if response.headers().get("content-length").and_then(|h| h.to_str().ok()).and_then(|s| s.parse::<usize>().ok()).is_some_and(|n| n > self.config.max_body_bytes) {
            return Err(failure(url.as_str(), FetchState::TooLarge, "declared response exceeds body limit".into(), discovery, from).with_status(status));
        }
        let body = response.into_body().with_config().limit(self.config.max_body_bytes as u64).read_to_string()
            .map_err(|e| failure(url.as_str(), FetchState::NetworkError, format!("body read failed: {e}"), discovery.clone(), from.clone()).with_status(status))?;
        let effective = url.clone();
        let title = extract_title(&body);
        let mut content = html_to_text(&body);
        content.truncate(char_boundary(&content, self.config.max_content_chars));
        let excerpt = make_excerpt(&content, &[]);
        let links = extract_links(&body, &effective);
        Ok((Evidence {
            url: url.to_string(), final_url, title, excerpt, content: Some(content),
            retrieved_at_unix_ms: now, http_status: Some(status), content_type: Some(content_type),
            discovery, discovered_from: from, state: FetchState::Fetched, error: None,
            score: 0.0, robots_allowed: Some(true),
        }, links))
    }
}

impl Default for SearchEngine { fn default() -> Self { Self::new(SearchConfig::default()) } }

#[derive(Debug, Clone)]
struct RobotsRules { rules: Vec<(bool, String)> }
impl RobotsRules {
    fn allow_all() -> Self { Self { rules: vec![] } }
    fn deny_all() -> Self { Self { rules: vec![(false, "/".into())] } }
    fn parse(text: &str) -> Self {
        let mut applies = false;
        let mut rules = Vec::new();
        for raw in text.lines() {
            let line = raw.split('#').next().unwrap_or("").trim();
            let Some((name, value)) = line.split_once(':') else { continue };
            match name.trim().to_ascii_lowercase().as_str() {
                "user-agent" => {
                    let v = value.trim().to_ascii_lowercase();
                    applies = v == "*" || v == "rex-search" || v.starts_with("rex-search/");
                }
                "allow" if applies && !value.trim().is_empty() => rules.push((true, value.trim().to_string())),
                "disallow" if applies && !value.trim().is_empty() => rules.push((false, value.trim().to_string())),
                _ => {}
            }
        }
        Self { rules }
    }
    fn allowed(&self, path: &str) -> bool {
        self.rules.iter().filter(|(_, p)| path.starts_with(p.as_str()))
            .max_by_key(|(_, p)| p.len()).map(|(allow, _)| *allow).unwrap_or(true)
    }
}

fn normalize_public_url(raw: &str) -> Result<Url, (FetchState, String)> {
    let url = Url::parse(raw).map_err(|e| (FetchState::InvalidUrl, e.to_string()))?;
    if !matches!(url.scheme(), "http" | "https") { return Err((FetchState::InvalidUrl, "only http and https are supported".into())) }
    reject_private_destination(&url).map_err(|e| (FetchState::UnsafeAddress, e))?;
    Ok(url)
}

fn reject_private_destination(url: &Url) -> Result<(), String> {
    let host = url.host_str().ok_or_else(|| "URL has no host".to_string())?;
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".local") || host.ends_with(".internal") { return Err("local hostnames are blocked".into()) }
    let port = url.port_or_known_default().unwrap_or(80);
    let addrs = (host, port).to_socket_addrs().map_err(|e| format!("host resolution failed: {e}"))?;
    let mut resolved = false;
    for addr in addrs {
        resolved = true;
        if !is_public_ip(addr.ip()) { return Err(format!("non-public destination {} is blocked", addr.ip())) }
    }
    if !resolved { return Err("host resolved to no addresses".into()) }
    Ok(())
}
fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => !(v.is_private() || v.is_loopback() || v.is_link_local() || v.is_broadcast() || v.is_unspecified() || v.octets()[0] == 0),
        IpAddr::V6(v) => !(v.is_loopback() || v.is_unspecified() || v.is_unique_local() || v.is_unicast_link_local()),
    }
}
fn same_scope(root: &Url, candidate: &Url, subdomains: bool) -> bool {
    let (Some(a), Some(b)) = (root.host_str(), candidate.host_str()) else { return false };
    a.eq_ignore_ascii_case(b) || (subdomains && b.to_ascii_lowercase().ends_with(&format!(".{}", a.to_ascii_lowercase())))
}
fn origin_key(url: &Url) -> String { format!("{}://{}:{}", url.scheme(), url.host_str().unwrap_or(""), url.port_or_known_default().unwrap_or(0)) }
fn canonical_key(url: &Url) -> String { let mut u=url.clone(); u.set_fragment(None); u.to_string() }
fn throttle(last: &mut HashMap<String, Instant>, origin: &str, delay: Duration) {
    if let Some(previous) = last.get(origin) { let elapsed=previous.elapsed(); if elapsed < delay { std::thread::sleep(delay-elapsed); } }
    last.insert(origin.to_string(), Instant::now());
}
fn now_ms() -> u128 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() }

fn terms(query: &str) -> Vec<String> { query.split(|c: char| !c.is_alphanumeric()).filter(|s| s.len() > 1).map(str::to_ascii_lowercase).collect() }
fn rank(title: Option<&str>, body: Option<&str>, query: &[String]) -> f32 {
    if query.is_empty() { return 0.0 }
    let title = title.unwrap_or("").to_ascii_lowercase(); let body = body.unwrap_or("").to_ascii_lowercase();
    query.iter().map(|t| (if title.contains(t){4.0}else{0.0}) + body.matches(t).take(8).count() as f32).sum::<f32>() / query.len() as f32
}
fn extract_title(html: &str) -> Option<String> { extract_between_ci(html, "<title", "</title>").and_then(|s| s.split_once('>').map(|(_,v)| decode_entities(v).trim().to_string())).filter(|s| !s.is_empty()) }
fn extract_between_ci<'a>(s: &'a str, start: &str, end: &str) -> Option<&'a str> { let lower=s.to_ascii_lowercase(); let a=lower.find(start)?; let b=lower[a..].find(end)?+a; Some(&s[a..b]) }
fn extract_links(html: &str, base: &Url) -> Vec<Url> {
    let lower=html.to_ascii_lowercase(); let mut out=Vec::new(); let mut at=0;
    while let Some(pos)=lower[at..].find("href=") { let i=at+pos+5; let bytes=html.as_bytes(); if i>=bytes.len(){break}; let quote=bytes[i] as char; let (start, end)=if quote=='\''||quote=='"' { let start=i+1; let Some(n)=html[start..].find(quote) else {break}; (start,start+n) } else { let end=html[i..].find(|c: char| c.is_whitespace()||c=='>').map(|n|i+n).unwrap_or(html.len()); (i,end) }; if let Ok(mut u)=base.join(html[start..end].trim()) { u.set_fragment(None); if matches!(u.scheme(),"http"|"https") { out.push(u); } } at=end.saturating_add(1); if out.len()>=128 {break} }
    out
}
fn html_to_text(html: &str) -> String {
    let mut out=String::with_capacity(html.len().min(32_000)); let mut tag=false; let mut skip: Option<&str>=None; let lower=html.to_ascii_lowercase(); let bytes=html.as_bytes(); let mut i=0;
    while i<bytes.len() { if skip.is_some() { let needle=format!("</{}",skip.unwrap()); if let Some(n)=lower[i..].find(&needle){ i+=n; skip=None; continue } else {break} } if bytes[i]==b'<' { let tail=&lower[i..]; if tail.starts_with("<script") {skip=Some("script")} else if tail.starts_with("<style") {skip=Some("style")} else if tail.starts_with("<noscript") {skip=Some("noscript")} tag=true; if !out.ends_with(' '){out.push(' ')} } else if bytes[i]==b'>' {tag=false} else if !tag { out.push(bytes[i] as char) } i+=1 }
    decode_entities(&out).split_whitespace().collect::<Vec<_>>().join(" ")
}
fn decode_entities(s:&str)->String { s.replace("&amp;","&").replace("&lt;","<").replace("&gt;",">").replace("&quot;","\"").replace("&#39;","'").replace("&nbsp;"," ") }
fn floor_boundary(s:&str, mut i:usize)->usize { i=i.min(s.len()); while !s.is_char_boundary(i){i-=1} i }
fn char_boundary(s:&str,max:usize)->usize { if s.len()<=max{return s.len()} let mut i=max; while !s.is_char_boundary(i){i-=1} i }
fn make_excerpt(content:&str, query:&[String])->Option<String>{ if content.is_empty(){return None} let lower=content.to_ascii_lowercase(); let pos=query.iter().filter_map(|t|lower.find(t)).min().unwrap_or(0); let start=pos.saturating_sub(180); let start=floor_boundary(content,start); let end=floor_boundary(content,(start+520).min(content.len())); Some(content[start..end].to_string()) }

fn failure(url:&str,state:FetchState,detail:String,discovery:DiscoveryKind,from:Option<String>)->Evidence { Evidence { url:url.into(),final_url:url.into(),title:None,excerpt:None,content:None,retrieved_at_unix_ms:now_ms(),http_status:None,content_type:None,discovery,discovered_from:from,state,error:Some(detail),score:0.0,robots_allowed:None } }
impl Evidence { fn with_status(mut self,status:u16)->Self{self.http_status=Some(status);self} fn with_robots(mut self,allowed:bool)->Self{self.robots_allowed=Some(allowed);self} }

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn robots_longest_match_wins(){ let r=RobotsRules::parse("User-agent: *\nDisallow: /private\nAllow: /private/public\n"); assert!(!r.allowed("/private/a")); assert!(r.allowed("/private/public/a")); }
    #[test] fn named_agent_rules_apply(){ let r=RobotsRules::parse("User-agent: REX-search\nDisallow: /no\n"); assert!(!r.allowed("/no/a")); }
    #[test] fn private_targets_are_rejected(){ assert!(normalize_public_url("http://127.0.0.1/x").is_err()); assert!(normalize_public_url("file:///etc/passwd").is_err()); }
    #[test] fn scope_is_exact_by_default(){ let a=Url::parse("https://example.com/a").unwrap(); assert!(same_scope(&a,&Url::parse("https://example.com/b").unwrap(),false)); assert!(!same_scope(&a,&Url::parse("https://evil-example.com/b").unwrap(),true)); assert!(same_scope(&a,&Url::parse("https://docs.example.com/b").unwrap(),true)); }
    #[test] fn extraction_removes_scripts(){ let h="<html><head><title>A &amp; B</title><style>x</style></head><body>Hello <b>world</b><script>secret</script></body></html>"; assert_eq!(extract_title(h).as_deref(),Some("A & B")); let t=html_to_text(h); assert!(t.contains("Hello world")); assert!(!t.contains("secret")); }
    #[test] fn ranking_favors_title(){ let q=terms("rust agents"); assert!(rank(Some("Rust agents"),Some("x"),&q)>rank(Some("x"),Some("rust agents"),&q)); }
}
