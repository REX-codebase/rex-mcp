//! Live task runs: one bounded pipeline from a real provider turn through a
//! trusted approval decision into a tool receipt and the native preview.
//!
//! A run never executes a model-requested write on its own. `begin` only
//! prepares; execution happens in `decide` after the trusted UI answers.
//! Keys are read inside the backend for the provider call only and are never
//! serialized into snapshots, events, or logs.

use crate::agent_loop::{AgentLoop, LoopEvent};
use crate::conversation::decode_turn;
use crate::http::Transport;
use crate::providers::ProviderProtocol;
use crate::secrets::SecretStore;
use crate::service::ProviderService;
use rex_preview::{
    BrowserAction, BrowserEvidence, IterationReceipt, PreviewSupervisor, ProductionGate,
};
use rex_tools::{PreparedCall, ToolResult, ToolRuntime};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_RUNS: usize = 8;
const DESKTOP: (u16, u16, f32) = (1280, 800, 1.0);
const MOBILE: (u16, u16, f32) = (390, 844, 2.0);

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    AwaitingApproval,
    Live,
    Denied,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct PreviewSnapshot {
    pub session_id: String,
    pub url: String,
    pub state: String,
    pub framework: String,
    pub iteration: u8,
    pub receipts: Vec<IterationReceipt>,
    pub desktop_shot: Option<String>,
    pub mobile_shot: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunSnapshot {
    pub id: String,
    pub task: String,
    pub status: RunStatus,
    pub model: String,
    pub catalog_count: usize,
    pub events: Vec<LoopEvent>,
    pub approval: Option<PreparedCall>,
    pub result: Option<ToolResult>,
    pub preview: Option<PreviewSnapshot>,
    pub error: Option<String>,
}

struct LiveRun {
    task: String,
    status: RunStatus,
    model: String,
    catalog_count: usize,
    events: Vec<LoopEvent>,
    approval: Option<PreparedCall>,
    result: Option<ToolResult>,
    looper: AgentLoop,
    tools: ToolRuntime,
    workspace: PathBuf,
    preview: Option<PreviewSupervisor>,
    preview_session: Option<String>,
    preview_url: String,
    preview_framework: String,
    preview_iteration: u8,
    receipts: Vec<IterationReceipt>,
    desktop_shot: Option<String>,
    mobile_shot: Option<String>,
    error: Option<String>,
}

impl LiveRun {
    fn snapshot(&self, id: &str) -> RunSnapshot {
        RunSnapshot {
            id: id.to_string(),
            task: self.task.clone(),
            status: self.status.clone(),
            model: self.model.clone(),
            catalog_count: self.catalog_count,
            events: self.events.clone(),
            approval: self.approval.clone(),
            result: self.result.clone(),
            preview: self.preview_session.as_ref().map(|sid| PreviewSnapshot {
                session_id: sid.clone(),
                url: self.preview_url.clone(),
                state: "running".into(),
                framework: self.preview_framework.clone(),
                iteration: self.preview_iteration,
                receipts: self.receipts.clone(),
                desktop_shot: self.desktop_shot.clone(),
                mobile_shot: self.mobile_shot.clone(),
            }),
            error: self.error.clone(),
        }
    }
}

pub struct LiveRunService<S: SecretStore, T: Transport> {
    service: ProviderService<S, T>,
    runs: Mutex<HashMap<String, LiveRun>>,
    runs_root: PathBuf,
}

impl<S: SecretStore, T: Transport> LiveRunService<S, T> {
    pub fn new(service: ProviderService<S, T>, runs_root: PathBuf) -> Self {
        Self {
            service,
            runs: Mutex::new(HashMap::new()),
            runs_root,
        }
    }

    pub fn service(&self) -> &ProviderService<S, T> {
        &self.service
    }

    fn now_ms() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    }

    /// Real provider turn. The model catalogs and the generate call both go
    /// over the live transport; no replayed fixtures exist in this path.
    pub fn begin(&self, task: &str, provider: &str) -> Result<RunSnapshot, String> {
        let task = task.trim();
        if task.is_empty() {
            return Err("task is empty".into());
        }
        if task.chars().count() > 4_000 {
            return Err("task is too long".into());
        }
        if provider != "gemini" {
            return Err(format!(
                "live runs are implemented for gemini, not {provider}"
            ));
        }
        let spec = crate::providers::find_spec(provider).ok_or("unknown provider")?;
        let catalog = self.service.refresh(provider).map_err(|e| e.to_string())?;
        let model = pick_flash_lite(
            &catalog
                .models
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>(),
        )
        .ok_or("no live Flash Lite model in the current catalog")?;
        let key = self
            .service
            .get_key(provider)
            .map_err(|e| e.to_string())?
            .ok_or("no API key stored for this provider")?;

        let id = format!("run-{}-{:x}", std::process::id(), Self::now_ms());
        let workspace = self.runs_root.join(&id);
        fs::create_dir_all(&workspace).map_err(|e| e.to_string())?;
        let tools = ToolRuntime::new(&workspace).map_err(|e| e.detail)?;
        let mut looper = AgentLoop::new(spec.protocol, 12);
        let body = generate(self.service.transport(), &key, &model, task)?;
        let events = looper.accept(&body, &tools)?;
        let approval = events.iter().find_map(|e| match e {
            LoopEvent::ApprovalRequired { call } => Some(call.clone()),
            _ => None,
        });
        let catalog_count = catalog.models.len();
        let mut run = LiveRun {
            task: task.to_string(),
            status: RunStatus::AwaitingApproval,
            model,
            catalog_count,
            events,
            approval,
            result: None,
            looper,
            tools,
            workspace,
            preview: None,
            preview_session: None,
            preview_url: String::new(),
            preview_framework: String::new(),
            preview_iteration: 0,
            receipts: Vec::new(),
            desktop_shot: None,
            mobile_shot: None,
            error: None,
        };
        if run.approval.is_none() {
            run.status = RunStatus::Failed;
            run.error = Some("model did not request an approvable write".into());
        }
        let snapshot = run.snapshot(&id);
        let mut runs = self.runs.lock().map_err(|_| "run registry poisoned")?;
        if runs.len() >= MAX_RUNS {
            return Err("too many live runs; finish one first".into());
        }
        runs.insert(id, run);
        Ok(snapshot)
    }

    /// Trusted UI decision. Only this method can release a prepared write.
    pub fn decide(&self, run_id: &str, approved: bool) -> Result<RunSnapshot, String> {
        let mut runs = self.runs.lock().map_err(|_| "run registry poisoned")?;
        let run = runs.get_mut(run_id).ok_or("unknown run")?;
        if run.status != RunStatus::AwaitingApproval {
            return Err("run is not waiting for a decision".into());
        }
        let call_id = run
            .approval
            .as_ref()
            .map(|c| c.call_id.clone())
            .ok_or("no pending approval")?;
        let result = run.looper.resolve(&run.tools, &call_id, approved);
        run.result = Some(result.clone());
        run.events.push(LoopEvent::ToolFinished {
            result: result.clone(),
        });
        if !approved {
            let _ = run.tools.cancel(&call_id);
            run.status = RunStatus::Denied;
            return Ok(run.snapshot(run_id));
        }
        if !result.ok {
            run.status = RunStatus::Failed;
            run.error = result.error.as_ref().map(|e| e.detail.clone());
            return Ok(run.snapshot(run_id));
        }
        start_preview(run).inspect_err(|e| {
            run.status = RunStatus::Failed;
            run.error = Some(e.clone());
        })?;
        run.status = RunStatus::Live;
        Ok(run.snapshot(run_id))
    }

    pub fn snapshot(&self, run_id: &str) -> Option<RunSnapshot> {
        self.runs
            .lock()
            .ok()?
            .get(run_id)
            .map(|r| r.snapshot(run_id))
    }

    pub fn preview_action(&self, run_id: &str, action: &BrowserAction) -> Result<(), String> {
        let runs = self.runs.lock().map_err(|_| "run registry poisoned")?;
        let run = runs.get(run_id).ok_or("unknown run")?;
        let (sup, sid) = preview_handle(run)?;
        sup.action(&sid, action).map_err(|e| e.to_string())
    }

    pub fn capture(&self, run_id: &str) -> Result<BrowserEvidence, String> {
        let runs = self.runs.lock().map_err(|_| "run registry poisoned")?;
        let run = runs.get(run_id).ok_or("unknown run")?;
        let (sup, sid) = preview_handle(run)?;
        sup.capture(&sid).map_err(|e| e.to_string())
    }

    pub fn teardown(&self, run_id: &str) -> Result<(), String> {
        let mut runs = self.runs.lock().map_err(|_| "run registry poisoned")?;
        if let Some(mut run) = runs.remove(run_id) {
            if let (Some(sup), Some(sid)) = (run.preview.take(), run.preview_session.take()) {
                let _ = sup.teardown(&sid);
            }
        }
        Ok(())
    }
}

fn preview_handle(run: &LiveRun) -> Result<(&PreviewSupervisor, String), String> {
    match (&run.preview, &run.preview_session) {
        (Some(s), Some(id)) => Ok((s, id.clone())),
        _ => Err("preview is not running".into()),
    }
}

fn evidence_ids(ev: &BrowserEvidence) -> Vec<String> {
    ev.items
        .iter()
        .filter_map(|item| match item {
            rex_preview::Evidence::Screenshot { id, .. } => Some(id.clone()),
            rex_preview::Evidence::DomSnapshot { id, .. } => Some(id.clone()),
            rex_preview::Evidence::Accessibility { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect()
}

fn start_preview(run: &mut LiveRun) -> Result<(), String> {
    let supervisor = PreviewSupervisor::new(&run.workspace).map_err(|e| e.to_string())?;
    let summary = supervisor
        .start(Path::new("."))
        .map_err(|e| e.to_string())?;
    let sid = summary.id.clone();
    run.preview_url = summary.url.clone();
    run.preview_framework = format!("{:?}", summary.framework).to_lowercase();

    // Iteration 1: desktop capture only. It is preserved as rejected because
    // the mobile gate has no evidence yet.
    let it1 = supervisor
        .begin_iteration(&sid)
        .map_err(|e| e.to_string())?;
    supervisor
        .action(
            &sid,
            &BrowserAction::SetViewport {
                width: DESKTOP.0,
                height: DESKTOP.1,
                scale: DESKTOP.2,
            },
        )
        .map_err(|e| e.to_string())?;
    let ev1 = supervisor.capture(&sid).map_err(|e| e.to_string())?;
    run.desktop_shot = ev1.screenshot_data_url.clone();
    supervisor
        .record_iteration(
            &sid,
            IterationReceipt {
                iteration: it1,
                accepted: false,
                diff_id: "tool-receipt-live-model-write".into(),
                evidence_ids: evidence_ids(&ev1),
                failed_gates: vec![ProductionGate::MobileViewport],
                reason: "Desktop capture only; mobile viewport evidence still missing.".into(),
            },
        )
        .map_err(|e| e.to_string())?;

    // Iteration 2: mobile capture then restore desktop. Accepted only after
    // both viewports exist with no console or network failure evidence.
    let it2 = supervisor
        .begin_iteration(&sid)
        .map_err(|e| e.to_string())?;
    supervisor
        .action(
            &sid,
            &BrowserAction::SetViewport {
                width: MOBILE.0,
                height: MOBILE.1,
                scale: MOBILE.2,
            },
        )
        .map_err(|e| e.to_string())?;
    let evm = supervisor.capture(&sid).map_err(|e| e.to_string())?;
    run.mobile_shot = evm.screenshot_data_url.clone();
    supervisor
        .action(
            &sid,
            &BrowserAction::SetViewport {
                width: DESKTOP.0,
                height: DESKTOP.1,
                scale: DESKTOP.2,
            },
        )
        .map_err(|e| e.to_string())?;
    let ev2 = supervisor.capture(&sid).map_err(|e| e.to_string())?;
    run.desktop_shot = ev2.screenshot_data_url.clone().or(run.desktop_shot.take());
    let failed = measured_failures(&ev2);
    supervisor
        .record_iteration(
            &sid,
            IterationReceipt {
                iteration: it2,
                accepted: failed.is_empty(),
                diff_id: "tool-receipt-live-model-write".into(),
                evidence_ids: evidence_ids(&ev2),
                failed_gates: failed,
                reason: if ev2.items.iter().any(|i| matches!(i, rex_preview::Evidence::Console { level: rex_preview::ConsoleLevel::Error, .. })) || ev2.items.iter().any(|i| matches!(i, rex_preview::Evidence::NetworkFailure { .. })) {
                    "Console or network failure evidence present; iteration stays rejected."
                } else {
                    "Mobile 390x844 and desktop 1280x800 captures landed with no console or network failure evidence."
                }
                .into(),
            },
        )
        .map_err(|e| e.to_string())?;
    run.preview_iteration = it2;
    run.receipts = supervisor
        .production_report(&sid)
        .map(|r| r.receipts)
        .unwrap_or_default();
    run.preview = Some(supervisor);
    run.preview_session = Some(sid);
    Ok(())
}

fn measured_failures(ev: &BrowserEvidence) -> Vec<ProductionGate> {
    let mut out = Vec::new();
    if ev.items.iter().any(|i| {
        matches!(
            i,
            rex_preview::Evidence::Console {
                level: rex_preview::ConsoleLevel::Error,
                ..
            }
        )
    }) {
        out.push(ProductionGate::NoConsoleErrors);
    }
    if ev
        .items
        .iter()
        .any(|i| matches!(i, rex_preview::Evidence::NetworkFailure { .. }))
    {
        out.push(ProductionGate::NoFailedRequests);
    }
    out
}

pub fn pick_flash_lite(ids: &[&str]) -> Option<String> {
    ids.iter()
        .find(|id| **id == "gemini-3.5-flash-lite")
        .map(|s| s.to_string())
        .or_else(|| {
            ids.iter()
                .find(|id| id.contains("flash-lite") && !id.contains("preview"))
                .map(|s| s.to_string())
        })
        .or_else(|| {
            ids.iter()
                .find(|id| id.contains("flash-lite"))
                .map(|s| s.to_string())
        })
}

fn tool_definitions() -> Value {
    json!([{"functionDeclarations":[
        {"name":"create_file","description":"Create a file inside the selected workspace. Requires trusted approval.","parameters":{"type":"OBJECT","properties":{"path":{"type":"STRING"},"content":{"type":"STRING"},"overwrite":{"type":"BOOLEAN"}},"required":["path","content","overwrite"]}},
        {"name":"read_file","description":"Read a file inside the selected workspace.","parameters":{"type":"OBJECT","properties":{"path":{"type":"STRING"}},"required":["path"]}}
    ]}])
}

fn generate<T: Transport>(
    transport: &T,
    key: &str,
    model: &str,
    task: &str,
) -> Result<String, String> {
    let url =
        format!("https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent");
    let prompt = format!(
        "You are REX, an agent that builds software inside a bounded workspace. \
         The user asked:\n\n{task}\n\n\
         Deliver the result as one self-contained index.html with inline CSS and JavaScript \
         and no external assets. Call create_file exactly once with path \"index.html\" and \
         overwrite=true. Do not explain outside the tool call."
    );
    let body = json!({
        "contents":[{"role":"user","parts":[{"text":prompt}]}],
        "tools":tool_definitions(),
        "toolConfig":{"functionCallingConfig":{"mode":"AUTO"}},
        "generationConfig":{"temperature":0.2,"maxOutputTokens":8192}
    })
    .to_string();
    let (status, text) = transport
        .post(
            &url,
            &[
                ("x-goog-api-key".into(), key.into()),
                ("content-type".into(), "application/json".into()),
            ],
            &body,
        )
        .map_err(|e| e.to_string())?;
    if status / 100 != 2 {
        return Err(format!("provider HTTP {status}"));
    }
    // Decode once here so a malformed body fails before any run is recorded.
    decode_turn(ProviderProtocol::Gemini, &text)?;
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ProviderError;
    use crate::secrets::MemorySecretStore;

    struct Script;
    impl Transport for Script {
        fn get(
            &self,
            _url: &str,
            _headers: &[(String, String)],
        ) -> Result<(u16, String), ProviderError> {
            Ok((200, r#"{"models":[{"name":"models/gemini-3.5-flash-lite","displayName":"Gemini 3.5 Flash Lite","supportedGenerationMethods":["generateContent"]}]}"#.into()))
        }
        fn post(
            &self,
            _url: &str,
            _headers: &[(String, String)],
            _body: &str,
        ) -> Result<(u16, String), ProviderError> {
            Ok((200, r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"create_file","args":{"path":"index.html","content":"<!doctype html><html><body><button id=\"switch\">Switch to weekly</button><script>document.getElementById(\"switch\").onclick=()=>{document.body.dataset.mode=\"weekly\"}</script></body></html>","overwrite":true}}}]},"finishReason":"STOP"}]}"#.into()))
        }
    }

    struct ScriptNoCall;
    impl Transport for ScriptNoCall {
        fn get(
            &self,
            _url: &str,
            _headers: &[(String, String)],
        ) -> Result<(u16, String), ProviderError> {
            Ok((200, r#"{"models":[{"name":"models/gemini-3.5-flash-lite","displayName":"Gemini 3.5 Flash Lite","supportedGenerationMethods":["generateContent"]}]}"#.into()))
        }
        fn post(
            &self,
            _url: &str,
            _headers: &[(String, String)],
            _body: &str,
        ) -> Result<(u16, String), ProviderError> {
            Ok((200, r#"{"candidates":[{"content":{"parts":[{"text":"no tool needed"}]},"finishReason":"STOP"}]}"#.into()))
        }
    }

    fn service(root: &Path) -> LiveRunService<MemorySecretStore, Script> {
        let store = MemorySecretStore::new();
        store.set_key("gemini", "test-key").unwrap();
        LiveRunService::new(ProviderService::new(store, Script), root.join("runs"))
    }

    #[test]
    fn begin_pauses_for_trusted_approval() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = service(tmp.path());
        let snap = svc.begin("build the aeris climate ui", "gemini").unwrap();
        assert_eq!(snap.status, RunStatus::AwaitingApproval);
        assert_eq!(snap.model, "gemini-3.5-flash-lite");
        assert_eq!(snap.catalog_count, 1);
        let call = snap.approval.expect("write must pause");
        assert_eq!(call.tool, "create_file");
        assert!(call.approval_required);
        assert!(snap.result.is_none());
        assert!(snap.preview.is_none());
        // nothing executes before the trusted decision
        assert!(!tmp
            .path()
            .join("runs")
            .join(&snap.id)
            .join("index.html")
            .exists());
        svc.teardown(&snap.id).unwrap();
    }

    #[test]
    fn deny_stops_without_a_write() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = service(tmp.path());
        let snap = svc.begin("build the aeris climate ui", "gemini").unwrap();
        let after = svc.decide(&snap.id, false).unwrap();
        assert_eq!(after.status, RunStatus::Denied);
        assert!(!tmp
            .path()
            .join("runs")
            .join(&snap.id)
            .join("index.html")
            .exists());
        assert!(after.preview.is_none());
        svc.teardown(&snap.id).unwrap();
    }

    #[test]
    fn approve_executes_and_opens_the_native_preview() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = service(tmp.path());
        let snap = svc.begin("build the aeris climate ui", "gemini").unwrap();
        let after = svc.decide(&snap.id, true).unwrap();
        assert_eq!(after.status, RunStatus::Live);
        let written = tmp.path().join("runs").join(&snap.id).join("index.html");
        assert!(written.exists());
        let result = after.result.expect("receipt");
        assert!(result.ok);
        assert!(result.receipt.bytes_written > 0);
        let preview = after.preview.expect("preview");
        assert!(preview.url.starts_with("http://127.0.0.1:"));
        assert_eq!(preview.receipts.len(), 2);
        assert!(
            !preview.receipts[0].accepted,
            "first iteration stays rejected"
        );
        assert_eq!(
            preview.receipts[0].failed_gates,
            vec![ProductionGate::MobileViewport]
        );
        assert!(preview.receipts[1].accepted);
        assert!(preview.desktop_shot.is_some(), "desktop capture present");
        assert!(preview.mobile_shot.is_some(), "mobile capture present");
        svc.teardown(&snap.id).unwrap();
    }

    #[test]
    fn begin_fails_truthfully_without_a_tool_call() {
        let tmp = tempfile::tempdir().unwrap();
        let store = MemorySecretStore::new();
        store.set_key("gemini", "test-key").unwrap();
        let svc: LiveRunService<MemorySecretStore, ScriptNoCall> = LiveRunService::new(
            ProviderService::new(store, ScriptNoCall),
            tmp.path().join("runs"),
        );
        let snap = svc.begin("say hello", "gemini").unwrap();
        assert_eq!(snap.status, RunStatus::Failed);
        assert!(snap.error.is_some());
        svc.teardown(&snap.id).unwrap();
    }

    #[test]
    fn begin_rejects_unsupported_provider_and_empty_task() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = service(tmp.path());
        assert!(svc.begin("build ui", "anthropic").is_err());
        assert!(svc.begin("   ", "gemini").is_err());
    }
}
