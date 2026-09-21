//! Autonomous agent loop for Simple Mode.
//!
//! One durable run drives many bounded model turns. The model plans in a
//! visible todo list, picks the next action, calls tools, consumes grounded
//! evidence, retries or replans, and stops only when concrete completion
//! gates pass or a real limit is hit. State lives outside the model context:
//!
//! - `brief.json` - immutable task brief, written once, never rewritten.
//! - `plan.json` - the structured todo plan the model maintains.
//! - `ledger.jsonl` - append-only evidence ledger; every turn, tool receipt,
//!   approval decision, gate result, and terminal reason lands here with a
//!   stable sequence number.
//! - `checkpoint.json` - resumable loop state, rewritten after every turn.
//! - `evidence/` - raw turn requests/responses, tool receipts, and captures,
//!   addressable by the ids the ledger references.
//!
//! The context handed to the model on each turn is rebuilt from that state:
//! brief + plan + budgets + a structured digest of recent turns + the exact
//! function-call/response pairing for the previous turn. There is no giant
//! rolling transcript and no summary-of-summary prose; old material stays on
//! disk, addressable, instead of being repeatedly rewritten.
//!
//! One loop is shared across provider protocols. Capability discovery selects a documented
//! Gemini, Anthropic Messages, or OpenAI-compatible wire adapter; models never get
//! their own control loop. Unknown/private protocols stop truthfully instead of being guessed.
//!
//! Hard controls: step/tool-call/time/token budgets, provider retry with
//! backoff, cancellation, trusted approval suspension (the model can never
//! release its own write), repeated-failure and no-progress detection, and
//! truthful terminal reasons.

use crate::http::Transport;
use crate::provider_failure::structured_http_failure;
use crate::providers::{find_spec, ProviderProtocol};
use crate::search::SearchRouter;
use crate::secrets::SecretStore;
use crate::service::ProviderService;
use rex_preview::{
    BrowserAction, BrowserEvidence, IterationReceipt, PreviewSupervisor, ProductionGate,
};
use rex_prompt::roles::{Role, ToolPolicy};
use rex_prompt::tools::ToolSpec;
use rex_prompt::{Assembler, ModuleKind};
use rex_search::SearchRequest;
use rex_tools::{PreparedCall, ToolRequest, ToolResult, ToolRuntime};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const DEFAULT_MAX_STEPS: usize = 24;
pub const HARD_MAX_STEPS: usize = 64;
pub const DEFAULT_MAX_TOOL_CALLS: usize = 80;
pub const HARD_MAX_TOOL_CALLS: usize = 200;
pub const DEFAULT_MAX_WALL_MS: u64 = 20 * 60 * 1000;
pub const DEFAULT_MAX_TOKENS: u64 = 250_000;
pub const MAX_GATE_ATTEMPTS: u8 = 3;
pub const MAX_CONSEC_FAILURES: u32 = 3;
pub const MAX_STALL_TURNS: u32 = 3;
pub const MAX_NO_PROGRESS_TURNS: u32 = 6;
pub const MAX_DENIALS: u32 = 2;
pub const APPROVAL_WAIT_MS: u64 = 60 * 60 * 1000;
const MAX_ACTIVE_RUNS: usize = 4;
const MAX_EVENTS: usize = 200;
const PROVIDER_RETRIES: u32 = 3;
const DIGEST_WINDOW: usize = 6;
const OUTCOME_CHARS: usize = 4_000;

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Budgets {
    pub max_steps: usize,
    pub max_tool_calls: usize,
    pub max_wall_ms: u64,
    pub max_tokens: u64,
}

impl Default for Budgets {
    fn default() -> Self {
        Self {
            max_steps: DEFAULT_MAX_STEPS,
            max_tool_calls: DEFAULT_MAX_TOOL_CALLS,
            max_wall_ms: DEFAULT_MAX_WALL_MS,
            max_tokens: DEFAULT_MAX_TOKENS,
        }
    }
}

impl Budgets {
    fn clamped(mut self) -> Self {
        self.max_steps = self.max_steps.clamp(1, HARD_MAX_STEPS);
        self.max_tool_calls = self.max_tool_calls.clamp(1, HARD_MAX_TOOL_CALLS);
        self.max_wall_ms = self.max_wall_ms.clamp(10_000, 6 * 60 * 60 * 1000);
        self.max_tokens = self.max_tokens.clamp(2_000, 4_000_000);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Pending,
    InProgress,
    Done,
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanItem {
    pub id: String,
    pub title: String,
    pub status: PlanStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Planning,
    Running,
    AwaitingApproval,
    Verifying,
    Completed,
    Blocked,
    Denied,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TerminalReason {
    Completed,
    GatesFailed { failures: Vec<String> },
    Blocked { detail: String },
    BudgetSteps { max_steps: usize },
    BudgetTime { max_wall_ms: u64 },
    BudgetTokens { max_tokens: u64 },
    BudgetToolCalls { max_tool_calls: usize },
    RepeatedFailure { tool: String },
    NoProgress { turns: u32 },
    Cancelled,
    Denied,
    ApprovalTimeout,
    ProviderError { detail: String },
    ModelStalled,
}

impl TerminalReason {
    fn status(&self) -> AgentStatus {
        match self {
            TerminalReason::Completed => AgentStatus::Completed,
            TerminalReason::Cancelled => AgentStatus::Cancelled,
            TerminalReason::Denied => AgentStatus::Denied,
            TerminalReason::GatesFailed { .. }
            | TerminalReason::Blocked { .. }
            | TerminalReason::BudgetSteps { .. }
            | TerminalReason::BudgetTime { .. }
            | TerminalReason::BudgetTokens { .. }
            | TerminalReason::BudgetToolCalls { .. }
            | TerminalReason::ApprovalTimeout => AgentStatus::Blocked,
            TerminalReason::RepeatedFailure { .. }
            | TerminalReason::NoProgress { .. }
            | TerminalReason::ProviderError { .. }
            | TerminalReason::ModelStalled => AgentStatus::Failed,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AgentEvent {
    PlanUpdated {
        items: Vec<PlanItem>,
    },
    ModelText {
        text: String,
    },
    ToolFinished {
        result: ToolResult,
    },
    ApprovalRequired {
        call: PreparedCall,
    },
    ApprovalResolved {
        call_id: String,
        approved: bool,
    },
    GateResult {
        attempt: u8,
        passed: bool,
        failures: Vec<String>,
    },
    Retry {
        attempt: u32,
        reason: String,
    },
    Info {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentPreview {
    pub session_id: String,
    pub url: String,
    pub desktop_shot: Option<String>,
    pub mobile_shot: Option<String>,
    pub receipts: Vec<IterationReceipt>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentSnapshot {
    pub id: String,
    pub task: String,
    pub status: AgentStatus,
    pub terminal_reason: Option<TerminalReason>,
    pub provider: String,
    pub model: String,
    pub plan: Vec<PlanItem>,
    pub step: usize,
    pub max_steps: usize,
    pub tool_calls: usize,
    pub max_tool_calls: usize,
    pub tokens_used: u64,
    pub max_tokens: u64,
    pub elapsed_ms: u64,
    pub max_wall_ms: u64,
    pub pending_approval: Option<PreparedCall>,
    /// Prompt-architecture identity this run is executing under.
    pub prompt_version: String,
    pub prompt_hash: String,
    pub events: Vec<AgentEvent>,
    pub preview: Option<AgentPreview>,
    pub completion_summary: Option<String>,
    pub error: Option<String>,
}

/// Immutable task brief. Written exactly once; `write_brief` refuses to
/// touch an existing brief so the original ask can never drift.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TaskBrief {
    id: String,
    task: String,
    provider: String,
    budgets: Budgets,
    created_at_ms: u128,
    /// Prompt-architecture identity this run started under. Briefs written
    /// before prompt versioning deserialize as "legacy-unknown".
    #[serde(default = "legacy_prompt_marker")]
    prompt_version: String,
    #[serde(default = "legacy_prompt_marker")]
    prompt_hash: String,
    /// Role the run executes under (worker, builder, adversary, ...).
    #[serde(default)]
    role: Option<String>,
    /// Least-authority tool scope. `None` = the full offering (and every
    /// legacy brief); `Some` = only these tool names may execute.
    #[serde(default)]
    allowed_tools: Option<Vec<String>>,
}

fn legacy_prompt_marker() -> String {
    "legacy-unknown".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct DigestAction {
    tool: String,
    ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error_kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct DigestEntry {
    turn: usize,
    actions: Vec<DigestAction>,
}

/// The exact protocol pairing for the previous model turn: the model's raw
/// parts and the tool responses we returned. Rebuilt into the next request
/// so the provider conversation stays valid without a full transcript.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct TurnPair {
    /// Provider-native assistant tool-call blocks (or OpenAI tool_call objects).
    model_parts: Vec<Value>,
    /// Provider-native tool-result blocks (or complete OpenAI tool messages).
    response_parts: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Checkpoint {
    step: usize,
    tool_calls: usize,
    tokens_used: u64,
    elapsed_base_ms: u64,
    gate_attempts: u8,
    consec_fail: u32,
    last_failure_sig: Option<String>,
    consec_stall: u32,
    turns_since_progress: u32,
    total_denials: u32,
    any_mutating_success: bool,
    model: String,
    digest: Vec<DigestEntry>,
    last_pair: Option<TurnPair>,
    /// Prompt identity when the checkpoint was written. Resuming across a
    /// different identity fails closed; a missing (legacy) identity migrates.
    #[serde(default = "legacy_prompt_marker")]
    prompt_version: String,
    #[serde(default = "legacy_prompt_marker")]
    prompt_hash: String,
}

struct RunShared {
    status: AgentStatus,
    terminal: Option<TerminalReason>,
    plan: Vec<PlanItem>,
    events: VecDeque<AgentEvent>,
    pending_approval: Option<PreparedCall>,
    decision: Option<bool>,
    step: usize,
    tool_calls: usize,
    tokens_used: u64,
    elapsed_ms: u64,
    model: String,
    preview: Option<AgentPreview>,
    preview_supervisor: Option<PreviewSupervisor>,
    preview_session: Option<String>,
    completion_summary: Option<String>,
    error: Option<String>,
}

struct RunHandle {
    shared: Mutex<RunShared>,
    cond: Condvar,
    cancel: AtomicBool,
    /// Test-only: stop the loop thread without writing a terminal state,
    /// exactly as a process kill would leave the run.
    #[cfg(test)]
    silent_cancel: AtomicBool,
}

impl RunHandle {
    fn push_event(shared: &Mutex<RunShared>, event: AgentEvent) {
        if let Ok(mut state) = shared.lock() {
            if state.events.len() >= MAX_EVENTS {
                state.events.pop_front();
            }
            state.events.push_back(event);
        }
    }
}

/// One decoded model-side call: either a rex-tools request or a loop-level
/// instruction handled by the harness itself.
enum AgentCall {
    /// A call we could not decode; reported back to the model as an error
    /// so it can correct itself instead of dying as a provider failure.
    BadCall {
        name: String,
        id: String,
        error: String,
    },
    Tool {
        id: String,
        request: ToolRequest,
    },
    UpdatePlan {
        items: Vec<PlanItem>,
    },
    WebSearch {
        id: String,
        query: String,
        max_results: usize,
    },
    CompleteTask {
        summary: String,
    },
}

pub struct AutonomousRunService<S: SecretStore + 'static, T: Transport + 'static> {
    service: Arc<ProviderService<S, T>>,
    search: Option<Arc<SearchRouter<S, T>>>,
    runs: Arc<Mutex<HashMap<String, Arc<RunHandle>>>>,
    runs_root: PathBuf,
}

impl<S: SecretStore + 'static, T: Transport + 'static> AutonomousRunService<S, T> {
    pub fn new(
        service: ProviderService<S, T>,
        search: Option<SearchRouter<S, T>>,
        runs_root: PathBuf,
    ) -> Self {
        Self {
            service: Arc::new(service),
            search: search.map(Arc::new),
            runs: Arc::new(Mutex::new(HashMap::new())),
            runs_root,
        }
    }

    fn run_dir(&self, id: &str) -> PathBuf {
        self.runs_root.join(id)
    }

    /// The provider core behind this run service. Ultra's one-shot roles
    /// (contract drafting, judge) call through the same safe routes.
    pub fn service(&self) -> &ProviderService<S, T> {
        &self.service
    }

    fn snapshot_of(&self, id: &str, handle: &RunHandle) -> AgentSnapshot {
        let brief = read_json::<TaskBrief>(&self.run_dir(id).join("state").join("brief.json")).ok();
        let state = handle.shared.lock().expect("run state poisoned");
        AgentSnapshot {
            id: id.to_string(),
            task: brief.as_ref().map(|b| b.task.clone()).unwrap_or_default(),
            status: state.status.clone(),
            terminal_reason: state.terminal.clone(),
            provider: brief
                .as_ref()
                .map(|b| b.provider.clone())
                .unwrap_or_default(),
            model: state.model.clone(),
            plan: state.plan.clone(),
            step: state.step,
            max_steps: brief.as_ref().map(|b| b.budgets.max_steps).unwrap_or(0),
            tool_calls: state.tool_calls,
            max_tool_calls: brief
                .as_ref()
                .map(|b| b.budgets.max_tool_calls)
                .unwrap_or(0),
            tokens_used: state.tokens_used,
            max_tokens: brief.as_ref().map(|b| b.budgets.max_tokens).unwrap_or(0),
            elapsed_ms: state.elapsed_ms,
            max_wall_ms: brief.as_ref().map(|b| b.budgets.max_wall_ms).unwrap_or(0),
            pending_approval: state.pending_approval.clone(),
            prompt_version: brief
                .as_ref()
                .map(|b| b.prompt_version.clone())
                .unwrap_or_else(legacy_prompt_marker),
            prompt_hash: brief
                .as_ref()
                .map(|b| b.prompt_hash.clone())
                .unwrap_or_else(legacy_prompt_marker),
            events: state.events.iter().cloned().collect(),
            preview: state.preview.clone(),
            completion_summary: state.completion_summary.clone(),
            error: state.error.clone(),
        }
    }

    pub fn begin(
        &self,
        task: &str,
        provider: &str,
        budgets: Option<Budgets>,
    ) -> Result<AgentSnapshot, String> {
        self.begin_with_model(task, provider, None, budgets)
    }

    /// Start the same autonomous runtime with any documented provider wire
    /// protocol. The selected model is data, not a separate loop.
    pub fn begin_with_model(
        &self,
        task: &str,
        provider: &str,
        requested_model: Option<&str>,
        budgets: Option<Budgets>,
    ) -> Result<AgentSnapshot, String> {
        self.begin_in_workspace(task, provider, requested_model, budgets, None)
    }

    /// Start a run in a caller-provided workspace. Ultra uses this so the
    /// builder, the adversary and the verifier all operate on one shared,
    /// disposable workspace. The workspace must sit under the runs root.
    pub fn begin_in_workspace(
        &self,
        task: &str,
        provider: &str,
        requested_model: Option<&str>,
        budgets: Option<Budgets>,
        workspace: Option<PathBuf>,
    ) -> Result<AgentSnapshot, String> {
        self.begin_in_workspace_with_role(
            task,
            provider,
            requested_model,
            budgets,
            workspace,
            Role::Worker,
        )
    }

    /// Start a run under an explicit REX role. The role selects the
    /// assembled system prompt and the least-authority tool scope: an
    /// adversary run, for example, can read but never write.
    pub fn begin_in_workspace_with_role(
        &self,
        task: &str,
        provider: &str,
        requested_model: Option<&str>,
        budgets: Option<Budgets>,
        workspace: Option<PathBuf>,
        role: Role,
    ) -> Result<AgentSnapshot, String> {
        let task = task.trim();
        if task.is_empty() {
            return Err("task is empty".into());
        }
        if task.chars().count() > 4_000 {
            return Err("task is too long".into());
        }
        let spec = find_spec(provider).ok_or_else(|| format!("unknown provider {provider}"))?;
        if !matches!(
            spec.protocol,
            ProviderProtocol::Gemini
                | ProviderProtocol::Anthropic
                | ProviderProtocol::OpenAiCompatible
        ) {
            return Err(format!(
                "provider {provider} has no documented autonomous protocol adapter"
            ));
        }
        let budgets = budgets.unwrap_or_default().clamped();
        {
            let runs = self.runs.lock().map_err(|_| "run registry poisoned")?;
            let active = runs
                .values()
                .filter(|h| {
                    h.shared
                        .lock()
                        .map(|s| s.terminal.is_none())
                        .unwrap_or(false)
                })
                .count();
            if active >= MAX_ACTIVE_RUNS {
                return Err("too many active runs; finish one first".into());
            }
        }
        let id = format!("agent-{}-{:x}", std::process::id(), now_ms());
        let run_dir = self.run_dir(&id);
        let state_dir = run_dir.join("state");
        let workspace = match workspace {
            Some(dir) => {
                let root = self.runs_root.canonicalize().map_err(|e| e.to_string())?;
                let canon = if dir.exists() {
                    dir.canonicalize().map_err(|e| e.to_string())?
                } else {
                    let parent = dir
                        .parent()
                        .ok_or_else(|| "invalid workspace path".to_string())?;
                    let parent = parent.canonicalize().map_err(|e| e.to_string())?;
                    parent.join(
                        dir.file_name()
                            .ok_or_else(|| "invalid workspace path".to_string())?,
                    )
                };
                if !canon.starts_with(&root) {
                    return Err("workspace must live under the runs root".into());
                }
                canon
            }
            None => run_dir.join("workspace"),
        };
        fs::create_dir_all(state_dir.join("evidence")).map_err(|e| e.to_string())?;
        fs::create_dir_all(&workspace).map_err(|e| e.to_string())?;
        let (scoped_tools, allowed_tools) = scoped_tools_for(role);
        let (system_prompt, prompt_version, prompt_hash) = assemble_run_prompt(role, &scoped_tools);
        let brief = TaskBrief {
            id: id.clone(),
            task: task.to_string(),
            provider: provider.to_string(),
            budgets,
            created_at_ms: now_ms(),
            prompt_version,
            prompt_hash,
            role: Some(role.name().to_string()),
            allowed_tools,
        };
        write_brief(&state_dir, &brief)?;
        write_json(&state_dir.join("plan.json"), &Vec::<PlanItem>::new())?;

        let handle = Arc::new(RunHandle {
            shared: Mutex::new(RunShared {
                status: AgentStatus::Planning,
                terminal: None,
                plan: Vec::new(),
                events: VecDeque::new(),
                pending_approval: None,
                decision: None,
                step: 0,
                tool_calls: 0,
                tokens_used: 0,
                elapsed_ms: 0,
                model: String::new(),
                preview: None,
                preview_supervisor: None,
                preview_session: None,
                completion_summary: None,
                error: None,
            }),
            cond: Condvar::new(),
            cancel: AtomicBool::new(false),
            #[cfg(test)]
            silent_cancel: AtomicBool::new(false),
        });
        self.runs
            .lock()
            .map_err(|_| "run registry poisoned")?
            .insert(id.clone(), handle.clone());

        let checkpoint = Checkpoint {
            model: requested_model.unwrap_or_default().trim().to_string(),
            prompt_version: brief.prompt_version.clone(),
            prompt_hash: brief.prompt_hash.clone(),
            ..Checkpoint::default()
        };
        let loop_ctx = LoopCtx {
            system_prompt,
            brief,
            handle: handle.clone(),
            service: self.service.clone(),
            search: self.search.clone(),
            state_dir,
            workspace,
            checkpoint,
        };
        std::thread::spawn(move || drive(loop_ctx));
        Ok(self.snapshot_of(&id, &handle))
    }

    pub fn snapshot(&self, run_id: &str) -> Option<AgentSnapshot> {
        let runs = self.runs.lock().ok()?;
        let handle = runs.get(run_id)?;
        Some(self.snapshot_of(run_id, handle))
    }

    /// Trusted UI decision. Only this path can release a prepared write; the
    /// model has no route to it.
    pub fn decide(&self, run_id: &str, approved: bool) -> Result<AgentSnapshot, String> {
        let runs = self.runs.lock().map_err(|_| "run registry poisoned")?;
        let handle = runs.get(run_id).ok_or("unknown run")?;
        {
            let mut state = handle.shared.lock().map_err(|_| "run state poisoned")?;
            if state.status != AgentStatus::AwaitingApproval || state.pending_approval.is_none() {
                return Err("run is not waiting for a decision".into());
            }
            state.decision = Some(approved);
            let call_id = state
                .pending_approval
                .as_ref()
                .map(|c| c.call_id.clone())
                .unwrap_or_default();
            push_locked(
                &mut state,
                AgentEvent::ApprovalResolved { call_id, approved },
            );
        }
        handle.cond.notify_all();
        Ok(self.snapshot_of(run_id, handle))
    }

    pub fn cancel(&self, run_id: &str) -> Result<AgentSnapshot, String> {
        let runs = self.runs.lock().map_err(|_| "run registry poisoned")?;
        let handle = runs.get(run_id).ok_or("unknown run")?;
        handle.cancel.store(true, Ordering::SeqCst);
        handle.cond.notify_all();
        // If the loop thread is mid-turn it flips the terminal state itself;
        // a parked approval wait wakes immediately on the notify above.
        std::thread::sleep(Duration::from_millis(50));
        Ok(self.snapshot_of(run_id, handle))
    }

    /// Rehydrate a run from its on-disk checkpoint after a process restart
    /// and continue the loop where it stopped.
    pub fn resume(&self, run_id: &str) -> Result<AgentSnapshot, String> {
        {
            let runs = self.runs.lock().map_err(|_| "run registry poisoned")?;
            if let Some(handle) = runs.get(run_id) {
                if handle
                    .shared
                    .lock()
                    .map(|s| s.terminal.is_none())
                    .unwrap_or(false)
                {
                    return Err("run is already active".into());
                }
            }
        }
        let run_dir = self.run_dir(run_id);
        let state_dir = run_dir.join("state");
        let brief = read_json::<TaskBrief>(&state_dir.join("brief.json"))
            .map_err(|_| "no checkpointed run with this id".to_string())?;
        if state_dir.join("terminal.json").exists() {
            return Err("run already reached a terminal state".into());
        }
        let checkpoint = read_json::<Checkpoint>(&state_dir.join("checkpoint.json"))
            .map_err(|_| "no checkpoint to resume from".to_string())?;
        let role = brief
            .role
            .as_deref()
            .and_then(Role::from_name)
            .unwrap_or(Role::Worker);
        let (scoped_tools, _) = scoped_tools_for(role);
        let (system_prompt, current_version, current_hash) =
            assemble_run_prompt(role, &scoped_tools);
        // Prompt-identity gate. A checkpoint written under different prompt
        // semantics must never resume into them silently: fail closed. A
        // legacy checkpoint (no identity) migrates onto the current one and
        // the run log says so.
        let migrated_legacy = check_resume_identity(&checkpoint, &current_version, &current_hash)?;
        let plan: Vec<PlanItem> = read_json(&state_dir.join("plan.json")).unwrap_or_default();
        let workspace = run_dir.join("workspace");
        if !workspace.is_dir() {
            return Err("checkpointed workspace is missing".into());
        }
        let handle = Arc::new(RunHandle {
            shared: Mutex::new(RunShared {
                status: AgentStatus::Running,
                terminal: None,
                plan,
                events: VecDeque::new(),
                pending_approval: None,
                decision: None,
                step: checkpoint.step,
                tool_calls: checkpoint.tool_calls,
                tokens_used: checkpoint.tokens_used,
                elapsed_ms: checkpoint.elapsed_base_ms,
                model: checkpoint.model.clone(),
                preview: None,
                preview_supervisor: None,
                preview_session: None,
                completion_summary: None,
                error: None,
            }),
            cond: Condvar::new(),
            cancel: AtomicBool::new(false),
            #[cfg(test)]
            silent_cancel: AtomicBool::new(false),
        });
        self.runs
            .lock()
            .map_err(|_| "run registry poisoned")?
            .insert(run_id.to_string(), handle.clone());
        RunHandle::push_event(
            &handle.shared,
            AgentEvent::Info {
                message: format!("resumed from checkpoint at step {}", checkpoint.step),
            },
        );
        if migrated_legacy {
            RunHandle::push_event(
                &handle.shared,
                AgentEvent::Info {
                    message: format!(
                        "legacy checkpoint without prompt metadata migrated to rex-prompt/{}#{:.12}",
                        current_version, current_hash
                    ),
                },
            );
        }
        let loop_ctx = LoopCtx {
            system_prompt,
            brief,
            handle: handle.clone(),
            service: self.service.clone(),
            search: self.search.clone(),
            state_dir,
            workspace,
            checkpoint,
        };
        std::thread::spawn(move || drive(loop_ctx));
        Ok(self.snapshot_of(run_id, &handle))
    }

    pub fn preview_action(&self, run_id: &str, action: &BrowserAction) -> Result<(), String> {
        let runs = self.runs.lock().map_err(|_| "run registry poisoned")?;
        let handle = runs.get(run_id).ok_or("unknown run")?;
        let state = handle.shared.lock().map_err(|_| "run state poisoned")?;
        match (&state.preview_supervisor, &state.preview_session) {
            (Some(sup), Some(sid)) => sup.action(sid, action).map_err(|e| e.to_string()),
            _ => Err("preview is not running".into()),
        }
    }

    pub fn capture(&self, run_id: &str) -> Result<BrowserEvidence, String> {
        let runs = self.runs.lock().map_err(|_| "run registry poisoned")?;
        let handle = runs.get(run_id).ok_or("unknown run")?;
        let state = handle.shared.lock().map_err(|_| "run state poisoned")?;
        match (&state.preview_supervisor, &state.preview_session) {
            (Some(sup), Some(sid)) => sup.capture(sid).map_err(|e| e.to_string()),
            _ => Err("preview is not running".into()),
        }
    }

    #[cfg(test)]
    fn halt_without_terminal(&self, run_id: &str) {
        if let Ok(runs) = self.runs.lock() {
            if let Some(handle) = runs.get(run_id) {
                handle.silent_cancel.store(true, Ordering::SeqCst);
                handle.cancel.store(true, Ordering::SeqCst);
                handle.cond.notify_all();
            }
        }
    }

    pub fn teardown(&self, run_id: &str) -> Result<(), String> {
        let handle = self
            .runs
            .lock()
            .map_err(|_| "run registry poisoned")?
            .remove(run_id);
        if let Some(handle) = handle {
            handle.cancel.store(true, Ordering::SeqCst);
            handle.cond.notify_all();
            if let Ok(mut state) = handle.shared.lock() {
                if let (Some(sup), Some(sid)) = (
                    state.preview_supervisor.take(),
                    state.preview_session.take(),
                ) {
                    let _ = sup.teardown(&sid);
                }
            }
        }
        Ok(())
    }
}

fn push_locked(state: &mut RunShared, event: AgentEvent) {
    if state.events.len() >= MAX_EVENTS {
        state.events.pop_front();
    }
    state.events.push_back(event);
}

fn write_brief(state_dir: &Path, brief: &TaskBrief) -> Result<(), String> {
    let path = state_dir.join("brief.json");
    if path.exists() {
        return Err("task brief is immutable and already exists".into());
    }
    write_json(&path, brief)
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    fs::write(path, text).map_err(|e| e.to_string())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

struct LoopCtx<S: SecretStore + 'static, T: Transport + 'static> {
    brief: TaskBrief,
    system_prompt: String,
    handle: Arc<RunHandle>,
    service: Arc<ProviderService<S, T>>,
    search: Option<Arc<SearchRouter<S, T>>>,
    state_dir: PathBuf,
    workspace: PathBuf,
    checkpoint: Checkpoint,
}

struct Ledger {
    file: fs::File,
    seq: u64,
}

impl Ledger {
    fn open(state_dir: &Path) -> Option<Self> {
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(state_dir.join("ledger.jsonl"))
            .ok()?;
        let seq = fs::read_to_string(state_dir.join("ledger.jsonl"))
            .map(|t| t.lines().count() as u64)
            .unwrap_or(0);
        Some(Self { file, seq })
    }
    fn append(&mut self, kind: &str, data: Value) {
        self.seq += 1;
        let line = json!({"seq": self.seq, "ts_ms": now_ms(), "kind": kind, "data": data});
        let _ = writeln!(self.file, "{line}");
        let _ = self.file.flush();
    }
}

struct DriveOut {
    pair: Option<TurnPair>,
    digest_actions: Vec<DigestAction>,
    progress: bool,
    stall: bool,
    complete: Option<String>,
    mutating_success: bool,
    fatal: Option<TerminalReason>,
}

// The provider key is resolved through catalog refresh with several
// diverging failure finishes; a single let-expression would hide them.
#[allow(clippy::needless_late_init)]
fn drive<S: SecretStore + 'static, T: Transport + 'static>(ctx: LoopCtx<S, T>) {
    let started = Instant::now();
    let mut ledger = Ledger::open(&ctx.state_dir);
    let tools = match ToolRuntime::new(&ctx.workspace) {
        Ok(t) => t,
        Err(e) => {
            finish(
                &ctx,
                &mut ledger,
                TerminalReason::ProviderError {
                    detail: format!("workspace tools failed: {}", e.detail),
                },
                started,
                &ctx.checkpoint,
            );
            return;
        }
    };

    // Provider identity: catalog + key, resolved inside the backend. The key
    // is used for provider calls only and never enters state, ledger, or UI.
    let mut model = ctx.checkpoint.model.clone();
    let key: String;
    if model.is_empty() {
        match ctx.service.refresh(&ctx.brief.provider) {
            Ok(catalog) => {
                let ids: Vec<&str> = catalog.models.iter().map(|m| m.id.as_str()).collect();
                model = if ctx.brief.provider == "gemini" {
                    crate::live::pick_flash_lite(&ids)
                        .or_else(|| ids.first().map(|s| (*s).to_string()))
                } else {
                    ids.first().map(|s| (*s).to_string())
                }
                .unwrap_or_default();
                if model.is_empty() {
                    finish(
                        &ctx,
                        &mut ledger,
                        TerminalReason::ProviderError {
                            detail: "provider returned no callable models".into(),
                        },
                        started,
                        &ctx.checkpoint,
                    );
                    return;
                }
            }
            Err(e) => {
                finish(
                    &ctx,
                    &mut ledger,
                    TerminalReason::ProviderError {
                        detail: format!("catalog failed: {e}"),
                    },
                    started,
                    &ctx.checkpoint,
                );
                return;
            }
        }
    }
    match ctx.service.get_key(&ctx.brief.provider) {
        Ok(Some(k)) => key = k,
        _ => {
            finish(
                &ctx,
                &mut ledger,
                TerminalReason::ProviderError {
                    detail: "no API key stored for this provider".into(),
                },
                started,
                &ctx.checkpoint,
            );
            return;
        }
    }
    if let Ok(mut s) = ctx.handle.shared.lock() {
        s.model = model.clone();
        s.status = AgentStatus::Running;
    }

    let protocol = match find_spec(&ctx.brief.provider) {
        Some(spec) => spec.protocol,
        None => {
            finish(
                &ctx,
                &mut ledger,
                TerminalReason::ProviderError {
                    detail: "provider disappeared from registry".into(),
                },
                started,
                &ctx.checkpoint,
            );
            return;
        }
    };
    let base_url = match ctx.service.base_url(&ctx.brief.provider) {
        Ok(url) => url,
        Err(e) => {
            finish(
                &ctx,
                &mut ledger,
                TerminalReason::ProviderError {
                    detail: e.to_string(),
                },
                started,
                &ctx.checkpoint,
            );
            return;
        }
    };
    let mut cp = ctx.checkpoint.clone();
    let budgets = ctx.brief.budgets;

    loop {
        // ---- hard stops, checked at every turn boundary ------------------
        if ctx.handle.cancel.load(Ordering::SeqCst) {
            #[cfg(test)]
            if ctx.handle.silent_cancel.load(Ordering::SeqCst) {
                let _ = write_json(&ctx.state_dir.join("checkpoint.json"), &cp);
                return;
            }
            finish(&ctx, &mut ledger, TerminalReason::Cancelled, started, &cp);
            return;
        }
        let elapsed = cp.elapsed_base_ms + started.elapsed().as_millis() as u64;
        if let Ok(mut s) = ctx.handle.shared.lock() {
            s.elapsed_ms = elapsed;
            s.step = cp.step;
            s.tool_calls = cp.tool_calls;
            s.tokens_used = cp.tokens_used;
        }
        if cp.step >= budgets.max_steps {
            finish(
                &ctx,
                &mut ledger,
                TerminalReason::BudgetSteps {
                    max_steps: budgets.max_steps,
                },
                started,
                &cp,
            );
            return;
        }
        if elapsed > budgets.max_wall_ms {
            finish(
                &ctx,
                &mut ledger,
                TerminalReason::BudgetTime {
                    max_wall_ms: budgets.max_wall_ms,
                },
                started,
                &cp,
            );
            return;
        }
        if cp.tokens_used > budgets.max_tokens {
            finish(
                &ctx,
                &mut ledger,
                TerminalReason::BudgetTokens {
                    max_tokens: budgets.max_tokens,
                },
                started,
                &cp,
            );
            return;
        }
        if cp.turns_since_progress >= MAX_NO_PROGRESS_TURNS && cp.step > 0 {
            finish(
                &ctx,
                &mut ledger,
                TerminalReason::NoProgress {
                    turns: cp.turns_since_progress,
                },
                started,
                &cp,
            );
            return;
        }

        // ---- dynamic context build ---------------------------------------
        let state_msg = build_state_message(&ctx.brief, &cp, budgets, current_plan(&ctx.handle));
        let request_body = build_request(
            protocol,
            &model,
            &ctx.system_prompt,
            &state_msg,
            cp.last_pair.as_ref(),
        );
        let turn_no = cp.step + 1;
        let _ = fs::write(
            ctx.state_dir
                .join("evidence")
                .join(format!("turn-{turn_no}-request.json")),
            &request_body,
        );

        // ---- provider turn with bounded retry/backoff --------------------
        let response = match generate_with_retry(
            ctx.service.transport(),
            protocol,
            &base_url,
            &key,
            &model,
            &request_body,
            &ctx.handle,
        ) {
            Ok(r) => r,
            Err(detail) => {
                finish(
                    &ctx,
                    &mut ledger,
                    TerminalReason::ProviderError { detail },
                    started,
                    &cp,
                );
                return;
            }
        };
        let _ = fs::write(
            ctx.state_dir
                .join("evidence")
                .join(format!("turn-{turn_no}-response.json")),
            &response,
        );
        cp.step += 1;
        let (usage_total, response_text) =
            (usage_tokens(protocol, &response), response.len() as u64);
        cp.tokens_used += usage_total.unwrap_or((request_body.len() as u64 + response_text) / 4);
        if let Some(l) = ledger.as_mut() {
            l.append(
                "model_turn",
                json!({
                    "turn": turn_no, "model": model,
                    "request_bytes": request_body.len(), "response_bytes": response.len(),
                    "usage_tokens": usage_total,
                }),
            );
        }

        let decoded = match decode_provider_calls(protocol, &response) {
            Ok(v) => v,
            Err(e) => {
                finish(
                    &ctx,
                    &mut ledger,
                    TerminalReason::ProviderError {
                        detail: format!("undecodable model turn: {e}"),
                    },
                    started,
                    &cp,
                );
                return;
            }
        };
        for text in &decoded.texts {
            RunHandle::push_event(
                &ctx.handle.shared,
                AgentEvent::ModelText { text: text.clone() },
            );
        }

        // ---- execute the turn's calls ------------------------------------
        let out = execute_turn(
            &ctx,
            &tools,
            &mut ledger,
            protocol,
            decoded.calls,
            decoded.thought_signature,
            &mut cp,
            started,
        );
        if let Some(fatal) = out.fatal {
            #[cfg(test)]
            if matches!(fatal, TerminalReason::Cancelled)
                && ctx.handle.silent_cancel.load(Ordering::SeqCst)
            {
                let _ = write_json(&ctx.state_dir.join("checkpoint.json"), &cp);
                return;
            }
            finish(&ctx, &mut ledger, fatal, started, &cp);
            return;
        }
        if let Some(summary) = out.complete {
            // `complete` is set only after gates pass; the preview, when one
            // was verified, already lives in shared state for the UI.
            if let Ok(mut s) = ctx.handle.shared.lock() {
                s.completion_summary = Some(summary);
            }
            finish(&ctx, &mut ledger, TerminalReason::Completed, started, &cp);
            return;
        }

        // ---- stall / progress bookkeeping --------------------------------
        if out.stall {
            cp.consec_stall += 1;
            if cp.consec_stall >= MAX_STALL_TURNS {
                finish(
                    &ctx,
                    &mut ledger,
                    TerminalReason::ModelStalled,
                    started,
                    &cp,
                );
                return;
            }
            RunHandle::push_event(
                &ctx.handle.shared,
                AgentEvent::Info {
                    message: "model produced no tool calls; nudging it toward the plan".into(),
                },
            );
        } else {
            cp.consec_stall = 0;
        }
        if out.progress {
            cp.turns_since_progress = 0;
        } else {
            cp.turns_since_progress += 1;
        }
        if out.mutating_success {
            cp.any_mutating_success = true;
        }
        cp.digest.push(DigestEntry {
            turn: turn_no,
            actions: out.digest_actions,
        });
        if cp.digest.len() > 12 {
            cp.digest.remove(0);
        }
        cp.model = model.clone();
        cp.last_pair = out.pair;
        // `started` measures this drive session; elapsed_base_ms carries time
        // from before a resume, so total elapsed stays truthful.
        let _ = write_json(&ctx.state_dir.join("checkpoint.json"), &cp);
    }
}

fn current_plan(handle: &Arc<RunHandle>) -> Vec<PlanItem> {
    handle
        .shared
        .lock()
        .map(|s| s.plan.clone())
        .unwrap_or_default()
}

fn build_state_message(
    brief: &TaskBrief,
    cp: &Checkpoint,
    budgets: Budgets,
    plan: Vec<PlanItem>,
) -> String {
    let digest: Vec<&DigestEntry> = cp.digest.iter().rev().take(DIGEST_WINDOW).collect();
    let digest: Vec<&DigestEntry> = digest.into_iter().rev().collect();
    let payload = json!({
        "contract": {
            "role": "You are REX, an autonomous agent inside a bounded workspace. Work the plan until the task is verifiably done.",
            "rules": [
                "Maintain the todo plan with update_plan: mark the active step in_progress, mark steps done only when actually done.",
                "Use read_file/search_files/web_search to ground yourself before writing. Writes and commands pause for trusted human approval; a denial is information - replan, never retry the identical denied call.",
                "When the plan is fully done, call complete_task. Gates then verify your work; false completion claims fail the gates.",
                "Evidence from older turns stays in the run ledger; the digest below carries the recent truth.",
            ],
            "tools": ["update_plan", "read_file", "create_file", "edit_file", "search_files", "run_command", "web_search", "complete_task"],
        },
        "brief": {"task": brief.task, "created_at_ms": brief.created_at_ms},
        "budget": {
            "step": cp.step, "max_steps": budgets.max_steps,
            "tool_calls": cp.tool_calls, "max_tool_calls": budgets.max_tool_calls,
            "tokens_used": cp.tokens_used, "max_tokens": budgets.max_tokens,
            "elapsed_ms": cp.elapsed_base_ms, "max_wall_ms": budgets.max_wall_ms,
        },
        "plan": plan,
        "recent_turns": digest,
        "note": if cp.consec_stall > 0 {
            "Your last turn produced no tool calls. Act on the plan: call the next tool, update the plan, or call complete_task when everything is verifiably done."
        } else {
            ""
        },
    });
    serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".into())
}

/// The loop's actual tool offering, mirrored into the prompt's tool
/// contract. `tool_definitions_match_contract` pins this list to the wire
/// definitions so the model is never told about tools it does not have -
/// or given tools it was never told about.
fn offered_tool_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec::new(
            "update_plan",
            "Replace the visible todo plan.",
            false,
            false,
        ),
        ToolSpec::new(
            "read_file",
            "Read a file inside the selected workspace.",
            false,
            false,
        ),
        ToolSpec::new(
            "create_file",
            "Create a file inside the workspace.",
            true,
            true,
        ),
        ToolSpec::new(
            "edit_file",
            "Replace exact text in a workspace file.",
            true,
            true,
        ),
        ToolSpec::new(
            "search_files",
            "Search file contents inside the workspace.",
            false,
            false,
        ),
        ToolSpec::new(
            "run_command",
            "Run an allowed command inside the workspace.",
            true,
            true,
        ),
        ToolSpec::new(
            "web_search",
            "Search the public web for grounded facts.",
            false,
            false,
        ),
        ToolSpec::new(
            "complete_task",
            "Declare the task finished; harness gates verify the claim.",
            false,
            false,
        ),
    ]
}

/// Apply a role's tool policy to the actual offering. Returns the scoped
/// specs plus the enforced allowlist (`None` = full offering, nothing to
/// enforce).
fn scoped_tools_for(role: Role) -> (Vec<ToolSpec>, Option<Vec<String>>) {
    let scoped = rex_prompt::tools::filter_for_policy(&offered_tool_specs(), role.tool_policy());
    let allowlist = match role.tool_policy() {
        ToolPolicy::All => None,
        _ => Some(scoped.iter().map(|s| s.name.clone()).collect()),
    };
    (scoped, allowlist)
}

/// Assemble the modular system prompt for an autonomous run under `role`
/// from the tools actually enabled for it: constitution, role card, the
/// least-authority tool contract and the completion gate.
fn assemble_run_prompt(role: Role, specs: &[ToolSpec]) -> (String, String, String) {
    let assembly = Assembler::new()
        .constitution()
        .role(role)
        .module(
            ModuleKind::ToolContract,
            rex_prompt::tools::render_contract(specs),
        )
        .expect("role allows its tool contract")
        .module(
            ModuleKind::CompletionGate,
            rex_prompt::gate::COMPLETION_GATE,
        )
        .expect("role allows the completion gate")
        .assemble();
    (assembly.system, assembly.version, assembly.prompt_hash)
}

/// The resume identity gate. Returns Ok(true) when a legacy checkpoint
/// (no prompt metadata) migrated onto the current identity, Ok(false) when
/// the identity matched, and Err when the checkpoint belongs to different
/// prompt semantics - which must never resume silently.
fn check_resume_identity(
    checkpoint: &Checkpoint,
    current_version: &str,
    current_hash: &str,
) -> Result<bool, String> {
    if checkpoint.prompt_hash == legacy_prompt_marker() {
        return Ok(true);
    }
    if checkpoint.prompt_hash != current_hash {
        return Err(format!(
            "checkpoint was written under prompt identity rex-prompt/{}#{:.12}; this build assembles rex-prompt/{}#{:.12}. Refusing to mix prompt semantics mid-run.",
            checkpoint.prompt_version, checkpoint.prompt_hash, current_version, current_hash
        ));
    }
    Ok(false)
}

/// The prompt identity the Simple worker runs under: version + hash of the
/// assembled system prompt for the full offering. Benchmark records pin
/// this so comparisons stay honest across prompt iterations.
pub fn worker_prompt_identity() -> (String, String) {
    let (_, version, hash) = assemble_run_prompt(Role::Worker, &offered_tool_specs());
    (version, hash)
}

/// The prompt identity any role runs under. Ultra pins the builder and
/// adversary identities in its run records for the same reason.
pub fn role_prompt_identity(role: Role) -> (String, String) {
    let (scoped, _) = scoped_tools_for(role);
    let (_, version, hash) = assemble_run_prompt(role, &scoped);
    (version, hash)
}

fn build_request(
    protocol: ProviderProtocol,
    model: &str,
    system: &str,
    state_msg: &str,
    prev: Option<&TurnPair>,
) -> String {
    match protocol {
        ProviderProtocol::Gemini => {
            let mut contents = vec![json!({"role":"user","parts":[{"text": state_msg}]})];
            if let Some(pair) = prev {
                if !pair.model_parts.is_empty() {
                    contents.push(json!({"role":"model","parts": pair.model_parts}));
                }
                if !pair.response_parts.is_empty() {
                    contents.push(json!({"role":"user","parts": pair.response_parts}));
                }
            }
            json!({"contents": contents, "tools": gemini_tool_definitions(),
                "systemInstruction": {"parts":[{"text": system}]},
                "toolConfig":{"functionCallingConfig":{"mode":"AUTO"}},
                "generationConfig":{"temperature":0.2,"maxOutputTokens":8192}})
            .to_string()
        }
        ProviderProtocol::Anthropic => {
            let mut messages = Vec::new();
            if let Some(pair) = prev {
                if !pair.model_parts.is_empty() {
                    messages.push(json!({"role":"assistant","content":pair.model_parts}));
                }
                if !pair.response_parts.is_empty() {
                    messages.push(json!({"role":"user","content":pair.response_parts}));
                }
            }
            messages.push(json!({"role":"user","content":state_msg}));
            json!({"model":model,"max_tokens":8192,"temperature":0.2,
                "system":system,
                "messages":messages,"tools":anthropic_tool_definitions()})
            .to_string()
        }
        ProviderProtocol::OpenAiCompatible => {
            let mut messages = vec![json!({"role":"system","content":system})];
            if let Some(pair) = prev {
                if !pair.model_parts.is_empty() {
                    messages.push(json!({"role":"assistant","content":Value::Null,"tool_calls":pair.model_parts}));
                }
                messages.extend(pair.response_parts.clone());
            }
            messages.push(json!({"role":"user","content":state_msg}));
            json!({"model":model,"messages":messages,"tools":openai_tool_definitions(),
                "tool_choice":"auto","temperature":0.2,"max_tokens":8192})
            .to_string()
        }
    }
}

fn gemini_tool_definitions() -> Value {
    json!([{"functionDeclarations":[
        {"name":"update_plan","description":"Replace the visible todo plan. Keep 2-8 items; one in_progress at a time.","parameters":{"type":"OBJECT","properties":{"items":{"type":"ARRAY","items":{"type":"OBJECT","properties":{"id":{"type":"STRING"},"title":{"type":"STRING"},"status":{"type":"STRING","enum":["pending","in_progress","done","blocked"]},"note":{"type":"STRING"}},"required":["id","title","status"]}}},"required":["items"]}},
        {"name":"read_file","description":"Read a file inside the selected workspace.","parameters":{"type":"OBJECT","properties":{"path":{"type":"STRING"}},"required":["path"]}},
        {"name":"create_file","description":"Create a file inside the workspace. Requires trusted approval.","parameters":{"type":"OBJECT","properties":{"path":{"type":"STRING"},"content":{"type":"STRING"},"overwrite":{"type":"BOOLEAN"}},"required":["path","content","overwrite"]}},
        {"name":"edit_file","description":"Replace exact text in a workspace file. Requires trusted approval.","parameters":{"type":"OBJECT","properties":{"path":{"type":"STRING"},"expected":{"type":"STRING"},"replacement":{"type":"STRING"},"replace_all":{"type":"BOOLEAN"}},"required":["path","expected","replacement","replace_all"]}},
        {"name":"search_files","description":"Search file contents inside the workspace.","parameters":{"type":"OBJECT","properties":{"query":{"type":"STRING"},"path":{"type":"STRING"},"max_results":{"type":"INTEGER"}},"required":["query"]}},
        {"name":"run_command","description":"Run an allowed command inside the workspace. Requires trusted approval.","parameters":{"type":"OBJECT","properties":{"argv":{"type":"ARRAY","items":{"type":"STRING"}},"cwd":{"type":"STRING"},"timeout_ms":{"type":"INTEGER"}},"required":["argv"]}},
        {"name":"web_search","description":"Search the public web for grounded facts. Returns ranked results with URLs.","parameters":{"type":"OBJECT","properties":{"query":{"type":"STRING"},"max_results":{"type":"INTEGER"}},"required":["query"]}},
        {"name":"complete_task","description":"Declare the task finished. Harness gates verify the claim before the run completes.","parameters":{"type":"OBJECT","properties":{"summary":{"type":"STRING"}},"required":["summary"]}}
    ]}])
}

fn anthropic_tool_definitions() -> Value {
    let declarations = gemini_tool_definitions()[0]["functionDeclarations"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    Value::Array(declarations.into_iter().map(|d| json!({
        "name": d["name"], "description": d["description"], "input_schema": lowercase_schema(d["parameters"].clone())
    })).collect())
}

fn openai_tool_definitions() -> Value {
    let declarations = gemini_tool_definitions()[0]["functionDeclarations"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    Value::Array(declarations.into_iter().map(|d| json!({"type":"function","function":{
        "name":d["name"],"description":d["description"],"parameters":lowercase_schema(d["parameters"].clone())
    }})).collect())
}

fn lowercase_schema(mut value: Value) -> Value {
    match &mut value {
        Value::Object(map) => {
            if let Some(Value::String(t)) = map.get_mut("type") {
                *t = t.to_ascii_lowercase();
            }
            for child in map.values_mut() {
                *child = lowercase_schema(child.take());
            }
        }
        Value::Array(items) => {
            for child in items {
                *child = lowercase_schema(child.take());
            }
        }
        _ => {}
    }
    value
}

fn generate_with_retry<T: Transport>(
    transport: &T,
    protocol: ProviderProtocol,
    base_url: &str,
    key: &str,
    model: &str,
    body: &str,
    handle: &Arc<RunHandle>,
) -> Result<String, String> {
    let (url, headers) = match protocol {
        ProviderProtocol::Gemini => (
            format!(
                "{}/v1beta/models/{model}:generateContent",
                base_url.trim_end_matches('/')
            ),
            vec![
                ("x-goog-api-key".into(), key.into()),
                ("content-type".into(), "application/json".into()),
            ],
        ),
        ProviderProtocol::Anthropic => (
            format!("{}/v1/messages", base_url.trim_end_matches('/')),
            vec![
                ("x-api-key".into(), key.into()),
                ("anthropic-version".into(), "2023-06-01".into()),
                ("content-type".into(), "application/json".into()),
            ],
        ),
        ProviderProtocol::OpenAiCompatible => (
            format!("{}/chat/completions", base_url.trim_end_matches('/')),
            vec![
                ("authorization".into(), format!("Bearer {key}")),
                ("content-type".into(), "application/json".into()),
            ],
        ),
    };
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        if handle.cancel.load(Ordering::SeqCst) {
            return Err("cancelled".into());
        }
        let result = transport.post(&url, &headers, body);
        let retryable = matches!(&result, Ok((429, _)) | Ok((500..=599, _)) | Err(_));
        match result {
            Ok((status, text)) if status / 100 == 2 => return Ok(text),
            Ok((status, text)) => {
                let detail = format!(
                    "provider HTTP {status}: {}",
                    structured_http_failure(status, &text)
                );
                if !retryable || attempt >= PROVIDER_RETRIES {
                    return Err(detail);
                }
                RunHandle::push_event(
                    &handle.shared,
                    AgentEvent::Retry {
                        attempt,
                        reason: detail,
                    },
                );
            }
            Err(e) => {
                if attempt >= PROVIDER_RETRIES {
                    return Err(format!("provider transport failed: {e}"));
                }
                RunHandle::push_event(
                    &handle.shared,
                    AgentEvent::Retry {
                        attempt,
                        reason: format!("transport error: {e}"),
                    },
                );
            }
        }
        std::thread::sleep(Duration::from_millis(500 * (1 << (attempt - 1))));
    }
}

fn usage_tokens(protocol: ProviderProtocol, response: &str) -> Option<u64> {
    let value: Value = serde_json::from_str(response).ok()?;
    match protocol {
        ProviderProtocol::Gemini => value
            .pointer("/usageMetadata/totalTokenCount")
            .and_then(Value::as_u64),
        ProviderProtocol::Anthropic => Some(
            value
                .pointer("/usage/input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                + value
                    .pointer("/usage/output_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
        ),
        ProviderProtocol::OpenAiCompatible => {
            value.pointer("/usage/total_tokens").and_then(Value::as_u64)
        }
    }
}

/// Decode a Gemini generateContent response into texts and loop calls. This
/// is the loop's own adapter: loop-level tools (update_plan, web_search,
/// complete_task) are separated from rex-tools requests here, and unknown
/// calls fail loudly instead of being guessed into a capability.
struct DecodedCalls {
    texts: Vec<String>,
    calls: Vec<(AgentCall, Option<Value>)>,
    /// Gemini 2.5 thinking models sign the model turn; the signature must be
    /// echoed back with the next request's function calls.
    thought_signature: Option<String>,
}

fn decode_gemini_calls(response: &str) -> Result<DecodedCalls, String> {
    let value: Value = serde_json::from_str(response).map_err(|e| format!("invalid JSON: {e}"))?;
    let parts = value
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|c| c.first())
        .and_then(|c| c.get("content"))
        .and_then(|c| c.get("parts"))
        .and_then(Value::as_array)
        .ok_or("missing candidates[0].content.parts")?;
    let mut texts = Vec::new();
    let mut calls: Vec<(AgentCall, Option<Value>)> = Vec::new();
    let mut thought_signature: Option<String> = None;
    let mut call_seq = 0usize;
    for part in parts {
        if thought_signature.is_none() {
            thought_signature = part
                .get("thoughtSignature")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        if let Some(text) = part.get("text").and_then(Value::as_str) {
            if !text.trim().is_empty() {
                texts.push(text.to_string());
            }
            continue;
        }
        let fc = match part.get("functionCall") {
            Some(fc) => fc,
            None => continue,
        };
        call_seq += 1;
        let id = format!("call-{call_seq}");
        let raw = Some(part.clone());
        let name = fc
            .get("name")
            .and_then(Value::as_str)
            .ok_or("functionCall missing name")?;
        let args = fc.get("args").cloned().unwrap_or_else(|| json!({}));
        match name {
            "update_plan" => {
                match serde_json::from_value::<Vec<PlanItem>>(
                    args.get("items").cloned().unwrap_or_else(|| json!([])),
                ) {
                    Ok(items) => calls.push((AgentCall::UpdatePlan { items }, raw)),
                    Err(e) => calls.push((AgentCall::BadCall {
                        name: "update_plan".into(),
                        id,
                        error: format!("invalid update_plan items: {e}; every item needs id, title, status"),
                    }, raw)),
                }
            }
            "web_search" => {
                let query = match args.get("query").and_then(Value::as_str) {
                    Some(q) => q.to_string(),
                    None => {
                        calls.push((
                            AgentCall::BadCall {
                                name: "web_search".into(),
                                id,
                                error: "web_search missing query".into(),
                            },
                            raw,
                        ));
                        continue;
                    }
                };
                let max_results = args
                    .get("max_results")
                    .and_then(Value::as_u64)
                    .map(|n| n as usize)
                    .unwrap_or(5)
                    .clamp(1, 8);
                calls.push((
                    AgentCall::WebSearch {
                        id,
                        query,
                        max_results,
                    },
                    raw,
                ));
            }
            "complete_task" => {
                let summary = args
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                calls.push((AgentCall::CompleteTask { summary }, raw));
            }
            _ => {
                let mut object = args;
                if !object.is_object() {
                    return Err(format!("tool {name} arguments must be an object"));
                }
                object
                    .as_object_mut()
                    .unwrap()
                    .insert("tool".into(), Value::String(name.into()));
                match serde_json::from_value::<ToolRequest>(object) {
                    Ok(request) => calls.push((AgentCall::Tool { id, request }, raw)),
                    Err(e) => calls.push((
                        AgentCall::BadCall {
                            name: name.into(),
                            id,
                            error: format!("invalid {name} request: {e}"),
                        },
                        raw,
                    )),
                }
            }
        }
    }
    Ok(DecodedCalls {
        texts,
        calls,
        thought_signature,
    })
}

fn decode_provider_calls(
    protocol: ProviderProtocol,
    response: &str,
) -> Result<DecodedCalls, String> {
    match protocol {
        ProviderProtocol::Gemini => decode_gemini_calls(response),
        ProviderProtocol::Anthropic => decode_anthropic_calls(response),
        ProviderProtocol::OpenAiCompatible => decode_openai_calls(response),
    }
}

fn decode_named_call(
    name: &str,
    id: String,
    args: Value,
    raw: Value,
) -> (AgentCall, Option<Value>) {
    let bad = |error: String| {
        (
            AgentCall::BadCall {
                name: name.into(),
                id: id.clone(),
                error,
            },
            Some(raw.clone()),
        )
    };
    match name {
        "update_plan" => match serde_json::from_value::<Vec<PlanItem>>(
            args.get("items").cloned().unwrap_or_else(|| json!([])),
        ) {
            Ok(items) => (AgentCall::UpdatePlan { items }, Some(raw)),
            Err(e) => bad(format!("invalid update_plan items: {e}")),
        },
        "web_search" => match args.get("query").and_then(Value::as_str) {
            Some(q) => (
                AgentCall::WebSearch {
                    id,
                    query: q.into(),
                    max_results: args
                        .get("max_results")
                        .and_then(Value::as_u64)
                        .unwrap_or(5)
                        .clamp(1, 8) as usize,
                },
                Some(raw),
            ),
            None => bad("web_search missing query".into()),
        },
        "complete_task" => (
            AgentCall::CompleteTask {
                summary: args
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .into(),
            },
            Some(raw),
        ),
        _ => {
            let mut object = args;
            if !object.is_object() {
                return bad(format!("tool {name} arguments must be an object"));
            }
            object
                .as_object_mut()
                .unwrap()
                .insert("tool".into(), Value::String(name.into()));
            match serde_json::from_value::<ToolRequest>(object) {
                Ok(request) => (AgentCall::Tool { id, request }, Some(raw)),
                Err(e) => bad(format!("invalid {name} request: {e}")),
            }
        }
    }
}

fn decode_anthropic_calls(response: &str) -> Result<DecodedCalls, String> {
    let value: Value = serde_json::from_str(response).map_err(|e| format!("invalid JSON: {e}"))?;
    let blocks = value
        .get("content")
        .and_then(Value::as_array)
        .ok_or("missing content")?;
    let mut texts = Vec::new();
    let mut calls = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(t) = block
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|t| !t.trim().is_empty())
                {
                    texts.push(t.into())
                }
            }
            Some("tool_use") => {
                let id = block
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or("tool_use missing id")?
                    .to_string();
                let name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or("tool_use missing name")?;
                calls.push(decode_named_call(
                    name,
                    id,
                    block.get("input").cloned().unwrap_or_else(|| json!({})),
                    block.clone(),
                ));
            }
            _ => {}
        }
    }
    Ok(DecodedCalls {
        texts,
        calls,
        thought_signature: None,
    })
}

fn decode_openai_calls(response: &str) -> Result<DecodedCalls, String> {
    let value: Value = serde_json::from_str(response).map_err(|e| format!("invalid JSON: {e}"))?;
    let msg = value
        .pointer("/choices/0/message")
        .ok_or("missing choices[0].message")?;
    let texts = msg
        .get("content")
        .and_then(Value::as_str)
        .filter(|t| !t.trim().is_empty())
        .map(|t| vec![t.into()])
        .unwrap_or_default();
    let mut calls = Vec::new();
    for call in msg
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let id = call
            .get("id")
            .and_then(Value::as_str)
            .ok_or("tool call missing id")?
            .to_string();
        let f = call.get("function").ok_or("tool call missing function")?;
        let name = f
            .get("name")
            .and_then(Value::as_str)
            .ok_or("tool call missing name")?;
        let raw = f
            .get("arguments")
            .and_then(Value::as_str)
            .ok_or("tool call missing arguments")?;
        let args = serde_json::from_str(raw).map_err(|e| format!("invalid tool arguments: {e}"))?;
        calls.push(decode_named_call(name, id, args, call.clone()));
    }
    Ok(DecodedCalls {
        texts,
        calls,
        thought_signature: None,
    })
}

fn call_signature(tool: &str, request: &Value) -> String {
    format!(
        "{}:{}",
        tool,
        serde_json::to_string(request).unwrap_or_default()
    )
}

// One turn needs the full loop context plus per-turn inputs; the parts are
// not a meaningful struct of their own.
#[allow(clippy::too_many_arguments)]
fn execute_turn<S: SecretStore + 'static, T: Transport + 'static>(
    ctx: &LoopCtx<S, T>,
    tools: &ToolRuntime,
    ledger: &mut Option<Ledger>,
    protocol: ProviderProtocol,
    calls: Vec<(AgentCall, Option<Value>)>,
    thought_signature: Option<String>,
    cp: &mut Checkpoint,
    started: Instant,
) -> DriveOut {
    let mut out = DriveOut {
        pair: Some(TurnPair::default()),
        digest_actions: Vec::new(),
        progress: false,
        stall: calls.is_empty(),
        complete: None,
        mutating_success: false,
        fatal: None,
    };
    let mut response_parts: Vec<Value> = Vec::new();
    let mut model_parts: Vec<Value> = Vec::new();

    let mut first_model_part = true;
    let mut push_model_part =
        |model_parts: &mut Vec<Value>, raw: Option<Value>, fallback: Value| {
            let mut part = raw.unwrap_or(fallback);
            // Gemini 2.5 thinking models require the turn's thoughtSignature on
            // the first function call echoed back; carry it if the raw part
            // lost it (older models simply ignore nothing - they never send one).
            if first_model_part {
                if let Some(sig) = &thought_signature {
                    if part.get("thoughtSignature").is_none() {
                        part["thoughtSignature"] = json!(sig);
                    }
                }
            }
            first_model_part = false;
            model_parts.push(part);
        };
    for (call, raw) in calls {
        if ctx.handle.cancel.load(Ordering::SeqCst) {
            out.fatal = Some(TerminalReason::Cancelled);
            return out;
        }
        if cp.tool_calls >= ctx.brief.budgets.max_tool_calls {
            out.fatal = Some(TerminalReason::BudgetToolCalls {
                max_tool_calls: ctx.brief.budgets.max_tool_calls,
            });
            return out;
        }
        cp.tool_calls += 1;
        match call {
            AgentCall::BadCall { name, id, error } => {
                out.digest_actions.push(DigestAction {
                    tool: name.clone(),
                    ok: false,
                    target: None,
                    error_kind: Some("bad_call".into()),
                });
                push_model_part(
                    &mut model_parts,
                    raw,
                    json!({"functionCall":{"name": name,"args": {}}}),
                );
                response_parts.push(function_response(protocol, &name, &id, false, &error));
            }
            AgentCall::UpdatePlan { items } => {
                let items: Vec<PlanItem> = items.into_iter().take(8).collect();
                let in_progress = items
                    .iter()
                    .filter(|i| i.status == PlanStatus::InProgress)
                    .count();
                let note = if in_progress > 1 {
                    Some("multiple in_progress items; keep one active step")
                } else {
                    None
                };
                if let Ok(mut s) = ctx.handle.shared.lock() {
                    s.plan = items.clone();
                    push_locked(
                        &mut s,
                        AgentEvent::PlanUpdated {
                            items: items.clone(),
                        },
                    );
                }
                let _ = write_json(&ctx.state_dir.join("plan.json"), &items);
                if let Some(l) = ledger.as_mut() {
                    l.append("plan", json!({"items": items}));
                }
                out.progress = true;
                out.digest_actions.push(DigestAction {
                    tool: "update_plan".into(),
                    ok: true,
                    target: Some(format!("{} items", items.len())),
                    error_kind: note.map(str::to_string),
                });
            }
            AgentCall::WebSearch {
                id,
                query,
                max_results,
            } => {
                let (ok, content) = match &ctx.search {
                    Some(router) => {
                        let request: SearchRequest = match serde_json::from_value(json!({
                            "query": query, "seeds": [], "max_results": max_results
                        })) {
                            Ok(r) => r,
                            Err(e) => {
                                response_parts.push(function_response(
                                    protocol,
                                    "web_search",
                                    &id,
                                    false,
                                    &format!("bad search request: {e}"),
                                ));
                                continue;
                            }
                        };
                        match router.search(request) {
                            Ok(resp) => {
                                let results: Vec<Value> = resp
                                    .evidence
                                    .iter()
                                    .take(5)
                                    .map(|e| json!({
                                        "url": e.url,
                                        "title": e.title,
                                        "excerpt": e.excerpt.as_deref().map(|x| &x[..x.len().min(300)]),
                                    }))
                                    .collect();
                                (
                                    true,
                                    serde_json::to_string(&json!({
                                        "ok": true,
                                        "results": results,
                                        "note": resp.coverage.disclaimer,
                                    }))
                                    .unwrap_or_else(|_| "{\"ok\":true}".into()),
                                )
                            }
                            Err(e) => (false, format!("search failed: {e}")),
                        }
                    }
                    None => (false, "web search is not configured in this build".into()),
                };
                if let Some(l) = ledger.as_mut() {
                    l.append("web_search", json!({"query": query, "ok": ok}));
                }
                out.digest_actions.push(DigestAction {
                    tool: "web_search".into(),
                    ok,
                    target: Some(query.clone()),
                    error_kind: if ok {
                        None
                    } else {
                        Some("search_failed".into())
                    },
                });
                push_model_part(
                    &mut model_parts,
                    raw,
                    json!({"functionCall":{"name":"web_search","args":{"query": query}}}),
                );
                response_parts.push(function_response(protocol, "web_search", &id, ok, &content));
            }
            AgentCall::CompleteTask { summary } => {
                cp.gate_attempts += 1;
                if let Ok(mut s) = ctx.handle.shared.lock() {
                    s.status = AgentStatus::Verifying;
                }
                let (passed, failures) = verify_gates(ctx, cp, ledger);
                RunHandle::push_event(
                    &ctx.handle.shared,
                    AgentEvent::GateResult {
                        attempt: cp.gate_attempts,
                        passed,
                        failures: failures.clone(),
                    },
                );
                if let Some(l) = ledger.as_mut() {
                    l.append("gate", json!({"attempt": cp.gate_attempts, "passed": passed, "failures": failures}));
                }
                if passed {
                    out.progress = true;
                    out.complete = Some(summary);
                    return out;
                }
                if cp.gate_attempts >= MAX_GATE_ATTEMPTS {
                    out.fatal = Some(TerminalReason::GatesFailed { failures });
                    return out;
                }
                if let Ok(mut s) = ctx.handle.shared.lock() {
                    s.status = AgentStatus::Running;
                }
                let feedback = format!(
                    "gate verification failed (attempt {}/{MAX_GATE_ATTEMPTS}): {}",
                    cp.gate_attempts,
                    failures.join("; ")
                );
                out.digest_actions.push(DigestAction {
                    tool: "complete_task".into(),
                    ok: false,
                    target: None,
                    error_kind: Some("gates_failed".into()),
                });
                push_model_part(
                    &mut model_parts,
                    raw,
                    json!({"functionCall":{"name":"complete_task","args":{"summary": summary}}}),
                );
                response_parts.push(function_response(
                    protocol,
                    "complete_task",
                    "gate",
                    false,
                    &feedback,
                ));
            }
            AgentCall::Tool { id, request } => {
                let tool_name = tool_name_of(&request);
                // Least-authority scope: a role-limited run (e.g. read-only
                // adversary) gets a harness-level refusal, not just prose.
                if let Some(allowed) = &ctx.brief.allowed_tools {
                    if !allowed.iter().any(|t| t == tool_name) {
                        let content = format!(
                            "tool {tool_name} is not enabled for this run's role; use only the tools listed in your tool contract"
                        );
                        RunHandle::push_event(
                            &ctx.handle.shared,
                            AgentEvent::Info {
                                message: format!(
                                    "refused out-of-scope tool {tool_name} under this run's role"
                                ),
                            },
                        );
                        out.digest_actions.push(DigestAction {
                            tool: tool_name.into(),
                            ok: false,
                            target: None,
                            error_kind: Some("out_of_scope".into()),
                        });
                        push_model_part(
                            &mut model_parts,
                            raw,
                            json!({"functionCall":{"name": tool_name,"args": serde_json::to_value(&request).unwrap_or_default()}}),
                        );
                        response_parts
                            .push(function_response(protocol, tool_name, &id, false, &content));
                        continue;
                    }
                }
                let sig = call_signature(
                    tool_name,
                    &serde_json::to_value(&request).unwrap_or_default(),
                );
                let prepared = match tools.prepare(request.clone()) {
                    Ok(p) => p,
                    Err(e) => {
                        let content = format!("prepare failed: {}", e.detail);
                        out.digest_actions.push(DigestAction {
                            tool: tool_name.into(),
                            ok: false,
                            target: None,
                            error_kind: Some(format!("{:?}", e.kind).to_lowercase()),
                        });
                        push_model_part(
                            &mut model_parts,
                            raw,
                            json!({"functionCall":{"name": tool_name,"args": serde_json::to_value(&request).unwrap_or_default()}}),
                        );
                        response_parts
                            .push(function_response(protocol, tool_name, &id, false, &content));
                        continue;
                    }
                };
                let result = if prepared.approval_required {
                    match wait_for_decision(ctx, &prepared, started) {
                        Decision::Approved => {
                            let _ = tools.resolve_approval(&prepared.call_id, true);
                            tools.execute(&prepared.call_id)
                        }
                        Decision::Denied => {
                            let _ = tools.resolve_approval(&prepared.call_id, false);
                            cp.total_denials += 1;
                            let r = tools.execute(&prepared.call_id);
                            if cp.total_denials >= MAX_DENIALS {
                                RunHandle::push_event(
                                    &ctx.handle.shared,
                                    AgentEvent::ToolFinished { result: r.clone() },
                                );
                                out.fatal = Some(TerminalReason::Denied);
                                return out;
                            }
                            r
                        }
                        Decision::Timeout => {
                            out.fatal = Some(TerminalReason::ApprovalTimeout);
                            return out;
                        }
                        Decision::Cancelled => {
                            let _ = tools.cancel(&prepared.call_id);
                            out.fatal = Some(TerminalReason::Cancelled);
                            return out;
                        }
                    }
                } else {
                    tools.execute(&prepared.call_id)
                };
                if let Some(l) = ledger.as_mut() {
                    l.append(
                        "tool",
                        json!({
                            "call_id": result.call_id, "tool": result.tool, "ok": result.ok,
                            "receipt": result.receipt,
                        }),
                    );
                }
                let _ = fs::write(
                    ctx.state_dir
                        .join("evidence")
                        .join(format!("tool-{}.json", result.call_id)),
                    serde_json::to_string_pretty(&result).unwrap_or_default(),
                );
                RunHandle::push_event(
                    &ctx.handle.shared,
                    AgentEvent::ToolFinished {
                        result: result.clone(),
                    },
                );

                // repeated-failure detection on the exact call signature
                if result.ok {
                    cp.consec_fail = 0;
                    cp.last_failure_sig = None;
                    if matches!(
                        request,
                        ToolRequest::CreateFile { .. }
                            | ToolRequest::EditFile { .. }
                            | ToolRequest::RunCommand { .. }
                    ) {
                        out.mutating_success = true;
                        out.progress = true;
                    }
                } else {
                    if cp.last_failure_sig.as_deref() == Some(sig.as_str()) {
                        cp.consec_fail += 1;
                    } else {
                        cp.consec_fail = 1;
                        cp.last_failure_sig = Some(sig.clone());
                    }
                    if cp.consec_fail >= MAX_CONSEC_FAILURES {
                        out.fatal = Some(TerminalReason::RepeatedFailure {
                            tool: tool_name.into(),
                        });
                        return out;
                    }
                }

                let receipt = &result.receipt;
                let mut content = serde_json::to_string(&json!({
                    "ok": result.ok,
                    "output": result.output.as_deref().map(|o| &o[..o.len().min(OUTCOME_CHARS)]),
                    "error": result.error,
                    "receipt": {
                        "bytes_read": receipt.bytes_read,
                        "bytes_written": receipt.bytes_written,
                        "duration_ms": receipt.duration_ms,
                        "exit_code": receipt.exit_code,
                        "output_truncated": receipt.output_truncated,
                    }
                }))
                .unwrap_or_else(|_| "{\"ok\":false}".into());
                if !result.ok && cp.consec_fail == 2 {
                    content.push_str(" warning: this exact call has failed twice; change approach instead of retrying it unchanged");
                }
                out.digest_actions.push(DigestAction {
                    tool: tool_name.into(),
                    ok: result.ok,
                    target: receipt
                        .target
                        .clone()
                        .or(receipt.command.as_ref().map(|c| c.join(" "))),
                    error_kind: result
                        .error
                        .as_ref()
                        .map(|e| format!("{:?}", e.kind).to_lowercase()),
                });
                push_model_part(
                    &mut model_parts,
                    raw,
                    json!({"functionCall":{"name": tool_name,"args": serde_json::to_value(&request).unwrap_or_default()}}),
                );
                response_parts.push(function_response(
                    protocol, tool_name, &id, result.ok, &content,
                ));
            }
        }
    }
    if let Some(pair) = out.pair.as_mut() {
        pair.model_parts = model_parts;
        pair.response_parts = response_parts;
    }
    if out
        .pair
        .as_ref()
        .map(|p| p.model_parts.is_empty() && p.response_parts.is_empty())
        .unwrap_or(false)
    {
        out.pair = None;
    }
    out
}

fn function_response(
    protocol: ProviderProtocol,
    name: &str,
    id: &str,
    ok: bool,
    content: &str,
) -> Value {
    match protocol {
        ProviderProtocol::Gemini => {
            json!({"functionResponse":{"name":name,"response":{"ok":ok,"content":content}}})
        }
        ProviderProtocol::Anthropic => {
            json!({"type":"tool_result","tool_use_id":id,"is_error":!ok,"content":content})
        }
        ProviderProtocol::OpenAiCompatible => {
            json!({"role":"tool","tool_call_id":id,"content":content})
        }
    }
}

fn tool_name_of(request: &ToolRequest) -> &'static str {
    match request {
        ToolRequest::ReadFile { .. } => "read_file",
        ToolRequest::CreateFile { .. } => "create_file",
        ToolRequest::EditFile { .. } => "edit_file",
        ToolRequest::SearchFiles { .. } => "search_files",
        ToolRequest::RunCommand { .. } => "run_command",
    }
}

enum Decision {
    Approved,
    Denied,
    Timeout,
    Cancelled,
}

/// Park the loop thread until the trusted UI answers. The model cannot reach
/// this state transition; only `AutonomousRunService::decide` can.
fn wait_for_decision<S: SecretStore + 'static, T: Transport + 'static>(
    ctx: &LoopCtx<S, T>,
    call: &PreparedCall,
    _started: Instant,
) -> Decision {
    {
        let mut s = match ctx.handle.shared.lock() {
            Ok(s) => s,
            Err(_) => return Decision::Cancelled,
        };
        s.status = AgentStatus::AwaitingApproval;
        s.pending_approval = Some(call.clone());
        push_locked(&mut s, AgentEvent::ApprovalRequired { call: call.clone() });
    }
    let deadline = Instant::now() + Duration::from_millis(APPROVAL_WAIT_MS);
    let mut guard = match ctx.handle.shared.lock() {
        Ok(g) => g,
        Err(_) => return Decision::Cancelled,
    };
    loop {
        if ctx.handle.cancel.load(Ordering::SeqCst) {
            guard.status = AgentStatus::Running;
            guard.pending_approval = None;
            return Decision::Cancelled;
        }
        if let Some(approved) = guard.decision.take() {
            guard.status = AgentStatus::Running;
            guard.pending_approval = None;
            return if approved {
                Decision::Approved
            } else {
                Decision::Denied
            };
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            guard.status = AgentStatus::Running;
            guard.pending_approval = None;
            return Decision::Timeout;
        }
        let (g, _timeout) = ctx
            .handle
            .cond
            .wait_timeout(guard, remaining.min(Duration::from_secs(5)))
            .expect("run state poisoned");
        guard = g;
    }
}

/// Concrete completion gates. A completion claim is verified, never trusted:
/// the plan must be fully done, real work must exist, and a UI deliverable
/// must survive desktop + mobile preview verification with no console or
/// network failures.
fn verify_gates<S: SecretStore + 'static, T: Transport + 'static>(
    ctx: &LoopCtx<S, T>,
    cp: &Checkpoint,
    ledger: &mut Option<Ledger>,
) -> (bool, Vec<String>) {
    let mut failures = Vec::new();
    let plan = current_plan(&ctx.handle);
    if plan.is_empty() {
        failures.push("no plan was ever created".to_string());
    } else {
        let open: Vec<&PlanItem> = plan
            .iter()
            .filter(|i| i.status != PlanStatus::Done)
            .collect();
        if !open.is_empty() {
            failures.push(format!(
                "plan has {} unfinished item(s): {}",
                open.len(),
                open.iter()
                    .map(|i| i.title.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    // The mutation gate applies only to roles that may mutate. A read-only
    // role (adversary, shadow) can never create a file - its output is the
    // inspection itself - so requiring one would make completion impossible.
    let role_can_mutate = match &ctx.brief.allowed_tools {
        None => true,
        Some(allowed) => ["create_file", "edit_file", "run_command"]
            .iter()
            .any(|t| allowed.iter().any(|a| a == t)),
    };
    if role_can_mutate && !cp.any_mutating_success {
        failures.push("no file was created, edited, or built by an approved tool".to_string());
    }
    let index = ctx.workspace.join("index.html");
    if index.is_file() {
        match verify_ui(ctx, ledger) {
            Ok(ui_failures) => failures.extend(ui_failures),
            Err(e) => failures.push(format!("preview verification could not run: {e}")),
        }
    }
    (failures.is_empty(), failures)
}

fn verify_ui<S: SecretStore + 'static, T: Transport + 'static>(
    ctx: &LoopCtx<S, T>,
    ledger: &mut Option<Ledger>,
) -> Result<Vec<String>, String> {
    let supervisor = PreviewSupervisor::new(&ctx.workspace).map_err(|e| e.to_string())?;
    let summary = supervisor
        .start(Path::new("."))
        .map_err(|e| e.to_string())?;
    let sid = summary.id.clone();
    let iteration = supervisor
        .begin_iteration(&sid)
        .map_err(|e| e.to_string())?;
    supervisor
        .action(
            &sid,
            &BrowserAction::SetViewport {
                width: 1280,
                height: 800,
                scale: 1.0,
            },
        )
        .map_err(|e| e.to_string())?;
    let desktop = supervisor.capture(&sid).map_err(|e| e.to_string())?;
    supervisor
        .action(
            &sid,
            &BrowserAction::SetViewport {
                width: 390,
                height: 844,
                scale: 2.0,
            },
        )
        .map_err(|e| e.to_string())?;
    let mobile = supervisor.capture(&sid).map_err(|e| e.to_string())?;
    supervisor
        .action(
            &sid,
            &BrowserAction::SetViewport {
                width: 1280,
                height: 800,
                scale: 1.0,
            },
        )
        .map_err(|e| e.to_string())?;

    // Settle, then drain page-load noise (e.g. a favicon 404 or the initial
    // about:blank load) so gate evidence measures the rendered page only.
    std::thread::sleep(Duration::from_millis(400));
    let _ = supervisor.capture(&sid);
    let settled_desktop = supervisor.capture(&sid).map_err(|e| e.to_string())?;
    let settled_mobile = supervisor.capture(&sid).map_err(|e| e.to_string())?;
    let settled = [settled_desktop, settled_mobile];

    let mut failures = Vec::new();
    let mut failed_gates = Vec::new();
    for (label, ev) in [("desktop", &settled[0]), ("mobile", &settled[1])] {
        if ev.items.iter().any(|i| {
            matches!(
                i,
                rex_preview::Evidence::Console {
                    level: rex_preview::ConsoleLevel::Error,
                    ..
                }
            )
        }) {
            failures.push(format!("{label} viewport shows console errors"));
            failed_gates.push(ProductionGate::NoConsoleErrors);
        }
        if ev
            .items
            .iter()
            .any(|i| matches!(i, rex_preview::Evidence::NetworkFailure { .. }))
        {
            failures.push(format!("{label} viewport shows failed network requests"));
            failed_gates.push(ProductionGate::NoFailedRequests);
        }
    }
    let accepted = failures.is_empty();
    let evidence_ids: Vec<String> = desktop
        .items
        .iter()
        .chain(mobile.items.iter())
        .filter_map(|item| match item {
            rex_preview::Evidence::Screenshot { id, .. }
            | rex_preview::Evidence::DomSnapshot { id, .. }
            | rex_preview::Evidence::Accessibility { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();
    let _ = supervisor.record_iteration(
        &sid,
        IterationReceipt {
            iteration,
            accepted,
            diff_id: "agent-loop-gate".into(),
            evidence_ids,
            failed_gates,
            reason: if accepted {
                "Desktop and mobile captures landed clean.".into()
            } else {
                failures.join("; ")
            },
        },
    );
    let receipts = supervisor
        .production_report(&sid)
        .map(|r| r.receipts)
        .unwrap_or_default();
    if let Some(l) = ledger.as_mut() {
        l.append(
            "capture",
            json!({"iteration": iteration, "accepted": accepted}),
        );
    }
    if let Ok(mut s) = ctx.handle.shared.lock() {
        s.preview = Some(AgentPreview {
            session_id: sid.clone(),
            url: summary.url.clone(),
            desktop_shot: desktop.screenshot_data_url.clone(),
            mobile_shot: mobile.screenshot_data_url.clone(),
            receipts,
        });
        s.preview_supervisor = Some(supervisor);
        s.preview_session = Some(sid);
    }
    Ok(failures)
}

/// Terminal landing: status, event, ledger, checkpoint, and terminal.json all
/// agree, so a resumed process can never mistake a finished run for a live one.
fn finish<S: SecretStore + 'static, T: Transport + 'static>(
    ctx: &LoopCtx<S, T>,
    ledger: &mut Option<Ledger>,
    reason: TerminalReason,
    started: Instant,
    cp: &Checkpoint,
) {
    let elapsed = ctx.checkpoint.elapsed_base_ms + started.elapsed().as_millis() as u64;
    if let Ok(mut s) = ctx.handle.shared.lock() {
        s.status = reason.status();
        s.elapsed_ms = elapsed;
        s.pending_approval = None;
        if let TerminalReason::ProviderError { detail } = &reason {
            s.error = Some(detail.clone());
        }
        push_locked(
            &mut s,
            AgentEvent::Info {
                message: format!("run ended: {}", terminal_label(&reason)),
            },
        );
        s.terminal = Some(reason.clone());
    }
    if let Some(l) = ledger.as_mut() {
        l.append("terminal", json!({"reason": reason}));
    }
    let _ = write_json(
        &ctx.state_dir.join("terminal.json"),
        &json!({"reason": reason, "elapsed_ms": elapsed}),
    );
    let _ = write_json(&ctx.state_dir.join("checkpoint.json"), cp);
}

fn terminal_label(reason: &TerminalReason) -> String {
    match reason {
        TerminalReason::Completed => "completed".into(),
        TerminalReason::GatesFailed { .. } => "blocked: completion gates failed".into(),
        TerminalReason::Blocked { detail } => format!("blocked: {detail}"),
        TerminalReason::BudgetSteps { .. } => "blocked: step budget exhausted".into(),
        TerminalReason::BudgetTime { .. } => "blocked: time budget exhausted".into(),
        TerminalReason::BudgetTokens { .. } => "blocked: token budget exhausted".into(),
        TerminalReason::BudgetToolCalls { .. } => "blocked: tool-call budget exhausted".into(),
        TerminalReason::RepeatedFailure { tool } => format!("failed: {tool} kept failing"),
        TerminalReason::NoProgress { .. } => "failed: no progress across turns".into(),
        TerminalReason::Cancelled => "cancelled".into(),
        TerminalReason::Denied => "stopped: write denied".into(),
        TerminalReason::ApprovalTimeout => "blocked: approval wait timed out".into(),
        TerminalReason::ProviderError { detail } => format!("failed: {detail}"),
        TerminalReason::ModelStalled => "failed: model stopped calling tools".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ProviderError;
    use crate::secrets::MemorySecretStore;
    use std::collections::VecDeque;

    /// Deterministic adversarial transport: serves a fixed catalog, fails a
    /// scripted number of POSTs with a scripted status, then replays queued
    /// model turns in order and records every request body it saw.
    struct Script {
        turns: Mutex<VecDeque<String>>,
        fail_posts: Mutex<(u32, u16)>,
        posts: Mutex<Vec<String>>,
    }

    impl Script {
        fn new(turns: Vec<String>) -> Self {
            Self {
                turns: Mutex::new(turns.into()),
                fail_posts: Mutex::new((0, 500)),
                posts: Mutex::new(Vec::new()),
            }
        }
        fn failing(turns: Vec<String>, count: u32, status: u16) -> Self {
            Self {
                turns: Mutex::new(turns.into()),
                fail_posts: Mutex::new((count, status)),
                posts: Mutex::new(Vec::new()),
            }
        }
        fn seen(&self) -> Vec<String> {
            self.posts.lock().unwrap().clone()
        }
    }

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
            body: &str,
        ) -> Result<(u16, String), ProviderError> {
            self.posts.lock().unwrap().push(body.to_string());
            {
                let mut fail = self.fail_posts.lock().unwrap();
                if fail.0 > 0 {
                    fail.0 -= 1;
                    return Ok((fail.1, r#"{"error":{"message":"scripted failure"}}"#.into()));
                }
            }
            let next = self.turns.lock().unwrap().pop_front();
            match next {
                Some(body) => Ok((200, body)),
                None => Ok((200, text_turn("nothing more to do"))),
            }
        }
    }

    fn text_turn(text: &str) -> String {
        json!({"candidates":[{"content":{"parts":[{"text": text}]},"finishReason":"STOP"}],"usageMetadata":{"totalTokenCount":120}}).to_string()
    }

    fn call_turn(calls: Vec<Value>) -> String {
        let parts: Vec<Value> = calls;
        json!({"candidates":[{"content":{"parts": parts},"finishReason":"STOP"}],"usageMetadata":{"totalTokenCount":240}}).to_string()
    }

    fn plan_call(items: Vec<(&str, &str, &str)>) -> Value {
        json!({"functionCall":{"name":"update_plan","args":{"items": items.iter().map(|(id, title, status)| json!({"id": id, "title": title, "status": status})).collect::<Vec<_>>()}}})
    }

    fn create_call(path: &str, content: &str) -> Value {
        json!({"functionCall":{"name":"create_file","args":{"path": path, "content": content, "overwrite": true}}})
    }

    fn read_call(path: &str) -> Value {
        json!({"functionCall":{"name":"read_file","args":{"path": path}}})
    }

    fn complete_call(summary: &str) -> Value {
        json!({"functionCall":{"name":"complete_task","args":{"summary": summary}}})
    }

    const PAGE: &str = "<!doctype html><html><head><style>body{font-family:sans-serif;margin:0}header{padding:24px;background:#123;color:#fff}</style></head><body><header><h1>Tea House</h1></header><main><p>Fresh leaves.</p></main></body></html>";

    type Svc = AutonomousRunService<MemorySecretStore, Script>;

    fn service(root: &Path, script: Script) -> Svc {
        let store = MemorySecretStore::new();
        store.set_key("gemini", "test-key").unwrap();
        AutonomousRunService::new(ProviderService::new(store, script), None, root.join("runs"))
    }

    fn wait_terminal(svc: &Svc, id: &str, timeout_ms: u64) -> AgentSnapshot {
        let start = Instant::now();
        loop {
            let snap = svc.snapshot(id).expect("snapshot");
            if snap.terminal_reason.is_some() {
                return snap;
            }
            if start.elapsed().as_millis() as u64 > timeout_ms {
                panic!(
                    "run did not reach a terminal state in time; status {:?}",
                    snap.status
                );
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Approve every approval the run raises until it lands terminal.
    fn auto_approve(svc: Arc<Svc>, id: String) {
        std::thread::spawn(move || loop {
            let snap = svc.snapshot(&id).expect("snapshot");
            if snap.terminal_reason.is_some() {
                return;
            }
            if snap.status == AgentStatus::AwaitingApproval && snap.pending_approval.is_some() {
                let _ = svc.decide(&id, true);
            }
            std::thread::sleep(Duration::from_millis(10));
        });
    }

    fn budgets() -> Budgets {
        Budgets {
            max_steps: 12,
            max_tool_calls: 40,
            max_wall_ms: 120_000,
            max_tokens: 100_000,
        }
    }

    #[test]
    fn anthropic_adapter_normalizes_loop_calls_and_history() {
        let body = json!({"content":[
            {"type":"text","text":"Planning."},
            {"type":"tool_use","id":"toolu_1","name":"update_plan","input":{"items":[{"id":"1","title":"Build","status":"in_progress"}]}},
            {"type":"tool_use","id":"toolu_2","name":"read_file","input":{"path":"README.md"}}
        ],"usage":{"input_tokens":12,"output_tokens":8}}).to_string();
        let decoded = decode_provider_calls(ProviderProtocol::Anthropic, &body).unwrap();
        assert_eq!(decoded.texts, vec!["Planning."]);
        assert_eq!(decoded.calls.len(), 2);
        assert_eq!(usage_tokens(ProviderProtocol::Anthropic, &body), Some(20));
        let pair = TurnPair {
            model_parts: decoded
                .calls
                .iter()
                .filter_map(|(_, r)| r.clone())
                .collect(),
            response_parts: vec![function_response(
                ProviderProtocol::Anthropic,
                "read_file",
                "toolu_2",
                true,
                "ok",
            )],
        };
        let request = build_request(
            ProviderProtocol::Anthropic,
            "claude-test",
            "sys",
            "state",
            Some(&pair),
        );
        assert!(request.contains("tool_result"));
        assert!(request.contains("toolu_2"));
        assert!(request.contains("input_schema"));
    }

    #[test]
    fn openai_compatible_adapter_normalizes_calls_and_history() {
        let body = json!({"choices":[{"message":{"content":"Acting.","tool_calls":[
            {"id":"call_42","type":"function","function":{"name":"read_file","arguments":serde_json::to_string(&json!({"path":"README.md"})).unwrap()}}
        ]},"finish_reason":"tool_calls"}],"usage":{"total_tokens":31}}).to_string();
        let decoded = decode_provider_calls(ProviderProtocol::OpenAiCompatible, &body).unwrap();
        assert_eq!(decoded.calls.len(), 1);
        assert_eq!(
            usage_tokens(ProviderProtocol::OpenAiCompatible, &body),
            Some(31)
        );
        let pair = TurnPair {
            model_parts: decoded
                .calls
                .iter()
                .filter_map(|(_, r)| r.clone())
                .collect(),
            response_parts: vec![function_response(
                ProviderProtocol::OpenAiCompatible,
                "read_file",
                "call_42",
                true,
                "ok",
            )],
        };
        let request = build_request(
            ProviderProtocol::OpenAiCompatible,
            "gpt-test",
            "sys",
            "state",
            Some(&pair),
        );
        assert!(request.contains("tool_call_id"));
        assert!(request.contains("call_42"));
        assert!(request.contains("chat") || request.contains("tools"));
    }

    #[test]
    fn provider_specific_requests_keep_credentials_out_of_evidence() {
        let anthropic = build_request(
            ProviderProtocol::Anthropic,
            "claude-test",
            "sys",
            "state",
            None,
        );
        let openai = build_request(
            ProviderProtocol::OpenAiCompatible,
            "gpt-test",
            "sys",
            "state",
            None,
        );
        assert!(!anthropic.contains("api-key"));
        assert!(!openai.contains("Bearer"));
        assert!(anthropic.contains("claude-test"));
        assert!(openai.contains("gpt-test"));
    }

    #[test]
    fn brief_is_written_once_and_never_rewritten() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        fs::create_dir_all(&dir).unwrap();
        let brief = TaskBrief {
            id: "r1".into(),
            task: "original ask".into(),
            provider: "gemini".into(),
            budgets: Budgets::default(),
            created_at_ms: 1,
            prompt_version: legacy_prompt_marker(),
            prompt_hash: legacy_prompt_marker(),
            role: None,
            allowed_tools: None,
        };
        write_brief(&dir, &brief).unwrap();
        let mut changed = brief.clone();
        changed.task = "quietly rewritten ask".into();
        assert!(write_brief(&dir, &changed).is_err());
        let on_disk: TaskBrief = read_json(&dir.join("brief.json")).unwrap();
        assert_eq!(on_disk.task, "original ask");
    }

    #[test]
    fn repeated_identical_failure_terminates_truthfully() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = Arc::new(service(
            tmp.path(),
            Script::new(vec![
                call_turn(vec![read_call("missing.txt")]),
                call_turn(vec![read_call("missing.txt")]),
                call_turn(vec![read_call("missing.txt")]),
            ]),
        ));
        let snap = svc
            .begin("read something", "gemini", Some(budgets()))
            .unwrap();
        let done = wait_terminal(&svc, &snap.id, 15_000);
        assert!(matches!(
            done.terminal_reason,
            Some(TerminalReason::RepeatedFailure { .. })
        ));
        assert_eq!(done.status, AgentStatus::Failed);
        // the failure warning was injected after the second identical failure
        let posts = svc.service.transport().seen();
        assert!(posts.last().unwrap().contains("failed twice"));
        // evidence and checkpoint exist on disk
        let state = tmp.path().join("runs").join(&snap.id).join("state");
        assert!(state.join("checkpoint.json").exists());
        assert!(state.join("terminal.json").exists());
        let ledger = fs::read_to_string(state.join("ledger.jsonl")).unwrap();
        assert!(ledger.contains("\"terminal\""));
    }

    #[test]
    fn step_budget_is_a_hard_stop() {
        let tmp = tempfile::tempdir().unwrap();
        let mut b = budgets();
        b.max_steps = 2;
        let svc = Arc::new(service(
            tmp.path(),
            Script::new(vec![
                text_turn("thinking"),
                text_turn("more thinking"),
                text_turn("never stops"),
            ]),
        ));
        let snap = svc.begin("ramble", "gemini", Some(b)).unwrap();
        let done = wait_terminal(&svc, &snap.id, 15_000);
        assert!(matches!(
            done.terminal_reason,
            Some(TerminalReason::BudgetSteps { max_steps: 2 })
        ));
    }

    #[test]
    fn no_progress_across_turns_terminates() {
        let tmp = tempfile::tempdir().unwrap();
        let mut turns = vec![call_turn(vec![
            plan_call(vec![("1", "make a file", "in_progress")]),
            create_call("a.txt", "alpha"),
        ])];
        for _ in 0..7 {
            turns.push(call_turn(vec![read_call("a.txt")]));
        }
        let svc = Arc::new(service(tmp.path(), Script::new(turns)));
        let snap = svc
            .begin("work then idle", "gemini", Some(budgets()))
            .unwrap();
        auto_approve(svc.clone(), snap.id.clone());
        let done = wait_terminal(&svc, &snap.id, 20_000);
        assert!(matches!(
            done.terminal_reason,
            Some(TerminalReason::NoProgress { .. })
        ));
    }

    #[test]
    fn stalled_model_terminates_and_was_nudged() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = Arc::new(service(
            tmp.path(),
            Script::new(vec![
                text_turn("let me think"),
                text_turn("still thinking"),
                text_turn("hmm"),
            ]),
        ));
        let snap = svc.begin("stall", "gemini", Some(budgets())).unwrap();
        let done = wait_terminal(&svc, &snap.id, 15_000);
        assert!(matches!(
            done.terminal_reason,
            Some(TerminalReason::ModelStalled)
        ));
        let posts = svc.service.transport().seen();
        assert!(posts.len() >= 2);
        assert!(posts[1].contains("produced no tool calls"));
    }

    #[test]
    fn token_budget_stops_the_run() {
        let tmp = tempfile::tempdir().unwrap();
        let mut b = budgets();
        b.max_tokens = 2_000; // clamp floor
        let turns: Vec<String> = (0..10)
            .map(|i| {
                call_turn(vec![plan_call(vec![(
                    "1",
                    &format!("step {i}"),
                    "in_progress",
                )])])
            })
            .collect();
        let svc = Arc::new(service(tmp.path(), Script::new(turns)));
        let snap = svc.begin("spend tokens", "gemini", Some(b)).unwrap();
        let done = wait_terminal(&svc, &snap.id, 20_000);
        assert!(matches!(
            done.terminal_reason,
            Some(TerminalReason::BudgetTokens { max_tokens: 2_000 })
        ));
    }

    #[test]
    fn tool_call_budget_stops_mid_turn() {
        let tmp = tempfile::tempdir().unwrap();
        let mut b = budgets();
        b.max_tool_calls = 1;
        let svc = Arc::new(service(
            tmp.path(),
            Script::new(vec![call_turn(vec![read_call("x"), read_call("y")])]),
        ));
        let snap = svc.begin("two reads", "gemini", Some(b)).unwrap();
        let done = wait_terminal(&svc, &snap.id, 15_000);
        assert!(matches!(
            done.terminal_reason,
            Some(TerminalReason::BudgetToolCalls { max_tool_calls: 1 })
        ));
    }

    #[test]
    fn cancellation_during_approval_wait_is_terminal_and_truthful() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = Arc::new(service(
            tmp.path(),
            Script::new(vec![call_turn(vec![create_call("b.txt", "beta")])]),
        ));
        let snap = svc
            .begin("write then cancel", "gemini", Some(budgets()))
            .unwrap();
        let start = Instant::now();
        loop {
            let s = svc.snapshot(&snap.id).unwrap();
            if s.status == AgentStatus::AwaitingApproval {
                break;
            }
            assert!(start.elapsed().as_secs() < 10, "never reached approval");
            std::thread::sleep(Duration::from_millis(20));
        }
        svc.cancel(&snap.id).unwrap();
        let done = wait_terminal(&svc, &snap.id, 15_000);
        assert!(matches!(
            done.terminal_reason,
            Some(TerminalReason::Cancelled)
        ));
        assert!(!tmp
            .path()
            .join("runs")
            .join(&snap.id)
            .join("workspace")
            .join("b.txt")
            .exists());
    }

    #[test]
    fn second_denial_stops_the_run() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = Arc::new(service(
            tmp.path(),
            Script::new(vec![
                call_turn(vec![create_call("c.txt", "one")]),
                call_turn(vec![create_call("c.txt", "two")]),
            ]),
        ));
        let snap = svc
            .begin("denied twice", "gemini", Some(budgets()))
            .unwrap();
        let svc2 = svc.clone();
        let id = snap.id.clone();
        std::thread::spawn(move || loop {
            let s = svc2.snapshot(&id).unwrap();
            if s.terminal_reason.is_some() {
                return;
            }
            if s.status == AgentStatus::AwaitingApproval {
                let _ = svc2.decide(&id, false);
            }
            std::thread::sleep(Duration::from_millis(10));
        });
        let done = wait_terminal(&svc, &snap.id, 15_000);
        assert!(matches!(done.terminal_reason, Some(TerminalReason::Denied)));
        assert!(!tmp
            .path()
            .join("runs")
            .join(&snap.id)
            .join("workspace")
            .join("c.txt")
            .exists());
    }

    #[test]
    fn provider_retries_then_recovers() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = Arc::new(service(
            tmp.path(),
            Script::failing(
                vec![
                    text_turn("recovered"),
                    text_turn("s1"),
                    text_turn("s2"),
                    text_turn("s3"),
                ],
                2,
                500,
            ),
        ));
        let snap = svc
            .begin("flaky provider", "gemini", Some(budgets()))
            .unwrap();
        let done = wait_terminal(&svc, &snap.id, 20_000);
        // recovered, then stalled out on text-only turns
        assert!(matches!(
            done.terminal_reason,
            Some(TerminalReason::ModelStalled)
        ));
        assert!(done
            .events
            .iter()
            .any(|e| matches!(e, AgentEvent::Retry { attempt: 1, .. })));
        assert!(done
            .events
            .iter()
            .any(|e| matches!(e, AgentEvent::Retry { attempt: 2, .. })));
    }

    #[test]
    fn hard_provider_failure_is_not_retried_forever() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = Arc::new(service(tmp.path(), Script::failing(vec![], 99, 401)));
        let snap = svc
            .begin("unauthorized", "gemini", Some(budgets()))
            .unwrap();
        let done = wait_terminal(&svc, &snap.id, 20_000);
        match done.terminal_reason {
            Some(TerminalReason::ProviderError { detail }) => {
                assert!(detail.contains("401"), "truthful detail, got {detail}");
            }
            other => panic!("expected provider error, got {other:?}"),
        }
        assert_eq!(done.status, AgentStatus::Failed);
    }

    #[test]
    fn gate_failure_feeds_back_and_later_passes() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = Arc::new(service(
            tmp.path(),
            Script::new(vec![
                // turn 1: write without any plan
                call_turn(vec![create_call("index.html", PAGE)]),
                // turn 2: claim completion - must fail gates (no plan)
                call_turn(vec![complete_call("done")]),
                // turn 3: plan done + complete again
                call_turn(vec![
                    plan_call(vec![("1", "build page", "done")]),
                    complete_call("done"),
                ]),
            ]),
        ));
        let snap = svc
            .begin("build a tea house page", "gemini", Some(budgets()))
            .unwrap();
        auto_approve(svc.clone(), snap.id.clone());
        let done = wait_terminal(&svc, &snap.id, 120_000);
        assert!(
            matches!(done.terminal_reason, Some(TerminalReason::Completed)),
            "got {:?}",
            done.terminal_reason
        );
        assert_eq!(done.status, AgentStatus::Completed);
        let posts = svc.service.transport().seen();
        assert!(posts.len() >= 3);
        assert!(
            posts[2].contains("gate verification failed"),
            "gate failure feedback must reach the model, got: {}",
            &posts[2][..posts[2].len().min(400)]
        );
        assert!(done.events.iter().any(|e| matches!(
            e,
            AgentEvent::GateResult {
                attempt: 1,
                passed: false,
                ..
            }
        )));
        let preview = done.preview.expect("verified preview must be visible");
        assert!(preview.desktop_shot.is_some() && preview.mobile_shot.is_some());
    }

    #[test]
    fn full_loop_completes_with_real_gates() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = Arc::new(service(
            tmp.path(),
            Script::new(vec![
                call_turn(vec![
                    plan_call(vec![
                        ("1", "build the page", "in_progress"),
                        ("2", "verify it renders", "pending"),
                    ]),
                    create_call("index.html", PAGE),
                ]),
                call_turn(vec![
                    plan_call(vec![
                        ("1", "build the page", "done"),
                        ("2", "verify it renders", "done"),
                    ]),
                    complete_call("tea house page built"),
                ]),
            ]),
        ));
        let snap = svc
            .begin("build a tea house landing page", "gemini", Some(budgets()))
            .unwrap();
        auto_approve(svc.clone(), snap.id.clone());
        let done = wait_terminal(&svc, &snap.id, 120_000);
        if !matches!(done.terminal_reason, Some(TerminalReason::Completed)) {
            for e in &done.events {
                eprintln!("EVENT: {}", serde_json::to_string(e).unwrap_or_default());
            }
        }
        assert!(
            matches!(done.terminal_reason, Some(TerminalReason::Completed)),
            "got {:?}",
            done.terminal_reason
        );
        assert_eq!(
            done.completion_summary.as_deref(),
            Some("tea house page built")
        );
        let ws = tmp.path().join("runs").join(&snap.id).join("workspace");
        assert!(ws.join("index.html").exists());
        let preview = done.preview.expect("preview");
        assert!(preview.url.starts_with("http://127.0.0.1:"));
        assert!(preview.receipts.iter().any(|r| r.accepted));
        // raw evidence is addressable on disk
        let evidence = tmp
            .path()
            .join("runs")
            .join(&snap.id)
            .join("state")
            .join("evidence");
        assert!(evidence.join("turn-1-request.json").exists());
        assert!(evidence.join("turn-2-response.json").exists());
        svc.teardown(&snap.id).unwrap();
    }

    #[test]
    fn gemini_request_carries_the_assembled_system_instruction() {
        let (system, _, _) = assemble_run_prompt(Role::Worker, &offered_tool_specs());
        let request = build_request(
            ProviderProtocol::Gemini,
            "gemini-test",
            &system,
            "state",
            None,
        );
        let value: Value = serde_json::from_str(&request).unwrap();
        let text = value["systemInstruction"]["parts"][0]["text"]
            .as_str()
            .expect("gemini request carries a system instruction");
        assert!(text.contains("REX CONSTITUTION"));
        assert!(text.contains("ROLE: REX WORKER"));
        assert!(text.contains("TOOLS ENABLED FOR THIS CALL"));
        assert!(text.contains("COMPLETION GATE"));
        // same semantics reach the other protocols' system fields
        let anthropic = build_request(ProviderProtocol::Anthropic, "m", &system, "state", None);
        assert!(anthropic.contains("REX CONSTITUTION"));
        let openai = build_request(
            ProviderProtocol::OpenAiCompatible,
            "m",
            &system,
            "state",
            None,
        );
        assert!(openai.contains("REX CONSTITUTION"));
    }

    #[test]
    fn tool_definitions_match_contract() {
        // The wire definitions and the prompt's tool contract must describe
        // the same offering: no phantom tools, no undocumented ones.
        let declared = gemini_tool_definitions()[0]["functionDeclarations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["name"].as_str().unwrap().to_string())
            .collect::<std::collections::BTreeSet<_>>();
        let contracted = offered_tool_specs()
            .iter()
            .map(|s| s.name.clone())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(declared, contracted);
    }

    #[test]
    fn read_only_role_scope_excludes_mutating_tools() {
        let (scoped, allowlist) = scoped_tools_for(Role::Adversary);
        let allowed = allowlist.expect("adversary runs are scoped");
        for gone in ["create_file", "edit_file", "run_command"] {
            assert!(
                !allowed.iter().any(|t| t == gone),
                "{gone} must not be allowed"
            );
        }
        for kept in ["read_file", "search_files", "web_search"] {
            assert!(allowed.iter().any(|t| t == kept), "{kept} stays");
        }
        let contract = rex_prompt::tools::render_contract(&scoped);
        assert!(!contract.contains("create_file"));
        // the worker keeps the full offering with no enforced allowlist
        let (_, worker_allow) = scoped_tools_for(Role::Worker);
        assert!(worker_allow.is_none());
    }

    #[test]
    fn resume_identity_gate_fails_closed_on_mismatch_and_migrates_legacy() {
        let (_, version, hash) = assemble_run_prompt(Role::Worker, &offered_tool_specs());
        let mut cp = Checkpoint {
            prompt_version: version.clone(),
            prompt_hash: hash.clone(),
            ..Checkpoint::default()
        };
        assert_eq!(check_resume_identity(&cp, &version, &hash), Ok(false));
        cp.prompt_hash = "0000deadbeef".into();
        let err = check_resume_identity(&cp, &version, &hash).unwrap_err();
        assert!(err.contains("Refusing to mix prompt semantics"), "{err}");
        cp.prompt_version = legacy_prompt_marker();
        cp.prompt_hash = legacy_prompt_marker();
        assert_eq!(check_resume_identity(&cp, &version, &hash), Ok(true));
    }

    #[test]
    fn read_only_role_cannot_write_but_can_complete() {
        // An adversary-scoped run: the model tries create_file, the harness
        // refuses it as out of scope, and the run still completes on the
        // strength of its read-only inspection.
        let tmp = tempfile::tempdir().unwrap();
        let svc = Arc::new(service(
            tmp.path(),
            Script::new(vec![
                call_turn(vec![
                    plan_call(vec![("1", "inspect", "in_progress")]),
                    create_call("notes.md", "try to write"),
                ]),
                call_turn(vec![
                    plan_call(vec![("1", "inspect", "done")]),
                    complete_call("{\"defects\":[]}"),
                ]),
            ]),
        ));
        let snap = svc
            .begin_in_workspace_with_role(
                "inspect the workspace",
                "gemini",
                None,
                Some(budgets()),
                None,
                Role::Adversary,
            )
            .unwrap();
        auto_approve(svc.clone(), snap.id.clone());
        let done = wait_terminal(&svc, &snap.id, 60_000);
        assert!(
            matches!(done.terminal_reason, Some(TerminalReason::Completed)),
            "got {:?}",
            done.terminal_reason
        );
        assert!(done.events.iter().any(|e| matches!(
            e,
            AgentEvent::Info { message } if message.contains("refused out-of-scope tool create_file")
        )));
        // nothing was written
        assert!(
            std::fs::read_dir(tmp.path().join("runs").join(&snap.id).join("workspace"))
                .map(|mut d| d.next().is_none())
                .unwrap_or(false)
        );
    }

    #[test]
    fn resume_continues_from_checkpoint_after_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let run_id;
        {
            let svc = Arc::new(service(
                &root,
                Script::new(vec![
                    call_turn(vec![
                        plan_call(vec![("1", "write page", "in_progress")]),
                        create_call("index.html", PAGE),
                    ]),
                    call_turn(vec![create_call("second.txt", "never finished")]),
                ]),
            ));
            let snap = svc
                .begin("resumable task", "gemini", Some(budgets()))
                .unwrap();
            run_id = snap.id.clone();
            // approve turn 1's write only, so turn 1 completes and checkpoints
            let svc1 = svc.clone();
            let id1 = run_id.clone();
            std::thread::spawn(move || {
                let start = Instant::now();
                loop {
                    let s = svc1.snapshot(&id1).unwrap();
                    if s.step >= 1 || start.elapsed().as_secs() > 30 {
                        return;
                    }
                    if s.status == AgentStatus::AwaitingApproval {
                        let _ = svc1.decide(&id1, true);
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            });
            // wait for turn 2's approval park: checkpoint from turn 1 exists,
            // the second write is mid-flight and never completed
            let start = Instant::now();
            loop {
                let s = svc.snapshot(&run_id).unwrap();
                if s.step >= 1 && s.status == AgentStatus::AwaitingApproval {
                    break;
                }
                assert!(
                    start.elapsed().as_secs() < 30,
                    "never parked: {:?}",
                    s.status
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            // process death: the thread stops without writing a terminal state
            svc.halt_without_terminal(&run_id);
            std::thread::sleep(Duration::from_millis(100));
        }
        // a new service over the same runs root resumes from disk
        let svc2 = Arc::new(service(
            &root,
            Script::new(vec![call_turn(vec![
                plan_call(vec![("1", "write page", "done")]),
                complete_call("resumed and done"),
            ])]),
        ));
        let resumed = svc2.resume(&run_id).unwrap();
        assert!(resumed.step >= 1, "resumed at the checkpointed step");
        assert_eq!(resumed.plan.len(), 1);
        auto_approve(svc2.clone(), run_id.clone());
        let done = wait_terminal(&svc2, &run_id, 120_000);
        assert!(
            matches!(done.terminal_reason, Some(TerminalReason::Completed)),
            "got {:?}",
            done.terminal_reason
        );
        assert!(done.events.iter().any(|e| matches!(e, AgentEvent::Info { message } if message.contains("resumed from checkpoint"))));
        svc2.teardown(&run_id).unwrap();
    }

    #[test]
    fn resume_refuses_terminal_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let run_id;
        {
            let svc = Arc::new(service(
                &root,
                Script::new(vec![text_turn("a"), text_turn("b"), text_turn("c")]),
            ));
            let snap = svc.begin("short stall", "gemini", Some(budgets())).unwrap();
            run_id = snap.id.clone();
            wait_terminal(&svc, &run_id, 15_000);
        }
        let svc2 = Arc::new(service(&root, Script::new(vec![])));
        assert!(svc2.resume(&run_id).is_err());
    }

    #[test]
    fn budgets_are_clamped_to_hard_limits() {
        let b = Budgets {
            max_steps: 0,
            max_tool_calls: 100_000,
            max_wall_ms: 1,
            max_tokens: 1,
        }
        .clamped();
        assert_eq!(b.max_steps, 1);
        assert_eq!(b.max_tool_calls, HARD_MAX_TOOL_CALLS);
        assert!(b.max_wall_ms >= 10_000);
    }

    #[test]
    fn unknown_provider_fails_before_any_work() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = service(tmp.path(), Script::new(vec![]));
        assert!(svc
            .begin("task", "private-undocumented-wire", None)
            .is_err());
        assert!(svc.begin("   ", "gemini", None).is_err());
    }
}
