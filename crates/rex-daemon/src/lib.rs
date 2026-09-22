//! Durable local Harness daemon for caller-driven MCP work.
//!
//! The daemon has no model and no provider credentials. A subscribed host
//! agent proposes a plan and calls these methods; REX freezes the plan,
//! confines tools to one workspace, maintains custody and leases, and
//! decides completion from evidence. The trusted launcher, not an MCP
//! payload, decides whether mutations are pre-approved.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use rex_custody::capability::{hex_sha256, random_hex};
use rex_ultra::artifacts::{ArtifactError, ArtifactStore};
use rex_custody::{
    AgentProtocol, CapabilitySet, CapabilityToken, CompletionClaim, CompletionContract,
    Consumption, CustodiedToolRuntime, CustodyAcceptance, CustodyBudgets, CustodyError,
    CustodyRegistry, EvidenceGate, GateEvaluator, GateOutcome, LeaseTerms, OperatorIdentity,
    ToolClass, WorkerMode,
};
use rex_protocol::packets::{OperationStatus, PacketIdentity};
use rex_protocol::*;
use rex_tools::{ToolRequest, ToolResult, ToolRuntime};
use rex_ultra::external_kernel::{
    CandidateResponse, EvidenceKind, HostKernelStatus, KernelState,
};
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
        Ok(Self {
            root,
            policy,
            custody,
            tools,
            human_token_hash,
            artifacts,
        })
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
        let why = req
            .reason
            .unwrap_or_else(|| "human stop (final)".into());
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
            let handle = self.verify_and_rotate_resume_handle(&mut task, &req)?;
            let capability = self.rotate_capability(&mut task)?;
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
            let handle = self.verify_and_rotate_resume_handle(&mut task, &req)?;
            let capability = self.rotate_capability(&mut task)?;
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
        Ok(self.execute_view(&task, false, Some(host_resume_handle), Some(task_capability)))
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
        let result = self.call_tool(&mut t, ToolRequest::ReadFile { path: req.path })?;
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
            timeout_ms: Some(10 * 60 * 1000),
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
        let t = self.load(&req.task_id)?;
        let lease = lease_view(&t);
        let used_wall = elapsed(&t);
        let operation = status_operation(&t);
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
                ArtifactError::TooLarge { .. } | ArtifactError::Io(_) => perr(
                    ErrorCode::MalformedRequest,
                    e.to_string(),
                    &req.task_id,
                ),
                ArtifactError::ReusedDigest { .. } => perr(
                    ErrorCode::IdempotencyConflict,
                    e.to_string(),
                    &req.task_id,
                ),
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
                let reparsed = rex_ultra::contract::parse_contract(draft, &t.task)
                    .map_err(|errors| {
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
        let plan = self.skill_plan();
        if let Some(plan) = &plan {
            if t.ultra_skill_plan_hash.as_deref() != Some(plan.plan_hash.as_str()) {
                t.ultra_skill_plan_hash = Some(plan.plan_hash.clone());
                self.append_event(
                    &mut t,
                    "ultra_skill_plan",
                    json!({"plan_hash":plan.plan_hash,
                    "selected":plan.selected,"unsupported":plan.unsupported}),
                )?;
                self.persist(&t)?;
            }
        }
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
        Ok(ultra_view(t.task_id.clone(), &view, self.skill_plan()))
    }

    /// Promote the qualified candidate into the task workspace. Live lease,
    /// agent-operated tasks only; the kernel must be completed and the
    /// rollback path is verified by the promotion store.
    pub fn ultra_promote(
        &self,
        req: rex_protocol::UltraPromoteRequest,
    ) -> Result<rex_protocol::UltraPromoteResponse, ProtocolError> {
        let t = self.live(&req.task_id, req.lease_epoch, &req.capability)?;
        require_agent(&t)?;
        require_ultra(&t)?;
        // AUDIT FREEZE (2026-09-22): the external Ultra path was found to be a
        // host-assertion recorder, not an independent verifier. Promotion is
        // disabled until the rebuilt kernel (daemon-executed gates, isolated
        // candidate workspaces, signed proof) lands. Fail closed.
        return Err(perr(
            ErrorCode::GateFailed,
            "external Ultra promotion is disabled: the 2026-09-22 independent audit found no independent verification on this path; pending the protocol-2.0 rebuild",
            &t.task_id,
        ));
        #[allow(unreachable_code)]
        {
        let mut t = t;
        let bridge = UltraHostBridge::open(&self.root).map_err(|e| bridge_err(&t.task_id, e))?;
        let contract = bridge
            .frozen_contract(&t.task_id)
            .map_err(|e| bridge_err(&t.task_id, e))?;
        let receipt = bridge
            .promote(&t.task_id, &contract, &self.policy.workspace)
            .map_err(|e| bridge_err(&t.task_id, e))?;
        let state = match receipt.state {
            rex_ultra::promotion::PromotionState::Prepared => "prepared",
            rex_ultra::promotion::PromotionState::Committed => "committed",
            rex_ultra::promotion::PromotionState::RolledBack => "rolled_back",
            rex_ultra::promotion::PromotionState::CorruptState => "corrupt_state",
        };
        self.append_event(
            &mut t,
            "ultra_promotion",
            json!({"state":state,
            "candidate_id":receipt.candidate_id,"bundle_hash":receipt.bundle_hash,
            "staging_hash":receipt.staging_hash}),
        )?;
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
        drop(adapter);
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

    pub fn proof_bundle(&self, req: TaskRefRequest) -> Result<TaskProofBundle, ProtocolError> {
        let t = self.load(&req.task_id)?;
        let bridge = UltraHostBridge::open(&self.root).map_err(|e| bridge_err(&t.task_id, e))?;
        let adapter = bridge
            .load_existing(&t.task_id)
            .map_err(|e| bridge_err(&t.task_id, e))?;
        let (kernel_state, qualified_candidate) = match &adapter {
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
                (
                    Some(state.to_string()),
                    adapter.qualified_candidate().map(str::to_string),
                )
            }
            None => (None, None),
        };
        let skill_plan = self.skill_plan();
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
        let events = self
            .events(EventsRequest {
                task_id: t.task_id.clone(),
                after_seq: 0,
                limit: None,
            })
            .unwrap()
            .events;
        let bundle_hash = hash_json(&(
            &t.task_id,
            &t.request_hash,
            &t.task,
            &t.plan,
            t.state,
            &kernel_state,
            &qualified_candidate,
            &skill_plan,
            &promotion_state,
            &promotion,
            &events,
        ))?;
        let bundle = TaskProofBundle {
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
            bundle_hash,
        };
        let dir = self.root.join("proofs");
        fs::create_dir_all(&dir).map_err(internal)?;
        let path = dir.join(format!("{}.json", t.task_id));
        let temporary = path.with_extension("json.tmp");
        fs::write(
            &temporary,
            serde_json::to_vec_pretty(&bundle).map_err(internal)?,
        )
        .map_err(internal)?;
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
                })
                .collect(),
            unsupported: plan.unsupported,
        })
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
        let t = self.load(id)?;
        // Authorization first: a presented capability whose hash does not
        // match is denied before any state or lease detail is revealed.
        // Pre-2.0 records carry an empty hash and fail closed.
        if t.task_capability_hash.is_empty()
            || hex_sha256(capability.as_bytes()) != t.task_capability_hash
        {
            return Err(perr(
                ErrorCode::Unauthorized,
                "invalid task capability",
                id,
            ));
        }
        if t.state.is_terminal() {
            return Err(perr(ErrorCode::TaskTerminal, "task is terminal", id));
        }
        if epoch != t.lease_epoch {
            return Err(perr(ErrorCode::StaleLease, "lease epoch is stale", id));
        }
        if now_ms() >= t.lease_expires_ms {
            return Err(perr(ErrorCode::StaleLease, "lease expired", id));
        }
        self.custody
            .lock()
            .map_err(|_| internal("custody registry poisoned"))?
            .verify_token(&t.token, now_ms())
            .map_err(custody_err)?;
        Ok(t)
    }

    fn heartbeat(&self, t: &mut DurableTask) -> Result<(), ProtocolError> {
        let lease = self
            .custody
            .lock()
            .map_err(|_| internal("custody registry poisoned"))?
            .heartbeat(&t.token, t.heartbeat_seq, now_ms())
            .map_err(custody_err)?;
        t.heartbeat_seq = lease.next_seq;
        t.lease_expires_ms = lease.expires_ms;
        Ok(())
    }

    /// Mint a fresh operational capability and rotate the stored hash.
    /// Called at creation and on every verified resume: the rotated
    /// resume handle vouches for the new capability.
    fn rotate_capability(&self, t: &mut DurableTask) -> Result<String, ProtocolError> {
        let capability = format!("cap-{}", random_hex(24));
        t.task_capability_hash = hex_sha256(capability.as_bytes());
        Ok(capability)
    }

    fn verify_and_rotate_resume_handle(
        &self,
        t: &mut DurableTask,
        req: &ExecuteRequest,
    ) -> Result<String, ProtocolError> {
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
            "search_files",
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
            let content = format!(
                "{{\"files\":[{{\"path\":\"hello.txt\",\"content\":\"hello {i}\"}}]}}"
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
        // AUDIT FREEZE: even with a completed kernel, promotion fails closed
        // and writes nothing into the workspace.
        let err = daemon
            .ultra_promote(rex_protocol::UltraPromoteRequest {
                task_id: ex.task_id.clone(),
                capability: cap_of(&ex),
                lease_epoch: ex.lease.epoch,
            })
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::GateFailed);
        assert!(fs::read(w.join("result.txt")).is_err());
        let events = daemon
            .events(EventsRequest {
                task_id: ex.task_id.clone(),
                after_seq: 0,
                limit: None,
            })
            .unwrap();
        assert!(!events.events.iter().any(|e| e.kind == "ultra_promotion"));
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
        // AUDIT FREEZE: promotion fails closed; the proof bundle records no
        // promotion rather than claiming one.
        let err = daemon
            .ultra_promote(rex_protocol::UltraPromoteRequest {
                task_id: ex.task_id.clone(),
                capability: cap_of(&ex),
                lease_epoch: ex.lease.epoch,
            })
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::GateFailed);
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
        assert_eq!(bundle.promotion_state, None);
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
        assert!(persisted["promotion_state"].is_null());
    
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
        let put = |capability: &str, epoch: u64, bytes_b64: &str, cand: Option<&str>, round: Option<u64>| {
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
        assert_eq!(put("", ex.lease.epoch, &shot, Some("cand-1"), Some(1)).unwrap_err().code, ErrorCode::Unauthorized);
        assert_eq!(put("cap-forged", ex.lease.epoch, &shot, Some("cand-1"), Some(1)).unwrap_err().code, ErrorCode::Unauthorized);
        // Stale epoch is sequencing failure, not authorization.
        assert_eq!(put(&cap_of(&ex), ex.lease.epoch + 9, &shot, Some("cand-1"), Some(1)).unwrap_err().code, ErrorCode::StaleLease);
        // Garbage base64 is malformed, never stored.
        assert_eq!(put(&cap_of(&ex), ex.lease.epoch, "!!!not-base64!!!", Some("cand-1"), Some(1)).unwrap_err().code, ErrorCode::MalformedRequest);
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
            assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o444);
        }
        // Idempotent replay of the identical binding.
        let again = put(&cap_of(&ex), ex.lease.epoch, &shot, Some("cand-1"), Some(1)).unwrap();
        assert!(!again.fresh);
        assert_eq!(again.sha256, ok.sha256);
        // Reused digest across candidates or rounds is rejected.
        assert_eq!(put(&cap_of(&ex), ex.lease.epoch, &shot, Some("cand-2"), Some(1)).unwrap_err().code, ErrorCode::IdempotencyConflict);
        assert_eq!(put(&cap_of(&ex), ex.lease.epoch, &shot, Some("cand-1"), Some(2)).unwrap_err().code, ErrorCode::IdempotencyConflict);
        // The registration is on the durable event stream.
        let evs = daemon
            .events(EventsRequest { task_id: ex.task_id.clone(), after_seq: 0, limit: None })
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
}
