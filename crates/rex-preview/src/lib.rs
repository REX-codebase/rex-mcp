//! Rust-owned policy and lifecycle for live UI previews.
//!
//! Browser transport is deliberately represented as typed evidence and actions.
//! Provider text never gets a process handle, port allocator, filesystem path,
//! or an unvalidated URL.

use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, VecDeque},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Component, Path, PathBuf},
    time::{Duration, SystemTime},
};
use thiserror::Error;
use url::Url;

pub mod supervisor;
pub use supervisor::{LifecycleEvent, LifecycleKind, PreviewSupervisor, SupervisorSummary};

pub const MAX_ITERATIONS: u8 = 12;
pub const MAX_EVIDENCE_ITEMS: usize = 256;
pub const MAX_CONSOLE_BYTES: usize = 256 * 1024;
pub const MAX_SCREENSHOT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_STARTUP: Duration = Duration::from_secs(45);
pub const MAX_IDLE: Duration = Duration::from_secs(20 * 60);
pub const PORT_MIN: u16 = 41_000;
pub const PORT_MAX: u16 = 41_999;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Framework {
    StaticHtml,
    Vite,
    NextJs,
    CreateReactApp,
    Astro,
    SvelteKit,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PreviewRecipe {
    pub framework: Framework,
    pub project_dir: PathBuf,
    pub program: String,
    pub args: Vec<String>,
    pub readiness_path: String,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum PreviewError {
    #[error("project path is outside the selected workspace")]
    PathEscape,
    #[error("project path contains a symlink component")]
    SymlinkComponent,
    #[error("no supported UI entry point was detected")]
    UnsupportedProject,
    #[error("preview command is not on the allowlist")]
    CommandDenied,
    #[error("preview port is outside the reserved loopback range")]
    PortDenied,
    #[error("preview URL is not a permitted loopback URL")]
    UrlDenied,
    #[error("preview session is not running")]
    NotRunning,
    #[error("preview session is already terminal")]
    Terminal,
    #[error("iteration budget exhausted")]
    IterationLimit,
    #[error("evidence exceeded its bounded budget")]
    EvidenceLimit,
    #[error("production gates have not passed")]
    GatesFailed,
}

/// Detect from a bounded manifest snapshot collected by the trusted filesystem layer.
/// Values are file contents, never commands supplied by the model.
pub fn detect_project(
    workspace: &Path,
    project_dir: &Path,
    files: &BTreeMap<String, String>,
) -> Result<PreviewRecipe, PreviewError> {
    validate_project_path(workspace, project_dir)?;
    let package = files.get("package.json").map(String::as_str).unwrap_or("");
    let has = |needle: &str| package.contains(needle);
    let (framework, script) = if files.contains_key("next.config.js")
        || files.contains_key("next.config.mjs")
        || files.contains_key("next.config.ts")
        || has("\"next\"")
    {
        (Framework::NextJs, "dev")
    } else if has("\"vite\"")
        || files.contains_key("vite.config.ts")
        || files.contains_key("vite.config.js")
    {
        (Framework::Vite, "dev")
    } else if has("react-scripts") {
        (Framework::CreateReactApp, "start")
    } else if has("\"astro\"") {
        (Framework::Astro, "dev")
    } else if has("@sveltejs/kit") {
        (Framework::SvelteKit, "dev")
    } else if files.contains_key("index.html") {
        return Ok(PreviewRecipe {
            framework: Framework::StaticHtml,
            project_dir: project_dir.to_path_buf(),
            program: "rex-static-preview".into(),
            args: vec![],
            readiness_path: "/".into(),
        });
    } else {
        return Err(PreviewError::UnsupportedProject);
    };

    Ok(PreviewRecipe {
        framework,
        project_dir: project_dir.to_path_buf(),
        program: "npm".into(),
        args: vec!["run".into(), script.into(), "--".into()],
        readiness_path: "/".into(),
    })
}

pub fn validate_project_path(workspace: &Path, project: &Path) -> Result<(), PreviewError> {
    if project.is_absolute()
        || project.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(PreviewError::PathEscape);
    }
    let joined = workspace.join(project);
    if !joined.starts_with(workspace) {
        return Err(PreviewError::PathEscape);
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LaunchPlan {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub bind: SocketAddr,
    pub environment: BTreeMap<String, String>,
    pub startup_timeout_ms: u64,
    pub idle_timeout_ms: u64,
}

impl LaunchPlan {
    pub fn from_recipe(recipe: &PreviewRecipe, port: u16) -> Result<Self, PreviewError> {
        if !(PORT_MIN..=PORT_MAX).contains(&port) {
            return Err(PreviewError::PortDenied);
        }
        if recipe.program != "npm" && recipe.program != "rex-static-preview" {
            return Err(PreviewError::CommandDenied);
        }
        let mut args = recipe.args.clone();
        let mut environment = BTreeMap::from([
            ("NO_COLOR".into(), "1".into()),
            ("BROWSER".into(), "none".into()),
        ]);
        if recipe.program == "npm" {
            match recipe.framework {
                Framework::NextJs => args.extend([
                    "--hostname".into(),
                    "127.0.0.1".into(),
                    "--port".into(),
                    port.to_string(),
                ]),
                Framework::CreateReactApp => {
                    environment.insert("HOST".into(), "127.0.0.1".into());
                    environment.insert("PORT".into(), port.to_string());
                }
                Framework::Vite | Framework::Astro | Framework::SvelteKit => args.extend([
                    "--host".into(),
                    "127.0.0.1".into(),
                    "--port".into(),
                    port.to_string(),
                ]),
                Framework::StaticHtml => {}
            }
        }
        Ok(Self {
            program: recipe.program.clone(),
            args,
            cwd: recipe.project_dir.clone(),
            bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
            environment,
            startup_timeout_ms: MAX_STARTUP.as_millis() as u64,
            idle_timeout_ms: MAX_IDLE.as_millis() as u64,
        })
    }

    pub fn url(&self, path: &str) -> Result<Url, PreviewError> {
        let path = if path.starts_with('/') { path } else { "/" };
        validate_local_url(&format!("http://{}{}", self.bind, path), self.bind.port())
    }
}

pub fn validate_local_url(raw: &str, expected_port: u16) -> Result<Url, PreviewError> {
    let url = Url::parse(raw).map_err(|_| PreviewError::UrlDenied)?;
    let host_ok = matches!(url.host_str(), Some("127.0.0.1") | Some("localhost"));
    if url.scheme() != "http"
        || !host_ok
        || url.port_or_known_default() != Some(expected_port)
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(PreviewError::UrlDenied);
    }
    Ok(url)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BrowserAction {
    Navigate { path: String },
    PointerMove { x: f32, y: f32 },
    PointerDown { button: PointerButton },
    PointerUp { button: PointerButton },
    Key { key: SafeKey, state: KeyState },
    Text { value: String },
    Scroll { delta_x: f32, delta_y: f32 },
    SetViewport { width: u16, height: u16, scale: f32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PointerButton {
    Primary,
    Auxiliary,
    Secondary,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyState {
    Down,
    Up,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum SafeKey {
    Enter,
    Tab,
    Escape,
    Backspace,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    PageUp,
    PageDown,
    Space,
}

impl BrowserAction {
    pub fn validate(&self) -> Result<(), PreviewError> {
        match self {
            Self::Navigate { path }
                if !path.starts_with('/') || path.starts_with("//") || path.contains("..") =>
            {
                Err(PreviewError::UrlDenied)
            }
            Self::PointerMove { x, y }
                if !x.is_finite() || !y.is_finite() || *x < 0.0 || *y < 0.0 =>
            {
                Err(PreviewError::EvidenceLimit)
            }
            Self::Scroll { delta_x, delta_y }
                if !delta_x.is_finite()
                    || !delta_y.is_finite()
                    || delta_x.abs() > 10_000.0
                    || delta_y.abs() > 10_000.0 =>
            {
                Err(PreviewError::EvidenceLimit)
            }
            Self::SetViewport {
                width,
                height,
                scale,
            } if !(240..=3840).contains(width)
                || !(240..=2160).contains(height)
                || !scale.is_finite()
                || !(0.5..=4.0).contains(scale) =>
            {
                Err(PreviewError::EvidenceLimit)
            }
            Self::Text { value } if value.len() > 16 * 1024 => Err(PreviewError::EvidenceLimit),
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Evidence {
    Screenshot {
        id: String,
        width: u16,
        height: u16,
        byte_len: usize,
        sha256: String,
    },
    Accessibility {
        id: String,
        node_count: u32,
        text: String,
    },
    DomSnapshot {
        id: String,
        byte_len: usize,
        sha256: String,
    },
    Console {
        level: ConsoleLevel,
        message: String,
    },
    NetworkFailure {
        method: String,
        path: String,
        status: Option<u16>,
        error: String,
    },
    Viewport {
        width: u16,
        height: u16,
        scale_milli: u16,
    },
    Gate {
        name: ProductionGate,
        passed: bool,
        detail: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsoleLevel {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProductionGate {
    Starts,
    NoConsoleErrors,
    NoFailedRequests,
    DesktopViewport,
    MobileViewport,
    KeyboardReachable,
    AccessibilityTree,
    VisualEvidence,
    Tests,
    Build,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IterationReceipt {
    pub iteration: u8,
    pub accepted: bool,
    pub diff_id: String,
    pub evidence_ids: Vec<String>,
    pub failed_gates: Vec<ProductionGate>,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProductionReport {
    pub gates: BTreeMap<ProductionGate, bool>,
    pub receipts: Vec<IterationReceipt>,
}

impl ProductionReport {
    pub fn required_pass(&self) -> bool {
        use ProductionGate::*;
        [
            Starts,
            NoConsoleErrors,
            NoFailedRequests,
            DesktopViewport,
            MobileViewport,
            KeyboardReachable,
            AccessibilityTree,
            VisualEvidence,
            Tests,
            Build,
        ]
        .iter()
        .all(|g| self.gates.get(g) == Some(&true))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Created,
    Starting,
    Running,
    Iterating,
    Passed,
    Failed,
    Cancelled,
    TimedOut,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreviewSession {
    pub id: String,
    pub state: SessionState,
    pub launch: LaunchPlan,
    pub iteration: u8,
    pub evidence: VecDeque<Evidence>,
    pub receipts: Vec<IterationReceipt>,
    pub created_at: SystemTime,
    pub cancellation_requested: bool,
}

impl PreviewSession {
    pub fn new(id: String, launch: LaunchPlan, created_at: SystemTime) -> Self {
        Self {
            id,
            state: SessionState::Created,
            launch,
            iteration: 0,
            evidence: VecDeque::new(),
            receipts: vec![],
            created_at,
            cancellation_requested: false,
        }
    }

    pub fn start(&mut self) -> Result<(), PreviewError> {
        if self.terminal() {
            return Err(PreviewError::Terminal);
        }
        self.state = SessionState::Starting;
        Ok(())
    }

    pub fn ready(&mut self) -> Result<(), PreviewError> {
        if self.state != SessionState::Starting {
            return Err(PreviewError::NotRunning);
        }
        self.state = SessionState::Running;
        Ok(())
    }

    pub fn begin_iteration(&mut self) -> Result<u8, PreviewError> {
        if !matches!(self.state, SessionState::Running | SessionState::Iterating) {
            return Err(PreviewError::NotRunning);
        }
        if self.iteration >= MAX_ITERATIONS {
            return Err(PreviewError::IterationLimit);
        }
        self.iteration += 1;
        self.state = SessionState::Iterating;
        Ok(self.iteration)
    }

    pub fn push_evidence(&mut self, item: Evidence) -> Result<(), PreviewError> {
        if self.evidence.len() >= MAX_EVIDENCE_ITEMS {
            return Err(PreviewError::EvidenceLimit);
        }
        match &item {
            Evidence::Screenshot { byte_len, .. } if *byte_len > MAX_SCREENSHOT_BYTES => {
                return Err(PreviewError::EvidenceLimit)
            }
            Evidence::Console { message, .. } if message.len() > MAX_CONSOLE_BYTES => {
                return Err(PreviewError::EvidenceLimit)
            }
            _ => {}
        }
        self.evidence.push_back(item);
        Ok(())
    }

    /// Rejected iterations remain durable receipts. Passing requires external gate
    /// evidence; a model's self-score is intentionally absent from this API.
    pub fn record_iteration(&mut self, receipt: IterationReceipt) -> Result<(), PreviewError> {
        if receipt.iteration != self.iteration {
            return Err(PreviewError::NotRunning);
        }
        self.receipts.push(receipt);
        self.state = SessionState::Running;
        Ok(())
    }

    pub fn finish(&mut self, report: &ProductionReport) -> Result<(), PreviewError> {
        if !report.required_pass() {
            return Err(PreviewError::GatesFailed);
        }
        self.state = SessionState::Passed;
        Ok(())
    }

    pub fn cancel(&mut self) {
        self.cancellation_requested = true;
        self.state = SessionState::Cancelled;
    }

    pub fn fail(&mut self) {
        self.state = SessionState::Failed;
    }
    pub fn timeout(&mut self) {
        self.state = SessionState::TimedOut;
    }
    pub fn terminal(&self) -> bool {
        matches!(
            self.state,
            SessionState::Passed
                | SessionState::Failed
                | SessionState::Cancelled
                | SessionState::TimedOut
        )
    }
    pub fn needs_process_tree_kill(&self) -> bool {
        self.terminal() || self.cancellation_requested
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect()
    }

    #[test]
    fn detects_supported_frameworks_without_executing_manifest_content() {
        let w = Path::new("/workspace");
        let cases = [
            (
                files(&[("package.json", "{\"dependencies\":{\"next\":\"15\"}}")]),
                Framework::NextJs,
            ),
            (
                files(&[("package.json", "{\"devDependencies\":{\"vite\":\"6\"}}")]),
                Framework::Vite,
            ),
            (
                files(&[(
                    "package.json",
                    "{\"scripts\":{\"start\":\"react-scripts start\"}}",
                )]),
                Framework::CreateReactApp,
            ),
            (
                files(&[("index.html", "<main>ok</main>")]),
                Framework::StaticHtml,
            ),
        ];
        for (snapshot, expected) in cases {
            assert_eq!(
                detect_project(w, Path::new("app"), &snapshot)
                    .unwrap()
                    .framework,
                expected
            );
        }
    }

    #[test]
    fn rejects_path_escape_and_unknown_projects() {
        assert_eq!(
            detect_project(
                Path::new("/w"),
                Path::new("../etc"),
                &files(&[("index.html", "x")])
            )
            .unwrap_err(),
            PreviewError::PathEscape
        );
        assert_eq!(
            detect_project(Path::new("/w"), Path::new("app"), &BTreeMap::new()).unwrap_err(),
            PreviewError::UnsupportedProject
        );
    }

    #[test]
    fn launch_is_loopback_and_argv_only() {
        let recipe = detect_project(
            Path::new("/w"),
            Path::new("app"),
            &files(&[("package.json", "{\"vite\":true}")]),
        )
        .unwrap();
        let launch = LaunchPlan::from_recipe(&recipe, PORT_MIN).unwrap();
        assert_eq!(launch.bind.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(launch.program, "npm");
        assert!(launch.args.iter().all(|a| !a.contains(';')));
        assert_eq!(
            launch.url("/").unwrap().as_str(),
            format!("http://127.0.0.1:{PORT_MIN}/")
        );
    }

    #[test]
    fn blocks_ssrf_userinfo_and_wrong_ports() {
        for bad in [
            "https://127.0.0.1:41000/",
            "http://0.0.0.0:41000/",
            "http://localhost:80/",
            "http://user@localhost:41000/",
        ] {
            assert_eq!(
                validate_local_url(bad, PORT_MIN).unwrap_err(),
                PreviewError::UrlDenied
            );
        }
    }

    #[test]
    fn browser_actions_are_normalized_and_bounded() {
        assert!(BrowserAction::Navigate {
            path: "/settings".into()
        }
        .validate()
        .is_ok());
        assert!(BrowserAction::Navigate {
            path: "//evil.test".into()
        }
        .validate()
        .is_err());
        assert!(BrowserAction::SetViewport {
            width: 1440,
            height: 900,
            scale: 1.0
        }
        .validate()
        .is_ok());
        assert!(BrowserAction::SetViewport {
            width: 100,
            height: 100,
            scale: 1.0
        }
        .validate()
        .is_err());
        assert!(BrowserAction::Text {
            value: "x".repeat(20_000)
        }
        .validate()
        .is_err());
    }

    #[test]
    fn cancellation_is_terminal_and_requires_tree_cleanup() {
        let recipe = detect_project(
            Path::new("/w"),
            Path::new("app"),
            &files(&[("index.html", "x")]),
        )
        .unwrap();
        let mut s = PreviewSession::new(
            "p1".into(),
            LaunchPlan::from_recipe(&recipe, PORT_MIN).unwrap(),
            SystemTime::UNIX_EPOCH,
        );
        s.start().unwrap();
        s.ready().unwrap();
        s.cancel();
        assert!(s.terminal());
        assert!(s.needs_process_tree_kill());
        assert_eq!(s.begin_iteration().unwrap_err(), PreviewError::NotRunning);
    }

    #[test]
    fn rejected_iterations_and_diffs_are_preserved() {
        let recipe = detect_project(
            Path::new("/w"),
            Path::new("app"),
            &files(&[("index.html", "x")]),
        )
        .unwrap();
        let mut s = PreviewSession::new(
            "p1".into(),
            LaunchPlan::from_recipe(&recipe, PORT_MIN).unwrap(),
            SystemTime::UNIX_EPOCH,
        );
        s.start().unwrap();
        s.ready().unwrap();
        assert_eq!(s.begin_iteration().unwrap(), 1);
        s.record_iteration(IterationReceipt {
            iteration: 1,
            accepted: false,
            diff_id: "diff-a".into(),
            evidence_ids: vec!["shot-a".into()],
            failed_gates: vec![ProductionGate::MobileViewport],
            reason: "overflow at 320px".into(),
        })
        .unwrap();
        assert_eq!(s.receipts[0].diff_id, "diff-a");
        assert!(!s.receipts[0].accepted);
    }

    #[test]
    fn evidence_limits_are_enforced() {
        let recipe = detect_project(
            Path::new("/w"),
            Path::new("app"),
            &files(&[("index.html", "x")]),
        )
        .unwrap();
        let mut s = PreviewSession::new(
            "p1".into(),
            LaunchPlan::from_recipe(&recipe, PORT_MIN).unwrap(),
            SystemTime::UNIX_EPOCH,
        );
        let too_big = Evidence::Screenshot {
            id: "x".into(),
            width: 1,
            height: 1,
            byte_len: MAX_SCREENSHOT_BYTES + 1,
            sha256: "x".into(),
        };
        assert_eq!(
            s.push_evidence(too_big).unwrap_err(),
            PreviewError::EvidenceLimit
        );
    }

    #[test]
    fn cannot_finish_on_model_opinion_or_partial_gates() {
        let recipe = detect_project(
            Path::new("/w"),
            Path::new("app"),
            &files(&[("index.html", "x")]),
        )
        .unwrap();
        let mut s = PreviewSession::new(
            "p1".into(),
            LaunchPlan::from_recipe(&recipe, PORT_MIN).unwrap(),
            SystemTime::UNIX_EPOCH,
        );
        let report = ProductionReport {
            gates: BTreeMap::from([(ProductionGate::Starts, true)]),
            receipts: vec![],
        };
        assert_eq!(s.finish(&report).unwrap_err(), PreviewError::GatesFailed);
    }

    #[test]
    fn concrete_gate_matrix_can_finish() {
        use ProductionGate::*;
        let recipe = detect_project(
            Path::new("/w"),
            Path::new("app"),
            &files(&[("index.html", "x")]),
        )
        .unwrap();
        let mut s = PreviewSession::new(
            "p1".into(),
            LaunchPlan::from_recipe(&recipe, PORT_MIN).unwrap(),
            SystemTime::UNIX_EPOCH,
        );
        let gates = [
            Starts,
            NoConsoleErrors,
            NoFailedRequests,
            DesktopViewport,
            MobileViewport,
            KeyboardReachable,
            AccessibilityTree,
            VisualEvidence,
            Tests,
            Build,
        ]
        .into_iter()
        .map(|g| (g, true))
        .collect();
        s.finish(&ProductionReport {
            gates,
            receipts: vec![],
        })
        .unwrap();
        assert_eq!(s.state, SessionState::Passed);
    }
}
