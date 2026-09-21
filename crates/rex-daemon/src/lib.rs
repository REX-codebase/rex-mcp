//! Durable local Harness daemon for caller-driven MCP work.
//!
//! The daemon has no model and no provider credentials. A subscribed host
//! agent proposes a plan and calls these methods; REX freezes the plan,
//! confines tools to one workspace, maintains custody and leases, and
//! decides completion from evidence. The trusted launcher, not an MCP
//! payload, decides whether mutations are pre-approved.

use rex_custody::capability::{hex_sha256, random_hex};
use rex_custody::{
    AgentProtocol, CapabilitySet, CapabilityToken, CompletionClaim,
    CompletionContract, Consumption, CustodyAcceptance, CustodyBudgets,
    CustodyError, CustodyRegistry, CustodiedToolRuntime, EvidenceGate,
    GateEvaluator, GateOutcome, LeaseTerms, OperatorIdentity, ToolClass,
    WorkerMode,
};
use rex_protocol::*;
use rex_protocol::packets::{OperationStatus, PacketIdentity};
use rex_ultra::external_kernel::{
    AdversaryEvidence, CandidateResponse, EvidenceKind, HostKernelStatus, KernelState,
    VerifierEvidence,
};
use rex_ultra::host_bridge::{
    contract_from_plan, BridgeError, UltraHostBridge, UltraHostView, DEFAULT_MINIMUM_CANDIDATES,
};
use rex_tools::{ToolRequest, ToolResult, ToolRuntime};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_LEASE_MS: u64 = 5 * 60 * 1000;
const MAX_PLAN_STEPS: usize = 100;

#[derive(Debug, Clone)]
pub struct DaemonPolicy {
    pub workspace: PathBuf,
    /// Set only by the trusted UI/bootstrap after the human grants this task
    /// scope. It cannot be changed by any rex_* request.
    pub approve_task_mutations: bool,
    pub max_tool_calls: u64,
    pub max_wall_ms: u64,
}

impl DaemonPolicy {
    pub fn conservative(workspace: impl Into<PathBuf>) -> Self {
        Self { workspace: workspace.into(), approve_task_mutations: false,
            max_tool_calls: 80, max_wall_ms: 20 * 60 * 1000 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DurableTask {
    protocol_version: String,
    task_id: String,
    request_id: String,
    request_hash: String,
    task: String,
    host: HostKind,
    operator_is_agent: bool,
    plan_hash: String,
    plan: Vec<PlanStep>,
    cursor: usize,
    state: TaskState,
    open_action: Option<ActionSpec>,
    token: CapabilityToken,
    grant_id: String,
    lease_epoch: u64,
    lease_expires_ms: u128,
    heartbeat_seq: u64,
    heartbeat_interval_ms: u64,
    max_tool_calls: u64,
    used_tool_calls: u64,
    max_wall_ms: u64,
    created_ms: u128,
    last_event_seq: u64,
    proof: Option<String>,
    evidence: BTreeMap<String, String>,
    /// Harness-registered evidence ids (tool receipts, test evidence ids).
    /// A completion claim may only cite these.
    registered_evidence: BTreeSet<String>,
    result: Option<String>,
    terminal_reason: Option<String>,
    #[serde(default = "default_branch_id")]
    branch_id: String,
    #[serde(default)]
    resume_nonce: u64,
    #[serde(default)]
    operation_status: OperationStatus,
    #[serde(default)]
    ultra_skill_plan_hash: Option<String>,
    /// SHA-256 of the current host resume handle. The handle itself is
    /// never stored; it rotates on every accepted resume.
    #[serde(default)]
    host_resume_handle_hash: Option<String>,
    /// Persisted store schema, independent of the wire protocol string.
    /// Schema 1 records predate this field and migrate on load; unknown
    /// future versions fail closed.
    #[serde(default = "default_store_schema_version")]
    store_schema_version: u32,
}

pub struct HarnessDaemon {
    root: PathBuf,
    policy: DaemonPolicy,
    custody: Arc<Mutex<CustodyRegistry>>,
    tools: CustodiedToolRuntime,
}

impl HarnessDaemon {
    pub fn open(root: impl Into<PathBuf>, mut policy: DaemonPolicy) -> Result<Self, ProtocolError> {
        let root = root.into();
        fs::create_dir_all(root.join("tasks")).map_err(internal)?;
        fs::create_dir_all(root.join("custody")).map_err(internal)?;
        fs::create_dir_all(&policy.workspace).map_err(internal)?;
        policy.workspace = fs::canonicalize(&policy.workspace).map_err(internal)?;
        let custody = Arc::new(Mutex::new(
            CustodyRegistry::recover(root.join("custody"), now_ms()).map_err(custody_err)?));
        let runtime = ToolRuntime::new(&policy.workspace).map_err(|e| internal(e.detail))?;
        let tools = CustodiedToolRuntime::new(runtime, custody.clone());
        Ok(Self { root, policy, custody, tools })
    }

    pub fn execute(&self, req: ExecuteRequest) -> Result<ExecuteResponse, ProtocolError> {
        validate_execute(&req)?;
        if let Some(id) = &req.task_id {
            let mut task = self.load(id)?;
            if task.task != req.task { return Err(perr(ErrorCode::IdempotencyConflict,
                "task_id exists with different task text", id)); }
            let handle = self.verify_and_rotate_resume_handle(&mut task, &req)?;
            return Ok(self.execute_view(&task, true, Some(handle)));
        }
        if let Some(task) = self.find_by_request(&req.request_id)? {
            if task.request_hash != request_hash(&req)? { return Err(perr(
                ErrorCode::IdempotencyConflict, "request_id was already used with another payload", &task.task_id)); }
            let mut task = task;
            let handle = self.verify_and_rotate_resume_handle(&mut task, &req)?;
            return Ok(self.execute_view(&task, true, Some(handle)));
        }
        let now = now_ms();
        let task_id = format!("task-{}", random_id());
        let host_resume_handle = format!("hrh-{}", random_hex(24));
        let plan = req.plan.clone().filter(|p| !p.is_empty()).unwrap_or_else(|| vec![PlanStep {
            instructions: req.task.clone(), acceptance: req.proof.clone() }]);
        if plan.len() > MAX_PLAN_STEPS { return Err(ProtocolError::new(
            ErrorCode::MalformedRequest, "plan exceeds 100 steps")); }
        let max_tools = req.budgets.as_ref().and_then(|b| b.max_tool_calls)
            .unwrap_or(self.policy.max_tool_calls).min(self.policy.max_tool_calls);
        let max_wall = req.budgets.as_ref().and_then(|b| b.max_wall_ms)
            .unwrap_or(self.policy.max_wall_ms).min(self.policy.max_wall_ms);
        let caps = capability_set(&self.policy.workspace);
        let operator = if req.operator_is_agent {
            OperatorIdentity::Agent(CustodyRegistry::register_agent(
                host_label(req.host), AgentProtocol::Mcp { client: host_label(req.host).into(),
                    version: PROTOCOL_VERSION.into() }))
        } else { OperatorIdentity::Human };
        let budgets = CustodyBudgets { max_steps: plan.len() as u64 + 8,
            max_tool_calls: max_tools, max_wall_ms: max_wall, max_tokens: 0 };
        let lease_terms = LeaseTerms { lease_ms: DEFAULT_LEASE_MS,
            heartbeat_interval_ms: 30_000, resume_grace_ms: 15 * 60 * 1000 };
        let contract = CompletionContract { gates: vec![EvidenceGate::NoPendingApprovals,
            EvidenceGate::WithinScopeChanges,
            EvidenceGate::Custom { name: "all_plan_steps_accepted".into() }], max_claim_attempts: 2 };
        let (token, grant) = {
            let mut reg = self.custody.lock().map_err(|_| internal("custody registry poisoned"))?;
            let offer = reg.offer(&task_id, &req.task, operator, WorkerMode::ExternalAgent,
                caps, budgets, lease_terms, contract, now).map_err(custody_err)?;
            let acceptance = CustodyAcceptance { offer_id: offer.offer_id.clone(),
                nonce_echo: offer.nonce.clone(), commitment: offer.expected_acceptance() };
            reg.accept(&acceptance, now).map_err(custody_err)?
        };
        let action = make_action(&task_id, 0, &plan[0], max_wall);
        let req_hash = request_hash(&req)?;
        let mut task = DurableTask { protocol_version: PROTOCOL_VERSION.into(),
            task_id: task_id.clone(), request_id: req.request_id, request_hash: req_hash,
            task: req.task, host: req.host, operator_is_agent: req.operator_is_agent,
            plan_hash: hash_json(&plan)?, plan, cursor: 0, state: TaskState::Active,
            open_action: Some(action), token, grant_id: grant.grant_id,
            lease_epoch: grant.lease.epoch, lease_expires_ms: grant.lease.expires_ms,
            heartbeat_seq: grant.lease.next_seq, heartbeat_interval_ms: grant.lease.heartbeat_interval_ms,
            max_tool_calls: max_tools, used_tool_calls: 0, max_wall_ms: max_wall,
            created_ms: now, last_event_seq: 0, proof: req.proof, evidence: BTreeMap::new(),
            registered_evidence: BTreeSet::new(), result: None, terminal_reason: None,
            branch_id: "main".into(), resume_nonce: grant.lease.next_seq,
            operation_status: if req.operator_is_agent { OperationStatus::ExternalHostRequired } else { OperationStatus::Prepared },
            ultra_skill_plan_hash: None,
            host_resume_handle_hash: Some(hex_sha256(host_resume_handle.as_bytes())),
            store_schema_version: STORE_SCHEMA_VERSION };
        let plan_hash = task.plan_hash.clone(); let step_count = task.plan.len();
        self.append_event(&mut task, "task_created", json!({"plan_hash":plan_hash,
            "steps":step_count,"protocol":PROTOCOL_VERSION}))?;
        self.persist(&task)?;
        Ok(self.execute_view(&task, false, Some(host_resume_handle)))
    }

    pub fn next(&self, req: NextRequest) -> Result<NextResponse, ProtocolError> {
        let mut t = self.live(&req.task_id, req.lease_epoch)?;
        self.heartbeat(&mut t)?;
        self.persist(&t)?;
        Ok(NextResponse { state: t.state, next: t.open_action.clone(), lease: lease_view(&t) })
    }

    pub fn read(&self, req: ReadRequest) -> Result<ReadResponse, ProtocolError> {
        let mut t = self.live(&req.task_id, req.lease_epoch)?;
        let result = self.call_tool(&mut t, ToolRequest::ReadFile { path: req.path })?;
        let mut content = result.output.unwrap_or_default();
        if let Some((start, len)) = req.byte_range {
            let bytes = content.as_bytes();
            let a = (start as usize).min(bytes.len());
            let b = a.saturating_add(len as usize).min(bytes.len());
            content = String::from_utf8_lossy(&bytes[a..b]).into_owned();
        }
        let bytes = content.len() as u64;
        Ok(ReadResponse { content, truncated: result.receipt.output_truncated, bytes,
            receipt: Some(result.call_id) })
    }

    pub fn edit(&self, req: EditRequest) -> Result<EditResponse, ProtocolError> {
        let mut t = self.live(&req.task_id, req.lease_epoch)?;
        let tool = if req.create { ToolRequest::CreateFile { path: req.path,
            content: req.replacement, overwrite: false } } else { ToolRequest::EditFile {
            path: req.path, expected: req.expected.ok_or_else(|| ProtocolError::new(
                ErrorCode::MalformedRequest, "expected is required unless create=true"))?,
            replacement: req.replacement, replace_all: false } };
        let out = self.call_tool(&mut t, tool)?;
        Ok(EditResponse { receipt: out.call_id, bytes_written: out.receipt.bytes_written })
    }

    pub fn search(&self, req: SearchRequest) -> Result<SearchResponse, ProtocolError> {
        let mut t = self.live(&req.task_id, req.lease_epoch)?;
        let out = self.call_tool(&mut t, ToolRequest::SearchFiles { query: req.query,
            path: None, max_results: req.max_results })?;
        Ok(SearchResponse { hits: parse_search(&out.output.unwrap_or_default()),
            receipt: Some(out.call_id) })
    }

    pub fn run(&self, req: RunRequest) -> Result<RunResponse, ProtocolError> {
        let mut t = self.live(&req.task_id, req.lease_epoch)?;
        let out = self.call_tool(&mut t, ToolRequest::RunCommand { argv: req.argv,
            cwd: Some(".".into()), timeout_ms: req.timeout_ms })?;
        let (stdout, stderr) = split_output(out.output.as_deref().unwrap_or(""));
        Ok(RunResponse { exit_code: out.receipt.exit_code, stdout, stderr,
            output_truncated: out.receipt.output_truncated, receipt: Some(out.call_id) })
    }

    pub fn test(&self, req: TestRequest) -> Result<TestResponse, ProtocolError> {
        let argv = match req.recipe.as_str() {
            "cargo-test" => vec!["cargo".into(), "test".into(), "--workspace".into()],
            "npm-test" => vec!["npm".into(), "test".into()],
            _ => return Err(ProtocolError::new(ErrorCode::ScopeDenied,
                "unknown test recipe; allowed: cargo-test, npm-test")),
        };
        let run = self.run(RunRequest { task_id: req.task_id.clone(), lease_epoch: req.lease_epoch,
            argv, timeout_ms: Some(10 * 60 * 1000) })?;
        let passed = run.exit_code == Some(0);
        let evidence_id = format!("evidence-{}", random_id());
        let mut t = self.load(&req.task_id)?;
        t.registered_evidence.insert(evidence_id.clone());
        t.evidence.insert(req.recipe.clone(), format!("{}:{}", evidence_id, passed));
        self.append_event(&mut t, "test_finished", json!({"recipe":req.recipe,
            "passed":passed,"evidence_id":evidence_id}))?;
        self.persist(&t)?;
        Ok(TestResponse { recipe: req.recipe, passed,
            summary: if passed { "passed".into() } else { format!("failed: {}", run.stderr) }, evidence_id })
    }

    pub fn submit(&self, req: SubmitRequest) -> Result<SubmitResponse, ProtocolError> {
        let mut t = self.live(&req.task_id, req.lease_epoch)?;
        let open = t.open_action.clone().ok_or_else(|| perr(ErrorCode::TaskTerminal,
            "no open action", &t.task_id))?;
        if open.action_id != req.action_id { return Err(perr(ErrorCode::LeaseConflict,
            "action is stale or belongs to another task", &t.task_id)); }
        t.state = TaskState::Verifying;
        let narrative = req.narrative.clone(); let evidence = req.evidence.clone();
        let cited: Vec<String> = req.evidence.values().cloned().collect();
        self.append_event(&mut t, "action_submitted", json!({"action_id":req.action_id,
            "narrative":narrative,"evidence":evidence}))?;
        t.evidence.extend(req.evidence);
        t.cursor += 1;
        if t.cursor < t.plan.len() {
            t.state = TaskState::Active;
            let next = make_action(&t.task_id, t.cursor, &t.plan[t.cursor], t.max_wall_ms);
            t.open_action = Some(next.clone());
            self.append_event(&mut t, "action_accepted", json!({"action_id":open.action_id}))?;
            self.persist(&t)?;
            return Ok(SubmitResponse { state: t.state, accepted: true, repair: None, next: Some(next) });
        }
        // Fable completion gate: the claim may only cite evidence the
        // harness actually registered, and it must cite some.
        if let Err(failures) = rex_prompt::gate::validate_completion_claim(
            &req.narrative, &cited, &t.registered_evidence) {
            t.state = TaskState::Active; t.cursor = t.plan.len()-1;
            t.open_action = Some(open.clone());
            let repair = failures.join("; ");
            self.append_event(&mut t, "completion_rejected", json!({"failures":failures}))?;
            self.persist(&t)?;
            return Ok(SubmitResponse { state: t.state, accepted: false,
                repair: Some(repair), next: Some(open) });
        }
        t.open_action = None;
        let evaluator = FinalEvaluator { all_steps: true };
        let claim = CompletionClaim { summary: req.narrative.clone() };
        let completion = self.custody.lock().map_err(|_| internal("custody registry poisoned"))?
            .claim_completion(&t.token, &claim, &evaluator, now_ms());
        match completion {
            Ok(_) => { t.operation_status = OperationStatus::Committed; t.state = TaskState::Completed; t.result = Some(req.narrative);
                t.terminal_reason = Some("verified completion".into());
                let ev = t.evidence.clone();
                self.append_event(&mut t, "task_completed", json!({"evidence":ev}))?;
                self.persist(&t)?;
                Ok(SubmitResponse { state: t.state, accepted: true, repair: None, next: None }) }
            Err(e) => { t.state = TaskState::Active; t.cursor = t.plan.len()-1;
                t.open_action = Some(open.clone());
                self.append_event(&mut t, "completion_rejected", json!({"error":e.to_string()}))?;
                self.persist(&t)?;
                Ok(SubmitResponse { state: t.state, accepted: false,
                    repair: Some(e.to_string()), next: Some(open) }) }
        }
    }

    pub fn status(&self, req: TaskRefRequest) -> Result<StatusResponse, ProtocolError> {
        let t = self.load(&req.task_id)?;
        let lease = lease_view(&t); let used_wall = elapsed(&t);
        let operation = status_operation(&t);
        let packet = PacketIdentity::new(&t.branch_id, t.lease_epoch, t.resume_nonce, &t.request_id);
        Ok(StatusResponse { task_id: t.task_id, state: t.state, task: t.task,
            operator_is_agent: t.operator_is_agent, host: t.host, lease,
            open_action: t.open_action, budgets: BudgetView { max_tool_calls: t.max_tool_calls,
                used_tool_calls: t.used_tool_calls, max_wall_ms: t.max_wall_ms,
                used_wall_ms: used_wall }, last_event_seq: t.last_event_seq,
            operation, packet })
    }

    pub fn events(&self, req: EventsRequest) -> Result<EventsResponse, ProtocolError> {
        let t = self.load(&req.task_id)?;
        let path = self.task_dir(&t.task_id).join("events.jsonl");
        let data = fs::read_to_string(path).unwrap_or_default();
        let limit = req.limit.unwrap_or(100).min(1000);
        let events: Vec<TaskEvent> = data.lines().filter_map(|l| serde_json::from_str(l).ok())
            .filter(|e: &TaskEvent| e.seq > req.after_seq).take(limit).collect();
        Ok(EventsResponse { events, last_seq: t.last_event_seq })
    }

    pub fn result(&self, req: TaskRefRequest) -> Result<ResultResponse, ProtocolError> {
        let t = self.load(&req.task_id)?;
        if !t.state.is_terminal() { return Err(perr(ErrorCode::NoResult,
            "task is not terminal", &t.task_id)); }
        Ok(ResultResponse { task_id: t.task_id, state: t.state, output: t.result,
            proof_bundle: if t.evidence.is_empty() { None } else { Some(t.evidence) },
            terminal_reason: t.terminal_reason })
    }

    pub fn cancel(&self, req: CancelRequest) -> Result<CancelResponse, ProtocolError> {
        let mut t = self.load(&req.task_id)?;
        if t.state.is_terminal() { return Ok(CancelResponse { task_id: t.task_id,
            state: t.state, final_reason: t.terminal_reason.unwrap_or_else(|| "terminal".into()) }); }
        {
            let mut reg = self.custody.lock().map_err(|_| internal("custody registry poisoned"))?;
            if t.operator_is_agent {
                reg.operator_cancel(&t.token, now_ms()).map_err(custody_err)?;
            } else {
                // The human stop button: terminal fence in every phase,
                // never gated on operator state.
                reg.human_stop(&t.grant_id, now_ms()).map_err(custody_err)?;
            }
        }
        t.operation_status = OperationStatus::Aborted;
        t.state = TaskState::Cancelled;
        let why = req.reason.unwrap_or_else(|| "cancelled by operator".into());
        t.terminal_reason = Some(why.clone()); t.open_action = None;
        self.append_event(&mut t, "task_cancelled", json!({"reason":why}))?;
        self.persist(&t)?;
        Ok(CancelResponse { task_id: t.task_id, state: t.state, final_reason: why })
    }

    /// Single entry point shared by the MCP server and tests: deserialize
    /// the tool arguments, run the typed method, serialize the response.
    pub fn dispatch(&self, tool: ToolName, args: Value) -> Result<Value, ProtocolError> {
        fn parse<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, ProtocolError> {
            serde_json::from_value(v).map_err(|e| ProtocolError::new(
                ErrorCode::MalformedRequest, format!("bad arguments: {e}")))
        }
        macro_rules! go { ($args:expr, $m:ident) => {{
            let req = parse($args)?;
            serde_json::to_value(self.$m(req)?).map_err(internal)
        }}}
        match tool {
            ToolName::Execute => go!(args, execute),
            ToolName::Next => go!(args, next),
            ToolName::Read => go!(args, read),
            ToolName::Edit => go!(args, edit),
            ToolName::Search => go!(args, search),
            ToolName::Run => go!(args, run),
            ToolName::Test => go!(args, test),
            ToolName::Submit => go!(args, submit),
            ToolName::Status => go!(args, status),
            ToolName::Events => go!(args, events),
            ToolName::Result => go!(args, result),
            ToolName::Cancel => go!(args, cancel),
            ToolName::UltraOpen => go!(args, ultra_open),
            ToolName::UltraSubmit => go!(args, ultra_submit),
            ToolName::UltraPromote => go!(args, ultra_promote),
            ToolName::Proof => go!(args, proof_bundle),
        }
    }

    /// Open the Ultra external-host loop for a live agent-operated task.
    /// The first open attaches the host at the task's lease epoch; later
    /// opens are pure views over the durable kernel.
    pub fn ultra_open(&self, req: UltraOpenRequest) -> Result<UltraViewResponse, ProtocolError> {
        let mut t = self.live(&req.task_id, req.lease_epoch)?;
        require_agent(&t)?;
        let bridge = UltraHostBridge::open(&self.root).map_err(|e| bridge_err(&t.task_id, e))?;
        let contract = contract_from_plan(&t.task, &t.plan);
        let view = bridge
            .open_requests(&t.task_id, &contract, DEFAULT_MINIMUM_CANDIDATES, t.lease_epoch)
            .map_err(|e| bridge_err(&t.task_id, e))?;
        let plan = self.skill_plan();
        if let Some(plan) = &plan {
            if t.ultra_skill_plan_hash.as_deref() != Some(plan.plan_hash.as_str()) {
                t.ultra_skill_plan_hash = Some(plan.plan_hash.clone());
                self.append_event(&mut t, "ultra_skill_plan", json!({"plan_hash":plan.plan_hash,
                    "selected":plan.selected,"unsupported":plan.unsupported}))?;
                self.persist(&t)?;
            }
        }
        Ok(ultra_view(t.task_id.clone(), &view, plan))
    }

    /// Submit one candidate response or one adversary/verifier evidence item.
    /// The kernel alone decides whether the evidence gates a transition.
    pub fn ultra_submit(&self, req: UltraSubmitRequest) -> Result<UltraViewResponse, ProtocolError> {
        let mut t = self.live(&req.task_id, req.lease_epoch)?;
        require_agent(&t)?;
        let bridge = UltraHostBridge::open(&self.root).map_err(|e| bridge_err(&t.task_id, e))?;
        let contract = contract_from_plan(&t.task, &t.plan);
        let view = match req.kind {
            UltraSubmissionKind::Candidate => bridge.record_response(&t.task_id, &contract,
                CandidateResponse { candidate_id: req.request_id.clone(),
                    response_hash: req.response_hash, content: req.content }),
            UltraSubmissionKind::Adversary => bridge.record_adversary(&t.task_id, &contract,
                AdversaryEvidence { request_id: req.request_id.clone(), candidate_id: req.candidate_id,
                    response_hash: req.response_hash, content: req.content }),
            UltraSubmissionKind::Verifier => bridge.record_verifier(&t.task_id, &contract,
                VerifierEvidence { request_id: req.request_id.clone(), candidate_id: req.candidate_id,
                    response_hash: req.response_hash, content: req.content }),
            UltraSubmissionKind::Visual => bridge.record_visual(&t.task_id, &contract,
                rex_ultra::external_kernel::VisualEvidence { request_id: req.request_id.clone(), candidate_id: req.candidate_id,
                    response_hash: req.response_hash, content: req.content }),
        }.map_err(|e| bridge_err(&t.task_id, e))?;
        self.append_event(&mut t, "ultra_submission", json!({"kind":format!("{:?}",req.kind),
            "kernel_state":format!("{:?}",view.kernel_state)}))?;
        self.persist(&t)?;
        Ok(ultra_view(t.task_id.clone(), &view, self.skill_plan()))
    }

    /// Promote the qualified candidate into the task workspace. Live lease,
    /// agent-operated tasks only; the kernel must be completed and the
    /// rollback path is verified by the promotion store.
    pub fn ultra_promote(&self, req: rex_protocol::UltraPromoteRequest) -> Result<rex_protocol::UltraPromoteResponse, ProtocolError> {
        let mut t = self.live(&req.task_id, req.lease_epoch)?;
        require_agent(&t)?;
        let bridge = UltraHostBridge::open(&self.root).map_err(|e| bridge_err(&t.task_id, e))?;
        let contract = contract_from_plan(&t.task, &t.plan);
        let receipt = bridge.promote(&t.task_id, &contract, &self.policy.workspace)
            .map_err(|e| bridge_err(&t.task_id, e))?;
        let state = match receipt.state {
            rex_ultra::promotion::PromotionState::Prepared => "prepared",
            rex_ultra::promotion::PromotionState::Committed => "committed",
            rex_ultra::promotion::PromotionState::RolledBack => "rolled_back",
            rex_ultra::promotion::PromotionState::CorruptState => "corrupt_state",
        };
        self.append_event(&mut t, "ultra_promotion", json!({"state":state,
            "candidate_id":receipt.candidate_id,"bundle_hash":receipt.bundle_hash,
            "staging_hash":receipt.staging_hash}))?;
        self.persist(&t)?;
        Ok(rex_protocol::UltraPromoteResponse {
            task_id: t.task_id.clone(),
            state: state.into(),
            candidate_id: receipt.candidate_id,
            bundle_hash: receipt.bundle_hash,
            destination_hash_before: receipt.destination_hash_before,
            staging_hash: receipt.staging_hash,
            gates_rerun: receipt.gates_rerun,
            gates_not_rerun: receipt.gates_not_rerun,
            detail: receipt.detail,
        })
    }

    /// Assemble and persist the machine-readable proof bundle for one task:
    /// frozen plan, kernel state, qualified candidate, bound skill plan,
    /// promotion receipt, full event stream and a deterministic bundle hash.
    /// This is the artifact the proof journey and release evidence build on.
    pub fn proof_bundle(&self, req: TaskRefRequest) -> Result<TaskProofBundle, ProtocolError> {
        let t = self.load(&req.task_id)?;
        let bridge = UltraHostBridge::open(&self.root).map_err(|e| bridge_err(&t.task_id, e))?;
        let adapter = bridge.load_existing(&t.task_id).map_err(|e| bridge_err(&t.task_id, e))?;
        let (kernel_state, qualified_candidate) = match &adapter {
            Some(adapter) => {
                let state = match adapter.kernel().state {
                    KernelState::New => "new", KernelState::Leased => "leased",
                    KernelState::Running => "running", KernelState::AwaitingEvidence => "awaiting_evidence",
                    KernelState::Completed => "completed", KernelState::Failed => "failed",
                    KernelState::Revoked => "revoked",
                };
                (Some(state.to_string()), adapter.qualified_candidate().map(str::to_string))
            }
            None => (None, None),
        };
        let skill_plan = self.skill_plan();
        let store = rex_ultra::promotion::PromotionStore::open(self.root.join("ultra"))
            .map_err(|e| internal(format!("promotion store: {e:?}")))?;
        let promotion = store.receipt(&t.task_id)
            .map_err(|e| internal(format!("promotion receipt: {e:?}")))?;
        let promotion_state = promotion.as_ref().map(|r| match r.state {
            rex_ultra::promotion::PromotionState::Prepared => "prepared",
            rex_ultra::promotion::PromotionState::Committed => "committed",
            rex_ultra::promotion::PromotionState::RolledBack => "rolled_back",
            rex_ultra::promotion::PromotionState::CorruptState => "corrupt_state",
        }.to_string());
        let events = self.events(EventsRequest { task_id: t.task_id.clone(), after_seq: 0, limit: None })?.events;
        let bundle_hash = hash_json(&(
            &t.task_id, &t.request_hash, &t.task, &t.plan, t.state,
            &kernel_state, &qualified_candidate, &skill_plan, &promotion_state,
            &promotion, &events,
        ))?;
        let bundle = TaskProofBundle {
            task_id: t.task_id.clone(), request_hash: t.request_hash.clone(),
            task: t.task.clone(), plan: t.plan.clone(), state: t.state,
            kernel_state, qualified_candidate, skill_plan, promotion_state, promotion, events,
            bundle_hash,
        };
        let dir = self.root.join("proofs");
        fs::create_dir_all(&dir).map_err(internal)?;
        let path = dir.join(format!("{}.json", t.task_id));
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, serde_json::to_vec_pretty(&bundle).map_err(internal)?).map_err(internal)?;
        fs::rename(&temporary, &path).map_err(internal)?;
        Ok(bundle)
    }

    fn skill_plan(&self) -> Option<SkillPlanView> {
        let facts = rex_ultra::skills::collect_repository_facts(&self.policy.workspace);
        let mut registry = rex_ultra::skill_packs::first_class_registry();
        registry.extend(rex_ultra::generated_packs::broad_registry(&facts));
        let plan = rex_ultra::skills::compile_plan(&facts, &registry).ok()?;
        Some(SkillPlanView {
            plan_hash: plan.plan_hash,
            compiler_version: plan.compiler_version,
            selected: plan.selected.iter().map(|p| format!("{}@{}", p.id, p.version)).collect(),
            gates: plan.gates.iter().map(|g| SkillGateView {
                pack: g.pack.clone(), id: g.id.clone(),
                command_hint: g.command_hint.clone(), required: g.required,
            }).collect(),
            unsupported: plan.unsupported,
        })
    }

    fn call_tool(&self, t: &mut DurableTask, request: ToolRequest) -> Result<ToolResult, ProtocolError> {
        if t.used_tool_calls >= t.max_tool_calls { return Err(perr(ErrorCode::BudgetExceeded,
            "tool-call budget exhausted", &t.task_id)); }
        let now = now_ms();
        let prepared = self.tools.prepare(&t.token, request, now).map_err(|e| map_tool_err(&t.task_id, e.to_string()))?;
        if prepared.approval_required {
            if !self.policy.approve_task_mutations { return Err(perr(ErrorCode::ApprovalRequired,
                "trusted launcher has not approved mutations for this task", &t.task_id)); }
            self.tools.resolve_approval(&prepared.call_id, true)
                .map_err(|e| map_tool_err(&t.task_id, e.to_string()))?;
        }
        let out = self.tools.execute(&t.token, &prepared.call_id, now_ms());
        t.used_tool_calls += 1;
        t.registered_evidence.insert(out.call_id.clone());
        self.custody.lock().map_err(|_| internal("custody registry poisoned"))?
            .consume(&t.token, Consumption { tool_calls: 1, ..Default::default() }, now_ms())
            .map_err(custody_err)?;
        self.append_event(t, "tool_finished", json!({"tool":out.tool,"call_id":out.call_id,
            "ok":out.ok,"receipt":out.receipt}))?;
        self.persist(t)?;
        if !out.ok { return Err(map_tool_err(&t.task_id, out.error.as_ref()
            .map(|e| e.detail.clone()).unwrap_or_else(|| "tool failed".into()))); }
        Ok(out)
    }

    fn live(&self, id: &str, epoch: u64) -> Result<DurableTask, ProtocolError> {
        let t = self.load(id)?;
        if t.state.is_terminal() { return Err(perr(ErrorCode::TaskTerminal, "task is terminal", id)); }
        if epoch != t.lease_epoch { return Err(perr(ErrorCode::StaleLease, "lease epoch is stale", id)); }
        if now_ms() >= t.lease_expires_ms { return Err(perr(ErrorCode::StaleLease, "lease expired", id)); }
        self.custody.lock().map_err(|_| internal("custody registry poisoned"))?
            .verify_token(&t.token, now_ms()).map_err(custody_err)?;
        Ok(t)
    }

    fn heartbeat(&self, t: &mut DurableTask) -> Result<(), ProtocolError> {
        let lease = self.custody.lock().map_err(|_| internal("custody registry poisoned"))?
            .heartbeat(&t.token, t.heartbeat_seq, now_ms()).map_err(custody_err)?;
        t.heartbeat_seq = lease.next_seq; t.lease_expires_ms = lease.expires_ms;
        Ok(())
    }

    fn verify_and_rotate_resume_handle(&self, t: &mut DurableTask, req: &ExecuteRequest) -> Result<String, ProtocolError> {
        let expected = t.host_resume_handle_hash.clone().ok_or_else(|| perr(ErrorCode::ScopeDenied,
            "task predates host resume handles; resume through the trusted human launcher path", &t.task_id))?;
        let presented = req.resume_handle.as_deref().ok_or_else(|| perr(ErrorCode::ScopeDenied,
            "resume requires the host resume handle issued at creation", &t.task_id))?;
        if hex_sha256(presented.as_bytes()) != expected {
            return Err(perr(ErrorCode::ScopeDenied, "host resume handle mismatch", &t.task_id));
        }
        let rotated = format!("hrh-{}", random_hex(24));
        t.host_resume_handle_hash = Some(hex_sha256(rotated.as_bytes()));
        self.persist(t)?;
        Ok(rotated)
    }

    fn execute_view(&self, t: &DurableTask, resumed: bool, host_resume_handle: Option<String>) -> ExecuteResponse {
        ExecuteResponse { task_id: t.task_id.clone(), state: t.state, resumed, host_resume_handle,
            next: t.open_action.clone(), lease: lease_view(t),
            discipline: Some(operator_discipline()) }
    }
    fn task_dir(&self, id: &str) -> PathBuf { self.root.join("tasks").join(id) }
    fn persist(&self, t: &DurableTask) -> Result<(), ProtocolError> {
        let dir = self.task_dir(&t.task_id); fs::create_dir_all(&dir).map_err(internal)?;
        atomic_json(&dir.join("task.json"), t)
    }
    fn load(&self, id: &str) -> Result<DurableTask, ProtocolError> {
        if !safe_id(id) { return Err(ProtocolError::new(ErrorCode::MalformedRequest, "invalid task id")); }
        let bytes = fs::read(self.task_dir(id).join("task.json"))
            .map_err(|_| perr(ErrorCode::TaskNotFound, "task not found", id))?;
        self.parse_task(&bytes, id)
    }

    /// Store-schema gate, kept separate from the wire protocol version.
    /// Schema 1 records (no version field) migrate in place with a durable
    /// store_migrated event; unknown future versions fail closed so an older
    /// server can never silently misread a newer store.
    fn parse_task(&self, bytes: &[u8], id: &str) -> Result<DurableTask, ProtocolError> {
        let value: serde_json::Value = serde_json::from_slice(bytes).map_err(internal)?;
        let version = value.get("store_schema_version").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
        if version > STORE_SCHEMA_VERSION {
            return Err(perr(ErrorCode::VersionMismatch,
                &format!("stored task schema v{version} is newer than this server"), id));
        }
        let mut t: DurableTask = serde_json::from_value(value).map_err(internal)?;
        // Migrate on the raw file version, not the serde default, so a
        // schema-1 record (field absent) is detected and rewritten.
        if version != STORE_SCHEMA_VERSION {
            t.store_schema_version = STORE_SCHEMA_VERSION;
            self.append_event(&mut t, "store_migrated",
                json!({"from": version, "to": STORE_SCHEMA_VERSION}))?;
            self.persist(&t)?;
        }
        Ok(t)
    }
    fn find_by_request(&self, request_id: &str) -> Result<Option<DurableTask>, ProtocolError> {
        let dirs = fs::read_dir(self.root.join("tasks")).map_err(internal)?;
        for ent in dirs.flatten() {
            if let Ok(bytes) = fs::read(ent.path().join("task.json")) {
                if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                    if v.get("request_id").and_then(|r| r.as_str()) == Some(request_id) {
                        let id = v.get("task_id").and_then(|i| i.as_str()).unwrap_or("task");
                        return self.parse_task(&bytes, id).map(Some);
                    }
                }
            }
        }
        Ok(None)
    }
    fn append_event(&self, t: &mut DurableTask, kind: &str, detail: Value) -> Result<(), ProtocolError> {
        t.last_event_seq += 1;
        let ev = TaskEvent { seq: t.last_event_seq, ts_ms: now_ms(), kind: kind.into(), detail };
        let dir = self.task_dir(&t.task_id); fs::create_dir_all(&dir).map_err(internal)?;
        let mut f = OpenOptions::new().create(true).append(true).open(dir.join("events.jsonl")).map_err(internal)?;
        serde_json::to_writer(&mut f, &ev).map_err(internal)?; f.write_all(b"\n").map_err(internal)?;
        f.sync_data().map_err(internal)
    }
}

/// The Fable completion discipline, delivered through the protocol because
/// host agents never see REX's own system prompt.
fn operator_discipline() -> String {
    rex_prompt::gate::COMPLETION_GATE.to_string()
}

struct FinalEvaluator { all_steps: bool }
impl GateEvaluator for FinalEvaluator {
    fn evaluate(&self, gate: &EvidenceGate, _: &rex_custody::CustodyGrant) -> GateOutcome {
        match gate {
            EvidenceGate::Custom { name } if name == "all_plan_steps_accepted" && !self.all_steps =>
                GateOutcome::Failed("plan has open steps".into()),
            EvidenceGate::HumanConfirmation => GateOutcome::Failed("human confirmation unavailable".into()),
            _ => GateOutcome::Passed,
        }
    }
}

fn validate_execute(r: &ExecuteRequest) -> Result<(), ProtocolError> {
    if r.request_id.is_empty() || r.request_id.len() > 200 || r.task.trim().is_empty() {
        return Err(ProtocolError::new(ErrorCode::MalformedRequest, "request_id and task are required"));
    }
    Ok(())
}
fn capability_set(workspace: &Path) -> CapabilitySet {
    CapabilitySet { workspace_root: workspace.to_path_buf(),
        tool_classes: [ToolClass::Read,ToolClass::Write,ToolClass::Execute].into_iter().collect(),
        allowed_tools: ["read_file","create_file","edit_file","search_files","run_command"]
            .into_iter().map(String::from).collect::<BTreeSet<_>>(),
        allow_search: true, allow_preview: false, can_delegate: false }
}
fn make_action(task_id: &str, idx: usize, step: &PlanStep, max_wall_ms: u64) -> ActionSpec {
    ActionSpec { action_id: format!("action-{}-{}", task_id.trim_start_matches("task-"), idx+1),
        seq: idx as u64 + 1, instructions: step.instructions.clone(),
        permitted_tools: vec![ToolName::Read,ToolName::Edit,ToolName::Search,ToolName::Run,ToolName::Test],
        acceptance: step.acceptance.clone().unwrap_or_else(|| "submit evidence and a truthful outcome".into()),
        max_wall_ms }
}
/// The machine-readable proof bundle for one task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskProofBundle {
    pub task_id: String,
    pub request_hash: String,
    pub task: String,
    pub plan: Vec<PlanStep>,
    pub state: TaskState,
    pub kernel_state: Option<String>,
    pub qualified_candidate: Option<String>,
    pub skill_plan: Option<SkillPlanView>,
    pub promotion_state: Option<String>,
    pub promotion: Option<rex_ultra::promotion::PromotionReceipt>,
    pub events: Vec<TaskEvent>,
    pub bundle_hash: String,
}

fn require_agent(t: &DurableTask) -> Result<(), ProtocolError> {
    if !t.operator_is_agent { return Err(perr(ErrorCode::ScopeDenied,
        "ultra external-host loop requires an agent-operated task", &t.task_id)); }
    Ok(())
}
fn bridge_err(task_id: &str, e: BridgeError) -> ProtocolError {
    match e {
        BridgeError::InvalidTaskId => perr(ErrorCode::TaskNotFound, "invalid ultra task id", task_id),
        BridgeError::Adapter(a) => perr(ErrorCode::GateFailed,
            format!("ultra kernel rejected submission: {a:?}"), task_id),
        BridgeError::Promotion(p) => perr(ErrorCode::GateFailed,
            format!("ultra promotion rejected: {p:?}"), task_id),
        BridgeError::Io(m) => perr(ErrorCode::Internal, format!("ultra store: {m}"), task_id),
    }
}
fn ultra_view(task_id: String, view: &UltraHostView, skill_plan: Option<SkillPlanView>) -> UltraViewResponse {
    let status = match view.status {
        HostKernelStatus::HostRequired => "host_required",
        HostKernelStatus::Collecting => "collecting",
        HostKernelStatus::Ready => "ready",
    };
    let kernel_state = match view.kernel_state {
        KernelState::New => "new",
        KernelState::Leased => "leased",
        KernelState::Running => "running",
        KernelState::AwaitingEvidence => "awaiting_evidence",
        KernelState::Completed => "completed",
        KernelState::Failed => "failed",
        KernelState::Revoked => "revoked",
    };
    UltraViewResponse {
        task_id,
        status: status.into(),
        kernel_state: kernel_state.into(),
        candidate_requests: view.candidate_requests.iter().map(|r| UltraCandidateRequestView {
            work_kind: match r.work_kind { rex_ultra::contract::WorkKind::General => "general",
                rex_ultra::contract::WorkKind::Visual => "visual" }.into(),
            candidate_id: r.candidate_id.clone(), contract_hash: r.contract_hash.clone(),
            task: r.task.clone(), obligation_ids: r.obligation_ids.clone(),
        }).collect(),
        evidence_requests: view.evidence_requests.iter().map(|r| UltraEvidenceRequestView {
            request_id: r.request_id.clone(), candidate_id: r.candidate_id.clone(),
            contract_hash: r.contract_hash.clone(),
            kind: match r.kind { EvidenceKind::Adversary => "adversary",
                EvidenceKind::Verifier => "verifier",
                EvidenceKind::Visual => "visual" }.into(),
            obligation_ids: r.obligation_ids.clone(),
            candidate_response_hash: r.candidate_response_hash.clone(),
        }).collect(),
        skill_plan,
    }
}
fn request_hash(r: &ExecuteRequest) -> Result<String, ProtocolError> {
    // The resume handle is a credential, not payload identity: it must not
    // change the request hash or every resume would look like a conflict.
    let mut normalized = r.clone();
    normalized.resume_handle = None;
    hash_json(&normalized)
}
fn hash_json<T: Serialize>(v: &T) -> Result<String, ProtocolError> {
    let b=serde_json::to_vec(v).map_err(internal)?; Ok(hex_sha256(&b))
}
fn atomic_json<T: Serialize>(path: &Path, v: &T) -> Result<(), ProtocolError> {
    let bytes=serde_json::to_vec_pretty(v).map_err(internal)?; let tmp=path.with_extension("tmp");
    fs::write(&tmp, bytes).map_err(internal)?; fs::rename(tmp,path).map_err(internal)
}
fn safe_id(s:&str)->bool { !s.is_empty() && s.len()<200 && s.bytes().all(|b| b.is_ascii_alphanumeric()||b==b'-'||b==b'_') }
fn now_ms()->u128 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() }
fn random_id()->String { random_hex(8) }
fn elapsed(t:&DurableTask)->u64 { now_ms().saturating_sub(t.created_ms) as u64 }
fn lease_view(t:&DurableTask)->LeaseView { LeaseView { epoch:t.lease_epoch,
    expires_ms_from_now:t.lease_expires_ms.saturating_sub(now_ms()) as u64,
    heartbeat_interval_ms:t.heartbeat_interval_ms } }
fn default_branch_id() -> String { "main".into() }
/// Current persisted task-store schema. Bump when DurableTask changes shape;
/// never reuse the wire protocol version for storage decisions.
const STORE_SCHEMA_VERSION: u32 = 2;
fn default_store_schema_version() -> u32 { STORE_SCHEMA_VERSION }
fn status_operation(t: &DurableTask) -> OperationStatus {
    if now_ms() >= t.lease_expires_ms && !t.state.is_terminal() {
        OperationStatus::Stale
    } else {
        t.operation_status.clone()
    }
}
fn host_label(h:HostKind)->&'static str { match h { HostKind::Human=>"human-ui",HostKind::ClaudeCode=>"claude-code",
    HostKind::Antigravity=>"antigravity",HostKind::GenericAgent=>"generic-mcp" } }
fn perr(code:ErrorCode,msg:impl Into<String>,id:&str)->ProtocolError { ProtocolError::new(code,msg).for_task(id) }
fn internal(e:impl ToString)->ProtocolError { ProtocolError::new(ErrorCode::Internal,e.to_string()) }
fn custody_err(e:CustodyError)->ProtocolError { let code=match e { CustodyError::LeaseStale|CustodyError::HeartbeatReplay{..}=>ErrorCode::StaleLease,
    CustodyError::BudgetExhausted=>ErrorCode::BudgetExceeded,
    CustodyError::ClaimRejected{..}=>ErrorCode::GateFailed,_=>ErrorCode::Unauthorized}; ProtocolError::new(code,e.to_string()) }
fn map_tool_err(id:&str,e:String)->ProtocolError { perr(ErrorCode::ScopeDenied,e,id) }
fn parse_search(s:&str)->Vec<SearchHit> { s.lines().filter_map(|l| { let mut p=l.splitn(3,':');
    let path=p.next()?.to_string(); let line=p.next()?.parse().ok()?; let excerpt=p.next().unwrap_or("").trim().to_string();
    Some(SearchHit{path,line,excerpt}) }).collect() }
fn split_output(s:&str)->(String,String) { if let Some((a,b))=s.split_once("stderr:\n") {
    (a.strip_prefix("stdout:\n").unwrap_or(a).trim_end().into(),b.trim_end().into())
} else {(s.strip_prefix("stdout:\n").unwrap_or(s).trim_end().into(),String::new())} }

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    fn req(id:&str)->ExecuteRequest { ExecuteRequest { request_id:id.into(), task:"make hello".into(),
        task_id:None,resume_handle:None,host:HostKind::ClaudeCode,operator_is_agent:true,budgets:None,proof:None,
        plan:Some(vec![PlanStep{instructions:"write hello".into(),acceptance:Some("file exists".into())}]) } }
    #[test] fn store_schema_v1_migrates_and_future_versions_fail_closed() {
        let d=tempdir().unwrap(); let w=d.path().join("ws"); let root=d.path().join("state");
        let daemon=HarnessDaemon::open(&root,DaemonPolicy::conservative(&w)).unwrap();
        let ex=daemon.execute(req("rmig")).unwrap();
        let file=root.join("tasks").join(&ex.task_id).join("task.json");
        let mut v:serde_json::Value=serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        assert_eq!(v.get("store_schema_version").and_then(|s|s.as_u64()),Some(2));
        // A schema-1 record predates the version field; loading migrates in place.
        v.as_object_mut().unwrap().remove("store_schema_version");
        fs::write(&file,serde_json::to_vec_pretty(&v).unwrap()).unwrap();
        let s1=daemon.status(TaskRefRequest{task_id:ex.task_id.clone()}).unwrap();
        assert_eq!(s1.state,TaskState::Active);
        let v2:serde_json::Value=serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        assert_eq!(v2.get("store_schema_version").and_then(|s|s.as_u64()),Some(2));
        let log=fs::read_to_string(root.join("tasks").join(&ex.task_id).join("events.jsonl")).unwrap();
        assert!(log.contains("store_migrated"));
        // An unknown future schema fails closed instead of being misread.
        let mut v3=v2; v3.as_object_mut().unwrap().insert("store_schema_version".into(),serde_json::json!(99));
        fs::write(&file,serde_json::to_vec_pretty(&v3).unwrap()).unwrap();
        let e=daemon.status(TaskRefRequest{task_id:ex.task_id.clone()}).unwrap_err();
        assert_eq!(e.code,ErrorCode::VersionMismatch);
    }
    #[test] fn idempotency_and_recovery() {
        let d=tempdir().unwrap(); let w=d.path().join("ws"); let root=d.path().join("state");
        let daemon=HarnessDaemon::open(&root,DaemonPolicy::conservative(&w)).unwrap();
        let a=daemon.execute(req("r1")).unwrap();
        let mut replay=req("r1"); replay.resume_handle=a.host_resume_handle.clone();
        let b=daemon.execute(replay).unwrap();
        assert_eq!(a.task_id,b.task_id); assert!(b.resumed); drop(daemon);
        let reopened=HarnessDaemon::open(&root,DaemonPolicy::conservative(&w)).unwrap();
        let status = reopened.status(TaskRefRequest{task_id:a.task_id.clone()}).unwrap();
        assert_eq!(status.state,TaskState::Active);
        assert_eq!(status.operation, OperationStatus::ExternalHostRequired);
        assert_eq!(status.packet.branch_id, "main");
        assert_eq!(status.packet.idempotency_key, "r1");
    }
    #[test] fn read_confined_and_cancel_durable() {
        let d=tempdir().unwrap(); let w=d.path().join("ws"); fs::create_dir_all(&w).unwrap();
        fs::write(w.join("hello.txt"),"hello").unwrap();
        let daemon=HarnessDaemon::open(d.path().join("state"),DaemonPolicy::conservative(&w)).unwrap();
        let ex=daemon.execute(req("r2")).unwrap();
        let got=daemon.read(ReadRequest{task_id:ex.task_id.clone(),lease_epoch:ex.lease.epoch,
            path:"hello.txt".into(),byte_range:None}).unwrap(); assert_eq!(got.content,"hello");
        let c=daemon.cancel(CancelRequest{task_id:ex.task_id.clone(),reason:Some("stop".into())}).unwrap();
        assert_eq!(c.state,TaskState::Cancelled);
        assert_eq!(daemon.status(TaskRefRequest{task_id:ex.task_id.clone()}).unwrap().operation, OperationStatus::Aborted);
        // A path escape quarantines the custody grant outright.
        let ex2=daemon.execute(req("r3b")).unwrap();
        let e=daemon.read(ReadRequest{task_id:ex2.task_id.clone(),lease_epoch:ex2.lease.epoch,
            path:"../escape".into(),byte_range:None}).unwrap_err();
        assert_eq!(e.code,ErrorCode::ScopeDenied);
        assert!(daemon.status(TaskRefRequest{task_id:ex2.task_id}).is_ok());
    }
    #[test] fn human_stop_is_final_and_discipline_is_delivered() {
        let d=tempdir().unwrap(); let w=d.path().join("ws");
        let daemon=HarnessDaemon::open(d.path().join("state"),DaemonPolicy::conservative(&w)).unwrap();
        let mut r=req("r4"); r.operator_is_agent=false; r.host=HostKind::Human;
        let ex=daemon.execute(r).unwrap();
        // Hosts receive the Fable completion discipline at task start.
        let disc=ex.discipline.unwrap();
        assert!(disc.contains("declare completion") && disc.contains("evidence"));
        // Human cancel goes through the terminal human-stop fence.
        let c=daemon.cancel(CancelRequest{task_id:ex.task_id.clone(),reason:Some("stop button".into())}).unwrap();
        assert_eq!(c.state,TaskState::Cancelled);
        let e=daemon.next(NextRequest{task_id:ex.task_id,lease_epoch:ex.lease.epoch}).unwrap_err();
        assert_eq!(e.code,ErrorCode::TaskTerminal);
    }
    #[test] fn writes_require_trusted_launcher_approval() {
        let d=tempdir().unwrap(); let w=d.path().join("ws");
        let daemon=HarnessDaemon::open(d.path().join("state"),DaemonPolicy::conservative(&w)).unwrap();
        let ex=daemon.execute(req("r3")).unwrap();
        let e=daemon.edit(EditRequest{task_id:ex.task_id,lease_epoch:ex.lease.epoch,path:"x".into(),
            expected:None,replacement:"y".into(),create:true}).unwrap_err();
        assert_eq!(e.code,ErrorCode::ApprovalRequired);
    }    fn ultra_req(task:&str,epoch:u64,kind:UltraSubmissionKind,request_id:&str,candidate_id:&str,content:&str)->UltraSubmitRequest {
        UltraSubmitRequest{task_id:task.into(),lease_epoch:epoch,kind,request_id:request_id.into(),
            candidate_id:candidate_id.into(),
            response_hash:rex_protocol::schema::canonical_hash(&content).unwrap(),content:content.into()}
    }
    #[test] fn ultra_loop_runs_over_the_protocol_surface() {
        let d=tempdir().unwrap(); let w=d.path().join("ws");
        let daemon=HarnessDaemon::open(d.path().join("state"),DaemonPolicy::conservative(&w)).unwrap();
        let ex=daemon.execute(req("r-ultra")).unwrap();
        let open=daemon.ultra_open(UltraOpenRequest{task_id:ex.task_id.clone(),lease_epoch:ex.lease.epoch}).unwrap();
        assert_eq!(open.status,"collecting");
        assert_eq!(open.candidate_requests.len(),2);
        assert!(open.evidence_requests.is_empty());
        assert_eq!(open.candidate_requests[0].obligation_ids,vec!["step-1".to_string()]);
        let mut view=open.clone();
        for c in &open.candidate_requests {
            let content=format!("candidate answer {}",c.candidate_id);
            view=daemon.ultra_submit(ultra_req(&ex.task_id,ex.lease.epoch,UltraSubmissionKind::Candidate,
                &c.candidate_id,&c.candidate_id,&content)).unwrap();
        }
        assert_eq!(view.status,"ready");
        assert_eq!(view.kernel_state,"awaiting_evidence");
        assert_eq!(view.evidence_requests.len(),4);
        let candidate=view.evidence_requests[0].candidate_id.clone();
        let pending: Vec<UltraEvidenceRequestView> = view.evidence_requests.iter()
            .filter(|r| r.candidate_id==candidate).cloned().collect();
        for r in &pending {
            view = match r.kind.as_str() {
                "adversary" => daemon.ultra_submit(ultra_req(&ex.task_id,ex.lease.epoch,
                    UltraSubmissionKind::Adversary,&r.request_id,&r.candidate_id,"{\"defects\":[]}")).unwrap(),
                _ => daemon.ultra_submit(ultra_req(&ex.task_id,ex.lease.epoch,
                    UltraSubmissionKind::Verifier,&r.request_id,&r.candidate_id,
                    "{\"outcomes\":[{\"obligation_id\":\"step-1\",\"status\":\"proven\"}]}")).unwrap(),
            };
        }
        assert_eq!(view.kernel_state,"completed");
        let reopened=HarnessDaemon::open(d.path().join("state"),DaemonPolicy::conservative(&w)).unwrap();
        let again=reopened.ultra_open(UltraOpenRequest{task_id:ex.task_id.clone(),lease_epoch:ex.lease.epoch}).unwrap();
        assert_eq!(again.kernel_state,"completed");
        assert!(again.evidence_requests.is_empty());
    }
    #[test] fn ultra_loop_rejects_human_tasks_and_forged_submissions() {
        let d=tempdir().unwrap(); let w=d.path().join("ws");
        let daemon=HarnessDaemon::open(d.path().join("state"),DaemonPolicy::conservative(&w)).unwrap();
        let mut r=req("r-human"); r.operator_is_agent=false; r.host=HostKind::Human;
        let human=daemon.execute(r).unwrap();
        let e=daemon.ultra_open(UltraOpenRequest{task_id:human.task_id.clone(),lease_epoch:human.lease.epoch}).unwrap_err();
        assert_eq!(e.code,ErrorCode::ScopeDenied);
        let ex=daemon.execute(req("r-forge")).unwrap();
        let open=daemon.ultra_open(UltraOpenRequest{task_id:ex.task_id.clone(),lease_epoch:ex.lease.epoch}).unwrap();
        let target=&open.candidate_requests[0];
        let mut forged=ultra_req(&ex.task_id,ex.lease.epoch,UltraSubmissionKind::Candidate,
            &target.candidate_id,&target.candidate_id,"honest answer");
        forged.response_hash="deadbeef".into();
        let e=daemon.ultra_submit(forged).unwrap_err();
        assert_eq!(e.code,ErrorCode::GateFailed);
        let unknown=ultra_req(&ex.task_id,ex.lease.epoch,UltraSubmissionKind::Candidate,
            "not-a-candidate","not-a-candidate","anything");
        let e=daemon.ultra_submit(unknown).unwrap_err();
        assert_eq!(e.code,ErrorCode::GateFailed);
    }    #[test] fn ultra_open_binds_a_compiled_skill_plan_once() {
        let d=tempdir().unwrap(); let w=d.path().join("ws"); fs::create_dir_all(&w).unwrap();
        fs::write(w.join("Cargo.toml"),"[package]").unwrap();
        let daemon=HarnessDaemon::open(d.path().join("state"),DaemonPolicy::conservative(&w)).unwrap();
        let ex=daemon.execute(req("r-skills")).unwrap();
        let open=daemon.ultra_open(UltraOpenRequest{task_id:ex.task_id.clone(),lease_epoch:ex.lease.epoch}).unwrap();
        let plan=open.skill_plan.expect("skill plan bound");
        assert!(plan.selected.iter().any(|s| s.starts_with("shared-laws@")));
        assert!(plan.gates.iter().any(|g| g.id=="scope-diff" && g.required));
        let hash=plan.plan_hash.clone();
        assert!(!hash.is_empty());
        let again=daemon.ultra_open(UltraOpenRequest{task_id:ex.task_id.clone(),lease_epoch:ex.lease.epoch}).unwrap();
        assert_eq!(again.skill_plan.unwrap().plan_hash,hash);
        let events=daemon.events(EventsRequest{task_id:ex.task_id.clone(),after_seq:0,limit:None}).unwrap();
        let bindings=events.events.iter().filter(|e| e.kind=="ultra_skill_plan").count();
        assert_eq!(bindings,1,"plan binding is recorded once per hash, not per open");
    }    #[test] fn ultra_promotion_commits_the_winning_bundle_into_the_workspace() {
        let d=tempdir().unwrap(); let w=d.path().join("ws"); fs::create_dir_all(&w).unwrap();
        let daemon=HarnessDaemon::open(d.path().join("state"),DaemonPolicy::conservative(&w)).unwrap();
        let ex=daemon.execute(req("r-promote")).unwrap();
        let open=daemon.ultra_open(UltraOpenRequest{task_id:ex.task_id.clone(),lease_epoch:ex.lease.epoch}).unwrap();
        // promotion before completion fails closed
        assert!(daemon.ultra_promote(rex_protocol::UltraPromoteRequest{
            task_id:ex.task_id.clone(),lease_epoch:ex.lease.epoch}).is_err());
        let mut view=open.clone();
        for (i,c) in open.candidate_requests.iter().enumerate() {
            let content=format!("{{\"files\":[{{\"path\":\"result.txt\",\"content\":\"PASS {i}\"}}]}}");
            view=daemon.ultra_submit(ultra_req(&ex.task_id,ex.lease.epoch,UltraSubmissionKind::Candidate,
                &c.candidate_id,&c.candidate_id,&content)).unwrap();
        }
        let candidate=view.evidence_requests[0].candidate_id.clone();
        let pending: Vec<UltraEvidenceRequestView> = view.evidence_requests.iter()
            .filter(|r| r.candidate_id==candidate).cloned().collect();
        for r in &pending {
            view = match r.kind.as_str() {
                "adversary" => daemon.ultra_submit(ultra_req(&ex.task_id,ex.lease.epoch,
                    UltraSubmissionKind::Adversary,&r.request_id,&r.candidate_id,"{\"defects\":[]}")).unwrap(),
                _ => daemon.ultra_submit(ultra_req(&ex.task_id,ex.lease.epoch,
                    UltraSubmissionKind::Verifier,&r.request_id,&r.candidate_id,
                    "{\"outcomes\":[{\"obligation_id\":\"step-1\",\"status\":\"proven\"}]}")).unwrap(),
            };
        }
        assert_eq!(view.kernel_state,"completed");
        let receipt=daemon.ultra_promote(rex_protocol::UltraPromoteRequest{
            task_id:ex.task_id.clone(),lease_epoch:ex.lease.epoch}).unwrap();
        assert_eq!(receipt.state,"committed");
        assert_eq!(receipt.candidate_id,candidate);
        assert!(!receipt.staging_hash.is_empty());
        assert!(fs::read_to_string(w.join("result.txt")).unwrap().starts_with("PASS"));
        let events=daemon.events(EventsRequest{task_id:ex.task_id.clone(),after_seq:0,limit:None}).unwrap();
        assert!(events.events.iter().any(|e| e.kind=="ultra_promotion"));
        // a second promotion is deterministic and reports the same bundle
        let again=daemon.ultra_promote(rex_protocol::UltraPromoteRequest{
            task_id:ex.task_id.clone(),lease_epoch:ex.lease.epoch}).unwrap();
        assert_eq!(again.state,"committed");
        assert_eq!(again.bundle_hash,receipt.bundle_hash);
    }    #[test] fn host_resume_requires_and_rotates_the_handle() {
        let d=tempdir().unwrap(); let w=d.path().join("ws");
        let daemon=HarnessDaemon::open(d.path().join("state"),DaemonPolicy::conservative(&w)).unwrap();
        let a=daemon.execute(req("r-handle")).unwrap();
        let handle=a.host_resume_handle.clone().expect("handle issued at creation");
        assert!(handle.starts_with("hrh-"));
        // missing handle -> denied
        assert_eq!(daemon.execute(req("r-handle")).unwrap_err().code,ErrorCode::ScopeDenied);
        // wrong handle -> denied
        let mut wrong=req("r-handle"); wrong.resume_handle=Some("hrh-forged".into());
        assert_eq!(daemon.execute(wrong).unwrap_err().code,ErrorCode::ScopeDenied);
        // correct handle -> resumed and rotated
        let mut good=req("r-handle"); good.resume_handle=Some(handle.clone());
        let b=daemon.execute(good).unwrap();
        assert!(b.resumed);
        assert_eq!(b.task_id,a.task_id);
        let rotated=b.host_resume_handle.clone().expect("rotated handle issued");
        assert_ne!(rotated,handle);
        // the old handle is dead; the rotated handle works
        let mut stale=req("r-handle"); stale.resume_handle=Some(handle);
        assert_eq!(daemon.execute(stale).unwrap_err().code,ErrorCode::ScopeDenied);
        let mut current=req("r-handle"); current.resume_handle=Some(rotated);
        assert!(daemon.execute(current).unwrap().resumed);
    }    #[test] fn proof_bundle_records_the_full_verified_journey() {
        let d=tempdir().unwrap(); let w=d.path().join("ws"); fs::create_dir_all(&w).unwrap();
        let root=d.path().join("state");
        let daemon=HarnessDaemon::open(&root,DaemonPolicy::conservative(&w)).unwrap();
        let ex=daemon.execute(req("r-proof")).unwrap();
        let open=daemon.ultra_open(UltraOpenRequest{task_id:ex.task_id.clone(),lease_epoch:ex.lease.epoch}).unwrap();
        let mut view=open.clone();
        for (i,c) in open.candidate_requests.iter().enumerate() {
            let content=format!("{{\"files\":[{{\"path\":\"out.txt\",\"content\":\"v{i}\"}}]}}");
            view=daemon.ultra_submit(ultra_req(&ex.task_id,ex.lease.epoch,UltraSubmissionKind::Candidate,
                &c.candidate_id,&c.candidate_id,&content)).unwrap();
        }
        let candidate=view.evidence_requests[0].candidate_id.clone();
        for r in view.evidence_requests.iter().filter(|r| r.candidate_id==candidate).cloned().collect::<Vec<_>>() {
            view = match r.kind.as_str() {
                "adversary" => daemon.ultra_submit(ultra_req(&ex.task_id,ex.lease.epoch,
                    UltraSubmissionKind::Adversary,&r.request_id,&r.candidate_id,"{\"defects\":[]}")).unwrap(),
                _ => daemon.ultra_submit(ultra_req(&ex.task_id,ex.lease.epoch,
                    UltraSubmissionKind::Verifier,&r.request_id,&r.candidate_id,
                    "{\"outcomes\":[{\"obligation_id\":\"step-1\",\"status\":\"proven\"}]}")).unwrap(),
            };
        }
        let receipt=daemon.ultra_promote(rex_protocol::UltraPromoteRequest{
            task_id:ex.task_id.clone(),lease_epoch:ex.lease.epoch}).unwrap();
        assert_eq!(receipt.state,"committed");
        let bundle=daemon.proof_bundle(TaskRefRequest{task_id:ex.task_id.clone()}).unwrap();
        assert_eq!(bundle.kernel_state.as_deref(),Some("completed"));
        assert_eq!(bundle.qualified_candidate.as_deref(),Some(candidate.as_str()));
        assert_eq!(bundle.promotion_state.as_deref(),Some("committed"));
        assert!(bundle.skill_plan.is_some());
        assert!(!bundle.events.is_empty());
        assert!(!bundle.bundle_hash.is_empty());
        let again=daemon.proof_bundle(TaskRefRequest{task_id:ex.task_id.clone()}).unwrap();
        assert_eq!(bundle.bundle_hash,again.bundle_hash,"proof bundle hash is deterministic");
        let persisted: serde_json::Value = serde_json::from_slice(
            &fs::read(root.join("proofs").join(format!("{}.json",ex.task_id))).unwrap()).unwrap();
        assert_eq!(persisted["bundle_hash"].as_str().unwrap(),bundle.bundle_hash);
        assert_eq!(persisted["promotion_state"].as_str().unwrap(),"committed");
    }
}
