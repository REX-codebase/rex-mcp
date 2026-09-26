//! Durable local Harness daemon for caller-driven MCP work.
//!
//! The daemon has no model and no provider credentials. A subscribed host
//! agent proposes a plan and calls these methods; REX freezes the plan,
//! confines tools to one workspace, maintains custody and leases, and
//! decides completion from evidence. The trusted launcher, not an MCP
//! payload, decides whether mutations are pre-approved.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use rex_custody::capability::{hex_sha256, hmac_sha256_hex, random_hex};
use rex_custody::{
    AgentProtocol, CapabilitySet, CapabilityToken, CompletionClaim, CompletionContract,
    Consumption, CustodiedToolRuntime, CustodyAcceptance, CustodyBudgets, CustodyError,
    CustodyRegistry, EvidenceGate, GateEvaluator, GateOutcome, LeaseTerms, OperatorIdentity,
    ToolClass, WorkerMode,
};
use rex_protocol::packets::{OperationStatus, PacketIdentity};
use rex_protocol::*;
use rex_tools::{ToolRequest, ToolResult, ToolRuntime};
use rex_ultra::artifacts::{ArtifactError, ArtifactStore};
use rex_ultra::external_kernel::{CandidateResponse, EvidenceKind, HostKernelStatus, KernelState};
use rex_ultra::host_bridge::{
    BridgeError, UltraHostBridge, UltraHostView, DEFAULT_MINIMUM_CANDIDATES,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

mod lease_keeper;
pub use lease_keeper::{LeaseKeeper, LeaseKeeperConfig, LeaseRenewalReport, RenewedLease};
mod lease_failsafe;
pub use lease_failsafe::{FailsafeReport, PauseReason, PauseRecord, PauseStatus};

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
        Self {
            workspace: workspace.into(),
            approve_task_mutations: false,
            max_tool_calls: 80,
            max_wall_ms: 20 * 60 * 1000,
        }
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
    #[serde(default)]
    ultra: bool,
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
    /// SHA-256 of the per-task operational capability (protocol 2.0). The
    /// capability itself is only ever held by the task's operator; every
    /// operational call must present it. Empty means a pre-2.0 record,
    /// which can never match a presented capability: it fails closed.
    #[serde(default)]
    task_capability_hash: String,
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
    /// SHA-256 of the human-stop token. The token itself lives only in
    /// `<root>/human-stop-token` (0600) for the trusted local launcher;
    /// the daemon never stores it in task state or logs.
    human_token_hash: String,
    /// Content-addressed immutable artifact store under `<root>/evidence`.
    artifacts: ArtifactStore,
}

/// Project a compiled skill plan into the protocol view. `executable`
/// tells the UI exactly which gates the daemon will run at promotion and
/// which are advisory-only - no "bound" pack implies an enforced gate.
fn skill_plan_view(plan: rex_ultra::skills::CompiledSkillPlan) -> SkillPlanView {
    SkillPlanView {
        plan_hash: plan.plan_hash,
        compiler_version: plan.compiler_version,
        selected: plan
            .selected
            .iter()
            .map(|p| format!("{}@{}", p.id, p.version))
            .collect(),
        gates: plan
            .gates
            .iter()
            .map(|g| SkillGateView {
                pack: g.pack.clone(),
                id: g.id.clone(),
                command_hint: g.command_hint.clone(),
                required: g.required,
                executable: !matches!(g.exec, rex_ultra::skills::GateExec::Unsupported),
            })
            .collect(),
        unsupported: plan.unsupported,
    }
}

impl HarnessDaemon {
    pub fn open(root: impl Into<PathBuf>, mut policy: DaemonPolicy) -> Result<Self, ProtocolError> {
        let root = root.into();
        fs::create_dir_all(root.join("tasks")).map_err(internal)?;
        fs::create_dir_all(root.join("custody")).map_err(internal)?;
        fs::create_dir_all(&policy.workspace).map_err(internal)?;
        policy.workspace = fs::canonicalize(&policy.workspace).map_err(internal)?;
        let custody = Arc::new(Mutex::new(
            CustodyRegistry::recover(root.join("custody"), now_ms()).map_err(custody_err)?,
        ));
        let runtime = ToolRuntime::new(&policy.workspace).map_err(|e| internal(e.detail))?;
        let tools = CustodiedToolRuntime::new(runtime, custody.clone());
        let human_token_hash = load_or_create_human_token(&root)?;
        let artifacts = ArtifactStore::open(&root).map_err(|e| internal(e.to_string()))?;
        let daemon = Self {
            root,
            policy,
            custody,
            tools,
            human_token_hash,
            artifacts,
        };
        // Heal any crash window between a promotion commit and the task's
        // terminal persist before serving new calls.
        daemon.reconcile_ultra_tasks();
        // Grants that recover() suspended need their durable pause record even
        // when the lease keeper is disabled.
        let _ = daemon.pause_lapsed_leases(now_ms());
        Ok(daemon)
    }

    /// The final human Stop: terminal fence for any task, gated on the
    /// human-stop token that only the trusted local launcher can read.
    /// Distinct from operator `cancel`, which requires the task capability.
    pub fn human_stop(&self, req: HumanStopRequest) -> Result<CancelResponse, ProtocolError> {
        if hex_sha256(req.human_token.as_bytes()) != self.human_token_hash {
            return Err(perr(
                ErrorCode::Unauthorized,
                "invalid human-stop token",
                &req.task_id,
            ));
        }
        let mut t = self.load(&req.task_id)?;
        if t.state.is_terminal() {
            return Ok(CancelResponse {
                task_id: t.task_id,
                state: t.state,
                final_reason: t.terminal_reason.unwrap_or_else(|| "terminal".into()),
            });
        }
        {
            let mut reg = self
                .custody
                .lock()
                .map_err(|_| internal("custody registry poisoned"))?;
            // The human stop is final in every phase, never gated on the
            // operator's state, even for an agent-operated task.
            reg.human_stop(&t.grant_id, now_ms()).map_err(custody_err)?;
        }
        t.operation_status = OperationStatus::Aborted;
        t.state = TaskState::Cancelled;
        let why = req.reason.unwrap_or_else(|| "human stop (final)".into());
        t.terminal_reason = Some(why.clone());
        t.open_action = None;
        self.append_event(&mut t, "human_stop", json!({"reason":why}))?;
        self.persist(&t)?;
        Ok(CancelResponse {
            task_id: t.task_id,
            state: t.state,
            final_reason: why,
        })
    }

    pub fn execute(&self, req: ExecuteRequest) -> Result<ExecuteResponse, ProtocolError> {
        validate_execute(&req)?;
        if let Some(id) = &req.task_id {
            let mut task = self.load(id)?;
            if task.task != req.task {
                return Err(perr(
                    ErrorCode::IdempotencyConflict,
                    "task_id exists with different task text",
                    id,
                ));
            }
            // Verify, gate, then rotate: a resume the lease gate refuses must
            // not burn the host's only resume handle.
            self.verify_resume_handle(&task, &req)?;
            let renewed = self.renew_lease_on_resume(&mut task)?;
            let handle = self.rotate_resume_handle(&mut task)?;
            let capability = self.rotate_capability(&mut task)?;
            if renewed {
                self.append_event(&mut task, "lease_renewed_on_resume", json!({}))?;
            }
            if let Some(follow_up) = req
                .follow_up
                .as_deref()
                .filter(|text| !text.trim().is_empty())
            {
                self.append_event(&mut task, "host_follow_up", json!({ "text": follow_up }))?;
            }
            self.persist(&task)?;
            return Ok(self.execute_view(&task, true, Some(handle), Some(capability)));
        }
        if let Some(task) = self.find_by_request(&req.request_id)? {
            if task.request_hash != request_hash(&req)? {
                return Err(perr(
                    ErrorCode::IdempotencyConflict,
                    "request_id was already used with another payload",
                    &task.task_id,
                ));
            }
            let mut task = task;
            // Verify, gate, then rotate: a resume the lease gate refuses must
            // not burn the host's only resume handle.
            self.verify_resume_handle(&task, &req)?;
            let renewed = self.renew_lease_on_resume(&mut task)?;
            let handle = self.rotate_resume_handle(&mut task)?;
            let capability = self.rotate_capability(&mut task)?;
            if renewed {
                self.append_event(&mut task, "lease_renewed_on_resume", json!({}))?;
            }
            if let Some(follow_up) = req
                .follow_up
                .as_deref()
                .filter(|text| !text.trim().is_empty())
            {
                self.append_event(&mut task, "host_follow_up", json!({ "text": follow_up }))?;
            }
            self.persist(&task)?;
            return Ok(self.execute_view(&task, true, Some(handle), Some(capability)));
        }
        let now = now_ms();
        let task_id = format!("task-{}", random_id());
        let host_resume_handle = format!("hrh-{}", random_hex(24));
        let task_capability = format!("cap-{}", random_hex(24));
        let plan = req
            .plan
            .clone()
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| {
                vec![PlanStep {
                    instructions: req.task.clone(),
                    acceptance: req.proof.clone(),
                }]
            });
        if plan.len() > MAX_PLAN_STEPS {
            return Err(ProtocolError::new(
                ErrorCode::MalformedRequest,
                "plan exceeds 100 steps",
            ));
        }
        let max_tools = req
            .budgets
            .as_ref()
            .and_then(|b| b.max_tool_calls)
            .unwrap_or(self.policy.max_tool_calls)
            .min(self.policy.max_tool_calls);
        let max_wall = req
            .budgets
            .as_ref()
            .and_then(|b| b.max_wall_ms)
            .unwrap_or(self.policy.max_wall_ms)
            .min(self.policy.max_wall_ms);
        let caps = capability_set(&self.policy.workspace);
        let operator = if req.operator_is_agent {
            OperatorIdentity::Agent(CustodyRegistry::register_agent(
                host_label(req.host),
                AgentProtocol::Mcp {
                    client: host_label(req.host).into(),
                    version: PROTOCOL_VERSION.into(),
                },
            ))
        } else {
            OperatorIdentity::Human
        };
        let budgets = CustodyBudgets {
            max_steps: plan.len() as u64 + 8,
            max_tool_calls: max_tools,
            max_wall_ms: max_wall,
            max_tokens: 0,
        };
        let lease_terms = LeaseTerms {
            lease_ms: DEFAULT_LEASE_MS,
            heartbeat_interval_ms: 30_000,
            resume_grace_ms: 15 * 60 * 1000,
        };
        let contract = CompletionContract {
            gates: vec![
                EvidenceGate::NoPendingApprovals,
                EvidenceGate::WithinScopeChanges,
                EvidenceGate::Custom {
                    name: "all_plan_steps_accepted".into(),
                },
            ],
            max_claim_attempts: 2,
        };
        let (token, grant) = {
            let mut reg = self
                .custody
                .lock()
                .map_err(|_| internal("custody registry poisoned"))?;
            let offer = reg
                .offer(
                    &task_id,
                    &req.task,
                    operator,
                    WorkerMode::ExternalAgent,
                    caps,
                    budgets,
                    lease_terms,
                    contract,
                    now,
                )
                .map_err(custody_err)?;
            let acceptance = CustodyAcceptance {
                offer_id: offer.offer_id.clone(),
                nonce_echo: offer.nonce.clone(),
                commitment: offer.expected_acceptance(),
            };
            reg.accept(&acceptance, now).map_err(custody_err)?
        };
        let action = make_action(&task_id, 0, &plan[0], max_wall);
        let req_hash = request_hash(&req)?;
        let mut task = DurableTask {
            protocol_version: PROTOCOL_VERSION.into(),
            task_id: task_id.clone(),
            request_id: req.request_id,
            request_hash: req_hash,
            task: req.task,
            host: req.host,
            operator_is_agent: req.operator_is_agent,
            ultra: req.ultra,
            plan_hash: hash_json(&plan)?,
            plan,
            cursor: 0,
            state: TaskState::Active,
            open_action: Some(action),
            token,
            grant_id: grant.grant_id,
            lease_epoch: grant.lease.epoch,
            lease_expires_ms: grant.lease.expires_ms,
            heartbeat_seq: grant.lease.next_seq,
            heartbeat_interval_ms: grant.lease.heartbeat_interval_ms,
            max_tool_calls: max_tools,
            used_tool_calls: 0,
            max_wall_ms: max_wall,
            created_ms: now,
            last_event_seq: 0,
            proof: req.proof,
            evidence: BTreeMap::new(),
            registered_evidence: BTreeSet::new(),
            result: None,
            terminal_reason: None,
            branch_id: "main".into(),
            resume_nonce: grant.lease.next_seq,
            operation_status: if req.operator_is_agent {
                OperationStatus::ExternalHostRequired
            } else {
                OperationStatus::Prepared
            },
            ultra_skill_plan_hash: None,
            host_resume_handle_hash: Some(hex_sha256(host_resume_handle.as_bytes())),
            task_capability_hash: hex_sha256(task_capability.as_bytes()),
            store_schema_version: STORE_SCHEMA_VERSION,
        };
        let plan_hash = task.plan_hash.clone();
        let step_count = task.plan.len();
        self.append_event(
            &mut task,
            "task_created",
            json!({"plan_hash":plan_hash,
            "steps":step_count,"protocol":PROTOCOL_VERSION,"ultra":req.ultra}),
        )?;
        self.persist(&task)?;
        Ok(self.execute_view(
            &task,
            false,
            Some(host_resume_handle),
            Some(task_capability),
        ))
    }

    pub fn next(&self, req: NextRequest) -> Result<NextResponse, ProtocolError> {
        let mut t = self.live(&req.task_id, req.lease_epoch, &req.capability)?;
        self.heartbeat(&mut t)?;
        self.persist(&t)?;
        Ok(NextResponse {
            state: t.state,
            next: t.open_action.clone(),
            lease: lease_view(&t),
        })
    }

    pub fn read(&self, req: ReadRequest) -> Result<ReadResponse, ProtocolError> {
        let mut t = self.live(&req.task_id, req.lease_epoch, &req.capability)?;
        let result = self.call_tool(
            &mut t,
            ToolRequest::ReadFile {
                path: req.path,
                offset: None,
                limit: None,
            },
        )?;
        let mut content = result.output.unwrap_or_default();
        if let Some((start, len)) = req.byte_range {
            let bytes = content.as_bytes();
            let a = (start as usize).min(bytes.len());
            let b = a.saturating_add(len as usize).min(bytes.len());
            content = String::from_utf8_lossy(&bytes[a..b]).into_owned();
        }
        let bytes = content.len() as u64;
        Ok(ReadResponse {
            content,
            truncated: result.receipt.output_truncated,
            bytes,
            receipt: Some(result.call_id),
        })
    }

    pub fn edit(&self, req: EditRequest) -> Result<EditResponse, ProtocolError> {
        let mut t = self.live(&req.task_id, req.lease_epoch, &req.capability)?;
        let tool = if req.create {
            ToolRequest::CreateFile {
                path: req.path,
                content: req.replacement,
                overwrite: false,
            }
        } else {
            ToolRequest::EditFile {
                path: req.path,
                expected: req.expected.ok_or_else(|| {
                    ProtocolError::new(
                        ErrorCode::MalformedRequest,
                        "expected is required unless create=true",
                    )
                })?,
                replacement: req.replacement,
                replace_all: false,
            }
        };
        let out = self.call_tool(&mut t, tool)?;
        Ok(EditResponse {
            receipt: out.call_id,
            bytes_written: out.receipt.bytes_written,
        })
    }

    pub fn search(&self, req: SearchRequest) -> Result<SearchResponse, ProtocolError> {
        let mut t = self.live(&req.task_id, req.lease_epoch, &req.capability)?;
        let out = self.call_tool(
            &mut t,
            ToolRequest::SearchFiles {
                query: req.query,
                path: None,
                max_results: req.max_results,
                regex: None,
                include: None,
            },
        )?;
        Ok(SearchResponse {
            hits: parse_search(&out.output.unwrap_or_default()),
            receipt: Some(out.call_id),
        })
    }

    pub fn run(&self, req: RunRequest) -> Result<RunResponse, ProtocolError> {
        let mut t = self.live(&req.task_id, req.lease_epoch, &req.capability)?;
        let out = self.call_tool(
            &mut t,
            ToolRequest::RunCommand {
                argv: req.argv,
                cwd: Some(".".into()),
                timeout_ms: req.timeout_ms,
            },
        )?;
        let (stdout, stderr) = split_output(out.output.as_deref().unwrap_or(""));
        Ok(RunResponse {
            exit_code: out.receipt.exit_code,
            stdout,
            stderr,
            output_truncated: out.receipt.output_truncated,
            receipt: Some(out.call_id),
        })
    }

    pub fn test(&self, req: TestRequest) -> Result<TestResponse, ProtocolError> {
        let argv = match req.recipe.as_str() {
            "cargo-test" => vec!["cargo".into(), "test".into(), "--workspace".into()],
            "npm-test" => vec!["npm".into(), "test".into()],
            _ => {
                return Err(ProtocolError::new(
                    ErrorCode::ScopeDenied,
                    "unknown test recipe; allowed: cargo-test, npm-test",
                ))
            }
        };
        let run = self.run(RunRequest {
            task_id: req.task_id.clone(),
            capability: req.capability.clone(),
            lease_epoch: req.lease_epoch,
            argv,
            // run_command cuts every request to its cap, so ask for the cap
            // itself rather than a 10-minute limit the recipe never gets.
            timeout_ms: Some(rex_tools::MAX_TIMEOUT_MS),
        })?;
        let passed = run.exit_code == Some(0);
        let evidence_id = format!("evidence-{}", random_id());
        let mut t = self.load(&req.task_id)?;
        t.registered_evidence.insert(evidence_id.clone());
        t.evidence
            .insert(req.recipe.clone(), format!("{}:{}", evidence_id, passed));
        self.append_event(
            &mut t,
            "test_finished",
            json!({"recipe":req.recipe,
            "passed":passed,"evidence_id":evidence_id}),
        )?;
        self.persist(&t)?;
        Ok(TestResponse {
            recipe: req.recipe,
            passed,
            summary: if passed {
                "passed".into()
            } else {
                format!("failed: {}", run.stderr)
            },
            evidence_id,
        })
    }

    pub fn submit(&self, req: SubmitRequest) -> Result<SubmitResponse, ProtocolError> {
        let t0 = self.live(&req.task_id, req.lease_epoch, &req.capability)?;
        // State-machine join (audit finding 2): an Ultra task can never
        // complete, or advance, through the Standard submission path. Its
        // only terminal route is a rebuilt, fully gated Ultra promotion.
        if t0.ultra {
            return Err(perr(
                ErrorCode::GateFailed,
                "ultra tasks cannot use rex_submit; completion is only possible through a gated Ultra promotion",
                &t0.task_id,
            ));
        }
        let mut t = t0;
        let open = t
            .open_action
            .clone()
            .ok_or_else(|| perr(ErrorCode::TaskTerminal, "no open action", &t.task_id))?;
        if open.action_id != req.action_id {
            return Err(perr(
                ErrorCode::LeaseConflict,
                "action is stale or belongs to another task",
                &t.task_id,
            ));
        }
        t.state = TaskState::Verifying;
        let narrative = req.narrative.clone();
        let evidence = req.evidence.clone();
        let cited: Vec<String> = req.evidence.values().cloned().collect();
        self.append_event(
            &mut t,
            "action_submitted",
            json!({"action_id":req.action_id,
            "narrative":narrative,"evidence":evidence}),
        )?;
        t.evidence.extend(req.evidence);
        t.cursor += 1;
        if t.cursor < t.plan.len() {
            t.state = TaskState::Active;
            let next = make_action(&t.task_id, t.cursor, &t.plan[t.cursor], t.max_wall_ms);
            t.open_action = Some(next.clone());
            self.append_event(
                &mut t,
                "action_accepted",
                json!({"action_id":open.action_id}),
            )?;
            self.persist(&t)?;
            return Ok(SubmitResponse {
                state: t.state,
                accepted: true,
                repair: None,
                next: Some(next),
            });
        }
        // Fable completion gate: the claim may only cite evidence the
        // harness actually registered, and it must cite some.
        if let Err(failures) = rex_prompt::gate::validate_completion_claim(
            &req.narrative,
            &cited,
            &t.registered_evidence,
        ) {
            t.state = TaskState::Active;
            t.cursor = t.plan.len() - 1;
            t.open_action = Some(open.clone());
            let repair = failures.join("; ");
            self.append_event(&mut t, "completion_rejected", json!({"failures":failures}))?;
            self.persist(&t)?;
            return Ok(SubmitResponse {
                state: t.state,
                accepted: false,
                repair: Some(repair),
                next: Some(open),
            });
        }
        t.open_action = None;
        let evaluator = FinalEvaluator { all_steps: true };
        let claim = CompletionClaim {
            summary: req.narrative.clone(),
        };
        let completion = self
            .custody
            .lock()
            .map_err(|_| internal("custody registry poisoned"))?
            .claim_completion(&t.token, &claim, &evaluator, now_ms());
        match completion {
            Ok(_) => {
                t.operation_status = OperationStatus::Committed;
                t.state = TaskState::Completed;
                t.result = Some(req.narrative);
                t.terminal_reason = Some("verified completion".into());
                let ev = t.evidence.clone();
                self.append_event(&mut t, "task_completed", json!({"evidence":ev}))?;
                self.persist(&t)?;
                Ok(SubmitResponse {
                    state: t.state,
                    accepted: true,
                    repair: None,
                    next: None,
                })
            }
            Err(e) => {
                t.state = TaskState::Active;
                t.cursor = t.plan.len() - 1;
                t.open_action = Some(open.clone());
                self.append_event(
                    &mut t,
                    "completion_rejected",
                    json!({"error":e.to_string()}),
                )?;
                self.persist(&t)?;
                Ok(SubmitResponse {
                    state: t.state,
                    accepted: false,
                    repair: Some(e.to_string()),
                    next: Some(open),
                })
            }
        }
    }

    pub fn status(&self, req: TaskRefRequest) -> Result<StatusResponse, ProtocolError> {
        let mut t = self.load(&req.task_id)?;
        self.sync_lease_from_custody(&mut t)?;
        let lease = lease_view(&t);
        let used_wall = elapsed(&t);
        // A task the failsafe paused reports Stale until it is resumed. This
        // path is read-only: the pause record is never written from here.
        let paused = self
            .read_pause_record(&t.task_id)?
            .is_some_and(|rec| rec.status == PauseStatus::Paused);
        let operation = if paused {
            OperationStatus::Stale
        } else {
            status_operation(&t)
        };
        let packet =
            PacketIdentity::new(&t.branch_id, t.lease_epoch, t.resume_nonce, &t.request_id);
        Ok(StatusResponse {
            task_id: t.task_id,
            state: t.state,
            task: t.task,
            operator_is_agent: t.operator_is_agent,
            host: t.host,
            lease,
            open_action: t.open_action,
            budgets: BudgetView {
                max_tool_calls: t.max_tool_calls,
                used_tool_calls: t.used_tool_calls,
                max_wall_ms: t.max_wall_ms,
                used_wall_ms: used_wall,
            },
            last_event_seq: t.last_event_seq,
            operation,
            packet,
        })
    }

    pub fn events(&self, req: EventsRequest) -> Result<EventsResponse, ProtocolError> {
        let t = self.load(&req.task_id)?;
        let path = self.task_dir(&t.task_id).join("events.jsonl");
        let data = fs::read_to_string(path).unwrap_or_default();
        let limit = req.limit.unwrap_or(100).min(1000);
        let events: Vec<TaskEvent> = data
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .filter(|e: &TaskEvent| e.seq > req.after_seq)
            .take(limit)
            .collect();
        Ok(EventsResponse {
            events,
            last_seq: t.last_event_seq,
        })
    }

    pub fn result(&self, req: TaskRefRequest) -> Result<ResultResponse, ProtocolError> {
        let t = self.load(&req.task_id)?;
        if !t.state.is_terminal() {
            return Err(perr(
                ErrorCode::NoResult,
                "task is not terminal",
                &t.task_id,
            ));
        }
        Ok(ResultResponse {
            task_id: t.task_id,
            state: t.state,
            output: t.result,
            proof_bundle: if t.evidence.is_empty() {
                None
            } else {
                Some(t.evidence)
            },
            terminal_reason: t.terminal_reason,
        })
    }

    pub fn cancel(&self, req: CancelRequest) -> Result<CancelResponse, ProtocolError> {
        // Operator cancellation: the per-task capability is the authority.
        // The distinct final human Stop is `human_stop`, a separate route.
        let t0 = self.load(&req.task_id)?;
        if t0.task_capability_hash.is_empty()
            || hex_sha256(req.capability.as_bytes()) != t0.task_capability_hash
        {
            return Err(perr(
                ErrorCode::Unauthorized,
                "invalid task capability",
                &req.task_id,
            ));
        }
        let mut t = t0;
        if t.state.is_terminal() {
            return Ok(CancelResponse {
                task_id: t.task_id,
                state: t.state,
                final_reason: t.terminal_reason.unwrap_or_else(|| "terminal".into()),
            });
        }
        {
            let mut reg = self
                .custody
                .lock()
                .map_err(|_| internal("custody registry poisoned"))?;
            if t.operator_is_agent {
                reg.operator_cancel(&t.token, now_ms())
                    .map_err(custody_err)?;
            } else {
                // The human stop button: terminal fence in every phase,
                // never gated on operator state.
                reg.human_stop(&t.grant_id, now_ms()).map_err(custody_err)?;
            }
        }
        t.operation_status = OperationStatus::Aborted;
        t.state = TaskState::Cancelled;
        let why = req.reason.unwrap_or_else(|| "cancelled by operator".into());
        t.terminal_reason = Some(why.clone());
        t.open_action = None;
        self.append_event(&mut t, "task_cancelled", json!({"reason":why}))?;
        self.persist(&t)?;
        Ok(CancelResponse {
            task_id: t.task_id,
            state: t.state,
            final_reason: why,
        })
    }

    /// Single entry point shared by the MCP server and tests: deserialize
    /// the tool arguments, run the typed method, serialize the response.
    /// rex_artifact_put: anchor evidence bytes in the content-addressed
    /// immutable artifact store. Hosts may store evidence; they cannot
    /// alter it afterwards, and reusing one digest across candidates or
    /// rounds is rejected. Later gates cite the returned digest and the
    /// daemon re-hashes stored bytes when resolving it.
    pub fn artifact_put(
        &self,
        req: ArtifactPutRequest,
    ) -> Result<ArtifactPutResponse, ProtocolError> {
        let mut t = self.live(&req.task_id, req.lease_epoch, &req.capability)?;
        if req.kind.trim().is_empty() || req.kind.len() > 64 {
            return Err(perr(
                ErrorCode::MalformedRequest,
                "artifact kind must be 1-64 chars",
                &req.task_id,
            ));
        }
        let bytes = B64.decode(req.bytes_base64.as_bytes()).map_err(|e| {
            perr(
                ErrorCode::MalformedRequest,
                format!("bytes_base64 is not valid base64: {e}"),
                &req.task_id,
            )
        })?;
        let (binding, fresh) = self
            .artifacts
            .put(
                &t.task_id,
                req.kind.trim(),
                &bytes,
                req.candidate_id.clone(),
                req.round,
            )
            .map_err(|e| match e {
                ArtifactError::TooLarge { .. } | ArtifactError::Io(_) => {
                    perr(ErrorCode::MalformedRequest, e.to_string(), &req.task_id)
                }
                ArtifactError::ReusedDigest { .. } => {
                    perr(ErrorCode::IdempotencyConflict, e.to_string(), &req.task_id)
                }
                ArtifactError::Tampered { .. } | ArtifactError::Missing { .. } => {
                    perr(ErrorCode::Internal, e.to_string(), &req.task_id)
                }
            })?;
        self.append_event(
            &mut t,
            "artifact_registered",
            serde_json::json!({
                "sha256": binding.sha256,
                "kind": binding.kind,
                "bytes": binding.bytes,
                "candidate_id": binding.candidate_id,
                "round": binding.round,
                "fresh": fresh,
            }),
        )?;
        self.persist(&t)?;
        Ok(ArtifactPutResponse {
            sha256: binding.sha256,
            bytes: binding.bytes,
            candidate_id: binding.candidate_id,
            round: binding.round,
            fresh,
            recorded_ms: binding.recorded_ms,
        })
    }

    pub fn dispatch(&self, tool: ToolName, args: Value) -> Result<Value, ProtocolError> {
        fn parse<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, ProtocolError> {
            serde_json::from_value(v).map_err(|e| {
                ProtocolError::new(ErrorCode::MalformedRequest, format!("bad arguments: {e}"))
            })
        }
        macro_rules! go {
            ($args:expr, $m:ident) => {{
                let req = parse($args)?;
                serde_json::to_value(self.$m(req)?).map_err(internal)
            }};
        }
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
            ToolName::ProofVerify => go!(args, verify_proof_bundle),
            ToolName::HumanStop => go!(args, human_stop),
            ToolName::ArtifactPut => go!(args, artifact_put),
        }
    }

    /// Open the Ultra external-host loop for a live agent-operated task.
    /// The first open attaches the host at the task's lease epoch; later
    /// opens are pure views over the durable kernel.
    pub fn ultra_open(&self, req: UltraOpenRequest) -> Result<UltraViewResponse, ProtocolError> {
        let mut t = self.live(&req.task_id, req.lease_epoch, &req.capability)?;
        require_agent(&t)?;
        require_ultra(&t)?;
        let bridge = UltraHostBridge::open(&self.root).map_err(|e| bridge_err(&t.task_id, e))?;
        let contract = if bridge.exists(&t.task_id) {
            let frozen = bridge
                .frozen_contract(&t.task_id)
                .map_err(|e| bridge_err(&t.task_id, e))?;
            if let Some(draft) = req.contract_draft.as_deref() {
                let reparsed =
                    rex_ultra::contract::parse_contract(draft, &t.task).map_err(|errors| {
                        perr(
                            ErrorCode::MalformedRequest,
                            format!("contract draft is invalid: {}", errors.join("; ")),
                            &t.task_id,
                        )
                    })?;
                if hash_json(&reparsed)? != hash_json(&frozen)? {
                    return Err(perr(
                        ErrorCode::IdempotencyConflict,
                        "contract draft does not match the contract frozen at ultra_open",
                        &t.task_id,
                    ));
                }
            }
            frozen
        } else {
            // First open freezes the contract. A draft is mandatory and
            // every proof must be daemon-executable; host-judged behavior
            // prose fails closed here rather than at promotion.
            let draft = req.contract_draft.as_deref().ok_or_else(|| {
                perr(
                    ErrorCode::MalformedRequest,
                    "ultra_open requires contract_draft: a JSON acceptance contract whose obligations all carry daemon-executable proofs (file_exists/file_contains/command_succeeds/command_output_contains)",
                    &t.task_id,
                )
            })?;
            let parsed = rex_ultra::contract::parse_contract(draft, &t.task).map_err(|errors| {
                perr(
                    ErrorCode::MalformedRequest,
                    format!("contract draft is invalid: {}", errors.join("; ")),
                    &t.task_id,
                )
            })?;
            let non_executable = rex_ultra::contract::non_executable_obligations(&parsed);
            if !non_executable.is_empty() {
                return Err(perr(
                    ErrorCode::MalformedRequest,
                    format!(
                        "obligations with host-judged behavior proofs are not accepted on the external path: {}",
                        non_executable.join(", ")
                    ),
                    &t.task_id,
                ));
            }
            self.append_event(
                &mut t,
                "ultra_contract_frozen",
                json!({"contract_hash": hash_json(&parsed)?}),
            )?;
            self.persist(&t)?;
            parsed
        };
        let view = bridge
            .open_requests(
                &t.task_id,
                &contract,
                DEFAULT_MINIMUM_CANDIDATES,
                t.lease_epoch,
            )
            .map_err(|e| bridge_err(&t.task_id, e))?;
        let mut view = view;
        if matches!(view.kernel_state, KernelState::AwaitingEvidence) {
            self.run_daemon_verifier(&t, &bridge, &contract)?;
            view = bridge
                .current_view(&t.task_id)
                .map_err(|e| bridge_err(&t.task_id, e))?;
        }
        // The compiled skill plan is frozen exactly once, at first open;
        // later opens load the frozen plan and check it against the task
        // record. Promotion enforces this plan's gates, never a recompute
        // over a workspace the candidates already changed.
        let plan = match bridge
            .frozen_skill_plan(&t.task_id)
            .map_err(|e| bridge_err(&t.task_id, e))?
        {
            Some(frozen) => {
                if t.ultra_skill_plan_hash.as_deref() != Some(frozen.plan_hash.as_str()) {
                    return Err(perr(
                        ErrorCode::Internal,
                        "frozen skill plan does not match the task record",
                        &t.task_id,
                    ));
                }
                Some(skill_plan_view(frozen))
            }
            None => {
                let compiled = self.compile_skill_plan();
                if let Some(compiled) = compiled {
                    bridge
                        .freeze_skill_plan(&t.task_id, &compiled)
                        .map_err(|e| bridge_err(&t.task_id, e))?;
                    t.ultra_skill_plan_hash = Some(compiled.plan_hash.clone());
                    let view_for_event = skill_plan_view(compiled);
                    self.append_event(
                        &mut t,
                        "ultra_skill_plan",
                        json!({"plan_hash":view_for_event.plan_hash,
                        "selected":view_for_event.selected,"unsupported":view_for_event.unsupported}),
                    )?;
                    self.persist(&t)?;
                    Some(view_for_event)
                } else {
                    None
                }
            }
        };
        Ok(ultra_view(t.task_id.clone(), &view, plan))
    }

    /// Submit one candidate response or one adversary/verifier evidence item.
    /// The kernel alone decides whether the evidence gates a transition.
    pub fn ultra_submit(
        &self,
        req: UltraSubmitRequest,
    ) -> Result<UltraViewResponse, ProtocolError> {
        let mut t = self.live(&req.task_id, req.lease_epoch, &req.capability)?;
        require_agent(&t)?;
        require_ultra(&t)?;
        let bridge = UltraHostBridge::open(&self.root).map_err(|e| bridge_err(&t.task_id, e))?;
        if !bridge.exists(&t.task_id) {
            return Err(perr(
                ErrorCode::GateFailed,
                "ultra kernel is not open for this task; call rex_ultra_open with a contract draft first",
                &t.task_id,
            ));
        }
        let contract = bridge
            .frozen_contract(&t.task_id)
            .map_err(|e| bridge_err(&t.task_id, e))?;
        let mut view = match req.kind {
            UltraSubmissionKind::Candidate => {
                // Candidates are sealed file bundles, materialized by the
                // daemon into an isolated per-candidate workspace before
                // any check runs. Prose answers are rejected here.
                let candidate_root =
                    self.materialize_candidate(&t.task_id, &req.request_id, &req.content)?;
                match bridge.record_response(
                    &t.task_id,
                    &contract,
                    CandidateResponse {
                        candidate_id: req.request_id.clone(),
                        response_hash: req.response_hash,
                        content: req.content,
                    },
                ) {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = fs::remove_dir_all(&candidate_root);
                        return Err(bridge_err(&t.task_id, e));
                    }
                }
            }
            UltraSubmissionKind::Adversary => {
                // Adversary scans are executed by the daemon walking each
                // candidate workspace, never reported by the host whose
                // work is under review.
                return Err(perr(
                    ErrorCode::GateFailed,
                    "adversary evidence is executed by the daemon; hosts cannot submit verdicts",
                    &t.task_id,
                ));
            }
            UltraSubmissionKind::Verifier => {
                // Verifier outcomes come from the daemon executing the
                // frozen contract inside each candidate workspace, never
                // from the host whose work is being verified.
                return Err(perr(
                    ErrorCode::GateFailed,
                    "verifier evidence is executed by the daemon; hosts cannot submit verdicts",
                    &t.task_id,
                ));
            }
            UltraSubmissionKind::Visual => bridge
                .record_visual(
                    &t.task_id,
                    &contract,
                    rex_ultra::external_kernel::VisualEvidence {
                        request_id: req.request_id.clone(),
                        candidate_id: req.candidate_id,
                        response_hash: req.response_hash,
                        content: req.content,
                    },
                )
                .map_err(|e| bridge_err(&t.task_id, e))?,
        };
        // Whenever the evidence stage is open, the daemon runs the contract
        // proofs itself for every candidate still missing a verifier record.
        if matches!(view.kernel_state, KernelState::AwaitingEvidence) {
            self.run_daemon_verifier(&t, &bridge, &contract)?;
            view = bridge
                .current_view(&t.task_id)
                .map_err(|e| bridge_err(&t.task_id, e))?;
        }
        self.append_event(
            &mut t,
            "ultra_submission",
            json!({"kind":format!("{:?}",req.kind),
            "kernel_state":format!("{:?}",view.kernel_state)}),
        )?;
        self.persist(&t)?;
        // The joined state machine: a kernel failure is a task failure,
        // and a committed promotion is task completion - replayed from the
        // durable records on every transition.
        self.join_ultra_terminal(&mut t)?;
        let view = bridge
            .current_view(&t.task_id)
            .map_err(|e| bridge_err(&t.task_id, e))?;
        let plan = self.task_skill_plan_view(&t.task_id);
        Ok(ultra_view(t.task_id.clone(), &view, plan))
    }

    /// Fast authorization check for a host-visible asynchronous promotion.
    /// The worker calls ultra_promote again so state changes after this check
    /// still fail closed under the daemon's normal rules.
    pub fn validate_ultra_promotion(
        &self,
        req: &rex_protocol::UltraPromoteRequest,
    ) -> Result<(), ProtocolError> {
        let task = self.live(&req.task_id, req.lease_epoch, &req.capability)?;
        require_agent(&task)?;
        require_ultra(&task)?;
        Ok(())
    }

    /// Promote the qualified candidate into the task workspace. Live lease,
    /// agent-operated tasks only; the kernel must be completed and the
    /// rollback path is verified by the promotion store.
    pub fn ultra_promote(
        &self,
        req: rex_protocol::UltraPromoteRequest,
    ) -> Result<rex_protocol::UltraPromoteResponse, ProtocolError> {
        // The rebuild is in place: daemon-executed evidence, isolated
        // candidate workspaces, a joined crash-safe state machine, and
        // promotion that re-executes every mandatory gate on a confined
        // stage before one atomic swap. Promotion runs through the same
        // joined path as the internal caller.
        let receipt = self.promote_and_join(&req)?;
        let state = match receipt.state {
            rex_ultra::promotion::PromotionState::Prepared => "prepared",
            rex_ultra::promotion::PromotionState::Committed => "committed",
            rex_ultra::promotion::PromotionState::RolledBack => "rolled_back",
            rex_ultra::promotion::PromotionState::CorruptState => "corrupt_state",
        };
        Ok(rex_protocol::UltraPromoteResponse {
            task_id: receipt.task_id.clone(),
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
    /// Isolated workspace root for one Ultra candidate.
    fn candidate_root(&self, task_id: &str, candidate_id: &str) -> PathBuf {
        self.root
            .join("workspaces")
            .join("candidates")
            .join(task_id)
            .join(candidate_id)
    }

    /// Materialize a candidate's sealed bundle into its isolated workspace.
    /// Returns the workspace root; the tree contains exactly the bundle.
    fn materialize_candidate(
        &self,
        task_id: &str,
        candidate_id: &str,
        content: &str,
    ) -> Result<PathBuf, ProtocolError> {
        let bundle = rex_ultra::promotion::parse_bundle(content).map_err(|e| {
            perr(
                ErrorCode::GateFailed,
                format!("candidate content is not a sealed file bundle: {e:?}"),
                task_id,
            )
        })?;
        let root = self.candidate_root(task_id, candidate_id);
        if root.exists() {
            fs::remove_dir_all(&root).map_err(internal)?;
        }
        for file in &bundle.files {
            let dest = root.join(&file.path);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).map_err(internal)?;
            }
            fs::write(&dest, &file.content).map_err(internal)?;
        }
        Ok(root)
    }

    /// The daemon-executed gates: run the adversary scan and every frozen
    /// contract proof inside each candidate workspace that still lacks the
    /// record, record the outcomes, and let the kernel finalize. Hosts play
    /// no part.
    fn run_daemon_verifier(
        &self,
        t: &DurableTask,
        bridge: &UltraHostBridge,
        contract: &rex_ultra::contract::AcceptanceContract,
    ) -> Result<(), ProtocolError> {
        let Some(adapter) = bridge
            .load_existing(&t.task_id)
            .map_err(|e| bridge_err(&t.task_id, e))?
        else {
            return Ok(());
        };
        if !matches!(
            adapter.kernel().state,
            rex_ultra::external_kernel::KernelState::AwaitingEvidence
        ) {
            return Ok(());
        }
        let pending_verifier: Vec<String> = adapter
            .responses()
            .map(|r| r.candidate_id.clone())
            .filter(|cid| {
                adapter
                    .candidate_evidence(cid)
                    .and_then(|e| e.verifier.as_ref())
                    .is_none()
            })
            .collect();
        let pending_adversary: Vec<String> = adapter
            .responses()
            .map(|r| r.candidate_id.clone())
            .filter(|cid| {
                adapter
                    .candidate_evidence(cid)
                    .and_then(|e| e.daemon_adversary.as_ref())
                    .is_none()
            })
            .collect();
        // Visual contracts: the daemon decodes the artifacts the host's
        // visual declaration pointed at. Candidates without a declaration
        // yet simply wait - their host owes the declaration.
        let pending_visual: Vec<(String, rex_ultra::taste::TasteGateReport)> = adapter
            .responses()
            .filter_map(|r| {
                let evidence = adapter.candidate_evidence(&r.candidate_id)?;
                if evidence.daemon_visual.is_some() {
                    return None;
                }
                evidence
                    .visual
                    .as_ref()
                    .map(|v| (r.candidate_id.clone(), v.report.clone()))
            })
            .collect();
        drop(adapter);
        for (candidate_id, declaration) in pending_visual {
            let view = self.run_daemon_visual(&t.task_id, bridge, &candidate_id, &declaration)?;
            if !matches!(view.kernel_state, KernelState::AwaitingEvidence) {
                return Ok(());
            }
        }
        for candidate_id in pending_adversary {
            let root = self.candidate_root(&t.task_id, &candidate_id);
            let report = rex_ultra::daemon_adversary::scan(&root);
            let receipts_hash = hash_json(&report)?;
            let view = bridge
                .record_daemon_adversary(&t.task_id, &candidate_id, report, receipts_hash)
                .map_err(|e| bridge_err(&t.task_id, e))?;
            // A recorded scan can settle the kernel (qualified or failed
            // closed); later candidates then keep their incomplete records
            // as the honest state of the run.
            if !matches!(view.kernel_state, KernelState::AwaitingEvidence) {
                return Ok(());
            }
        }
        for candidate_id in pending_verifier {
            let root = self.candidate_root(&t.task_id, &candidate_id);
            let outcomes = rex_ultra::daemon_verify::execute_proofs(contract, &root);
            let receipts_hash = hash_json(&outcomes)?;
            let map: BTreeMap<String, bool> = outcomes
                .iter()
                .map(|o| (o.obligation_id.clone(), o.proven))
                .collect();
            let view = bridge
                .record_daemon_verifier(&t.task_id, &candidate_id, map, receipts_hash)
                .map_err(|e| bridge_err(&t.task_id, e))?;
            if !matches!(view.kernel_state, KernelState::AwaitingEvidence) {
                return Ok(());
            }
        }
        Ok(())
    }

    /// The daemon-executed visual gate: read every declared artifact from
    /// the content-addressed store (which re-hashes on read), decode the
    /// pixels, and record measured metrics. Any missing, tampered or
    /// undecodable artifact fails the candidate honestly.
    fn run_daemon_visual(
        &self,
        task_id: &str,
        bridge: &UltraHostBridge,
        candidate_id: &str,
        declaration: &rex_ultra::taste::TasteGateReport,
    ) -> Result<UltraHostView, ProtocolError> {
        let store = rex_ultra::artifacts::ArtifactStore::open(&self.root)
            .map_err(|e| internal(e.to_string()))?;
        let mut metrics = Vec::new();
        let mut failures: Vec<String> = Vec::new();
        let mut desktop_ahash: Option<u64> = None;
        for shot in &declaration.screenshots {
            match store
                .get(&shot.artifact_hash)
                .map_err(|e| e.to_string())
                .and_then(|bytes| rex_ultra::pixel::decode_metrics(&bytes, &shot.artifact_hash))
            {
                Ok(m) => {
                    if let Err(floor) = m.passes_floor() {
                        failures.push(format!("{:?} viewport: {floor}", shot.viewport));
                    }
                    if shot.viewport == rex_ultra::taste::ViewportClass::Desktop {
                        desktop_ahash = Some(m.ahash);
                    }
                    metrics.push(m);
                }
                Err(e) => failures.push(format!("{:?} viewport: {e}", shot.viewport)),
            }
        }
        if store.get(&declaration.interaction_replay_hash).is_err() {
            failures.push("interaction replay artifact is not in the store".into());
        }
        let passed = failures.is_empty() && metrics.len() == declaration.screenshots.len();
        let detail = if passed {
            "every declared artifact decoded and passed the pixel floor".to_string()
        } else {
            format!("pixel gate failed: {}", failures.join("; "))
        };
        let record = rex_ultra::external_kernel::DaemonVisualRecord {
            request_id: String::new(),
            response_hash: hash_json(&metrics)?,
            thesis_id: declaration.thesis_id.clone(),
            metrics,
            replay_hash: declaration.interaction_replay_hash.clone(),
            ahash: desktop_ahash.unwrap_or(0),
            passed,
            detail,
        };
        bridge
            .record_daemon_visual(task_id, candidate_id, record)
            .map_err(|e| bridge_err(task_id, e))
    }

    /// The joined terminal transition (audit finding 2): an Ultra task's
    /// durable state, its kernel state and its promotion receipt are one
    /// state machine. The kernel failing fails the task; a committed
    /// promotion completes it. Both directions are replayed here from the
    /// durable records, so a crash between the promotion commit and the
    /// task persist is healed by simply running the join again - at the
    /// next submission, at promotion, or at daemon open.
    fn join_ultra_terminal(&self, t: &mut DurableTask) -> Result<(), ProtocolError> {
        if !t.ultra || t.state.is_terminal() {
            return Ok(());
        }
        let bridge = UltraHostBridge::open(&self.root).map_err(|e| bridge_err(&t.task_id, e))?;
        let Some(adapter) = bridge
            .load_existing(&t.task_id)
            .map_err(|e| bridge_err(&t.task_id, e))?
        else {
            return Ok(());
        };
        match adapter.kernel().state {
            KernelState::Failed => {
                t.state = TaskState::Failed;
                t.terminal_reason = Some(
                    "ultra kernel failed: no candidate passed the daemon-executed gates".into(),
                );
                self.append_event(
                    t,
                    "ultra_task_failed",
                    json!({"kernel_state":"failed","join":"kernel failure is task failure"}),
                )?;
                self.persist(t)?;
            }
            KernelState::Completed => {
                let store = rex_ultra::promotion::PromotionStore::open(self.root.join("ultra"))
                    .map_err(|e| internal(format!("promotion store: {e:?}")))?;
                let committed = store
                    .receipt(&t.task_id)
                    .map_err(|e| internal(format!("promotion receipt: {e:?}")))?
                    .filter(|r| r.state == rex_ultra::promotion::PromotionState::Committed);
                let Some(receipt) = committed else {
                    // Kernel qualified a candidate but promotion has not
                    // committed: the task waits, honestly non-terminal.
                    return Ok(());
                };
                let receipt_hash = hash_json(&receipt)?;
                let qualified = adapter
                    .qualified_candidate()
                    .unwrap_or("unknown")
                    .to_string();
                t.evidence
                    .insert("ultra_qualified_candidate".into(), qualified.clone());
                t.evidence
                    .insert("ultra_promotion_receipt".into(), receipt_hash.clone());
                t.state = TaskState::Completed;
                t.terminal_reason = Some(format!(
                    "ultra promotion committed: candidate {qualified}, receipt {receipt_hash}"
                ));
                self.append_event(
                    t,
                    "ultra_task_completed",
                    json!({"kernel_state":"completed","qualified_candidate":qualified,"receipt":receipt_hash}),
                )?;
                self.persist(t)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Promotion and the terminal task transition as one crash-safe step.
    /// The MCP surface keeps this frozen until mandatory promotion gates
    /// are enforced (the compiled-contracts phase); the rebuilt symlink-safe
    /// promotion path underneath it landed with phase F, and the join
    /// itself is exercised directly.
    #[allow(dead_code)]
    pub(crate) fn promote_and_join(
        &self,
        req: &rex_protocol::UltraPromoteRequest,
    ) -> Result<rex_ultra::promotion::PromotionReceipt, ProtocolError> {
        let mut t = self.live(&req.task_id, req.lease_epoch, &req.capability)?;
        require_agent(&t)?;
        require_ultra(&t)?;
        let bridge = UltraHostBridge::open(&self.root).map_err(|e| bridge_err(&t.task_id, e))?;
        let contract = bridge
            .frozen_contract(&t.task_id)
            .map_err(|e| bridge_err(&t.task_id, e))?;
        let adapter = bridge
            .load_existing(&t.task_id)
            .map_err(|e| bridge_err(&t.task_id, e))?
            .ok_or_else(|| {
                perr(
                    ErrorCode::GateFailed,
                    "ultra kernel is not open",
                    &t.task_id,
                )
            })?;
        if !matches!(adapter.kernel().state, KernelState::Completed) {
            return Err(perr(
                ErrorCode::GateFailed,
                "ultra kernel has not qualified a candidate",
                &t.task_id,
            ));
        }
        // Promotion enforces the skill plan frozen at ultra_open, never a
        // recompute over a workspace candidates already changed.
        let plan = bridge
            .frozen_skill_plan(&t.task_id)
            .map_err(|e| bridge_err(&t.task_id, e))?
            .ok_or_else(|| {
                perr(
                    ErrorCode::GateFailed,
                    "no skill plan is frozen for this task; call rex_ultra_open first",
                    &t.task_id,
                )
            })?;
        // The promotion store commits with its own crash-safe receipt
        // sequence; the join below turns a committed receipt into the
        // task's terminal state. A crash between the two is healed by the
        // next join (daemon open sweep or any later call).
        let receipt = bridge
            .promote(&t.task_id, &contract, &self.policy.workspace, &plan.gates)
            .map_err(|e| bridge_err(&t.task_id, e))?;
        self.append_event(
            &mut t,
            "ultra_promotion",
            json!({"state":format!("{:?}",receipt.state),"bundle_hash":receipt.bundle_hash}),
        )?;
        self.persist(&t)?;
        self.join_ultra_terminal(&mut t)?;
        Ok(receipt)
    }

    /// Crash-recovery sweep at daemon open: replay the join for every
    /// non-terminal Ultra task. Best-effort per task; a task that cannot
    /// be loaded fails closed on its next direct access instead of
    /// blocking the daemon.
    fn reconcile_ultra_tasks(&self) {
        let tasks_dir = self.root.join("tasks");
        let Ok(read) = fs::read_dir(&tasks_dir) else {
            return;
        };
        for entry in read.flatten() {
            let id = entry.file_name().to_string_lossy().into_owned();
            let Ok(mut t) = self.load(&id) else {
                continue;
            };
            let _ = self.join_ultra_terminal(&mut t);
        }
    }

    pub fn proof_bundle(&self, req: TaskRefRequest) -> Result<TaskProofBundle, ProtocolError> {
        let bundle = self.assemble_proof_bundle(&req.task_id)?;
        let dir = self.root.join("proofs");
        fs::create_dir_all(&dir).map_err(internal)?;
        let path = dir.join(format!("{}.json", bundle.task_id));
        let temporary = path.with_extension("json.tmp");
        fs::write(
            &temporary,
            serde_json::to_vec_pretty(&bundle).map_err(internal)?,
        )
        .map_err(internal)?;
        fs::rename(&temporary, &path).map_err(internal)?;
        Ok(bundle)
    }

    /// The content hash binding every field of a bundle except the hash
    /// and MAC themselves. Assembly computes it over fresh state; the
    /// verifier recomputes it over a persisted bundle, so editing any
    /// persisted field without the daemon key is detected.
    fn proof_content_hash(b: &TaskProofBundle) -> Result<String, ProtocolError> {
        hash_json(&(
            b.proof_version,
            &b.task_id,
            &b.request_hash,
            &b.task,
            &b.plan,
            b.state,
            &b.kernel_state,
            &b.qualified_candidate,
            &b.skill_plan,
            &b.skill_plan_hash,
            &b.promotion_state,
            &b.promotion,
            &b.evidence_manifest,
            &b.events_chain_head,
            &b.events,
        ))
    }

    /// Assemble the proof bundle from immutable records only: the frozen
    /// skill plan (never a fresh recompile for ultra tasks), the event
    /// log folded into a hash chain, per-candidate evidence manifests,
    /// and the promotion receipt. The same immutable state always yields
    /// the same bundle hash; that is what makes the bundle verifiable.
    fn assemble_proof_bundle(&self, task_id: &str) -> Result<TaskProofBundle, ProtocolError> {
        let t = self.load(task_id)?;
        let bridge = UltraHostBridge::open(&self.root).map_err(|e| bridge_err(&t.task_id, e))?;
        let adapter = bridge
            .load_existing(&t.task_id)
            .map_err(|e| bridge_err(&t.task_id, e))?;
        // Frozen-plan drift is tamper evidence; fail assembly closed.
        if let Some(frozen) = bridge
            .frozen_skill_plan(&t.task_id)
            .map_err(|e| bridge_err(&t.task_id, e))?
        {
            match t.ultra_skill_plan_hash.as_deref() {
                Some(recorded) if recorded == frozen.plan_hash => {}
                _ => {
                    return Err(internal(format!(
                        "frozen skill plan hash drift for {}",
                        t.task_id
                    )))
                }
            }
        }
        let (kernel_state, qualified_candidate, evidence_manifest) = match &adapter {
            Some(adapter) => {
                let state = match adapter.kernel().state {
                    KernelState::New => "new",
                    KernelState::Leased => "leased",
                    KernelState::Running => "running",
                    KernelState::AwaitingEvidence => "awaiting_evidence",
                    KernelState::Completed => "completed",
                    KernelState::Failed => "failed",
                    KernelState::Revoked => "revoked",
                };
                let visual_contract =
                    adapter.contract().work_kind == rex_ultra::contract::WorkKind::Visual;
                let mut manifest: BTreeMap<String, CandidateProofManifest> = BTreeMap::new();
                for response in adapter.responses() {
                    let evidence = adapter
                        .candidate_evidence(&response.candidate_id)
                        .cloned()
                        .unwrap_or_default();
                    let adversary_record_hash = evidence
                        .daemon_adversary
                        .as_ref()
                        .and_then(|r| serde_json::to_value(r).ok())
                        .and_then(|v| rex_protocol::schema::canonical_json(&v).ok())
                        .map(|b| hex_sha256(&b));
                    let verifier_record_hash = evidence
                        .verifier
                        .as_ref()
                        .and_then(|r| serde_json::to_value(r).ok())
                        .and_then(|v| rex_protocol::schema::canonical_json(&v).ok())
                        .map(|b| hex_sha256(&b));
                    let visual_record_hash = evidence
                        .visual
                        .as_ref()
                        .and_then(|r| serde_json::to_value(r).ok())
                        .and_then(|v| rex_protocol::schema::canonical_json(&v).ok())
                        .map(|b| hex_sha256(&b));
                    let daemon_visual_record_hash = evidence
                        .daemon_visual
                        .as_ref()
                        .and_then(|r| serde_json::to_value(r).ok())
                        .and_then(|v| rex_protocol::schema::canonical_json(&v).ok())
                        .map(|b| hex_sha256(&b));
                    let fully_evidenced = evidence.daemon_adversary.is_some()
                        && evidence.verifier.is_some()
                        && (!visual_contract
                            || (evidence.visual.is_some() && evidence.daemon_visual.is_some()));
                    manifest.insert(
                        response.candidate_id.clone(),
                        CandidateProofManifest {
                            response_hash_recorded: response.response_hash.clone(),
                            response_hash_recomputed: rex_protocol::schema::canonical_hash(
                                &response.content,
                            )
                            .map_err(internal)?,
                            adversary_record_hash,
                            verifier_record_hash,
                            visual_record_hash,
                            daemon_visual_record_hash,
                            fully_evidenced,
                        },
                    );
                }
                (
                    Some(state.to_string()),
                    adapter.qualified_candidate().map(str::to_string),
                    manifest,
                )
            }
            None => (None, None, BTreeMap::new()),
        };
        // The frozen plan when one exists; informational compile otherwise.
        let skill_plan = self.task_skill_plan_view(&t.task_id);
        let skill_plan_hash = t.ultra_skill_plan_hash.clone();
        let store = rex_ultra::promotion::PromotionStore::open(self.root.join("ultra"))
            .map_err(|e| internal(format!("promotion store: {e:?}")))?;
        let promotion = store
            .receipt(&t.task_id)
            .map_err(|e| internal(format!("promotion receipt: {e:?}")))?;
        let promotion_state = promotion.as_ref().map(|r| {
            match r.state {
                rex_ultra::promotion::PromotionState::Prepared => "prepared",
                rex_ultra::promotion::PromotionState::Committed => "committed",
                rex_ultra::promotion::PromotionState::RolledBack => "rolled_back",
                rex_ultra::promotion::PromotionState::CorruptState => "corrupt_state",
            }
            .to_string()
        });
        let mut events = self
            .events(EventsRequest {
                task_id: t.task_id.clone(),
                after_seq: 0,
                limit: None,
            })
            .unwrap()
            .events;
        events.sort_by_key(|e| e.seq);
        // Append-only hash chain over the canonical event log.
        let mut events_chain_head = hex_sha256(b"rex-proof-events-genesis");
        for event in &events {
            let canonical = rex_protocol::schema::canonical_json(event).map_err(internal)?;
            events_chain_head =
                hex_sha256(format!("{events_chain_head}:{}", hex_sha256(&canonical)).as_bytes());
        }
        let mut bundle = TaskProofBundle {
            task_id: t.task_id.clone(),
            request_hash: t.request_hash.clone(),
            task: t.task.clone(),
            plan: t.plan.clone(),
            state: t.state,
            kernel_state,
            qualified_candidate,
            skill_plan,
            promotion_state,
            promotion,
            events,
            bundle_hash: String::new(),
            proof_version: 2,
            skill_plan_hash,
            evidence_manifest,
            events_chain_head,
            bundle_mac: String::new(),
        };
        bundle.bundle_hash = Self::proof_content_hash(&bundle)?;
        let proof_key = load_or_create_proof_key(&self.root)?;
        bundle.bundle_mac = hmac_sha256_hex(proof_key.as_bytes(), bundle.bundle_hash.as_bytes());
        Ok(bundle)
    }

    /// Independently verify the persisted proof bundle: the MAC must
    /// validate against the daemon-held key (a state editor without the
    /// key cannot forge it), a fresh assembly from current immutable
    /// records must reproduce the same hash (the proof is deterministic
    /// and bound to the frozen plan), and every recorded candidate
    /// response hash must match the stored content.
    pub fn verify_proof_bundle(
        &self,
        req: TaskRefRequest,
    ) -> Result<ProofVerificationReport, ProtocolError> {
        let fresh = self.assemble_proof_bundle(&req.task_id)?;
        let proof_key = load_or_create_proof_key(&self.root)?;
        let path = self
            .root
            .join("proofs")
            .join(format!("{}.json", fresh.task_id));
        let persisted: Option<TaskProofBundle> = match fs::read(&path) {
            Ok(bytes) => Some(serde_json::from_slice(&bytes).map_err(internal)?),
            Err(_) => None,
        };
        let manifest_ok = |m: &BTreeMap<String, CandidateProofManifest>| {
            m.values()
                .all(|c| c.response_hash_recorded == c.response_hash_recomputed)
        };
        let (
            persisted_present,
            content_hash_consistent,
            mac_valid,
            deterministic,
            response_hashes_verified,
        ) = match &persisted {
            Some(p) => (
                true,
                Self::proof_content_hash(p)? == p.bundle_hash,
                hmac_sha256_hex(proof_key.as_bytes(), p.bundle_hash.as_bytes()) == p.bundle_mac,
                p.bundle_hash == fresh.bundle_hash,
                manifest_ok(&p.evidence_manifest) && manifest_ok(&fresh.evidence_manifest),
            ),
            None => (
                false,
                false,
                false,
                false,
                manifest_ok(&fresh.evidence_manifest),
            ),
        };
        let verdict = persisted_present
            && content_hash_consistent
            && mac_valid
            && deterministic
            && response_hashes_verified;
        Ok(ProofVerificationReport {
            task_id: fresh.task_id,
            persisted_present,
            content_hash_consistent,
            mac_valid,
            deterministic,
            response_hashes_verified,
            events_chain_head: fresh.events_chain_head,
            bundle_hash: fresh.bundle_hash,
            verdict,
        })
    }

    /// The task's bound skill plan: the frozen plan when ultra_open
    /// compiled one, otherwise a fresh workspace-level compile for
    /// informational views.
    fn task_skill_plan_view(&self, task_id: &str) -> Option<SkillPlanView> {
        let bridge = UltraHostBridge::open(&self.root).ok()?;
        match bridge.frozen_skill_plan(task_id) {
            Ok(Some(frozen)) => Some(skill_plan_view(frozen)),
            _ => self.skill_plan(),
        }
    }

    fn compile_skill_plan(&self) -> Option<rex_ultra::skills::CompiledSkillPlan> {
        let facts = rex_ultra::skills::collect_repository_facts(&self.policy.workspace);
        let mut registry = rex_ultra::skill_packs::first_class_registry();
        registry.extend(rex_ultra::generated_packs::broad_registry(&facts));
        rex_ultra::skills::compile_plan(&facts, &registry).ok()
    }

    fn skill_plan(&self) -> Option<SkillPlanView> {
        let plan = self.compile_skill_plan()?;
        Some(skill_plan_view(plan))
    }

    fn call_tool(
        &self,
        t: &mut DurableTask,
        request: ToolRequest,
    ) -> Result<ToolResult, ProtocolError> {
        if t.used_tool_calls >= t.max_tool_calls {
            return Err(perr(
                ErrorCode::BudgetExceeded,
                "tool-call budget exhausted",
                &t.task_id,
            ));
        }
        let now = now_ms();
        let prepared = self
            .tools
            .prepare(&t.token, request, now)
            .map_err(|e| map_tool_err(&t.task_id, e.to_string()))?;
        if prepared.approval_required {
            if !self.policy.approve_task_mutations {
                return Err(perr(
                    ErrorCode::ApprovalRequired,
                    "trusted launcher has not approved mutations for this task",
                    &t.task_id,
                ));
            }
            self.tools
                .resolve_approval(&prepared.call_id, true)
                .map_err(|e| map_tool_err(&t.task_id, e.to_string()))?;
        }
        let out = self.tools.execute(&t.token, &prepared.call_id, now_ms());
        t.used_tool_calls += 1;
        t.registered_evidence.insert(out.call_id.clone());
        self.custody
            .lock()
            .map_err(|_| internal("custody registry poisoned"))?
            .consume(
                &t.token,
                Consumption {
                    tool_calls: 1,
                    ..Default::default()
                },
                now_ms(),
            )
            .map_err(custody_err)?;
        self.append_event(
            t,
            "tool_finished",
            json!({"tool":out.tool,"call_id":out.call_id,
            "ok":out.ok,"receipt":out.receipt}),
        )?;
        self.persist(t)?;
        if !out.ok {
            return Err(map_tool_err(
                &t.task_id,
                out.error
                    .as_ref()
                    .map(|e| e.detail.clone())
                    .unwrap_or_else(|| "tool failed".into()),
            ));
        }
        Ok(out)
    }

    fn live(&self, id: &str, epoch: u64, capability: &str) -> Result<DurableTask, ProtocolError> {
        let mut t = self.load(id)?;
        // Authorization first: a presented capability whose hash does not
        // match is denied before any state or lease detail is revealed.
        // Pre-2.0 records carry an empty hash and fail closed.
        if t.task_capability_hash.is_empty()
            || hex_sha256(capability.as_bytes()) != t.task_capability_hash
        {
            return Err(perr(ErrorCode::Unauthorized, "invalid task capability", id));
        }
        if t.state.is_terminal() {
            return Err(perr(ErrorCode::TaskTerminal, "task is terminal", id));
        }
        if epoch != t.lease_epoch {
            return Err(perr(ErrorCode::StaleLease, "lease epoch is stale", id));
        }
        // The keeper may have renewed the lease in custody since this
        // record was written; judge expiry on the live view.
        self.sync_lease_from_custody(&mut t)?;
        // The failsafe already paused this task (this process, the keeper, or a
        // restart sweep). Land the pause in the task's own durable state and
        // refuse work; only a verified resume moves it again.
        if self.grant_suspended(&t)? {
            self.absorb_pause(&mut t)?;
            return Err(perr(
                ErrorCode::StaleLease,
                "lease lapsed: task paused; resume with rex_execute, task_id and the current resume handle",
                id,
            ));
        }
        let now = now_ms();
        if now >= t.lease_expires_ms {
            // Lapse discovered on a live call: pause here and now, durably.
            let reason = if now >= t.created_ms + t.max_wall_ms as u128 {
                PauseReason::WallBudgetSpent
            } else {
                PauseReason::LeaseLapsed
            };
            self.pause_now(&t, reason)?;
            self.absorb_pause(&mut t)?;
            return Err(perr(
                ErrorCode::StaleLease,
                "lease lapsed: task paused; resume with rex_execute, task_id and the current resume handle",
                id,
            ));
        }
        self.custody
            .lock()
            .map_err(|_| internal("custody registry poisoned"))?
            .verify_token(&t.token, now)
            .map_err(custody_err)?;
        Ok(t)
    }

    fn heartbeat(&self, t: &mut DurableTask) -> Result<(), ProtocolError> {
        // The sequence comes from the custody grant, not from the task
        // record: the lease keeper may have heartbeated since the last
        // agent call, and a cached sequence would then read as a replay.
        let beat = {
            let mut reg = self
                .custody
                .lock()
                .map_err(|_| internal("custody registry poisoned"))?;
            let seq = match reg.grant(&t.grant_id) {
                Some(g) => g.lease.next_seq,
                None => t.heartbeat_seq,
            };
            reg.heartbeat(&t.token, seq, now_ms())
        };
        let lease = beat.map_err(custody_err)?;
        t.heartbeat_seq = lease.next_seq;
        t.lease_expires_ms = lease.expires_ms;
        Ok(())
    }

    /// Mint a fresh operational capability and rotate the stored hash.
    /// A verified resume (the rotated host handle) also renews a lapsed
    /// lease: docs/rex-mcp-ultra.md promises that an expired lease plus the
    /// current handle resumes the task. Without this, a host working longer
    /// than one lease window between calls - exactly the long Ultra loop,
    /// bricked its own task with no recovery path.
    ///
    /// A lapse is never healed by a bare heartbeat: a paused task is fenced
    /// through custody suspend + resume (epoch bump, rotated secrets) and its
    /// pause record is closed out. A live lease still resumes without an epoch
    /// bump.
    fn renew_lease_on_resume(&self, t: &mut DurableTask) -> Result<bool, ProtocolError> {
        // The pause record is authoritative and read first: a paused task is
        // always re-fenced even if custody's lease view has since been
        // extended, and a record past its grace window refuses every resume.
        // Reading it fails closed on corruption.
        match self.read_pause_record(&t.task_id)?.map(|rec| rec.status) {
            Some(PauseStatus::Paused) => {
                self.reacquire_lapsed(t, now_ms())?;
                return Ok(true);
            }
            Some(PauseStatus::Expired) => {
                return Err(perr(
                    ErrorCode::StaleLease,
                    "lease expired beyond the resume grace window",
                    &t.task_id,
                ));
            }
            Some(PauseStatus::Resumed) | None => {}
        }
        // A lapse already recorded on the task is fenced before the live
        // custody view can heal the record.
        if now_ms() >= t.lease_expires_ms {
            self.reacquire_lapsed(t, now_ms())?;
            return Ok(true);
        }
        // The keeper may have renewed this lease already; only a lapsed
        // lease needs the renewal work below.
        self.sync_lease_from_custody(t)?;
        if now_ms() < t.lease_expires_ms {
            return Ok(false);
        }
        let phase = {
            let reg = self
                .custody
                .lock()
                .map_err(|_| internal("custody registry poisoned"))?;
            reg.grant(&t.grant_id).map(|g| g.phase)
        };
        match phase {
            Some(rex_custody::CustodyPhase::Active) => {
                // Never swept in this process: fence it like every other
                // lapse (suspend, then resume), never heartbeat it alive.
                self.reacquire_lapsed(t, now_ms())?;
            }
            Some(rex_custody::CustodyPhase::Suspended) => {
                let epoch_before = self.resume_suspended(t, now_ms())?;
                self.complete_resume_after_pause(t, epoch_before)?;
            }
            _ => {
                return Err(perr(
                    ErrorCode::StaleLease,
                    "lease expired beyond the resume grace window",
                    &t.task_id,
                ));
            }
        }
        Ok(true)
    }

    /// Called at creation and on every verified resume: the rotated
    /// resume handle vouches for the new capability.
    fn rotate_capability(&self, t: &mut DurableTask) -> Result<String, ProtocolError> {
        let capability = format!("cap-{}", random_hex(24));
        t.task_capability_hash = hex_sha256(capability.as_bytes());
        Ok(capability)
    }

    fn verify_resume_handle(
        &self,
        t: &DurableTask,
        req: &ExecuteRequest,
    ) -> Result<(), ProtocolError> {
        let expected = t.host_resume_handle_hash.clone().ok_or_else(|| {
            perr(
                ErrorCode::ScopeDenied,
                "task predates host resume handles; resume through the trusted human launcher path",
                &t.task_id,
            )
        })?;
        let presented = req.resume_handle.as_deref().ok_or_else(|| {
            perr(
                ErrorCode::ScopeDenied,
                "resume requires the host resume handle issued at creation",
                &t.task_id,
            )
        })?;
        if hex_sha256(presented.as_bytes()) != expected {
            return Err(perr(
                ErrorCode::ScopeDenied,
                "host resume handle mismatch",
                &t.task_id,
            ));
        }
        Ok(())
    }

    /// Only after the verified resume has passed every gate.
    fn rotate_resume_handle(&self, t: &mut DurableTask) -> Result<String, ProtocolError> {
        let rotated = format!("hrh-{}", random_hex(24));
        t.host_resume_handle_hash = Some(hex_sha256(rotated.as_bytes()));
        self.persist(t)?;
        Ok(rotated)
    }

    fn execute_view(
        &self,
        t: &DurableTask,
        resumed: bool,
        host_resume_handle: Option<String>,
        task_capability: Option<String>,
    ) -> ExecuteResponse {
        ExecuteResponse {
            task_id: t.task_id.clone(),
            state: t.state,
            resumed,
            host_resume_handle,
            task_capability,
            next: t.open_action.clone(),
            lease: lease_view(t),
            discipline: Some(operator_discipline()),
        }
    }
    fn task_dir(&self, id: &str) -> PathBuf {
        self.root.join("tasks").join(id)
    }
    fn persist(&self, t: &DurableTask) -> Result<(), ProtocolError> {
        let dir = self.task_dir(&t.task_id);
        fs::create_dir_all(&dir).map_err(internal)?;
        atomic_json(&dir.join("task.json"), t)
    }
    fn load(&self, id: &str) -> Result<DurableTask, ProtocolError> {
        if !safe_id(id) {
            return Err(ProtocolError::new(
                ErrorCode::MalformedRequest,
                "invalid task id",
            ));
        }
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
        let version = value
            .get("store_schema_version")
            .and_then(|v| v.as_u64())
            .unwrap_or(1) as u32;
        if version > STORE_SCHEMA_VERSION {
            return Err(perr(
                ErrorCode::VersionMismatch,
                format!("stored task schema v{version} is newer than this server"),
                id,
            ));
        }
        let mut t: DurableTask = serde_json::from_value(value).map_err(internal)?;
        // Migrate on the raw file version, not the serde default, so a
        // schema-1 record (field absent) is detected and rewritten.
        if version != STORE_SCHEMA_VERSION {
            t.store_schema_version = STORE_SCHEMA_VERSION;
            self.append_event(
                &mut t,
                "store_migrated",
                json!({"from": version, "to": STORE_SCHEMA_VERSION}),
            )?;
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
    fn append_event(
        &self,
        t: &mut DurableTask,
        kind: &str,
        detail: Value,
    ) -> Result<(), ProtocolError> {
        t.last_event_seq += 1;
        let ev = TaskEvent {
            seq: t.last_event_seq,
            ts_ms: now_ms(),
            kind: kind.into(),
            detail,
        };
        let dir = self.task_dir(&t.task_id);
        fs::create_dir_all(&dir).map_err(internal)?;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("events.jsonl"))
            .map_err(internal)?;
        serde_json::to_writer(&mut f, &ev).map_err(internal)?;
        f.write_all(b"\n").map_err(internal)?;
        f.sync_data().map_err(internal)
    }
}

/// The Fable completion discipline, delivered through the protocol because
/// host agents never see REX's own system prompt.
fn operator_discipline() -> String {
    rex_prompt::gate::COMPLETION_GATE.to_string()
}

struct FinalEvaluator {
    all_steps: bool,
}
impl GateEvaluator for FinalEvaluator {
    fn evaluate(&self, gate: &EvidenceGate, _: &rex_custody::CustodyGrant) -> GateOutcome {
        match gate {
            EvidenceGate::Custom { name }
                if name == "all_plan_steps_accepted" && !self.all_steps =>
            {
                GateOutcome::Failed("plan has open steps".into())
            }
            EvidenceGate::HumanConfirmation => {
                GateOutcome::Failed("human confirmation unavailable".into())
            }
            _ => GateOutcome::Passed,
        }
    }
}

fn validate_execute(r: &ExecuteRequest) -> Result<(), ProtocolError> {
    if r.request_id.is_empty() || r.request_id.len() > 200 || r.task.trim().is_empty() {
        return Err(ProtocolError::new(
            ErrorCode::MalformedRequest,
            "request_id and task are required",
        ));
    }
    Ok(())
}
fn capability_set(workspace: &Path) -> CapabilitySet {
    CapabilitySet {
        workspace_root: workspace.to_path_buf(),
        tool_classes: [ToolClass::Read, ToolClass::Write, ToolClass::Execute]
            .into_iter()
            .collect(),
        allowed_tools: [
            "read_file",
            "create_file",
            "edit_file",
            "apply_patch",
            "search_files",
            "glob_files",
            "run_command",
        ]
        .into_iter()
        .map(String::from)
        .collect::<BTreeSet<_>>(),
        allow_search: true,
        allow_preview: false,
        can_delegate: false,
    }
}
fn make_action(task_id: &str, idx: usize, step: &PlanStep, max_wall_ms: u64) -> ActionSpec {
    ActionSpec {
        action_id: format!("action-{}-{}", task_id.trim_start_matches("task-"), idx + 1),
        seq: idx as u64 + 1,
        instructions: step.instructions.clone(),
        permitted_tools: vec![
            ToolName::Read,
            ToolName::Edit,
            ToolName::Search,
            ToolName::Run,
            ToolName::Test,
        ],
        acceptance: step
            .acceptance
            .clone()
            .unwrap_or_else(|| "submit evidence and a truthful outcome".into()),
        max_wall_ms,
    }
}
/// Per-candidate immutable evidence manifest: what the host submitted
/// (recorded hash vs recomputed hash of the stored content) and the
/// canonical hashes of the daemon's own evidence records.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CandidateProofManifest {
    pub response_hash_recorded: String,
    pub response_hash_recomputed: String,
    #[serde(default)]
    pub adversary_record_hash: Option<String>,
    #[serde(default)]
    pub verifier_record_hash: Option<String>,
    #[serde(default)]
    pub visual_record_hash: Option<String>,
    #[serde(default)]
    pub daemon_visual_record_hash: Option<String>,
    pub fully_evidenced: bool,
}

/// The machine-readable proof bundle for one task. Version 2 binds the
/// frozen skill plan, carries per-candidate evidence manifests, a
/// hash-chained event log, and a MAC keyed by the daemon-held proof key.
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
    #[serde(default)]
    pub proof_version: u32,
    #[serde(default)]
    pub skill_plan_hash: Option<String>,
    #[serde(default)]
    pub evidence_manifest: BTreeMap<String, CandidateProofManifest>,
    #[serde(default)]
    pub events_chain_head: String,
    #[serde(default)]
    pub bundle_mac: String,
}

/// The outcome of independently verifying a persisted proof bundle
/// against the daemon-held key and a fresh assembly from current
/// immutable records.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofVerificationReport {
    pub task_id: String,
    pub persisted_present: bool,
    /// The persisted bundle's own content re-hashes to its stored
    /// bundle_hash: no field was edited after the hash was taken.
    pub content_hash_consistent: bool,
    pub mac_valid: bool,
    /// The persisted bundle hash equals a fresh assembly: the proof did
    /// not drift when the workspace or other mutable inputs changed.
    pub deterministic: bool,
    /// Every candidate's recorded response hash matches the recomputed
    /// hash of the stored content in both persisted and fresh bundles.
    pub response_hashes_verified: bool,
    pub events_chain_head: String,
    pub bundle_hash: String,
    pub verdict: bool,
}

fn require_agent(t: &DurableTask) -> Result<(), ProtocolError> {
    if !t.operator_is_agent {
        return Err(perr(
            ErrorCode::ScopeDenied,
            "ultra external-host loop requires an agent-operated task",
            &t.task_id,
        ));
    }
    Ok(())
}
fn require_ultra(t: &DurableTask) -> Result<(), ProtocolError> {
    if !t.ultra {
        return Err(perr(
            ErrorCode::ScopeDenied,
            "Ultra operations require an Ultra-mode task",
            &t.task_id,
        ));
    }
    Ok(())
}
fn bridge_err(task_id: &str, e: BridgeError) -> ProtocolError {
    match e {
        BridgeError::InvalidTaskId => {
            perr(ErrorCode::TaskNotFound, "invalid ultra task id", task_id)
        }
        BridgeError::Adapter(a) => perr(
            ErrorCode::GateFailed,
            format!("ultra kernel rejected submission: {a:?}"),
            task_id,
        ),
        BridgeError::Promotion(p) => perr(
            ErrorCode::GateFailed,
            format!("ultra promotion rejected: {p:?}"),
            task_id,
        ),
        BridgeError::Io(m) => perr(ErrorCode::Internal, format!("ultra store: {m}"), task_id),
    }
}
fn ultra_view(
    task_id: String,
    view: &UltraHostView,
    skill_plan: Option<SkillPlanView>,
) -> UltraViewResponse {
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
        candidate_requests: view
            .candidate_requests
            .iter()
            .map(|r| UltraCandidateRequestView {
                work_kind: match r.work_kind {
                    rex_ultra::contract::WorkKind::General => "general",
                    rex_ultra::contract::WorkKind::Visual => "visual",
                }
                .into(),
                candidate_id: r.candidate_id.clone(),
                contract_hash: r.contract_hash.clone(),
                task: r.task.clone(),
                obligation_ids: r.obligation_ids.clone(),
            })
            .collect(),
        evidence_requests: view
            .evidence_requests
            .iter()
            .map(|r| UltraEvidenceRequestView {
                request_id: r.request_id.clone(),
                candidate_id: r.candidate_id.clone(),
                contract_hash: r.contract_hash.clone(),
                kind: match r.kind {
                    EvidenceKind::Adversary => "adversary",
                    EvidenceKind::Verifier => "verifier",
                    EvidenceKind::Visual => "visual",
                }
                .into(),
                obligation_ids: r.obligation_ids.clone(),
                candidate_response_hash: r.candidate_response_hash.clone(),
            })
            .collect(),
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
    let b = serde_json::to_vec(v).map_err(internal)?;
    Ok(hex_sha256(&b))
}
fn atomic_json<T: Serialize>(path: &Path, v: &T) -> Result<(), ProtocolError> {
    let bytes = serde_json::to_vec_pretty(v).map_err(internal)?;
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes).map_err(internal)?;
    fs::rename(tmp, path).map_err(internal)
}
/// Load or create the daemon's human-stop token. The token file is the
/// human authority boundary: only the trusted local launcher can read a
/// 0600 file in the daemon state dir; a remote MCP host cannot mint one.
fn load_or_create_human_token(root: &Path) -> Result<String, ProtocolError> {
    let path = root.join("human-stop-token");
    if let Ok(existing) = fs::read_to_string(&path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return Ok(hex_sha256(trimmed.as_bytes()));
        }
    }
    let token = format!("hst-{}", random_hex(32));
    {
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&path).map_err(internal)?;
        f.write_all(token.as_bytes()).map_err(internal)?;
        f.sync_all().map_err(internal)?;
    }
    Ok(hex_sha256(token.as_bytes()))
}

/// Load or create the daemon-held proof-bundle MAC key. Same authority
/// boundary as the human-stop token: a 0600 file in the daemon state dir a
/// remote MCP host cannot read, so it cannot mint a valid bundle MAC.
fn load_or_create_proof_key(root: &Path) -> Result<String, ProtocolError> {
    let dir = root.join("proofs");
    fs::create_dir_all(&dir).map_err(internal)?;
    let path = dir.join("hmac-key");
    if let Ok(existing) = fs::read_to_string(&path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }
    let key = format!("prk-{}", random_hex(32));
    {
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&path).map_err(internal)?;
        f.write_all(key.as_bytes()).map_err(internal)?;
        f.sync_all().map_err(internal)?;
    }
    Ok(key)
}

fn safe_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() < 200
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
fn random_id() -> String {
    random_hex(8)
}
fn elapsed(t: &DurableTask) -> u64 {
    now_ms().saturating_sub(t.created_ms) as u64
}
fn lease_view(t: &DurableTask) -> LeaseView {
    LeaseView {
        epoch: t.lease_epoch,
        expires_ms_from_now: t.lease_expires_ms.saturating_sub(now_ms()) as u64,
        heartbeat_interval_ms: t.heartbeat_interval_ms,
    }
}
fn default_branch_id() -> String {
    "main".into()
}
/// Current persisted task-store schema. Bump when DurableTask changes shape;
/// never reuse the wire protocol version for storage decisions.
const STORE_SCHEMA_VERSION: u32 = 2;
fn default_store_schema_version() -> u32 {
    STORE_SCHEMA_VERSION
}
fn status_operation(t: &DurableTask) -> OperationStatus {
    if now_ms() >= t.lease_expires_ms && !t.state.is_terminal() {
        OperationStatus::Stale
    } else {
        t.operation_status.clone()
    }
}
fn host_label(h: HostKind) -> &'static str {
    match h {
        HostKind::Human => "human-ui",
        HostKind::ClaudeCode => "claude-code",
        HostKind::Codex => "codex",
        HostKind::OpenCode => "opencode",
        HostKind::Hermes => "hermes",
        HostKind::Antigravity => "antigravity",
        HostKind::GenericAgent => "generic-mcp",
    }
}
fn perr(code: ErrorCode, msg: impl Into<String>, id: &str) -> ProtocolError {
    ProtocolError::new(code, msg).for_task(id)
}
fn internal(e: impl ToString) -> ProtocolError {
    ProtocolError::new(ErrorCode::Internal, e.to_string())
}
fn custody_err(e: CustodyError) -> ProtocolError {
    let code = match e {
        CustodyError::LeaseStale | CustodyError::HeartbeatReplay { .. } => ErrorCode::StaleLease,
        CustodyError::BudgetExhausted => ErrorCode::BudgetExceeded,
        CustodyError::ClaimRejected { .. } => ErrorCode::GateFailed,
        _ => ErrorCode::Unauthorized,
    };
    ProtocolError::new(code, e.to_string())
}
fn map_tool_err(id: &str, e: String) -> ProtocolError {
    perr(ErrorCode::ScopeDenied, e, id)
}
fn parse_search(s: &str) -> Vec<SearchHit> {
    s.lines()
        .filter_map(|l| {
            let mut p = l.splitn(3, ':');
            let path = p.next()?.to_string();
            let line = p.next()?.parse().ok()?;
            let excerpt = p.next().unwrap_or("").trim().to_string();
            Some(SearchHit {
                path,
                line,
                excerpt,
            })
        })
        .collect()
}
fn split_output(s: &str) -> (String, String) {
    if let Some((a, b)) = s.split_once("stderr:\n") {
        (
            a.strip_prefix("stdout:\n").unwrap_or(a).trim_end().into(),
            b.trim_end().into(),
        )
    } else {
        (
            s.strip_prefix("stdout:\n").unwrap_or(s).trim_end().into(),
            String::new(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    fn req(id: &str) -> ExecuteRequest {
        ExecuteRequest {
            request_id: id.into(),
            task: "make hello".into(),
            task_id: None,
            resume_handle: None,
            follow_up: None,
            host: HostKind::ClaudeCode,
            operator_is_agent: true,
            ultra: true,
            budgets: None,
            proof: None,
            plan: Some(vec![PlanStep {
                instructions: "write hello".into(),
                acceptance: Some("file exists".into()),
            }]),
        }
    }
    #[test]
    fn store_schema_v1_migrates_and_future_versions_fail_closed() {
        let d = tempdir().unwrap();
        let w = d.path().join("ws");
        let root = d.path().join("state");
        let daemon = HarnessDaemon::open(&root, DaemonPolicy::conservative(&w)).unwrap();
        let ex = daemon.execute(req("rmig")).unwrap();
        let file = root.join("tasks").join(&ex.task_id).join("task.json");
        let mut v: serde_json::Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        assert_eq!(
            v.get("store_schema_version").and_then(|s| s.as_u64()),
            Some(2)
        );
        // A schema-1 record predates the version field; loading migrates in place.
        v.as_object_mut().unwrap().remove("store_schema_version");
        fs::write(&file, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
        let s1 = daemon
            .status(TaskRefRequest {
                task_id: ex.task_id.clone(),
            })
            .unwrap();
        assert_eq!(s1.state, TaskState::Active);
        let v2: serde_json::Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        assert_eq!(
            v2.get("store_schema_version").and_then(|s| s.as_u64()),
            Some(2)
        );
        let log =
            fs::read_to_string(root.join("tasks").join(&ex.task_id).join("events.jsonl")).unwrap();
        assert!(log.contains("store_migrated"));
        // An unknown future schema fails closed instead of being misread.
        let mut v3 = v2;
        v3.as_object_mut()
            .unwrap()
            .insert("store_schema_version".into(), serde_json::json!(99));
        fs::write(&file, serde_json::to_vec_pretty(&v3).unwrap()).unwrap();
        let e = daemon
            .status(TaskRefRequest {
                task_id: ex.task_id.clone(),
            })
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::VersionMismatch);
    }
    #[test]
    fn idempotency_and_recovery() {
        let d = tempdir().unwrap();
        let w = d.path().join("ws");
        let root = d.path().join("state");
        let daemon = HarnessDaemon::open(&root, DaemonPolicy::conservative(&w)).unwrap();
        let a = daemon.execute(req("r1")).unwrap();
        let mut replay = req("r1");
        replay.resume_handle = a.host_resume_handle.clone();
        let b = daemon.execute(replay).unwrap();
        assert_eq!(a.task_id, b.task_id);
        assert!(b.resumed);
        drop(daemon);
        let reopened = HarnessDaemon::open(&root, DaemonPolicy::conservative(&w)).unwrap();
        let status = reopened
            .status(TaskRefRequest {
                task_id: a.task_id.clone(),
            })
            .unwrap();
        assert_eq!(status.state, TaskState::Active);
        assert_eq!(status.operation, OperationStatus::ExternalHostRequired);
        assert_eq!(status.packet.branch_id, "main");
        assert_eq!(status.packet.idempotency_key, "r1");
    }
    #[test]
    fn read_confined_and_cancel_durable() {
        let d = tempdir().unwrap();
        let w = d.path().join("ws");
        fs::create_dir_all(&w).unwrap();
        fs::write(w.join("hello.txt"), "hello").unwrap();
        let daemon =
            HarnessDaemon::open(d.path().join("state"), DaemonPolicy::conservative(&w)).unwrap();
        let ex = daemon.execute(req("r2")).unwrap();
        let got = daemon
            .read(ReadRequest {
                task_id: ex.task_id.clone(),
                capability: cap_of(&ex),
                lease_epoch: ex.lease.epoch,
                path: "hello.txt".into(),
                byte_range: None,
            })
            .unwrap();
        assert_eq!(got.content, "hello");
        let c = daemon
            .cancel(CancelRequest {
                task_id: ex.task_id.clone(),
                capability: cap_of(&ex),
                reason: Some("stop".into()),
            })
            .unwrap();
        assert_eq!(c.state, TaskState::Cancelled);
        assert_eq!(
            daemon
                .status(TaskRefRequest {
                    task_id: ex.task_id.clone()
                })
                .unwrap()
                .operation,
            OperationStatus::Aborted
        );
        // A path escape quarantines the custody grant outright.
        let ex2 = daemon.execute(req("r3b")).unwrap();
        let e = daemon
            .read(ReadRequest {
                task_id: ex2.task_id.clone(),
                capability: cap_of(&ex2),
                lease_epoch: ex2.lease.epoch,
                path: "../escape".into(),
                byte_range: None,
            })
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::ScopeDenied);
        assert!(daemon
            .status(TaskRefRequest {
                task_id: ex2.task_id
            })
            .is_ok());
    }
    #[test]
    fn human_stop_is_final_and_discipline_is_delivered() {
        let d = tempdir().unwrap();
        let w = d.path().join("ws");
        let daemon =
            HarnessDaemon::open(d.path().join("state"), DaemonPolicy::conservative(&w)).unwrap();
        let mut r = req("r4");
        r.operator_is_agent = false;
        r.host = HostKind::Human;
        let ex = daemon.execute(r).unwrap();
        // Hosts receive the Fable completion discipline at task start.
        let cap = cap_of(&ex);
        let disc = ex.discipline.unwrap();
        assert!(disc.contains("declare completion") && disc.contains("evidence"));
        // Human cancel goes through the terminal human-stop fence.
        let c = daemon
            .cancel(CancelRequest {
                task_id: ex.task_id.clone(),
                capability: cap.clone(),
                reason: Some("stop button".into()),
            })
            .unwrap();
        assert_eq!(c.state, TaskState::Cancelled);
        let e = daemon
            .next(NextRequest {
                task_id: ex.task_id.clone(),
                capability: cap,
                lease_epoch: ex.lease.epoch,
            })
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::TaskTerminal);
    }
    #[test]
    fn writes_require_trusted_launcher_approval() {
        let d = tempdir().unwrap();
        let w = d.path().join("ws");
        let daemon =
            HarnessDaemon::open(d.path().join("state"), DaemonPolicy::conservative(&w)).unwrap();
        let ex = daemon.execute(req("r3")).unwrap();
        let e = daemon
            .edit(EditRequest {
                task_id: ex.task_id.clone(),
                capability: cap_of(&ex),
                lease_epoch: ex.lease.epoch,
                path: "x".into(),
                expected: None,
                replacement: "y".into(),
                create: true,
            })
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::ApprovalRequired);
    }
    /// A minimal contract draft whose proofs are all daemon-executable.
    fn valid_draft() -> &'static str {
        r#"{"obligations":[{"id":"ob-1","statement":"hello.txt exists and says hello","proof":{"kind":"file_contains","path":"hello.txt","needle":"hello"}}],"forbidden_regressions":[]}"#
    }

    fn cap_of(r: &ExecuteResponse) -> String {
        r.task_capability.clone().expect("capability issued")
    }
    fn ultra_req(
        task: &str,
        capability: &str,
        epoch: u64,
        kind: UltraSubmissionKind,
        request_id: &str,
        candidate_id: &str,
        content: &str,
    ) -> UltraSubmitRequest {
        UltraSubmitRequest {
            task_id: task.into(),
            capability: capability.into(),
            lease_epoch: epoch,
            kind,
            request_id: request_id.into(),
            candidate_id: candidate_id.into(),
            response_hash: rex_protocol::schema::canonical_hash(&content).unwrap(),
            content: content.into(),
        }
    }
    #[test]
    fn ultra_loop_runs_over_the_protocol_surface() {
        let d = tempdir().unwrap();
        let w = d.path().join("ws");
        let daemon =
            HarnessDaemon::open(d.path().join("state"), DaemonPolicy::conservative(&w)).unwrap();
        let ex = daemon.execute(req("r-ultra")).unwrap();
        let open = daemon
            .ultra_open(UltraOpenRequest {
                task_id: ex.task_id.clone(),
                capability: cap_of(&ex),
                lease_epoch: ex.lease.epoch,
                contract_draft: Some(valid_draft().into()),
            })
            .unwrap();
        assert_eq!(open.status, "collecting");
        assert_eq!(open.candidate_requests.len(), 3);
        assert!(open.evidence_requests.is_empty());
        assert_eq!(
            open.candidate_requests[0].obligation_ids,
            vec!["ob-1".to_string()]
        );
        // Candidates are sealed file bundles, materialized by the daemon
        // into isolated workspaces before any check runs.
        let mut view = open.clone();
        for (i, c) in open.candidate_requests.iter().enumerate() {
            let content =
                format!("{{\"files\":[{{\"path\":\"hello.txt\",\"content\":\"hello {i}\"}}]}}");
            view = daemon
                .ultra_submit(ultra_req(
                    &ex.task_id,
                    &cap_of(&ex),
                    ex.lease.epoch,
                    UltraSubmissionKind::Candidate,
                    &c.candidate_id,
                    &c.candidate_id,
                    &content,
                ))
                .unwrap();
        }
        assert_eq!(view.status, "ready");
        // The daemon has already executed the adversary scan and the
        // contract proofs inside every candidate workspace; the clean
        // bundles qualify the first candidate without any host input.
        assert_eq!(view.kernel_state, "completed");
        assert!(view.evidence_requests.is_empty());
        let candidate = view.candidate_requests[0].candidate_id.clone();
        // A host claiming verifier outcomes is rejected: the daemon runs
        // the verifier, hosts never set verdicts.
        let forged_verdict = daemon.ultra_submit(ultra_req(
            &ex.task_id,
            &cap_of(&ex),
            ex.lease.epoch,
            UltraSubmissionKind::Verifier,
            "anything",
            &candidate,
            "{\"outcomes\":[{\"obligation_id\":\"ob-1\",\"status\":\"proven\"}]}",
        ));
        assert!(forged_verdict.is_err());
        // A host claiming a clean adversary scan is likewise rejected.
        let forged_adversary = daemon.ultra_submit(ultra_req(
            &ex.task_id,
            &cap_of(&ex),
            ex.lease.epoch,
            UltraSubmissionKind::Adversary,
            "anything",
            &candidate,
            "{\"defects\":[]}",
        ));
        assert!(forged_adversary.is_err());
        let reopened =
            HarnessDaemon::open(d.path().join("state"), DaemonPolicy::conservative(&w)).unwrap();
        let again = reopened
            .ultra_open(UltraOpenRequest {
                task_id: ex.task_id.clone(),
                capability: cap_of(&ex),
                lease_epoch: ex.lease.epoch,
                contract_draft: Some(valid_draft().into()),
            })
            .unwrap();
        assert_eq!(again.kernel_state, "completed");
        assert!(again.evidence_requests.is_empty());
    }
    fn make_png(width: u32, height: u32, f: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
        let mut rgb = Vec::with_capacity((width * height * 3) as usize);
        for y in 0..height {
            for x in 0..width {
                rgb.extend_from_slice(&f(x, y));
            }
        }
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, width, height);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&rgb).unwrap();
        }
        out
    }

    #[test]
    fn visual_ultra_loop_is_gated_by_daemon_decoded_pixels() {
        let d = tempdir().unwrap();
        let w = d.path().join("ws");
        let daemon =
            HarnessDaemon::open(d.path().join("state"), DaemonPolicy::conservative(&w)).unwrap();
        let mut r = req("r-visual");
        r.task = "build a website landing page with animation".into();
        let ex = daemon.execute(r).unwrap();
        let open = daemon
            .ultra_open(UltraOpenRequest {
                task_id: ex.task_id.clone(),
                capability: cap_of(&ex),
                lease_epoch: ex.lease.epoch,
                contract_draft: Some(valid_draft().into()),
            })
            .unwrap();
        assert_eq!(open.candidate_requests[0].work_kind, "visual");
        let mut view = open.clone();
        for (i, c) in open.candidate_requests.iter().enumerate() {
            let content =
                format!("{{\"files\":[{{\"path\":\"hello.txt\",\"content\":\"hello {i}\"}}]}}");
            view = daemon
                .ultra_submit(ultra_req(
                    &ex.task_id,
                    &cap_of(&ex),
                    ex.lease.epoch,
                    UltraSubmissionKind::Candidate,
                    &c.candidate_id,
                    &c.candidate_id,
                    &content,
                ))
                .unwrap();
        }
        // Daemon adversary and verifier records exist, but visual work
        // waits for the host's declaration and the daemon's pixel check.
        assert_eq!(view.kernel_state, "awaiting_evidence");
        assert_eq!(view.evidence_requests.len(), 3);
        let put = |bytes: &[u8], kind: &str| {
            daemon
                .artifact_put(ArtifactPutRequest {
                    task_id: ex.task_id.clone(),
                    capability: cap_of(&ex),
                    lease_epoch: ex.lease.epoch,
                    kind: kind.into(),
                    bytes_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
                    candidate_id: None,
                    round: None,
                })
                .unwrap()
                .sha256
        };
        // Candidate 2 renders richest; the daemon must pick it over the
        // earlier declarations. Renders are pairwise machine-distinct.
        let renders = [
            make_png(128, 96, |x, y| [(x % 64) as u8 * 4, (y % 32) as u8 * 8, 0]),
            make_png(128, 96, |x, y| [0, (x % 32) as u8 * 8, (y % 64) as u8 * 4]),
            make_png(128, 96, |x, y| {
                [(x % 256) as u8, (y % 256) as u8, ((x * y) % 256) as u8]
            }),
        ];
        let phone = |i: u32| {
            make_png(64, 128, move |x, y| {
                [
                    255 - ((x + i * 16) % 64) as u8 * 4,
                    ((y + i * 32) % 128) as u8 * 2,
                    (7 + i) as u8,
                ]
            })
        };
        let requests = view.evidence_requests.clone();
        for (i, request) in requests.iter().enumerate() {
            let desktop = put(&renders[i], "screenshot");
            let phone_hash = put(&phone(i as u32), "screenshot");
            let replay = put(format!("replay-bytes-{i}").as_bytes(), "replay");
            let report = serde_json::json!({
                "thesis_id": format!("thesis-{i}"),
                "screenshots": [
                    {"viewport": "desktop", "artifact_hash": desktop},
                    {"viewport": "phone", "artifact_hash": phone_hash},
                ],
                "interaction_replay_hash": replay,
                "forbidden_patterns_hit": [],
                "critic_clean": true,
            })
            .to_string();
            view = daemon
                .ultra_submit(ultra_req(
                    &ex.task_id,
                    &cap_of(&ex),
                    ex.lease.epoch,
                    UltraSubmissionKind::Visual,
                    &request.request_id,
                    &request.candidate_id,
                    &report,
                ))
                .unwrap();
        }
        assert_eq!(view.kernel_state, "completed");
        let expected = &view.candidate_requests[2].candidate_id;
        let bundle = daemon
            .proof_bundle(TaskRefRequest {
                task_id: ex.task_id.clone(),
            })
            .unwrap();
        assert_eq!(
            bundle.qualified_candidate.as_deref(),
            Some(expected.as_str())
        );
    }

    #[test]
    fn visual_declaration_with_garbage_pixels_cannot_qualify() {
        let d = tempdir().unwrap();
        let w = d.path().join("ws");
        let daemon =
            HarnessDaemon::open(d.path().join("state"), DaemonPolicy::conservative(&w)).unwrap();
        let mut r = req("r-visual-garbage");
        r.task = "build a website landing page with animation".into();
        let ex = daemon.execute(r).unwrap();
        let open = daemon
            .ultra_open(UltraOpenRequest {
                task_id: ex.task_id.clone(),
                capability: cap_of(&ex),
                lease_epoch: ex.lease.epoch,
                contract_draft: Some(valid_draft().into()),
            })
            .unwrap();
        let mut view = open.clone();
        for (i, c) in open.candidate_requests.iter().enumerate() {
            let content =
                format!("{{\"files\":[{{\"path\":\"hello.txt\",\"content\":\"hello {i}\"}}]}}");
            view = daemon
                .ultra_submit(ultra_req(
                    &ex.task_id,
                    &cap_of(&ex),
                    ex.lease.epoch,
                    UltraSubmissionKind::Candidate,
                    &c.candidate_id,
                    &c.candidate_id,
                    &content,
                ))
                .unwrap();
        }
        // Every declaration points at bytes that are not a PNG. The daemon
        // decodes for itself, so the claims fail honestly and the kernel
        // fails closed instead of trusting the manifest.
        let requests = view.evidence_requests.clone();
        for (i, request) in requests.iter().enumerate() {
            let put = |bytes: &[u8]| {
                daemon
                    .artifact_put(ArtifactPutRequest {
                        task_id: ex.task_id.clone(),
                        capability: cap_of(&ex),
                        lease_epoch: ex.lease.epoch,
                        kind: "screenshot".into(),
                        bytes_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
                        candidate_id: None,
                        round: None,
                    })
                    .unwrap()
                    .sha256
            };
            let report = serde_json::json!({
                "thesis_id": format!("thesis-{i}"),
                "screenshots": [
                    {"viewport": "desktop", "artifact_hash": put(format!("fake-desktop-{i}").as_bytes())},
                    {"viewport": "phone", "artifact_hash": put(format!("fake-phone-{i}").as_bytes())},
                ],
                "interaction_replay_hash": put(format!("fake-replay-{i}").as_bytes()),
                "forbidden_patterns_hit": [],
                "critic_clean": true,
            })
            .to_string();
            view = daemon
                .ultra_submit(ultra_req(
                    &ex.task_id,
                    &cap_of(&ex),
                    ex.lease.epoch,
                    UltraSubmissionKind::Visual,
                    &request.request_id,
                    &request.candidate_id,
                    &report,
                ))
                .unwrap();
        }
        assert_eq!(view.kernel_state, "failed");
    }

    #[test]
    fn ultra_loop_rejects_human_tasks_and_forged_submissions() {
        let d = tempdir().unwrap();
        let w = d.path().join("ws");
        let daemon =
            HarnessDaemon::open(d.path().join("state"), DaemonPolicy::conservative(&w)).unwrap();
        let mut r = req("r-human");
        r.operator_is_agent = false;
        r.host = HostKind::Human;
        let human = daemon.execute(r).unwrap();
        let e = daemon
            .ultra_open(UltraOpenRequest {
                task_id: human.task_id.clone(),
                capability: cap_of(&human),
                lease_epoch: human.lease.epoch,
                contract_draft: Some(valid_draft().into()),
            })
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::ScopeDenied);
        let ex = daemon.execute(req("r-forge")).unwrap();
        let open = daemon
            .ultra_open(UltraOpenRequest {
                task_id: ex.task_id.clone(),
                capability: cap_of(&ex),
                lease_epoch: ex.lease.epoch,
                contract_draft: Some(valid_draft().into()),
            })
            .unwrap();
        let target = &open.candidate_requests[0];
        let mut forged = ultra_req(
            &ex.task_id,
            &cap_of(&ex),
            ex.lease.epoch,
            UltraSubmissionKind::Candidate,
            &target.candidate_id,
            &target.candidate_id,
            "honest answer",
        );
        forged.response_hash = "deadbeef".into();
        let e = daemon.ultra_submit(forged).unwrap_err();
        assert_eq!(e.code, ErrorCode::GateFailed);
        let unknown = ultra_req(
            &ex.task_id,
            &cap_of(&ex),
            ex.lease.epoch,
            UltraSubmissionKind::Candidate,
            "not-a-candidate",
            "not-a-candidate",
            "anything",
        );
        let e = daemon.ultra_submit(unknown).unwrap_err();
        assert_eq!(e.code, ErrorCode::GateFailed);
    }

    /// Drive an ultra task through candidates until the daemon gates
    /// settle the kernel; returns the final view.
    fn drive_ultra_candidates(
        daemon: &HarnessDaemon,
        ex: &ExecuteResponse,
        open: &rex_protocol::UltraViewResponse,
        contents: &[String],
    ) -> rex_protocol::UltraViewResponse {
        let mut view = open.clone();
        for (c, content) in open.candidate_requests.iter().zip(contents.iter()) {
            view = daemon
                .ultra_submit(ultra_req(
                    &ex.task_id,
                    &cap_of(ex),
                    ex.lease.epoch,
                    UltraSubmissionKind::Candidate,
                    &c.candidate_id,
                    &c.candidate_id,
                    content,
                ))
                .unwrap();
        }
        view
    }

    #[test]
    fn ultra_kernel_failure_fails_the_task_joined() {
        let d = tempdir().unwrap();
        let w = d.path().join("ws");
        let daemon =
            HarnessDaemon::open(d.path().join("state"), DaemonPolicy::conservative(&w)).unwrap();
        let ex = daemon.execute(req("r-join-fail")).unwrap();
        let open = daemon
            .ultra_open(UltraOpenRequest {
                task_id: ex.task_id.clone(),
                capability: cap_of(&ex),
                lease_epoch: ex.lease.epoch,
                contract_draft: Some(valid_draft().into()),
            })
            .unwrap();
        // Every candidate carries placeholder content: the daemon's own
        // adversary scan finds defects in all of them.
        let contents: Vec<String> = (0..open.candidate_requests.len())
            .map(|i| {
                format!(
                    "{{\"files\":[{{\"path\":\"hello.txt\",\"content\":\"hello {i} TODO: finish\"}}]}}"
                )
            })
            .collect();
        let view = drive_ultra_candidates(&daemon, &ex, &open, &contents);
        assert_eq!(view.kernel_state, "failed");
        // The joined state machine: kernel failure IS task failure.
        let t = daemon.load(&ex.task_id).unwrap();
        assert_eq!(t.state, TaskState::Failed);
        assert!(t
            .terminal_reason
            .unwrap_or_default()
            .contains("ultra kernel failed"));
    }

    #[test]
    fn promotion_and_task_completion_are_one_joined_transition() {
        let d = tempdir().unwrap();
        let w = d.path().join("ws");
        let daemon =
            HarnessDaemon::open(d.path().join("state"), DaemonPolicy::conservative(&w)).unwrap();
        let ex = daemon.execute(req("r-join-ok")).unwrap();
        let open = daemon
            .ultra_open(UltraOpenRequest {
                task_id: ex.task_id.clone(),
                capability: cap_of(&ex),
                lease_epoch: ex.lease.epoch,
                contract_draft: Some(valid_draft().into()),
            })
            .unwrap();
        let contents: Vec<String> = (0..open.candidate_requests.len())
            .map(|i| {
                format!("{{\"files\":[{{\"path\":\"hello.txt\",\"content\":\"hello {i}\"}}]}}")
            })
            .collect();
        let view = drive_ultra_candidates(&daemon, &ex, &open, &contents);
        assert_eq!(view.kernel_state, "completed");
        // Kernel completion alone leaves the task honestly non-terminal:
        // only a committed promotion completes it.
        let t = daemon.load(&ex.task_id).unwrap();
        assert!(!t.state.is_terminal());
        let receipt = daemon
            .promote_and_join(&rex_protocol::UltraPromoteRequest {
                task_id: ex.task_id.clone(),
                capability: cap_of(&ex),
                lease_epoch: ex.lease.epoch,
            })
            .unwrap();
        assert_eq!(
            receipt.state,
            rex_ultra::promotion::PromotionState::Committed
        );
        let t = daemon.load(&ex.task_id).unwrap();
        assert_eq!(t.state, TaskState::Completed);
        assert!(t.evidence.contains_key("ultra_promotion_receipt"));
        assert!(t.evidence.contains_key("ultra_qualified_candidate"));
        assert!(t
            .terminal_reason
            .unwrap_or_default()
            .contains("promotion committed"));
        // The promoted bundle really landed in the workspace.
        assert!(w.join("hello.txt").exists());
    }

    #[test]
    fn crash_between_promotion_and_task_persist_is_healed_on_open() {
        let d = tempdir().unwrap();
        let w = d.path().join("ws");
        let root = d.path().join("state");
        let daemon = HarnessDaemon::open(&root, DaemonPolicy::conservative(&w)).unwrap();
        let ex = daemon.execute(req("r-join-crash")).unwrap();
        let open = daemon
            .ultra_open(UltraOpenRequest {
                task_id: ex.task_id.clone(),
                capability: cap_of(&ex),
                lease_epoch: ex.lease.epoch,
                contract_draft: Some(valid_draft().into()),
            })
            .unwrap();
        let contents: Vec<String> = (0..open.candidate_requests.len())
            .map(|i| {
                format!("{{\"files\":[{{\"path\":\"hello.txt\",\"content\":\"hello {i}\"}}]}}")
            })
            .collect();
        let view = drive_ultra_candidates(&daemon, &ex, &open, &contents);
        assert_eq!(view.kernel_state, "completed");
        // Simulate the crash window: the promotion commits (its receipt is
        // durable) but the daemon dies before the task transition persists.
        let bridge = UltraHostBridge::open(&root).unwrap();
        let contract = bridge.frozen_contract(&ex.task_id).unwrap();
        let plan = bridge.frozen_skill_plan(&ex.task_id).unwrap().unwrap();
        let receipt = bridge
            .promote(&ex.task_id, &contract, &w, &plan.gates)
            .unwrap();
        assert_eq!(
            receipt.state,
            rex_ultra::promotion::PromotionState::Committed
        );
        drop(daemon);
        // Reopening replays the join from the durable records.
        let healed = HarnessDaemon::open(&root, DaemonPolicy::conservative(&w)).unwrap();
        let t = healed.load(&ex.task_id).unwrap();
        assert_eq!(t.state, TaskState::Completed);
        assert!(t.evidence.contains_key("ultra_promotion_receipt"));
    }

    #[test]
    fn standard_agent_tasks_reject_ultra_operations() {
        let d = tempdir().unwrap();
        let w = d.path().join("ws");
        let daemon =
            HarnessDaemon::open(d.path().join("state"), DaemonPolicy::conservative(&w)).unwrap();
        let mut standard = req("r-standard");
        standard.ultra = false;
        let ex = daemon.execute(standard).unwrap();
        let error = daemon
            .ultra_open(UltraOpenRequest {
                task_id: ex.task_id.clone(),
                capability: cap_of(&ex),
                lease_epoch: ex.lease.epoch,
                contract_draft: Some(valid_draft().into()),
            })
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::ScopeDenied);
        assert!(error.message.contains("Ultra-mode task"));
    }

    #[test]
    fn ultra_open_binds_a_compiled_skill_plan_once() {
        let d = tempdir().unwrap();
        let w = d.path().join("ws");
        fs::create_dir_all(&w).unwrap();
        fs::write(w.join("Cargo.toml"), "[package]").unwrap();
        let daemon =
            HarnessDaemon::open(d.path().join("state"), DaemonPolicy::conservative(&w)).unwrap();
        let ex = daemon.execute(req("r-skills")).unwrap();
        let open = daemon
            .ultra_open(UltraOpenRequest {
                task_id: ex.task_id.clone(),
                capability: cap_of(&ex),
                lease_epoch: ex.lease.epoch,
                contract_draft: Some(valid_draft().into()),
            })
            .unwrap();
        let plan = open.skill_plan.expect("skill plan bound");
        assert!(plan.selected.iter().any(|s| s.starts_with("shared-laws@")));
        assert!(plan
            .gates
            .iter()
            .any(|g| g.id == "scope-diff" && g.required));
        let hash = plan.plan_hash.clone();
        assert!(!hash.is_empty());
        let again = daemon
            .ultra_open(UltraOpenRequest {
                task_id: ex.task_id.clone(),
                capability: cap_of(&ex),
                lease_epoch: ex.lease.epoch,
                contract_draft: Some(valid_draft().into()),
            })
            .unwrap();
        assert_eq!(again.skill_plan.unwrap().plan_hash, hash);
        let events = daemon
            .events(EventsRequest {
                task_id: ex.task_id.clone(),
                after_seq: 0,
                limit: None,
            })
            .unwrap();
        let bindings = events
            .events
            .iter()
            .filter(|e| e.kind == "ultra_skill_plan")
            .count();
        assert_eq!(
            bindings, 1,
            "plan binding is recorded once per hash, not per open"
        );
    }
    #[test]
    fn ultra_promotion_fails_closed_during_the_audit_freeze() {
        let d = tempdir().unwrap();
        let w = d.path().join("ws");
        fs::create_dir_all(&w).unwrap();
        let daemon =
            HarnessDaemon::open(d.path().join("state"), DaemonPolicy::conservative(&w)).unwrap();
        let ex = daemon.execute(req("r-promote")).unwrap();
        let open = daemon
            .ultra_open(UltraOpenRequest {
                task_id: ex.task_id.clone(),
                capability: cap_of(&ex),
                lease_epoch: ex.lease.epoch,
                contract_draft: Some(valid_draft().into()),
            })
            .unwrap();
        // promotion before completion fails closed
        assert!(daemon
            .ultra_promote(rex_protocol::UltraPromoteRequest {
                task_id: ex.task_id.clone(),
                capability: cap_of(&ex),
                lease_epoch: ex.lease.epoch
            })
            .is_err());
        let mut view = open.clone();
        for (i, c) in open.candidate_requests.iter().enumerate() {
            let content = format!(
                "{{\"files\":[{{\"path\":\"hello.txt\",\"content\":\"hello {i}\"}},{{\"path\":\"result.txt\",\"content\":\"PASS {i}\"}}]}}"
            );
            view = daemon
                .ultra_submit(ultra_req(
                    &ex.task_id,
                    &cap_of(&ex),
                    ex.lease.epoch,
                    UltraSubmissionKind::Candidate,
                    &c.candidate_id,
                    &c.candidate_id,
                    &content,
                ))
                .unwrap();
        }
        // The daemon-executed gates qualify the first clean candidate with
        // no host input at all.
        assert_eq!(view.kernel_state, "completed");
        // The rebuilt path promotes through the joined state machine:
        // every mandatory gate re-executes on a confined stage, the swap is
        // atomic, and the task completes only on a committed receipt.
        let promoted = daemon
            .ultra_promote(rex_protocol::UltraPromoteRequest {
                task_id: ex.task_id.clone(),
                capability: cap_of(&ex),
                lease_epoch: ex.lease.epoch,
            })
            .unwrap();
        assert_eq!(promoted.state, "committed");
        assert!(!promoted.gates_rerun.is_empty());
        assert!(promoted.gates_not_rerun.is_empty());
        let written = fs::read_to_string(w.join("result.txt")).unwrap();
        assert!(written.starts_with("PASS"));
        let t = daemon.load(&ex.task_id).unwrap();
        assert_eq!(t.state, TaskState::Completed);
        let events = daemon
            .events(EventsRequest {
                task_id: ex.task_id.clone(),
                after_seq: 0,
                limit: None,
            })
            .unwrap();
        assert!(events.events.iter().any(|e| e.kind == "ultra_promotion"));
    }

    /// A fully promoted ultra task: frozen plan, committed receipt,
    /// daemon-executed evidence, completed durable task.
    fn promoted_ultra_task(
        tag: &str,
    ) -> (tempfile::TempDir, HarnessDaemon, String, std::path::PathBuf) {
        let d = tempdir().unwrap();
        let w = d.path().join("ws");
        fs::create_dir_all(&w).unwrap();
        let daemon =
            HarnessDaemon::open(d.path().join("state"), DaemonPolicy::conservative(&w)).unwrap();
        let ex = daemon.execute(req(tag)).unwrap();
        let open = daemon
            .ultra_open(UltraOpenRequest {
                task_id: ex.task_id.clone(),
                capability: cap_of(&ex),
                lease_epoch: ex.lease.epoch,
                contract_draft: Some(valid_draft().into()),
            })
            .unwrap();
        for (i, c) in open.candidate_requests.iter().enumerate() {
            let content = format!(
                "{{\"files\":[{{\"path\":\"hello.txt\",\"content\":\"hello {i}\"}},{{\"path\":\"result.txt\",\"content\":\"PASS {i}\"}}]}}"
            );
            daemon
                .ultra_submit(ultra_req(
                    &ex.task_id,
                    &cap_of(&ex),
                    ex.lease.epoch,
                    UltraSubmissionKind::Candidate,
                    &c.candidate_id,
                    &c.candidate_id,
                    &content,
                ))
                .unwrap();
        }
        daemon
            .ultra_promote(rex_protocol::UltraPromoteRequest {
                task_id: ex.task_id.clone(),
                capability: cap_of(&ex),
                lease_epoch: ex.lease.epoch,
            })
            .unwrap();
        (d, daemon, ex.task_id.clone(), w)
    }

    #[test]
    fn proof_bundle_binds_frozen_plan_chains_events_and_verifies() {
        let (_d, daemon, task_id, _w) = promoted_ultra_task("r-proofv2");
        let bundle = daemon
            .proof_bundle(TaskRefRequest {
                task_id: task_id.clone(),
            })
            .unwrap();
        assert_eq!(bundle.proof_version, 2);
        let plan_hash = bundle.skill_plan_hash.clone().unwrap();
        assert_eq!(bundle.skill_plan.unwrap().plan_hash, plan_hash);
        assert_ne!(
            bundle.events_chain_head,
            rex_custody::capability::hex_sha256(b"rex-proof-events-genesis")
        );
        assert_eq!(bundle.bundle_mac.len(), 64);
        assert!(!bundle.evidence_manifest.is_empty());
        for manifest in bundle.evidence_manifest.values() {
            assert_eq!(
                manifest.response_hash_recorded, manifest.response_hash_recomputed,
                "stored candidate content matches its recorded hash"
            );
            assert!(manifest.fully_evidenced);
            assert!(manifest.adversary_record_hash.is_some());
            assert!(manifest.verifier_record_hash.is_some());
        }
        let report = daemon
            .verify_proof_bundle(TaskRefRequest { task_id })
            .unwrap();
        assert!(report.persisted_present);
        assert!(report.content_hash_consistent);
        assert!(report.mac_valid);
        assert!(report.deterministic);
        assert!(report.response_hashes_verified);
        assert!(report.verdict);
    }

    #[test]
    fn proof_bundle_is_deterministic_under_workspace_mutation() {
        let (_d, daemon, task_id, w) = promoted_ultra_task("r-proofdet");
        let first = daemon
            .proof_bundle(TaskRefRequest {
                task_id: task_id.clone(),
            })
            .unwrap();
        // Mutate the workspace: a fresh skill-plan compile would see
        // different repository facts, but the proof binds the frozen plan.
        fs::create_dir_all(w.join("src")).unwrap();
        fs::write(w.join("src").join("extra.rs"), "fn extra() {}").unwrap();
        fs::write(w.join("README.md"), "changed after promotion").unwrap();
        let second = daemon
            .proof_bundle(TaskRefRequest {
                task_id: task_id.clone(),
            })
            .unwrap();
        assert_eq!(first.bundle_hash, second.bundle_hash);
        let report = daemon
            .verify_proof_bundle(TaskRefRequest { task_id })
            .unwrap();
        assert!(report.deterministic);
        assert!(report.verdict);
    }

    #[test]
    fn proof_verification_detects_persisted_bundle_tampering() {
        let (d, daemon, task_id, _w) = promoted_ultra_task("r-prooftamper");
        daemon
            .proof_bundle(TaskRefRequest {
                task_id: task_id.clone(),
            })
            .unwrap();
        // A state editor rewrites the persisted bundle but cannot recompute
        // the MAC without the daemon-held key.
        let path = d
            .path()
            .join("state")
            .join("proofs")
            .join(format!("{task_id}.json"));
        let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value["state"] = json!("failed");
        value["qualified_candidate"] = json!("forged-candidate");
        fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        let report = daemon
            .verify_proof_bundle(TaskRefRequest { task_id })
            .unwrap();
        assert!(report.persisted_present);
        assert!(
            !report.content_hash_consistent,
            "edited fields no longer hash to the stored bundle hash"
        );
        assert!(
            report.mac_valid,
            "the MAC honestly covers the unedited hash"
        );
        assert!(
            report.deterministic,
            "the stored hash still equals a fresh assembly; the content check is what catches the edit"
        );
        assert!(!report.verdict);
    }

    #[test]
    fn proof_verification_detects_event_log_tampering() {
        let (d, daemon, task_id, _w) = promoted_ultra_task("r-proofev");
        daemon
            .proof_bundle(TaskRefRequest {
                task_id: task_id.clone(),
            })
            .unwrap();
        // Drop the promotion event from the durable log: the chain head
        // changes and the persisted bundle no longer reassembles.
        let log = d
            .path()
            .join("state")
            .join("tasks")
            .join(&task_id)
            .join("events.jsonl");
        let kept: Vec<String> = fs::read_to_string(&log)
            .unwrap()
            .lines()
            .filter(|line| !line.contains("ultra_promotion"))
            .map(str::to_string)
            .collect();
        fs::write(&log, kept.join("\n") + "\n").unwrap();
        let report = daemon
            .verify_proof_bundle(TaskRefRequest { task_id })
            .unwrap();
        assert!(!report.deterministic, "edited event log breaks the chain");
        assert!(!report.verdict);
    }

    #[test]
    fn proof_verification_detects_adapter_content_drift() {
        let (d, daemon, task_id, _w) = promoted_ultra_task("r-proofdrift");
        daemon
            .proof_bundle(TaskRefRequest {
                task_id: task_id.clone(),
            })
            .unwrap();
        // Rewrite a stored candidate response body in place: the recorded
        // response hash no longer matches the stored content.
        let adapter_path = d
            .path()
            .join("state")
            .join("ultra")
            .join(format!("{task_id}.json"));
        let raw = fs::read_to_string(&adapter_path).unwrap();
        assert!(raw.contains("PASS 0"));
        fs::write(&adapter_path, raw.replacen("PASS 0", "PWNED", 1)).unwrap();
        let report = daemon
            .verify_proof_bundle(TaskRefRequest { task_id })
            .unwrap();
        assert!(
            !report.response_hashes_verified,
            "edited candidate content is caught by hash comparison"
        );
        assert!(!report.verdict);
    }

    #[test]
    fn proof_bundle_fails_closed_on_frozen_plan_drift() {
        let (d, daemon, task_id, _w) = promoted_ultra_task("r-proofplandrift");
        // Corrupt the recorded frozen-plan hash in the durable task state.
        let task_path = d
            .path()
            .join("state")
            .join("tasks")
            .join(&task_id)
            .join("task.json");
        let mut value: Value = serde_json::from_slice(&fs::read(&task_path).unwrap()).unwrap();
        value["ultra_skill_plan_hash"] = json!("0".repeat(64));
        fs::write(&task_path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        let err = daemon.proof_bundle(TaskRefRequest { task_id }).unwrap_err();
        assert!(format!("{err:?}").contains("frozen skill plan hash drift"));
    }
    #[test]
    fn host_resume_requires_and_rotates_the_handle() {
        let d = tempdir().unwrap();
        let w = d.path().join("ws");
        let daemon =
            HarnessDaemon::open(d.path().join("state"), DaemonPolicy::conservative(&w)).unwrap();
        let a = daemon.execute(req("r-handle")).unwrap();
        let handle = a
            .host_resume_handle
            .clone()
            .expect("handle issued at creation");
        assert!(handle.starts_with("hrh-"));
        // missing handle -> denied
        assert_eq!(
            daemon.execute(req("r-handle")).unwrap_err().code,
            ErrorCode::ScopeDenied
        );
        // wrong handle -> denied
        let mut wrong = req("r-handle");
        wrong.resume_handle = Some("hrh-forged".into());
        assert_eq!(
            daemon.execute(wrong).unwrap_err().code,
            ErrorCode::ScopeDenied
        );
        // correct handle -> resumed and rotated
        let mut good = req("r-handle");
        good.resume_handle = Some(handle.clone());
        let b = daemon.execute(good).unwrap();
        assert!(b.resumed);
        assert_eq!(b.task_id, a.task_id);
        let rotated = b.host_resume_handle.clone().expect("rotated handle issued");
        assert_ne!(rotated, handle);
        // the old handle is dead; the rotated handle works
        let mut stale = req("r-handle");
        stale.resume_handle = Some(handle);
        assert_eq!(
            daemon.execute(stale).unwrap_err().code,
            ErrorCode::ScopeDenied
        );
        let mut current = req("r-handle");
        current.resume_handle = Some(rotated);
        assert!(daemon.execute(current).unwrap().resumed);
    }

    #[test]
    fn follow_up_resumes_active_task_without_creating_a_task() {
        let d = tempdir().unwrap();
        let w = d.path().join("ws");
        let root = d.path().join("state");
        let daemon = HarnessDaemon::open(&root, DaemonPolicy::conservative(&w)).unwrap();
        let first = daemon.execute(req("r-follow-active")).unwrap();
        let first_handle = first.host_resume_handle.clone().unwrap();

        let mut follow_up = req("r-follow-active");
        follow_up.task_id = Some(first.task_id.clone());
        follow_up.resume_handle = Some(first_handle.clone());
        follow_up.follow_up = Some("Continue with the next active step".into());
        let resumed = daemon.execute(follow_up).unwrap();

        assert!(resumed.resumed);
        assert_eq!(resumed.task_id, first.task_id);
        let rotated = resumed.host_resume_handle.clone().unwrap();
        assert_ne!(rotated, first_handle);
        assert_eq!(fs::read_dir(root.join("tasks")).unwrap().count(), 1);
        let events = daemon
            .events(EventsRequest {
                task_id: first.task_id.clone(),
                after_seq: 0,
                limit: None,
            })
            .unwrap();
        assert!(events.events.iter().any(|event| {
            event.kind == "host_follow_up"
                && event.detail.get("text").and_then(|value| value.as_str())
                    == Some("Continue with the next active step")
        }));

        let mut stale = req("r-follow-active");
        stale.task_id = Some(first.task_id.clone());
        stale.resume_handle = Some(first_handle);
        assert_eq!(
            daemon.execute(stale).unwrap_err().code,
            ErrorCode::ScopeDenied
        );

        let mut next = req("r-follow-active");
        next.task_id = Some(first.task_id);
        next.resume_handle = Some(rotated);
        assert!(daemon.execute(next).unwrap().resumed);
    }

    #[test]
    fn follow_up_resumes_completed_task_without_creating_a_task() {
        let d = tempdir().unwrap();
        let w = d.path().join("ws");
        let root = d.path().join("state");
        let daemon = HarnessDaemon::open(&root, DaemonPolicy::conservative(&w)).unwrap();
        let mut standard = req("r-follow-completed");
        standard.ultra = false;
        let first = daemon.execute(standard).unwrap();
        fs::write(w.join("proof.txt"), "verified").unwrap();
        let receipt = daemon
            .read(ReadRequest {
                task_id: first.task_id.clone(),
                capability: cap_of(&first),
                lease_epoch: first.lease.epoch,
                path: "proof.txt".into(),
                byte_range: None,
            })
            .unwrap()
            .receipt
            .unwrap();
        let completed = daemon
            .submit(SubmitRequest {
                task_id: first.task_id.clone(),
                capability: cap_of(&first),
                lease_epoch: first.lease.epoch,
                action_id: first.next.unwrap().action_id,
                narrative: "Completed after a verified run".into(),
                evidence: [("run".into(), receipt)].into_iter().collect(),
            })
            .unwrap();
        assert_eq!(completed.state, TaskState::Completed);

        let mut follow_up = req("r-follow-completed");
        follow_up.task_id = Some(first.task_id.clone());
        follow_up.resume_handle = Some(first.host_resume_handle.unwrap());
        follow_up.follow_up = Some("Revisit the completed result".into());
        let resumed = daemon.execute(follow_up).unwrap();

        assert!(resumed.resumed);
        assert_eq!(resumed.task_id, first.task_id);
        assert_eq!(resumed.state, TaskState::Completed);
        assert!(resumed.host_resume_handle.is_some());
        assert_eq!(fs::read_dir(root.join("tasks")).unwrap().count(), 1);
        let events = daemon
            .events(EventsRequest {
                task_id: resumed.task_id,
                after_seq: 0,
                limit: None,
            })
            .unwrap();
        assert!(events
            .events
            .iter()
            .any(|event| event.kind == "task_completed"));
        assert!(events.events.iter().any(|event| {
            event.kind == "host_follow_up"
                && event.detail.get("text").and_then(|value| value.as_str())
                    == Some("Revisit the completed result")
        }));
    }

    #[test]
    fn proof_bundle_records_the_full_verified_journey() {
        let d = tempdir().unwrap();
        let w = d.path().join("ws");
        fs::create_dir_all(&w).unwrap();
        let root = d.path().join("state");
        let daemon = HarnessDaemon::open(&root, DaemonPolicy::conservative(&w)).unwrap();
        let ex = daemon.execute(req("r-proof")).unwrap();
        let open = daemon
            .ultra_open(UltraOpenRequest {
                task_id: ex.task_id.clone(),
                capability: cap_of(&ex),
                lease_epoch: ex.lease.epoch,
                contract_draft: Some(valid_draft().into()),
            })
            .unwrap();
        let mut view = open.clone();
        for (i, c) in open.candidate_requests.iter().enumerate() {
            let content = format!(
                "{{\"files\":[{{\"path\":\"hello.txt\",\"content\":\"hello {i}\"}},{{\"path\":\"out.txt\",\"content\":\"v{i}\"}}]}}"
            );
            view = daemon
                .ultra_submit(ultra_req(
                    &ex.task_id,
                    &cap_of(&ex),
                    ex.lease.epoch,
                    UltraSubmissionKind::Candidate,
                    &c.candidate_id,
                    &c.candidate_id,
                    &content,
                ))
                .unwrap();
        }
        // The daemon-executed gates qualified a candidate with no host
        // input; qualification order is the kernel's deterministic
        // candidate-id order, not submission order.
        let candidate = view
            .candidate_requests
            .iter()
            .map(|c| c.candidate_id.clone())
            .min()
            .unwrap();
        assert_eq!(view.kernel_state, "completed");
        // The rebuilt promotion commits through the joined state machine;
        // the proof bundle records the committed receipt.
        let promoted = daemon
            .ultra_promote(rex_protocol::UltraPromoteRequest {
                task_id: ex.task_id.clone(),
                capability: cap_of(&ex),
                lease_epoch: ex.lease.epoch,
            })
            .unwrap();
        assert_eq!(promoted.state, "committed");
        let bundle = daemon
            .proof_bundle(TaskRefRequest {
                task_id: ex.task_id.clone(),
            })
            .unwrap();
        assert_eq!(bundle.kernel_state.as_deref(), Some("completed"));
        assert_eq!(
            bundle.qualified_candidate.as_deref(),
            Some(candidate.as_str())
        );
        assert_eq!(bundle.promotion_state.as_deref(), Some("committed"));
        assert!(bundle.skill_plan.is_some());
        assert!(!bundle.events.is_empty());
        assert!(!bundle.bundle_hash.is_empty());
        let again = daemon
            .proof_bundle(TaskRefRequest {
                task_id: ex.task_id.clone(),
            })
            .unwrap();
        assert_eq!(
            bundle.bundle_hash, again.bundle_hash,
            "proof bundle hash is deterministic"
        );
        let persisted: serde_json::Value = serde_json::from_slice(
            &fs::read(root.join("proofs").join(format!("{}.json", ex.task_id))).unwrap(),
        )
        .unwrap();
        assert_eq!(
            persisted["bundle_hash"].as_str().unwrap(),
            bundle.bundle_hash
        );
        assert_eq!(persisted["promotion_state"].as_str().unwrap(), "committed");
    }

    #[test]
    fn operational_calls_require_the_task_capability() {
        let d = tempdir().unwrap();
        let w = d.path().join("ws");
        fs::create_dir_all(&w).unwrap();
        fs::write(w.join("note.txt"), "hi").unwrap();
        let daemon =
            HarnessDaemon::open(d.path().join("state"), DaemonPolicy::conservative(&w)).unwrap();
        let ex = daemon.execute(req("r-authz")).unwrap();
        // Audit exploit: task id plus the status-visible lease epoch must
        // authorize nothing. Missing capability -> unauthorized.
        let bare = daemon
            .read(ReadRequest {
                task_id: ex.task_id.clone(),
                capability: String::new(),
                lease_epoch: ex.lease.epoch,
                path: "note.txt".into(),
                byte_range: None,
            })
            .unwrap_err();
        assert_eq!(bare.code, ErrorCode::Unauthorized);
        // Forged capability -> unauthorized.
        let forged = daemon
            .read(ReadRequest {
                task_id: ex.task_id.clone(),
                capability: "cap-forged".into(),
                lease_epoch: ex.lease.epoch,
                path: "note.txt".into(),
                byte_range: None,
            })
            .unwrap_err();
        assert_eq!(forged.code, ErrorCode::Unauthorized);
        // Another task's capability -> unauthorized (no cross-task deputy).
        let other = daemon.execute(req("r-authz-other")).unwrap();
        let cross = daemon
            .read(ReadRequest {
                task_id: ex.task_id.clone(),
                capability: cap_of(&other),
                lease_epoch: ex.lease.epoch,
                path: "note.txt".into(),
                byte_range: None,
            })
            .unwrap_err();
        assert_eq!(cross.code, ErrorCode::Unauthorized);
        // The real capability works.
        let ok = daemon
            .read(ReadRequest {
                task_id: ex.task_id.clone(),
                capability: cap_of(&ex),
                lease_epoch: ex.lease.epoch,
                path: "note.txt".into(),
                byte_range: None,
            })
            .unwrap();
        assert_eq!(ok.content, "hi");
        // Operator cancel requires the capability too.
        let denied = daemon
            .cancel(CancelRequest {
                task_id: ex.task_id.clone(),
                capability: "cap-forged".into(),
                reason: None,
            })
            .unwrap_err();
        assert_eq!(denied.code, ErrorCode::Unauthorized);
    }

    #[test]
    fn ultra_candidates_get_isolated_workspaces_and_daemon_verdicts() {
        let d = tempdir().unwrap();
        let w = d.path().join("ws");
        let root = d.path().join("state");
        let daemon = HarnessDaemon::open(&root, DaemonPolicy::conservative(&w)).unwrap();
        let ex = daemon.execute(req("r-isolation")).unwrap();
        let open = daemon
            .ultra_open(UltraOpenRequest {
                task_id: ex.task_id.clone(),
                capability: cap_of(&ex),
                lease_epoch: ex.lease.epoch,
                contract_draft: Some(valid_draft().into()),
            })
            .unwrap();
        // Prose candidates are rejected: only sealed file bundles become
        // isolated workspaces.
        let prose = daemon.ultra_submit(ultra_req(
            &ex.task_id,
            &cap_of(&ex),
            ex.lease.epoch,
            UltraSubmissionKind::Candidate,
            &open.candidate_requests[0].candidate_id,
            &open.candidate_requests[0].candidate_id,
            "trust me, the work is done",
        ));
        assert!(prose.is_err());
        // A bundle with a traversal path is rejected before any write.
        let escape = daemon.ultra_submit(ultra_req(
            &ex.task_id,
            &cap_of(&ex),
            ex.lease.epoch,
            UltraSubmissionKind::Candidate,
            &open.candidate_requests[0].candidate_id,
            &open.candidate_requests[0].candidate_id,
            "{\"files\":[{\"path\":\"../escape.txt\",\"content\":\"x\"}]}",
        ));
        assert!(escape.is_err());
        // Candidate 0 satisfies the contract; candidates 1 and 2 do not.
        for (i, c) in open.candidate_requests.iter().enumerate() {
            let body = if i == 0 {
                format!("{{\"files\":[{{\"path\":\"hello.txt\",\"content\":\"hello {i}\"}}]}}")
            } else {
                format!("{{\"files\":[{{\"path\":\"other.txt\",\"content\":\"v{i}\"}}]}}")
            };
            daemon
                .ultra_submit(ultra_req(
                    &ex.task_id,
                    &cap_of(&ex),
                    ex.lease.epoch,
                    UltraSubmissionKind::Candidate,
                    &c.candidate_id,
                    &c.candidate_id,
                    &body,
                ))
                .unwrap();
        }
        // Each candidate materialized into its own isolated root.
        let cand_dir = root.join("workspaces/candidates").join(&ex.task_id);
        let entries: Vec<_> = fs::read_dir(&cand_dir).unwrap().flatten().collect();
        assert_eq!(entries.len(), 3);
        let winner = &open.candidate_requests[0].candidate_id;
        assert!(cand_dir.join(winner).join("hello.txt").exists());
        assert!(!cand_dir.join(winner).join("other.txt").exists());
        // Clean adversary input for every candidate: the daemon verifier
        // has already decided which trees actually satisfy the contract,
        // so only candidate 0 can qualify and complete the kernel.
        let mut view = daemon
            .ultra_open(UltraOpenRequest {
                task_id: ex.task_id.clone(),
                capability: cap_of(&ex),
                lease_epoch: ex.lease.epoch,
                contract_draft: Some(valid_draft().into()),
            })
            .unwrap();
        for r in view.evidence_requests.clone() {
            view = daemon
                .ultra_submit(ultra_req(
                    &ex.task_id,
                    &cap_of(&ex),
                    ex.lease.epoch,
                    UltraSubmissionKind::Adversary,
                    &r.request_id,
                    &r.candidate_id,
                    "{\"defects\":[]}",
                ))
                .unwrap();
        }
        assert_eq!(view.kernel_state, "completed");
        // The qualified candidate is the one the daemon's own verifier
        // proved - never a host's say-so.
        let proof = daemon
            .proof_bundle(TaskRefRequest {
                task_id: ex.task_id.clone(),
            })
            .unwrap();
        assert_eq!(proof.qualified_candidate.as_deref(), Some(winner.as_str()));
    }

    #[test]
    fn human_stop_is_a_distinct_final_authority() {
        let d = tempdir().unwrap();
        let w = d.path().join("ws");
        let root = d.path().join("state");
        let daemon = HarnessDaemon::open(&root, DaemonPolicy::conservative(&w)).unwrap();
        let ex = daemon.execute(req("r-hstop")).unwrap();
        // Wrong token -> unauthorized; the task stays active.
        let bad = daemon
            .human_stop(HumanStopRequest {
                task_id: ex.task_id.clone(),
                human_token: "hst-forged".into(),
                reason: None,
            })
            .unwrap_err();
        assert_eq!(bad.code, ErrorCode::Unauthorized);
        assert_eq!(
            daemon
                .status(TaskRefRequest {
                    task_id: ex.task_id.clone()
                })
                .unwrap()
                .state,
            TaskState::Active
        );
        // The daemon-issued token (0600 in the state dir) stops an
        // agent-operated task finally, without the task capability.
        let token = fs::read_to_string(root.join("human-stop-token")).unwrap();
        let stopped = daemon
            .human_stop(HumanStopRequest {
                task_id: ex.task_id.clone(),
                human_token: token.trim().into(),
                reason: Some("stop".into()),
            })
            .unwrap();
        assert_eq!(stopped.state, TaskState::Cancelled);
        // Final: even the legitimate capability cannot act on a stopped task.
        let after = daemon
            .read(ReadRequest {
                task_id: ex.task_id.clone(),
                capability: cap_of(&ex),
                lease_epoch: ex.lease.epoch,
                path: "x".into(),
                byte_range: None,
            })
            .unwrap_err();
        assert_eq!(after.code, ErrorCode::TaskTerminal);
        // The token file is not world-readable.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(root.join("human-stop-token"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn resume_reissues_a_rotated_capability() {
        let d = tempdir().unwrap();
        let w = d.path().join("ws");
        fs::create_dir_all(&w).unwrap();
        fs::write(w.join("n.txt"), "v").unwrap();
        let daemon =
            HarnessDaemon::open(d.path().join("state"), DaemonPolicy::conservative(&w)).unwrap();
        let a = daemon.execute(req("r-rot")).unwrap();
        let original = cap_of(&a);
        let mut resume = req("r-rot");
        resume.resume_handle = a.host_resume_handle.clone();
        let b = daemon.execute(resume).unwrap();
        let rotated = cap_of(&b);
        assert_ne!(original, rotated, "capability rotates on resume");
        // The pre-rotation capability no longer authorizes.
        let stale = daemon
            .read(ReadRequest {
                task_id: a.task_id.clone(),
                capability: original,
                lease_epoch: b.lease.epoch,
                path: "n.txt".into(),
                byte_range: None,
            })
            .unwrap_err();
        assert_eq!(stale.code, ErrorCode::Unauthorized);
        let ok = daemon
            .read(ReadRequest {
                task_id: a.task_id.clone(),
                capability: rotated,
                lease_epoch: b.lease.epoch,
                path: "n.txt".into(),
                byte_range: None,
            })
            .unwrap();
        assert_eq!(ok.content, "v");
    }

    #[test]
    fn artifact_put_is_capability_gated_and_digest_bound() {
        let d = tempdir().unwrap();
        let w = d.path().join("ws");
        let root = d.path().join("state");
        let daemon = HarnessDaemon::open(&root, DaemonPolicy::conservative(&w)).unwrap();
        let ex = daemon.execute(req("r-artifact")).unwrap();
        let put = |capability: &str,
                   epoch: u64,
                   bytes_b64: &str,
                   cand: Option<&str>,
                   round: Option<u64>| {
            daemon.artifact_put(ArtifactPutRequest {
                task_id: ex.task_id.clone(),
                capability: capability.into(),
                lease_epoch: epoch,
                kind: "screenshot".into(),
                bytes_base64: bytes_b64.into(),
                candidate_id: cand.map(str::to_string),
                round,
            })
        };
        let shot = base64::engine::general_purpose::STANDARD.encode(b"png-v1");
        // Missing and forged capabilities are unauthorized.
        assert_eq!(
            put("", ex.lease.epoch, &shot, Some("cand-1"), Some(1))
                .unwrap_err()
                .code,
            ErrorCode::Unauthorized
        );
        assert_eq!(
            put("cap-forged", ex.lease.epoch, &shot, Some("cand-1"), Some(1))
                .unwrap_err()
                .code,
            ErrorCode::Unauthorized
        );
        // Stale epoch is sequencing failure, not authorization.
        assert_eq!(
            put(
                &cap_of(&ex),
                ex.lease.epoch + 9,
                &shot,
                Some("cand-1"),
                Some(1)
            )
            .unwrap_err()
            .code,
            ErrorCode::StaleLease
        );
        // Garbage base64 is malformed, never stored.
        assert_eq!(
            put(
                &cap_of(&ex),
                ex.lease.epoch,
                "!!!not-base64!!!",
                Some("cand-1"),
                Some(1)
            )
            .unwrap_err()
            .code,
            ErrorCode::MalformedRequest
        );
        // The real capability stores bytes and returns their digest.
        let ok = put(&cap_of(&ex), ex.lease.epoch, &shot, Some("cand-1"), Some(1)).unwrap();
        assert!(ok.fresh);
        assert_eq!(ok.bytes, 6);
        assert_eq!(ok.sha256.len(), 64);
        // Stored bytes land read-only under evidence/artifacts/<sha256>.
        let path = root.join("evidence/artifacts").join(&ok.sha256);
        assert!(path.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o444
            );
        }
        // Idempotent replay of the identical binding.
        let again = put(&cap_of(&ex), ex.lease.epoch, &shot, Some("cand-1"), Some(1)).unwrap();
        assert!(!again.fresh);
        assert_eq!(again.sha256, ok.sha256);
        // Reused digest across candidates or rounds is rejected.
        assert_eq!(
            put(&cap_of(&ex), ex.lease.epoch, &shot, Some("cand-2"), Some(1))
                .unwrap_err()
                .code,
            ErrorCode::IdempotencyConflict
        );
        assert_eq!(
            put(&cap_of(&ex), ex.lease.epoch, &shot, Some("cand-1"), Some(2))
                .unwrap_err()
                .code,
            ErrorCode::IdempotencyConflict
        );
        // The registration is on the durable event stream.
        let evs = daemon
            .events(EventsRequest {
                task_id: ex.task_id.clone(),
                after_seq: 0,
                limit: None,
            })
            .unwrap();
        assert!(evs.events.iter().any(|e| e.kind == "artifact_registered"
            && e.detail["sha256"] == serde_json::json!(ok.sha256)));
    }

    #[test]
    fn ultra_open_requires_an_executable_frozen_contract() {
        let d = tempdir().unwrap();
        let w = d.path().join("ws");
        let daemon =
            HarnessDaemon::open(d.path().join("state"), DaemonPolicy::conservative(&w)).unwrap();
        let ex = daemon.execute(req("r-contract")).unwrap();
        let open = |draft: Option<&str>| {
            daemon.ultra_open(UltraOpenRequest {
                task_id: ex.task_id.clone(),
                capability: cap_of(&ex),
                lease_epoch: ex.lease.epoch,
                contract_draft: draft.map(str::to_string),
            })
        };
        // No draft on first open fails closed: a plan-derived contract
        // would flatten every obligation into host-judged prose.
        let e = open(None).unwrap_err();
        assert_eq!(e.code, ErrorCode::MalformedRequest);
        // Host-judged behavior proofs are rejected, not flattened.
        let behavior = r#"{"obligations":[{"id":"ob-1","statement":"it works","proof":{"kind":"behavior_evidence","description":"check it works"}}]}"#;
        let e = open(Some(behavior)).unwrap_err();
        assert_eq!(e.code, ErrorCode::MalformedRequest);
        assert!(e.message.contains("ob-1"));
        // Malformed JSON is rejected with explainable errors.
        let e = open(Some("not a contract")).unwrap_err();
        assert_eq!(e.code, ErrorCode::MalformedRequest);
        // A valid executable draft opens and freezes; the candidate floor
        // is the taste floor (3), not the old two-candidate race.
        let view = open(Some(valid_draft())).unwrap();
        assert_eq!(view.candidate_requests.len(), 3);
        // Re-opening with the same draft is idempotent...
        let view2 = open(Some(valid_draft())).unwrap();
        assert_eq!(view2.candidate_requests.len(), 3);
        // ...but a conflicting draft is an idempotency conflict, never a
        // silent contract swap.
        let other = r#"{"obligations":[{"id":"ob-2","statement":"other","proof":{"kind":"file_exists","path":"x.txt"}}]}"#;
        let e = open(Some(other)).unwrap_err();
        assert_eq!(e.code, ErrorCode::IdempotencyConflict);
        // Ultra submit before open fails closed (fresh task, never opened).
        let ex2 = daemon.execute(req("r-contract-2")).unwrap();
        let target = &view.candidate_requests[0];
        let mut submit = ultra_req(
            &ex2.task_id,
            &cap_of(&ex2),
            ex2.lease.epoch,
            UltraSubmissionKind::Candidate,
            &target.candidate_id,
            &target.candidate_id,
            "answer",
        );
        submit.candidate_id = target.candidate_id.clone();
        let e = daemon.ultra_submit(submit).unwrap_err();
        assert_eq!(e.code, ErrorCode::GateFailed);
    }
    #[test]
    fn verified_resume_renews_a_lapsed_lease() {
        // Live Ultra runs work longer than one lease window between calls;
        // docs promise an expired lease plus the current handle still resumes.
        let d = tempdir().unwrap();
        let w = d.path().join("ws");
        let root = d.path().join("state");
        let daemon = HarnessDaemon::open(&root, DaemonPolicy::conservative(&w)).unwrap();
        let ex = daemon.execute(req("rrenew")).unwrap();
        let handle = ex.host_resume_handle.clone().unwrap();
        // Force the lease into the past in the persisted store.
        let file = root.join("tasks").join(&ex.task_id).join("task.json");
        let mut v: serde_json::Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        v.as_object_mut()
            .unwrap()
            .insert("lease_expires_ms".into(), serde_json::json!(1));
        fs::write(&file, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
        // Same-process lapse: resume renews the un-swept grant via heartbeat.
        let mut resume_req = req("rrenew-ignored");
        resume_req.task_id = Some(ex.task_id.clone());
        resume_req.resume_handle = Some(handle.clone());
        let r1 = daemon.execute(resume_req).unwrap();
        assert!(r1.resumed);
        assert!(r1.lease.expires_ms_from_now > 0);
        let cap1 = r1.task_capability.clone().unwrap();
        let n1 = daemon
            .next(NextRequest {
                task_id: ex.task_id.clone(),
                lease_epoch: r1.lease.epoch,
                capability: cap1,
            })
            .unwrap();
        assert_eq!(n1.state, TaskState::Active);
        // Cross-restart lapse: recovery suspends the grant; resume must
        // reactivate it through the custody resume path (epoch bumps).
        let epoch_before = r1.lease.epoch;
        let mut v2: serde_json::Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        v2.as_object_mut()
            .unwrap()
            .insert("lease_expires_ms".into(), serde_json::json!(1));
        fs::write(&file, serde_json::to_vec_pretty(&v2).unwrap()).unwrap();
        // Lapse the custody grant as well so recovery suspends it.
        let grants_dir = root.join("custody").join("grants");
        for entry in fs::read_dir(&grants_dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let mut g: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            g.as_object_mut()
                .unwrap()
                .get_mut("lease")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("expires_ms".into(), serde_json::json!(1));
            fs::write(&path, serde_json::to_vec_pretty(&g).unwrap()).unwrap();
        }
        drop(daemon);
        let daemon2 = HarnessDaemon::open(&root, DaemonPolicy::conservative(&w)).unwrap();
        let mut resume_req2 = req("rrenew-ignored-2");
        resume_req2.task_id = Some(ex.task_id.clone());
        resume_req2.resume_handle = Some(r1.host_resume_handle.clone().unwrap());
        let r2 = daemon2.execute(resume_req2).unwrap();
        assert!(r2.resumed);
        assert!(r2.lease.epoch > epoch_before);
        assert!(r2.lease.expires_ms_from_now > 0);
    }
}
