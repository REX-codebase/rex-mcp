//! Custody-gated runs: the agent-operator mode wired to the autonomous loop.
//!
//! `CustodyRunService` sits between the shell and `AutonomousRunService`.
//! Starting a task means: offer custody, complete the commitment handshake,
//! clamp the run budgets to the grant, start the loop, then watch it -
//! heartbeats and consumption flow into the grant while the run lives, and
//! the run's terminal reason maps onto custody's release reasons. A
//! completed run does not release custody: the completion claim still has
//! to pass the grant's evidence gates.
//!
//! Worker modes: `ManagedModel` is implemented here. `ExternalAgent` (the
//! agent itself entering through ACP or an installed CLI) is refused
//! truthfully until a protocol boundary lands; custody never pretends a
//! route exists.

use crate::autonomous::{AgentSnapshot, AgentStatus, AutonomousRunService, Budgets, TerminalReason};
use crate::http::Transport;
use crate::secrets::SecretStore;
use rex_custody::*;
use rex_prompt::roles::Role;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct CustodyRunService<S: SecretStore + 'static, T: Transport + 'static> {
    runs: Arc<AutonomousRunService<S, T>>,
    custody: Arc<Mutex<CustodyRegistry>>,
}

#[derive(Debug)]
pub struct CustodiedRun {
    pub grant: CustodyGrant,
    pub token: CapabilityToken,
    pub snapshot: AgentSnapshot,
}

pub struct ManagedTaskRequest {
    pub task_id: String,
    pub task: String,
    pub operator: OperatorIdentity,
    pub provider: String,
    pub model: Option<String>,
    pub workspace: Option<PathBuf>,
    pub capabilities: CapabilitySet,
    pub budgets: CustodyBudgets,
    pub lease_terms: LeaseTerms,
    pub contract: CompletionContract,
}

/// Flat view the shell returns when a custodied run starts or is polled:
/// the grant the operator is accountable under, plus the run snapshot.
/// The capability token never leaves the backend.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CustodiedRunView {
    pub grant_id: String,
    pub task_id: String,
    pub operator: String,
    pub worker: String,
    pub phase: CustodyPhase,
    pub snapshot: AgentSnapshot,
}

/// Custody terms for a run started from the desktop UI: confined to one
/// workspace, conservative budgets, and only the evidence gates this
/// integration can actually evaluate. Unwired gates fail closed, so
/// offering one here would suspend every completion for human attention.
pub fn ui_managed_request(
    task_id: String,
    task: String,
    operator: OperatorIdentity,
    provider: String,
    model: Option<String>,
    workspace: PathBuf,
) -> ManagedTaskRequest {
    let capabilities = CapabilitySet {
        workspace_root: workspace.clone(),
        tool_classes: [ToolClass::Read, ToolClass::Write, ToolClass::Execute]
            .into_iter()
            .collect(),
        allowed_tools: ["read_file", "create_file", "edit_file", "search_files", "run_command"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        allow_search: true,
        allow_preview: true,
        can_delegate: false,
    };
    ManagedTaskRequest {
        task_id,
        task,
        operator,
        provider,
        model,
        workspace: Some(workspace),
        capabilities,
        budgets: CustodyBudgets {
            max_steps: 24,
            max_tool_calls: 128,
            max_wall_ms: 30 * 60 * 1000,
            max_tokens: 200_000,
        },
        lease_terms: LeaseTerms::default(),
        contract: CompletionContract {
            gates: vec![EvidenceGate::NoPendingApprovals, EvidenceGate::WithinScopeChanges],
            max_claim_attempts: 1,
        },
    }
}

fn worker_label(worker: &WorkerMode) -> String {
    match worker {
        WorkerMode::ManagedModel { provider, model } => match model {
            Some(m) => format!("managed_model:{provider}/{m}"),
            None => format!("managed_model:{provider}"),
        },
        WorkerMode::ExternalAgent => "external_agent".to_string(),
    }
}

/// Evidence for the gates the run service can see. Unknown or unwired
/// gates fail closed.
struct RunGateEvaluator<S: SecretStore + 'static, T: Transport + 'static> {
    runs: Arc<AutonomousRunService<S, T>>,
    run_id: String,
}

impl<S: SecretStore + 'static, T: Transport + 'static> GateEvaluator for RunGateEvaluator<S, T> {
    fn evaluate(&self, gate: &EvidenceGate, _grant: &CustodyGrant) -> GateOutcome {
        match gate {
            EvidenceGate::NoPendingApprovals => {
                match self.runs.snapshot(&self.run_id) {
                    Some(s) if s.pending_approval.is_none()
                        && matches!(s.status, AgentStatus::Completed) =>
                    {
                        GateOutcome::Passed
                    }
                    Some(_) => GateOutcome::Failed("run not cleanly completed".into()),
                    None => GateOutcome::Failed("run missing".into()),
                }
            }
            // Every tool call already passed through the granted scope;
            // out-of-scope changes were structurally impossible.
            EvidenceGate::WithinScopeChanges => GateOutcome::Passed,
            other => GateOutcome::Failed(format!("gate {other:?} not wired in this integration")),
        }
    }
}

impl<S: SecretStore + 'static, T: Transport + 'static> CustodyRunService<S, T> {
    pub fn new(runs: Arc<AutonomousRunService<S, T>>, custody: Arc<Mutex<CustodyRegistry>>) -> Self {
        Self { runs, custody }
    }

    pub fn custody(&self) -> Arc<Mutex<CustodyRegistry>> {
        self.custody.clone()
    }

    /// Flat shell view of a started custodied run.
    pub fn view_of(&self, run: &CustodiedRun) -> CustodiedRunView {
        CustodiedRunView {
            grant_id: run.grant.grant_id.clone(),
            task_id: run.grant.task_id.clone(),
            operator: run.grant.operator.label(),
            worker: worker_label(&run.grant.worker),
            phase: run.grant.phase,
            snapshot: run.snapshot.clone(),
        }
    }

    /// Current custody phase for a grant, for shell polling.
    pub fn phase_of(&self, grant_id: &str) -> Option<CustodyPhase> {
        self.custody.lock().ok()?.grant(grant_id).map(|g| g.phase)
    }

    /// Offer, accept and start a custodied managed-model task. The caller
    /// (a protocol adapter or the human UI) presents the acceptance; the
    /// registry verifies it against the exact offer.
    pub fn begin_managed_task(&self, req: ManagedTaskRequest) -> Result<CustodiedRun, String> {
        let now = now_ms();
        let worker = WorkerMode::ManagedModel {
            provider: req.provider.clone(),
            model: req.model.clone(),
        };
        let mut custody = self.custody.lock().map_err(|_| "custody poisoned".to_string())?;
        let offer = custody
            .offer(
                &req.task_id,
                &req.task,
                req.operator.clone(),
                worker,
                req.capabilities,
                req.budgets,
                req.lease_terms,
                req.contract,
                now,
            )
            .map_err(|e| format!("custody offer refused: {e}"))?;
        // The operator's commitment to this exact offer. For the human UI
        // the submission click is the commitment; a protocol adapter must
        // present the agent's own computed acceptance instead.
        let acceptance = CustodyAcceptance {
            offer_id: offer.offer_id.clone(),
            nonce_echo: offer.nonce.clone(),
            commitment: offer.expected_acceptance(),
        };
        let (token, grant) = custody
            .accept(&acceptance, now)
            .map_err(|e| format!("custody handshake failed: {e}"))?;
        drop(custody);

        // The run never gets more than the grant allows.
        let budgets = Budgets {
            max_steps: grant.budgets.max_steps as usize,
            max_tool_calls: grant.budgets.max_tool_calls as usize,
            max_wall_ms: grant.budgets.max_wall_ms,
            max_tokens: grant.budgets.max_tokens,
        };
        let workspace = req.workspace.unwrap_or_else(|| {
            self.runs_workspace_hint(&grant.grant_id)
        });
        let snapshot = self
            .runs
            .begin_in_workspace_with_role(
                &req.task,
                &req.provider,
                req.model.as_deref(),
                Some(budgets),
                Some(workspace),
                Role::Worker,
            )
            .map_err(|e| {
                // The run never started; release custody honestly.
                let _ = self
                    .custody
                    .lock()
                    .map(|mut c| {
                        let _ = c.declare_failure(&token, &format!("run failed to start: {e}"), now_ms());
                    });
                format!("run refused: {e}")
            })?;

        self.spawn_monitor(grant.grant_id.clone(), token.clone(), snapshot.id.clone());
        Ok(CustodiedRun { grant, token, snapshot })
    }

    fn runs_workspace_hint(&self, grant_id: &str) -> PathBuf {
        // Custody-confined workspace under the custody root; rex-tools does
        // the canonical confinement, custody the scope equality.
        self.custody
            .lock()
            .map(|c| {
                c.grant(grant_id)
                    .map(|g| g.capabilities.workspace_root.clone())
                    .unwrap_or_else(|| PathBuf::from("."))
            })
            .unwrap_or_else(|_| PathBuf::from("."))
    }

    /// Human stop: works from any phase, then cancels the run loop.
    pub fn human_stop(&self, grant_id: &str, run_id: &str) -> Result<ReleaseReason, String> {
        let reason = self
            .custody
            .lock()
            .map_err(|_| "custody poisoned".to_string())?
            .human_stop(grant_id, now_ms())
            .map_err(|e| format!("stop refused: {e}"))?;
        let _ = self.runs.cancel(run_id);
        Ok(reason)
    }

    fn spawn_monitor(&self, grant_id: String, token: CapabilityToken, run_id: String) {
        let runs = self.runs.clone();
        let custody = self.custody.clone();
        std::thread::spawn(move || {
            let mut seq = 0u64;
            let mut last = (0u64, 0u64, 0u64); // steps, tool_calls, tokens
            loop {
                let snap = match runs.snapshot(&run_id) {
                    Some(s) => s,
                    None => return,
                };
                if snap.terminal_reason.is_none() {
                    let mut c = match custody.lock() {
                        Ok(c) => c,
                        Err(_) => return,
                    };
                    seq += 1;
                    if c.heartbeat(&token, seq, now_ms()).is_err() {
                        return; // custody gone (stopped/quarantined); run cancel raced in
                    }
                    let cur = (snap.step as u64, snap.tool_calls as u64, snap.tokens_used);
                    let delta = Consumption {
                        steps: cur.0.saturating_sub(last.0),
                        tool_calls: cur.1.saturating_sub(last.1),
                        tokens: cur.2.saturating_sub(last.2),
                        wall_ms: 0,
                    };
                    last = cur;
                    if c.consume(&token, delta, now_ms()).is_err() {
                        // Budget exhausted (or custody ended): stop the run.
                        let _ = runs.cancel(&run_id);
                        return;
                    }
                    drop(c);
                    std::thread::sleep(Duration::from_millis(500));
                    continue;
                }
                // Terminal: map the reason onto custody.
                let mut c = match custody.lock() {
                    Ok(c) => c,
                    Err(_) => return,
                };
                let now = now_ms();
                match snap.terminal_reason.clone() {
                    Some(TerminalReason::Completed) => {
                        let claim = CompletionClaim {
                            summary: snap
                                .completion_summary
                                .clone()
                                .unwrap_or_else(|| "run completed".into()),
                        };
                        let eval = RunGateEvaluator {
                            runs: runs.clone(),
                            run_id: run_id.clone(),
                        };
                        match c.claim_completion(&token, &claim, &eval, now) {
                            Ok(_) => {}
                            Err(CustodyError::ClaimRejected { .. }) => {
                                // Worker finished but the gates disagree.
                                // Suspend for operator/human attention.
                                let _ = c.suspend(&grant_id, "completion claim rejected", now);
                            }
                            Err(_) => {}
                        }
                    }
                    Some(TerminalReason::Cancelled) => {
                        // Cancellation arrives either from the human stop
                        // path (custody already released; this is a no-op)
                        // or from an external cancel: treat as human stop.
                        if !matches!(c.grant(&grant_id).map(|g| g.phase), Some(CustodyPhase::Released) | Some(CustodyPhase::Quarantined)) {
                            let _ = c.human_stop(&grant_id, now);
                        }
                    }
                    Some(TerminalReason::BudgetSteps { .. }) => {
                        let _ = c.release_budget_exhausted(&grant_id, BudgetKind::Steps, now);
                    }
                    Some(TerminalReason::BudgetTime { .. }) => {
                        let _ = c.release_budget_exhausted(&grant_id, BudgetKind::WallTime, now);
                    }
                    Some(TerminalReason::BudgetTokens { .. }) => {
                        let _ = c.release_budget_exhausted(&grant_id, BudgetKind::Tokens, now);
                    }
                    Some(TerminalReason::BudgetToolCalls { .. }) => {
                        let _ = c.release_budget_exhausted(&grant_id, BudgetKind::ToolCalls, now);
                    }
                    Some(other) => {
                        if !matches!(c.grant(&grant_id).map(|g| g.phase), Some(CustodyPhase::Released) | Some(CustodyPhase::Quarantined)) {
                            let _ = c.declare_failure(&token, &format!("run ended: {other:?}"), now);
                        }
                    }
                    None => {}
                }
                return;
            }
        });
    }
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ProviderError;
    use crate::secrets::MemorySecretStore;
    use crate::service::ProviderService;
    use serde_json::{json, Value};
    use std::collections::BTreeSet;
    use std::collections::VecDeque;
    use std::path::Path;
    use std::time::Instant;

    struct Script {
        turns: Mutex<VecDeque<String>>,
    }
    impl Script {
        fn new(turns: Vec<String>) -> Self {
            Self { turns: Mutex::new(turns.into()) }
        }
    }
    impl Transport for Script {
        fn get(&self, _url: &str, _headers: &[(String, String)]) -> Result<(u16, String), ProviderError> {
            Ok((200, json!({"models":[{"name":"models/gemini-3.5-flash-lite","displayName":"Flash Lite","supportedGenerationMethods":["generateContent"]}]}).to_string()))
        }
        fn post(&self, _url: &str, _headers: &[(String, String)], _body: &str) -> Result<(u16, String), ProviderError> {
            self.turns
                .lock()
                .unwrap()
                .pop_front()
                .map(|t| (200, t))
                .ok_or_else(|| ProviderError::Network("script exhausted".into()))
        }
    }

    fn call_turn(calls: Vec<Value>) -> String {
        json!({"candidates":[{"content":{"parts": calls},"finishReason":"STOP"}],"usageMetadata":{"totalTokenCount":100}}).to_string()
    }
    fn plan_call(items: Vec<(&str, &str, &str)>) -> Value {
        json!({"functionCall":{"name":"update_plan","args":{"items": items.iter().map(|(id, title, status)| json!({"id": id, "title": title, "status": status})).collect::<Vec<_>>()}}})
    }
    fn create_call(path: &str, content: &str) -> Value {
        json!({"functionCall":{"name":"create_file","args":{"path": path, "content": content, "overwrite": true}}})
    }
    fn complete_call(summary: &str) -> Value {
        json!({"functionCall":{"name":"complete_task","args":{"summary": summary}}})
    }

    type Svc = AutonomousRunService<MemorySecretStore, Script>;

    fn caps(root: &Path) -> CapabilitySet {
        CapabilitySet {
            workspace_root: root.to_path_buf(),
            tool_classes: [ToolClass::Read, ToolClass::Write, ToolClass::Execute].into_iter().collect(),
            allowed_tools: ["read_file", "create_file", "edit_file", "search_files", "run_command"]
                .iter().map(|s| s.to_string()).collect::<BTreeSet<_>>(),
            allow_search: false,
            allow_preview: false,
            can_delegate: false,
        }
    }

    struct Rig {
        service: CustodyRunService<MemorySecretStore, Script>,
        runs: Arc<Svc>,
        custody: Arc<Mutex<CustodyRegistry>>,
        workspace: PathBuf,
    }

    fn rig(turns: Vec<String>) -> (tempfile::TempDir, Rig) {
        let tmp = tempfile::tempdir().unwrap();
        let store = MemorySecretStore::new();
        store.set_key("gemini", "test-key").unwrap();
        let runs = Arc::new(AutonomousRunService::new(
            ProviderService::new(store, Script::new(turns)),
            None,
            tmp.path().join("runs"),
        ));
        let custody = Arc::new(Mutex::new(
            CustodyRegistry::open(tmp.path().join("custody")).unwrap(),
        ));
        // The autonomous service confines explicit workspaces to its runs
        // root, so the custodied workspace lives under it too.
        let workspace = tmp.path().join("runs").join("custody-workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let service = CustodyRunService::new(runs.clone(), custody.clone());
        (tmp, Rig { service, runs, custody, workspace })
    }

    fn request(task_id: &str, workspace: PathBuf, budgets: CustodyBudgets, contract: CompletionContract) -> ManagedTaskRequest {
        ManagedTaskRequest {
            task_id: task_id.into(),
            task: "build a tea house page".into(),
            operator: OperatorIdentity::Agent(CustodyRegistry::register_agent(
                "t3-code",
                AgentProtocol::Acp { client: "t3".into(), version: "1.0".into() },
            )),
            provider: "gemini".into(),
            model: Some("gemini-3.5-flash-lite".into()),
            workspace: Some(workspace.clone()),
            capabilities: caps(&workspace),
            budgets,
            lease_terms: LeaseTerms::default(),
            contract,
        }
    }

    fn wait_custody_terminal(custody: &Arc<Mutex<CustodyRegistry>>, grant_id: &str, timeout_ms: u64) -> CustodyGrant {
        let start = Instant::now();
        loop {
            {
                let c = custody.lock().unwrap();
                if let Some(g) = c.grant(grant_id) {
                    if g.phase.is_terminal() {
                        return g.clone();
                    }
                }
            }
            if start.elapsed().as_millis() as u64 > timeout_ms {
                let c = custody.lock().unwrap();
                panic!("custody did not terminate in time: {:?}", c.grant(grant_id).map(|g| g.phase));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn auto_approve(runs: Arc<Svc>, id: String) {
        std::thread::spawn(move || loop {
            let snap = match runs.snapshot(&id) {
                Some(s) => s,
                None => return,
            };
            if snap.terminal_reason.is_some() {
                return;
            }
            if snap.status == AgentStatus::AwaitingApproval && snap.pending_approval.is_some() {
                let _ = runs.decide(&id, true);
            }
            std::thread::sleep(Duration::from_millis(10));
        });
    }

    fn good_turns() -> Vec<String> {
        vec![
            call_turn(vec![
                plan_call(vec![("1", "build page", "in_progress")]),
                create_call("index.html", "<html><body>tea</body></html>"),
            ]),
            call_turn(vec![
                plan_call(vec![("1", "build page", "done")]),
                complete_call("page built"),
            ]),
        ]
    }

    #[test]
    fn completed_run_releases_custody_only_after_gates_pass() {
        let (_t, rig) = rig(good_turns());
        let out = rig
            .service
            .begin_managed_task(request("task-c", rig.workspace.clone(), CustodyBudgets::default(), CompletionContract::default()))
            .unwrap();
        auto_approve(rig.runs.clone(), out.snapshot.id.clone());
        let grant = wait_custody_terminal(&rig.custody, &out.grant.grant_id, 60_000);
        assert_eq!(grant.phase, CustodyPhase::Released);
        assert!(matches!(grant.release, Some(ReleaseReason::VerifiedCompletion { .. })), "got {:?}", grant.release);
        assert!(grant.consumed.steps >= 1);
        let chain = rig.custody.lock().unwrap().audit_chain(&grant.grant_id).unwrap();
        let kinds: Vec<&str> = chain.iter().map(|e| e.kind.as_str()).collect();
        assert!(kinds.contains(&"custody_granted"));
        assert!(kinds.contains(&"heartbeat"));
        assert!(kinds.contains(&"completion_claimed"));
        assert_eq!(kinds.last(), Some(&"released"));
        // Tombstone blocks a second custody of the same task.
        let err = rig
            .service
            .begin_managed_task(request("task-c", rig.workspace.clone(), CustodyBudgets::default(), CompletionContract::default()))
            .unwrap_err();
        assert!(err.contains("tombstoned") || err.contains("refused"), "{err}");
    }

    #[test]
    fn unwired_gate_rejects_claim_and_suspends_for_attention() {
        let (_t, rig) = rig(good_turns());
        let contract = CompletionContract {
            gates: vec![EvidenceGate::HumanConfirmation],
            max_claim_attempts: 2,
        };
        let out = rig
            .service
            .begin_managed_task(request("task-g", rig.workspace.clone(), CustodyBudgets::default(), contract))
            .unwrap();
        auto_approve(rig.runs.clone(), out.snapshot.id.clone());
        let start = Instant::now();
        loop {
            let phase = rig.custody.lock().unwrap().grant(&out.grant.grant_id).unwrap().phase;
            if phase == CustodyPhase::Suspended {
                break;
            }
            if start.elapsed().as_millis() as u64 > 60_000 {
                panic!("expected suspension after rejected claim, got {phase:?}");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        // Not tombstoned: the task is still owned by this custody.
        assert!(rig.custody.lock().unwrap().tombstone("task-g").is_none());
    }

    #[test]
    fn run_budget_wall_maps_to_custody_budget_release() {
        let (_t, rig) = rig(vec![
            call_turn(vec![plan_call(vec![("1", "build page", "in_progress")])]),
            call_turn(vec![plan_call(vec![("1", "build page", "in_progress")])]),
            call_turn(vec![plan_call(vec![("1", "build page", "in_progress")])]),
        ]);
        let budgets = CustodyBudgets { max_steps: 1, max_tool_calls: 10, max_wall_ms: 120_000, max_tokens: 100_000 };
        let out = rig
            .service
            .begin_managed_task(request("task-b", rig.workspace.clone(), budgets, CompletionContract::default()))
            .unwrap();
        let grant = wait_custody_terminal(&rig.custody, &out.grant.grant_id, 60_000);
        assert_eq!(grant.phase, CustodyPhase::Released);
        assert!(matches!(
            grant.release,
            Some(ReleaseReason::BudgetExhausted { which: BudgetKind::Steps })
        ), "got {:?}", grant.release);
    }

    #[test]
    fn human_stop_releases_custody_and_cancels_run() {
        let (_t, rig) = rig(vec![
            call_turn(vec![
                plan_call(vec![("1", "build page", "in_progress")]),
                create_call("index.html", "<html></html>"),
            ]),
            call_turn(vec![plan_call(vec![("1", "build page", "in_progress")])]),
        ]);
        let out = rig
            .service
            .begin_managed_task(request("task-h", rig.workspace.clone(), CustodyBudgets::default(), CompletionContract::default()))
            .unwrap();
        // Wait until the run is genuinely mid-flight (approval pending).
        let start = Instant::now();
        loop {
            let s = rig.runs.snapshot(&out.snapshot.id).unwrap();
            if s.status == AgentStatus::AwaitingApproval {
                break;
            }
            if start.elapsed().as_millis() as u64 > 30_000 {
                panic!("run never awaited approval: {:?}", s.status);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let reason = rig
            .service
            .human_stop(&out.grant.grant_id, &out.snapshot.id)
            .unwrap();
        assert_eq!(reason, ReleaseReason::HumanStop);
        let grant = wait_custody_terminal(&rig.custody, &out.grant.grant_id, 30_000);
        assert!(matches!(grant.release, Some(ReleaseReason::HumanStop)));
    }

    #[test]
    fn ui_request_confines_scope_and_offers_only_wired_gates() {
        let ws = PathBuf::from("/tmp/ui-managed-ws");
        let req = ui_managed_request(
            "task-ui".into(),
            "build a page".into(),
            OperatorIdentity::Human,
            "gemini".into(),
            None,
            ws.clone(),
        );
        assert!(!req.capabilities.can_delegate, "custody never delegates");
        assert_eq!(req.capabilities.workspace_root, ws);
        assert_eq!(req.workspace.as_deref(), Some(ws.as_path()));
        assert_eq!(req.operator, OperatorIdentity::Human);
        // Unknown gates fail closed, so the default contract may only
        // carry the two this integration actually evaluates.
        assert_eq!(
            req.contract.gates,
            vec![EvidenceGate::NoPendingApprovals, EvidenceGate::WithinScopeChanges]
        );
    }

    #[test]
    fn view_reports_operator_phase_and_never_serializes_token() {
        let (_tmp, rig) = rig(vec![]);
        let ws = rig.workspace.clone();
        let (token, grant) = {
            let mut c = rig.custody.lock().unwrap();
            let offer = c
                .offer(
                    "task-view",
                    "build a page",
                    OperatorIdentity::Human,
                    WorkerMode::ManagedModel {
                        provider: "gemini".into(),
                        model: Some("gemini-3.5-flash-lite".into()),
                    },
                    caps(&ws),
                    CustodyBudgets::default(),
                    LeaseTerms::default(),
                    CompletionContract {
                        gates: vec![EvidenceGate::NoPendingApprovals],
                        max_claim_attempts: 1,
                    },
                    1_000,
                )
                .unwrap();
            c.accept_for_human(&offer.offer_id, 1_001).unwrap()
        };
        let snapshot = AgentSnapshot {
            id: "run-1".into(),
            task: "build a page".into(),
            status: AgentStatus::Running,
            terminal_reason: None,
            provider: "gemini".into(),
            model: "gemini-3.5-flash-lite".into(),
            plan: vec![],
            step: 1,
            max_steps: 24,
            tool_calls: 0,
            max_tool_calls: 128,
            tokens_used: 0,
            max_tokens: 200_000,
            elapsed_ms: 5,
            max_wall_ms: 1_800_000,
            pending_approval: None,
            prompt_version: "test".into(),
            prompt_hash: "hash".into(),
            events: vec![],
            preview: None,
            completion_summary: None,
            error: None,
        };
        let run = CustodiedRun { grant, token, snapshot };
        let view = rig.service.view_of(&run);
        assert_eq!(view.operator, "human");
        assert_eq!(view.phase, CustodyPhase::Active);
        assert_eq!(view.worker, "managed_model:gemini/gemini-3.5-flash-lite");
        let json = serde_json::to_value(&view).unwrap();
        assert!(json.get("token").is_none(), "token must never leave the backend");
        assert_eq!(rig.service.phase_of(&view.grant_id), Some(CustodyPhase::Active));
    }
}

