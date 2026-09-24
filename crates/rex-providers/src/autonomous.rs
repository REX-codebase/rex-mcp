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
use crate::search::{SearchProvider, SearchRouter};
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
/// Identical consecutive calls (beyond the first) after which the model is
/// told it is looping. Termination stays with the no-progress budget.
pub const DOOM_LOOP_REPEATS: u32 = 2;
pub const MAX_STALL_TURNS: u32 = 3;
pub const MAX_NO_PROGRESS_TURNS: u32 = 6;
pub const MAX_DENIALS: u32 = 2;
pub const APPROVAL_WAIT_MS: u64 = 60 * 60 * 1000;
/// How long `ask_user` waits for the trusted UI before telling the model to
/// proceed on its own judgement. A missed question never ends the run.
pub const ANSWER_WAIT_MS: u64 = 30 * 60 * 1000;
/// Questions one run may ask; past this the model is told to decide.
pub const MAX_QUESTIONS_PER_RUN: u32 = 3;
const QUESTION_CHARS: usize = 500;
const CHOICE_CHARS: usize = 120;
const MAX_CHOICES: usize = 4;
/// Longest answer text passed back to the model.
pub const ANSWER_CHARS: usize = 2000;
const MAX_ACTIVE_RUNS: usize = 4;
const MAX_EVENTS: usize = 200;
const PROVIDER_RETRIES: u32 = 3;
const DIGEST_WINDOW: usize = 6;
const OUTCOME_CHARS: usize = 4_000;

#[path = "explore.rs"]
mod explore;
#[path = "fetch.rs"]
mod fetch;
use explore::{run_explore, ExploreLimits, ExploreOutcome, ProviderLink};

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
    /// Plan mode: the model produced a plan and the run is parked until the
    /// trusted UI approves or rejects it. No tool has run yet.
    AwaitingPlan,
    /// The model asked the user a question (`ask_user`) and the run is
    /// parked until the trusted UI answers or declines.
    AwaitingAnswer,
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
    /// The user approved this command and every later run of the exact
    /// same command line for the rest of this run (never persisted).
    StandingApprovalGranted {
        call_id: String,
        command: String,
    },
    /// A command ran under a standing approval the user granted earlier in
    /// this run; no new approval was asked for.
    ApprovedByStanding {
        call_id: String,
        command: String,
    },
    /// Plan mode: the proposed plan is waiting on the trusted UI.
    PlanApprovalRequired {
        items: Vec<PlanItem>,
    },
    PlanApprovalResolved {
        approved: bool,
    },
    QuestionAsked {
        question: PendingQuestion,
    },
    QuestionResolved {
        call_id: String,
        answered: bool,
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

/// A question the model put to the user through `ask_user`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PendingQuestion {
    pub call_id: String,
    pub question: String,
    /// Suggested answers, best first; the user may still type anything.
    pub choices: Vec<String>,
    /// 1-based position when the model asked several questions in one call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_index: Option<usize>,
    /// Number of questions in that call; `None` for a single question.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_total: Option<usize>,
    /// Every question of a batched call, so the UI can show them on one
    /// form and answer them together with `answer_many`. Empty otherwise.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub batch: Vec<BatchQuestion>,
}

/// One question of a batched `ask_user` call.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BatchQuestion {
    pub question: String,
    pub choices: Vec<String>,
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
    pub pending_question: Option<PendingQuestion>,
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
    /// Plan mode: the run must produce a plan and wait for trusted approval
    /// before any tool executes. Legacy briefs (no field) run un-gated.
    #[serde(default)]
    plan_mode: bool,
    /// Opt-in: fold tool results dropped from working memory into a
    /// model-written summary (see `history`). Off by default and for every
    /// legacy brief, because each summary spends tokens.
    #[serde(default)]
    summarize_history: bool,
    /// Human-given session label (`rex exec --name`). Purely cosmetic: it
    /// never changes what the run may do. Legacy briefs carry none.
    #[serde(default)]
    name: Option<String>,
    /// Run id this session continues (`rex resume`). Chains sessions
    /// together; `None` means the session started fresh.
    #[serde(default)]
    continued_from: Option<String>,
}

fn legacy_prompt_marker() -> String {
    "legacy-unknown".to_string()
}

/// Human-facing session metadata recorded on the run brief. Everything is
/// optional; `SessionMeta::default()` keeps the historical anonymous
/// behavior. The name is a label only — it grants no capability and changes
/// no behavior.
#[derive(Debug, Clone, Default)]
pub struct SessionMeta {
    pub name: Option<String>,
    pub continued_from: Option<String>,
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
    /// Signature of the previous tool call and how many times in a row it
    /// has been repeated (success or failure), for doom-loop nudges.
    #[serde(default)]
    last_call_sig: Option<String>,
    #[serde(default)]
    same_call_repeats: u32,
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
    /// Plan mode: set durably the moment the plan is approved, so a resumed
    /// run never re-enters the plan gate. Legacy checkpoints default to
    /// false, which re-runs the gate (fail closed: approval is never
    /// assumed).
    #[serde(default)]
    plan_approved: bool,
    /// `ask_user` calls made so far (capped by MAX_QUESTIONS_PER_RUN).
    #[serde(default)]
    questions_asked: u32,
    /// Bounded excerpts of older tool results (see `memory`).
    #[serde(default)]
    observations: Vec<crate::memory::Observation>,
    /// Opt-in history summary: the current summary, and excerpts dropped
    /// from `observations` that it has not folded in yet. Both stay empty
    /// when `summarize_history` is off.
    #[serde(default)]
    history_summary: Option<String>,
    #[serde(default)]
    history_pending: Vec<crate::memory::Observation>,
    /// The run's workspace, as resolved at begin time. Resume restores it
    /// from here (re-validated under the runs root); legacy checkpoints
    /// without it fall back to `<run_dir>/workspace`.
    #[serde(default)]
    workspace: Option<PathBuf>,
}

struct RunShared {
    status: AgentStatus,
    terminal: Option<TerminalReason>,
    plan: Vec<PlanItem>,
    events: VecDeque<AgentEvent>,
    pending_approval: Option<PreparedCall>,
    decision: Option<bool>,
    /// Standing approvals for this run: exact command keys the user chose
    /// "allow for this run" on. In memory only; a resumed run starts empty.
    standing: Vec<String>,
    pending_question: Option<PendingQuestion>,
    /// Trusted UI reply to `pending_question`: `Some(None)` = declined.
    answer: Option<Option<String>>,
    /// Replies for the later questions of the current batch, given
    /// together through `answer_many`. Cleared when the batch ends.
    queued_answers: VecDeque<Option<String>>,
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
        /// Model-supplied sites (URLs or bare domains) to seed the keyless
        /// engine; ignored by keyed providers.
        sites: Vec<String>,
    },
    CompleteTask {
        summary: String,
    },
    /// Read one public web page as text.
    WebFetch {
        id: String,
        url: String,
        offset: usize,
        /// Flat text instead of the default Markdown (`format: "text"`).
        text: bool,
    },
    /// Hand a focused read-only question to an explorer sub-agent.
    /// One task, or up to `explore::EXPLORE_MAX_PARALLEL` run concurrently.
    Explore {
        id: String,
        tasks: Vec<String>,
        kind: explore::ExplorerKind,
    },
    /// Ask the user a blocking question through the trusted UI.
    AskUser {
        id: String,
        /// One or more (question, choices) pairs, asked in order.
        questions: Vec<(String, Vec<String>)>,
    },
}

/// What the model hears when the keyless engine has nowhere to start.
const NO_SEED_GUIDANCE: &str = "The keyless REX search crawls outward from seed sites and has no web-wide index. \
    Call web_search again with `sites` (URLs or domains likely to hold the answer, e.g. docs.rs, developer.mozilla.org, \
    the project's own site), or use web_fetch on a URL you already know.";
const MAX_SEEDS: usize = 5;
/// Domains in free query text only count with one of these endings, so
/// file names like `main.rs` or `config.json` are not mistaken for sites.
const QUERY_TLDS: &[&str] = &[
    "com", "org", "net", "io", "dev", "app", "ai", "gov", "edu", "info", "co", "uk", "in", "de",
];

fn string_list(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(|t| t.trim().chars().take(300).collect::<String>())
                .filter(|t| !t.is_empty())
                .take(MAX_SEEDS * 2)
                .collect()
        })
        .unwrap_or_default()
}

/// A bare domain such as `docs.rs` -> `https://docs.rs/`. `strict` limits
/// the ending to QUERY_TLDS (used for words pulled out of free text).
fn domain_seed(token: &str, strict: bool) -> Option<String> {
    let t = token.trim().trim_end_matches('/').to_ascii_lowercase();
    if t.contains('@') || t.contains(':') || t.len() > 253 {
        return None;
    }
    let labels: Vec<&str> = t.split('.').collect();
    if labels.len() < 2
        || labels.iter().any(|l| {
            l.is_empty()
                || l.len() > 63
                || l.starts_with('-')
                || !l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
    {
        return None;
    }
    let tld = labels[labels.len() - 1];
    let tld_ok = if strict {
        QUERY_TLDS.contains(&tld)
    } else {
        tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic())
    };
    tld_ok.then(|| format!("https://{t}/"))
}

/// Seeds for the keyless engine: model-supplied sites first, then URLs and
/// well-formed domains that appear in the query. Every seed passes the same
/// URL vetting as web_fetch; duplicates are dropped and the list is capped.
fn search_seeds(query: &str, sites: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let from_sites = sites.iter().map(|s| (s.as_str(), false));
    let from_query = query
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| "()[]<>{},;\"'`".contains(c)))
        .map(|w| (w, true));
    for (token, strict) in from_sites.chain(from_query) {
        let lower = token.to_ascii_lowercase();
        let candidate = if lower.starts_with("http://") || lower.starts_with("https://") {
            Some(token.to_string())
        } else {
            domain_seed(token, strict)
        };
        let Some(candidate) = candidate else { continue };
        let Ok(url) = fetch::vet_url(&candidate) else {
            continue;
        };
        // Seeds name public sites. IP literals and local names are refused
        // here as well as at fetch time (which re-checks every hop).
        let local = match url.host() {
            Some(url::Host::Domain(d)) => {
                let d = d.trim_end_matches('.');
                d == "localhost"
                    || [".localhost", ".local", ".internal", ".lan", ".home.arpa"]
                        .iter()
                        .any(|suffix| d.ends_with(suffix))
            }
            Some(_) | None => true,
        };
        if local {
            continue;
        }
        let url = url.to_string();
        if !out.contains(&url) {
            out.push(url);
        }
        if out.len() >= MAX_SEEDS {
            break;
        }
    }
    out
}

/// Most questions accepted in one `ask_user` call.
pub const MAX_BATCH_QUESTIONS: usize = 3;

/// Decode `ask_user` args: either one `question` (+ `choices`) or a
/// `questions` array of such objects, capped at MAX_BATCH_QUESTIONS.
/// Items without a question are dropped; an empty batch is an error.
fn parse_ask_user(args: &Value) -> Result<Vec<(String, Vec<String>)>, String> {
    if let Some(items) = args.get("questions").and_then(Value::as_array) {
        let batch: Vec<_> = items
            .iter()
            .filter_map(|item| match item {
                Value::String(q) => parse_one_question(&json!({ "question": q })).ok(),
                other => parse_one_question(other).ok(),
            })
            .take(MAX_BATCH_QUESTIONS)
            .collect();
        if batch.is_empty() {
            return Err("ask_user questions has no usable question".into());
        }
        return Ok(batch);
    }
    parse_one_question(args).map(|q| vec![q])
}

/// Decode one question. Choices may arrive as strings or as objects with
/// a label-like field; blanks are dropped and everything is length-capped.
fn parse_one_question(args: &Value) -> Result<(String, Vec<String>), String> {
    let question = args
        .get("question")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if question.is_empty() {
        return Err("ask_user missing question".into());
    }
    let choices = args
        .get("choices")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|c| match c {
                    Value::String(t) => Some(t.trim().to_string()),
                    Value::Object(o) => ["label", "text", "title", "description"]
                        .iter()
                        .find_map(|k| o.get(*k).and_then(Value::as_str))
                        .map(|t| t.trim().to_string()),
                    _ => None,
                })
                .filter(|t| !t.is_empty())
                .map(|t| t.chars().take(CHOICE_CHARS).collect::<String>())
                .take(MAX_CHOICES)
                .collect()
        })
        .unwrap_or_default();
    Ok((question.chars().take(QUESTION_CHARS).collect(), choices))
}

/// Switches a run starts with. Everything defaults off.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunOptions {
    /// Park at a plan-approval gate before any tool runs.
    pub plan_mode: bool,
    /// Fold tool results that drop out of working memory into a
    /// model-written summary. Each summary is one extra model call.
    pub summarize_history: bool,
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
            pending_question: state.pending_question.clone(),
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
        self.begin_in_workspace(
            task,
            provider,
            requested_model,
            budgets,
            None,
            SessionMeta::default(),
        )
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
        session: SessionMeta,
    ) -> Result<AgentSnapshot, String> {
        self.begin_in_workspace_with_role(
            task,
            provider,
            requested_model,
            budgets,
            workspace,
            Role::Worker,
            false,
            session,
        )
    }

    /// Start a run under an explicit REX role. The role selects the
    /// assembled system prompt and the least-authority tool scope: an
    /// adversary run, for example, can read but never write.
    ///
    /// `plan_mode` parks the run at a plan-approval gate before any tool runs.
    /// The flag stays a plain parameter (rather than a builder) because this
    /// constructor mirrors the brief shape and every caller passes it
    /// explicitly. Scope: only the custody human path opts in; Ultra and the
    /// installed-agent paths pass `false` (their loops have their own
    /// gating).
    #[allow(clippy::too_many_arguments)]
    pub fn begin_in_workspace_with_role(
        &self,
        task: &str,
        provider: &str,
        requested_model: Option<&str>,
        budgets: Option<Budgets>,
        workspace: Option<PathBuf>,
        role: Role,
        plan_mode: bool,
        session: SessionMeta,
    ) -> Result<AgentSnapshot, String> {
        let opts = RunOptions {
            plan_mode,
            ..RunOptions::default()
        };
        self.begin_in_workspace_with_options(
            task,
            provider,
            requested_model,
            budgets,
            workspace,
            role,
            opts,
            session,
        )
    }

    /// [`Self::begin_in_workspace_with_role`] with every run switch,
    /// including the opt-in history summary.
    #[allow(clippy::too_many_arguments)]
    pub fn begin_in_workspace_with_options(
        &self,
        task: &str,
        provider: &str,
        requested_model: Option<&str>,
        budgets: Option<Budgets>,
        workspace: Option<PathBuf>,
        role: Role,
        opts: RunOptions,
        session: SessionMeta,
    ) -> Result<AgentSnapshot, String> {
        let RunOptions {
            plan_mode,
            summarize_history,
        } = opts;
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
            plan_mode,
            summarize_history,
            name: session.name,
            continued_from: session.continued_from,
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
                standing: Vec::new(),
                pending_question: None,
                answer: None,
                queued_answers: VecDeque::new(),
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
            workspace: Some(workspace.clone()),
            ..Checkpoint::default()
        };
        // The run is resumable from the moment it begins: a restart before
        // the first turn still finds a checkpoint.
        write_json(&state_dir.join("checkpoint.json"), &checkpoint)?;
        // External MCP servers connect once per run; the catalog is
        // appended to the prompt so the model can discover `mcp_call`.
        let mcp = setup_mcp(&workspace);
        let system_prompt = format!("{}{}", system_prompt, mcp.catalog);
        let loop_ctx = LoopCtx {
            system_prompt,
            brief,
            handle: handle.clone(),
            service: self.service.clone(),
            search: self.search.clone(),
            state_dir,
            workspace,
            checkpoint,
            mcp_tools: mcp.tools,
            mcp_caller: mcp.caller,
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

    /// Trusted UI decision: approve the pending command and allow the exact
    /// same command line for the rest of this run. Refused unless the
    /// pending call carries a standing key (bare, policy-clean commands
    /// only; never file writes). Like `decide`, the model has no route here.
    pub fn decide_always(&self, run_id: &str) -> Result<AgentSnapshot, String> {
        let runs = self.runs.lock().map_err(|_| "run registry poisoned")?;
        let handle = runs.get(run_id).ok_or("unknown run")?;
        {
            let mut state = handle.shared.lock().map_err(|_| "run state poisoned")?;
            if state.status != AgentStatus::AwaitingApproval {
                return Err("run is not waiting for a decision".into());
            }
            let Some(call) = state.pending_approval.clone() else {
                return Err("run is not waiting for a decision".into());
            };
            let Some(key) = call.standing_key.clone() else {
                return Err("this request can only be approved once".into());
            };
            grant_standing(&mut state.standing, key)?;
            state.decision = Some(true);
            push_locked(
                &mut state,
                AgentEvent::StandingApprovalGranted {
                    call_id: call.call_id.clone(),
                    command: call.summary.clone(),
                },
            );
            push_locked(
                &mut state,
                AgentEvent::ApprovalResolved {
                    call_id: call.call_id,
                    approved: true,
                },
            );
        }
        handle.cond.notify_all();
        Ok(self.snapshot_of(run_id, handle))
    }

    /// Trusted UI decision on a proposed plan. Only this path can release a
    /// plan-gated run from `AwaitingPlan`; the model has no route to it.
    /// Approval continues the run into the normal loop; denial ends it as
    /// `Denied` without any tool having executed.
    pub fn decide_plan(&self, run_id: &str, approved: bool) -> Result<AgentSnapshot, String> {
        let runs = self.runs.lock().map_err(|_| "run registry poisoned")?;
        let handle = runs.get(run_id).ok_or("unknown run")?;
        {
            let mut state = handle.shared.lock().map_err(|_| "run state poisoned")?;
            if state.status != AgentStatus::AwaitingPlan {
                return Err("run is not waiting for a plan decision".into());
            }
            state.decision = Some(approved);
            push_locked(&mut state, AgentEvent::PlanApprovalResolved { approved });
        }
        handle.cond.notify_all();
        Ok(self.snapshot_of(run_id, handle))
    }

    /// Trusted UI reply to an `ask_user` question. `None` declines, which
    /// tells the model to proceed on its own judgement. Blank text counts as
    /// declining; long text is capped at ANSWER_CHARS.
    pub fn answer(&self, run_id: &str, text: Option<&str>) -> Result<AgentSnapshot, String> {
        let runs = self.runs.lock().map_err(|_| "run registry poisoned")?;
        let handle = runs.get(run_id).ok_or("unknown run")?;
        {
            let mut state = handle.shared.lock().map_err(|_| "run state poisoned")?;
            if state.status != AgentStatus::AwaitingAnswer || state.pending_question.is_none() {
                return Err("run is not waiting for an answer".into());
            }
            let reply = text
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(|t| t.chars().take(ANSWER_CHARS).collect::<String>());
            let answered = reply.is_some();
            state.answer = Some(reply);
            let call_id = state
                .pending_question
                .as_ref()
                .map(|q| q.call_id.clone())
                .unwrap_or_default();
            push_locked(
                &mut state,
                AgentEvent::QuestionResolved { call_id, answered },
            );
        }
        handle.cond.notify_all();
        Ok(self.snapshot_of(run_id, handle))
    }

    /// Answer the open question and the rest of its batch in one go (the
    /// batch form). `answers[0]` answers the open question; later entries
    /// are used, in order, for the batch's remaining questions. Extra
    /// entries past the batch are ignored. `None` or blank declines one.
    pub fn answer_many(
        &self,
        run_id: &str,
        answers: &[Option<String>],
    ) -> Result<AgentSnapshot, String> {
        let Some((first, rest)) = answers.split_first() else {
            return Err("no answers given".into());
        };
        let runs = self.runs.lock().map_err(|_| "run registry poisoned")?;
        let handle = runs.get(run_id).ok_or("unknown run")?;
        {
            let mut state = handle.shared.lock().map_err(|_| "run state poisoned")?;
            let Some(q) = state.pending_question.clone() else {
                return Err("run is not waiting for an answer".into());
            };
            if state.status != AgentStatus::AwaitingAnswer {
                return Err("run is not waiting for an answer".into());
            }
            let clean = |a: &Option<String>| {
                a.as_deref()
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(|t| t.chars().take(ANSWER_CHARS).collect::<String>())
            };
            let left = q
                .batch_total
                .zip(q.batch_index)
                .map_or(0, |(t, i)| t.saturating_sub(i));
            state.queued_answers = rest.iter().take(left).map(clean).collect();
            let reply = clean(first);
            let answered = reply.is_some();
            state.answer = Some(reply);
            push_locked(
                &mut state,
                AgentEvent::QuestionResolved {
                    call_id: q.call_id,
                    answered,
                },
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

    /// Undo the newest file write a run made, from its on-disk journal.
    /// Refused while the run is active, and refused (changing nothing) when
    /// a touched file changed after the agent wrote it.
    pub fn undo_last_write(
        &self,
        run_id: &str,
    ) -> Result<rex_tools::journal::JournalEntry, String> {
        let (journal, workspace) = self.finished_run_journal(run_id)?;
        journal.undo_last(&workspace)
    }

    /// Undo every write after journal entry `seq` in one call (0 = the
    /// whole run), newest first. All or nothing: if any step is blocked by
    /// a later change to a file, no file is touched.
    pub fn undo_to_write(
        &self,
        run_id: &str,
        seq: u64,
    ) -> Result<Vec<rex_tools::journal::JournalEntry>, String> {
        let (journal, workspace) = self.finished_run_journal(run_id)?;
        journal.undo_to(&workspace, seq)
    }

    /// Rewind a finished run to just after the write made by tool call
    /// `call_id` (the id on its `ToolFinished` event), all or nothing.
    pub fn undo_after_call(
        &self,
        run_id: &str,
        call_id: &str,
    ) -> Result<Vec<rex_tools::journal::JournalEntry>, String> {
        let (journal, workspace) = self.finished_run_journal(run_id)?;
        let seq = journal
            .seq_of_call(call_id)
            .ok_or_else(|| format!("no journaled write for call {call_id}"))?;
        journal.undo_to(&workspace, seq)
    }

    /// The write journal and workspace of a run that is not active.
    fn finished_run_journal(
        &self,
        run_id: &str,
    ) -> Result<(rex_tools::journal::Journal, PathBuf), String> {
        {
            let runs = self.runs.lock().map_err(|_| "run registry poisoned")?;
            if let Some(handle) = runs.get(run_id) {
                if handle
                    .shared
                    .lock()
                    .map(|s| s.terminal.is_none())
                    .unwrap_or(false)
                {
                    return Err("run is still active; cancel or wait for it before undoing".into());
                }
            }
        }
        let run_dir = self.run_dir(run_id);
        let state_dir = run_dir.join("state");
        let checkpoint = read_json::<Checkpoint>(&state_dir.join("checkpoint.json"))
            .map_err(|_| "no checkpointed run with this id".to_string())?;
        let workspace = self.checkpoint_workspace(&run_dir, &checkpoint)?;
        let journal_dir = state_dir.join("journal");
        if !journal_dir.join("journal.jsonl").exists() {
            return Err("this run has no write journal".into());
        }
        let journal = rex_tools::journal::Journal::open(journal_dir).map_err(|e| e.to_string())?;
        Ok((journal, workspace))
    }

    /// The run's workspace from its checkpoint, re-validated under the
    /// runs root (a tampered checkpoint must not redirect a run or an undo).
    fn checkpoint_workspace(
        &self,
        run_dir: &Path,
        checkpoint: &Checkpoint,
    ) -> Result<PathBuf, String> {
        let runs_root_canon = self.runs_root.canonicalize().map_err(|e| e.to_string())?;
        let workspace = match checkpoint.workspace.clone() {
            Some(stored) => {
                let canon = stored
                    .canonicalize()
                    .map_err(|_| "checkpointed workspace is missing".to_string())?;
                if !canon.starts_with(&runs_root_canon) {
                    return Err("checkpointed workspace escaped the runs root".into());
                }
                canon
            }
            None => run_dir.join("workspace"),
        };
        if !workspace.is_dir() {
            return Err("checkpointed workspace is missing".into());
        }
        Ok(workspace)
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
        // The workspace is restored from the checkpoint when present
        // (explicit-workspace runs, e.g. custody); legacy checkpoints fall
        // back to the default `<run_dir>/workspace` layout. A stored path is
        // re-validated under the runs root: a tampered checkpoint must not
        // redirect the resumed run elsewhere.
        let workspace = self.checkpoint_workspace(&run_dir, &checkpoint)?;
        let handle = Arc::new(RunHandle {
            shared: Mutex::new(RunShared {
                status: AgentStatus::Running,
                terminal: None,
                plan,
                events: VecDeque::new(),
                pending_approval: None,
                decision: None,
                standing: Vec::new(),
                pending_question: None,
                answer: None,
                queued_answers: VecDeque::new(),
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
        // External MCP servers connect once per run; the catalog is
        // appended to the prompt so the model can discover `mcp_call`.
        let mcp = setup_mcp(&workspace);
        let system_prompt = format!("{}{}", system_prompt, mcp.catalog);
        let loop_ctx = LoopCtx {
            system_prompt,
            brief,
            handle: handle.clone(),
            service: self.service.clone(),
            search: self.search.clone(),
            state_dir,
            workspace,
            checkpoint,
            mcp_tools: mcp.tools,
            mcp_caller: mcp.caller,
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

use rex_mcp_ext::{ExtTool, McpExtClient};

/// Live external MCP sessions for one run. Sessions spawn at run start so
/// the model can call them through `mcp_call`; a broken server is skipped
/// with a stderr warning instead of failing the whole run.
struct McpPool {
    clients: Mutex<HashMap<String, McpExtClient>>,
}

impl rex_tools::McpCaller for McpPool {
    fn call(&self, server: &str, name: &str, arguments: &Value) -> Result<Value, String> {
        let mut clients = self
            .clients
            .lock()
            .map_err(|_| "mcp pool lock poisoned".to_string())?;
        let client = clients
            .get_mut(server)
            .ok_or_else(|| format!("unknown MCP server {server:?}"))?;
        client.call_tool(name, arguments.clone())
    }
}

struct McpSetup {
    caller: Option<Arc<McpPool>>,
    tools: Vec<ExtTool>,
    catalog: String,
}

/// Connect the run's external MCP servers, if any are configured. The model
/// learns the catalog from the system prompt and calls tools through the
/// single `mcp_call` agent tool (approval-gated like any Execute risk).
fn setup_mcp(workspace: &Path) -> McpSetup {
    let empty = McpSetup {
        caller: None,
        tools: Vec::new(),
        catalog: String::new(),
    };
    // No config at all is normal: external tools are simply absent. A
    // config file that exists but fails to parse is worth a warning.
    let state_dir = rex_mcp_ext::state_dir();
    let (_cfg_path, servers) = match rex_mcp_ext::load_config(Some(workspace), &state_dir) {
        Ok(found) => found,
        Err(e) => {
            if workspace.join(".rex").join("mcp.json").exists()
                || state_dir.join("mcp.json").exists()
            {
                eprintln!("rex: MCP config invalid ({e}); external tools disabled");
            }
            return empty;
        }
    };
    let mut clients = HashMap::new();
    let mut tools = Vec::new();
    for server in servers {
        match McpExtClient::spawn(&server) {
            Ok(mut client) => match client.list_tools() {
                Ok(ts) => {
                    tools.extend(ts);
                    clients.insert(server.name.clone(), client);
                }
                Err(e) => eprintln!(
                    "rex: MCP server {:?} failed tools/list ({e}); skipping",
                    server.name
                ),
            },
            Err(e) => eprintln!(
                "rex: MCP server {:?} failed to start ({e}); skipping",
                server.name
            ),
        }
    }
    if clients.is_empty() {
        return empty;
    }
    let mut catalog = String::from(
        "\n\nExternal MCP tools (third-party servers; every call needs approval):\n\
         Call them with the `mcp_call` tool, passing `server`, `name`, and `arguments`.\n",
    );
    for t in &tools {
        catalog.push_str(&format!(
            "- server {:?}: tool {:?} — {}\n",
            t.server, t.name, t.description
        ));
    }
    McpSetup {
        caller: Some(Arc::new(McpPool {
            clients: Mutex::new(clients),
        })),
        tools,
        catalog,
    }
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
    mcp_tools: Vec<ExtTool>,
    mcp_caller: Option<Arc<McpPool>>,
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
        Ok(t) => {
            // Journal writes so a finished run can be undone; a journal
            // failure leaves the run working, just without undo.
            let t = ToolRuntime::new(&ctx.workspace)
                .and_then(|j| j.with_journal(ctx.state_dir.join("journal")))
                .unwrap_or(t);
            match &ctx.mcp_caller {
                Some(caller) => t.with_mcp_caller(caller.clone()),
                None => t,
            }
        }
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

    // ---- plan mode: propose the plan, wait for trusted approval ---------
    // No tool executes before approval. Denial ends the run as Denied with
    // the plan preserved in the snapshot for inspection. A resumed run whose
    // checkpoint already records approval skips the gate; anything else
    // re-parks (approval is never assumed).
    if ctx.brief.plan_mode && !cp.plan_approved {
        match plan_gate(
            &ctx,
            &tools,
            &mut ledger,
            protocol,
            &base_url,
            &key,
            &model,
            started,
            &mut cp,
        ) {
            PlanGate::Approved => {}
            PlanGate::Denied => {
                // Test-only: a simulated death while parked must not write
                // a terminal state.
                #[cfg(test)]
                if silent_halt(&ctx, &cp) {
                    return;
                }
                finish(&ctx, &mut ledger, TerminalReason::Denied, started, &cp);
                return;
            }
            PlanGate::Failed(reason) => {
                #[cfg(test)]
                if silent_halt(&ctx, &cp) {
                    return;
                }
                finish(&ctx, &mut ledger, reason, started, &cp);
                return;
            }
        }
    }

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

        // ---- opt-in summary of history dropped from working memory -------
        if ctx.brief.summarize_history {
            summarize_history_step(
                &ctx,
                &mut ledger,
                protocol,
                &base_url,
                &key,
                &model,
                budgets,
                &mut cp,
            );
        } else {
            cp.history_pending.clear();
        }

        // ---- dynamic context build ---------------------------------------
        let project = rex_prompt::project::load(&ctx.workspace);
        let user_rules = rex_prompt::project::load_user();
        let state_msg = build_state_message(
            &ctx.brief,
            &cp,
            budgets,
            current_plan(&ctx.handle),
            project.as_ref(),
            user_rules.as_ref(),
        );
        let request_body = build_request(
            protocol,
            &model,
            &ctx.system_prompt,
            &state_msg,
            cp.last_pair.as_ref(),
            &ctx.mcp_tools,
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
        for note in &decoded.repairs {
            RunHandle::push_event(
                &ctx.handle.shared,
                AgentEvent::Info {
                    message: format!("repaired call: {note}"),
                },
            );
        }

        // ---- execute the turn's calls ------------------------------------
        let out = execute_turn(
            &ctx,
            &tools,
            &mut ledger,
            &ProviderLink {
                protocol,
                base_url: &base_url,
                key: &key,
                model: &model,
            },
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
    project: Option<&rex_prompt::project::ProjectInstructions>,
    user_rules: Option<&rex_prompt::project::ProjectInstructions>,
) -> String {
    let digest: Vec<&DigestEntry> = cp.digest.iter().rev().take(DIGEST_WINDOW).collect();
    let digest: Vec<&DigestEntry> = digest.into_iter().rev().collect();
    let older = crate::memory::earlier(&cp.observations, cp.step);
    let earlier_observations = if older.is_empty() {
        Value::Null
    } else {
        json!({"note": crate::memory::MEMORY_NOTE, "items": older})
    };
    let payload = json!({
        "contract": {
            "role": "You are REX, an autonomous agent inside a bounded workspace. Work the plan until the task is verifiably done.",
            "rules": [
                "Maintain the todo plan with update_plan: mark the active step in_progress, mark steps done only when actually done.",
                "Use read_file/search_files/web_search to ground yourself before writing. Writes and commands pause for trusted human approval; a denial is information - replan, never retry the identical denied call.",
                "Use ask_user only when a choice blocks progress and cannot be settled from the workspace or the brief; otherwise decide, and state the assumption in your completion summary.",
                "When the plan is fully done, call complete_task. Gates then verify your work; false completion claims fail the gates.",
                "Evidence from older turns stays in the run ledger; the digest below carries the recent truth.",
            ],
            "tools": ["update_plan", "read_file", "create_file", "edit_file", "search_files", "glob_files", "apply_patch", "run_command", "web_search", "web_fetch", "explore", "ask_user", "complete_task"],
        },
        "brief": {"task": brief.task, "created_at_ms": brief.created_at_ms},
        "project_instructions": project.map(|p| json!({
            "source": p.source,
            "sha256": p.sha256,
            "truncated": p.truncated,
            "precedence": rex_prompt::project::PRECEDENCE,
            "text": p.text,
        })),
        "user_instructions": user_rules.map(|u| json!({
            "source": u.source,
            "sha256": u.sha256,
            "truncated": u.truncated,
            "precedence": rex_prompt::project::USER_PRECEDENCE,
            "text": u.text,
        })),
        "budget": {
            "step": cp.step, "max_steps": budgets.max_steps,
            "tool_calls": cp.tool_calls, "max_tool_calls": budgets.max_tool_calls,
            "tokens_used": cp.tokens_used, "max_tokens": budgets.max_tokens,
            "elapsed_ms": cp.elapsed_base_ms, "max_wall_ms": budgets.max_wall_ms,
        },
        "plan": plan,
        "recent_turns": digest,
        "earlier_observations": earlier_observations,
        "history_summary": cp.history_summary.as_ref().map(|t| json!({"note": crate::history::SUMMARY_NOTE, "text": t})),
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
            "Replace text in a workspace file; exact match first, then unique indentation/whitespace-tolerant match.",
            true,
            true,
        ),
        ToolSpec::new(
            "search_files",
            "Search file contents (literal or regex, optional include glob); skips build output and .gitignore.",
            false,
            false,
        ),
        ToolSpec::new(
            "apply_patch",
            "Apply an atomic multi-file patch (add/update/move/delete); all hunks apply or nothing is written.",
            true,
            true,
        ),
        ToolSpec::new(
            "glob_files",
            "Find workspace files by glob pattern, newest first.",
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
            "web_fetch",
            "Read one public web page as text (robots-aware, private addresses refused).",
            false,
            false,
        ),
        ToolSpec::new(
            "explore",
            "Delegate a read-only question to an explorer sub-agent (workspace only) or a research sub-agent (workspace plus web_fetch); returns one findings report.",
            false,
            false,
        ),
        ToolSpec::new(
            "ask_user",
            "Ask the user one blocking question, or up to 3 related ones at once, through the trusted UI; returns their answers or tells you to decide.",
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
    mcp_tools: &[ExtTool],
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
            json!({"contents": contents, "tools": gemini_tool_definitions_with(mcp_tools),
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
                "messages":messages,"tools":anthropic_tool_definitions_with(mcp_tools)})
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
            json!({"model":model,"messages":messages,"tools":openai_tool_definitions_with(mcp_tools),
                "tool_choice":"auto","temperature":0.2,"max_tokens":8192})
            .to_string()
        }
    }
}

fn gemini_tool_definitions() -> Value {
    json!([{"functionDeclarations":[
        {"name":"update_plan","description":"Replace the visible todo plan. Keep 2-8 items; one in_progress at a time.","parameters":{"type":"OBJECT","properties":{"items":{"type":"ARRAY","items":{"type":"OBJECT","properties":{"id":{"type":"STRING"},"title":{"type":"STRING"},"status":{"type":"STRING","enum":["pending","in_progress","done","blocked"]},"note":{"type":"STRING"}},"required":["id","title","status"]}}},"required":["items"]}},
        {"name":"read_file","description":"Read a file inside the selected workspace. Small files return exact text; large files, or any call with offset/limit, return numbered lines with a footer saying where to continue. A directory path returns its listing.","parameters":{"type":"OBJECT","properties":{"path":{"type":"STRING"},"offset":{"type":"INTEGER","description":"1-based first line"},"limit":{"type":"INTEGER","description":"max lines (default 2000)"}},"required":["path"]}},
        {"name":"create_file","description":"Create a file inside the workspace. Requires trusted approval.","parameters":{"type":"OBJECT","properties":{"path":{"type":"STRING"},"content":{"type":"STRING"},"overwrite":{"type":"BOOLEAN"}},"required":["path","content","overwrite"]}},
        {"name":"edit_file","description":"Replace text in a workspace file. Copy `expected` from the file; exact match is tried first, then a unique indentation/whitespace-tolerant match, and a miss reports the closest region. Requires trusted approval.","parameters":{"type":"OBJECT","properties":{"path":{"type":"STRING"},"expected":{"type":"STRING"},"replacement":{"type":"STRING"},"replace_all":{"type":"BOOLEAN"}},"required":["path","expected","replacement","replace_all"]}},
        {"name":"search_files","description":"Search file contents inside the workspace. Literal case-insensitive by default; set regex=true for a regular expression. Skips .git, build output, node_modules and .gitignore'd paths.","parameters":{"type":"OBJECT","properties":{"query":{"type":"STRING"},"path":{"type":"STRING"},"max_results":{"type":"INTEGER"},"regex":{"type":"BOOLEAN"},"include":{"type":"STRING","description":"glob such as *.rs or src/**/*.{ts,tsx}"}},"required":["query"]}},
        {"name":"apply_patch","description":"Change several files at once, all-or-nothing. Format: '*** Begin Patch' then per file '*** Add File: path' (lines prefixed '+'), '*** Delete File: path', or '*** Update File: path' (optional '*** Move to: path') with '@@' hunks of ' ' context, '-' removed and '+' added lines; end with '*** End Patch'. Include enough context lines to make each hunk unique. Requires trusted approval.","parameters":{"type":"OBJECT","properties":{"patch":{"type":"STRING"}},"required":["patch"]}},
        {"name":"glob_files","description":"Find workspace files by glob pattern (e.g. **/*.rs), newest first. Skips build output and .gitignore'd paths.","parameters":{"type":"OBJECT","properties":{"pattern":{"type":"STRING"},"path":{"type":"STRING"},"max_results":{"type":"INTEGER"}},"required":["pattern"]}},
        {"name":"run_command","description":"Run an allowed command inside the workspace. Requires trusted approval.","parameters":{"type":"OBJECT","properties":{"argv":{"type":"ARRAY","items":{"type":"STRING"}},"cwd":{"type":"STRING"},"timeout_ms":{"type":"INTEGER"}},"required":["argv"]}},
        {"name":"web_search","description":"Search the public web for grounded facts. Returns ranked results with URLs. With the keyless engine, pass `sites`: URLs or domains likely to hold the answer (it crawls outward from them).","parameters":{"type":"OBJECT","properties":{"query":{"type":"STRING"},"max_results":{"type":"INTEGER"},"sites":{"type":"ARRAY","items":{"type":"STRING"},"description":"seed URLs or domains, e.g. docs.rs"}},"required":["query"]}},
        {"name":"web_fetch","description":"Read one public web page (http/https), e.g. docs or an issue you already have the URL for. HTML comes back as Markdown (headings, lists, absolute links, code blocks); format=text gives flat text. Robots.txt, private addresses and data-carrying URLs are refused. Long pages return next_offset; call again with offset to continue.","parameters":{"type":"OBJECT","properties":{"url":{"type":"STRING"},"offset":{"type":"INTEGER","description":"character offset to continue from"},"format":{"type":"STRING","enum":["markdown","text"],"description":"markdown (default) or text"}},"required":["url"]}},
        {"name":"explore","description":"Delegate a focused question about the workspace (e.g. where something is defined, how a module works) to an explorer sub-agent that can only read, search and glob, or (kind edit) one self-contained change to a writing sub-agent. Returns one findings report with file:line references; its raw tool output stays out of your context.","parameters":{"type":"OBJECT","properties":{"task":{"type":"STRING","description":"the question, with any paths or names you already know"},"tasks":{"type":"ARRAY","items":{"type":"STRING"},"description":"instead of task: up to 3 independent questions, explored in parallel (budget is split between them)"},"kind":{"type":"STRING","enum":["explore","research","edit"],"description":"explore (default): workspace only. research: workspace plus web_fetch of public pages, for questions that need docs or issue pages. edit: one self-contained change per task (up to 3 in parallel, each on different files); the child can write and run commands, each needing the user's approval as usual, and reports files changed"}}}},
        {"name":"ask_user","description":"Ask the user when a decision blocks progress and cannot be settled from the workspace or brief (e.g. which of two conflicting requirements wins). Pass one question, or up to 3 related questions at once in `questions` so the user answers them together. Give up to 4 short choices per question, best first; the user may also answer freely. At most 3 questions per run. Anything declined or unanswered in time means proceed on your own judgement.","parameters":{"type":"OBJECT","properties":{"question":{"type":"STRING"},"choices":{"type":"ARRAY","items":{"type":"STRING"}},"questions":{"type":"ARRAY","items":{"type":"OBJECT","properties":{"question":{"type":"STRING"},"choices":{"type":"ARRAY","items":{"type":"STRING"}}},"required":["question"]}}}}},
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

/// The single agent tool that reaches external MCP servers. Declared in the
/// Gemini function-declaration shape; the protocol converters below adapt it.
fn mcp_call_declaration() -> Value {
    json!({
        "name": "mcp_call",
        "description": "Call a tool on a connected external MCP server (see the prompt catalog for servers and tools). Requires trusted approval.",
        "parameters": {
            "type": "OBJECT",
            "properties": {
                "server": {"type": "STRING", "description": "MCP server name from the catalog"},
                "name": {"type": "STRING", "description": "Tool name on that server"},
                "arguments": {"type": "OBJECT", "description": "Tool arguments object"}
            },
            "required": ["server", "name"]
        }
    })
}

fn gemini_tool_definitions_with(mcp_tools: &[ExtTool]) -> Value {
    let mut defs = gemini_tool_definitions();
    if !mcp_tools.is_empty() {
        if let Some(decls) = defs[0]["functionDeclarations"].as_array_mut() {
            decls.push(mcp_call_declaration());
        }
    }
    defs
}

fn anthropic_tool_definitions_with(mcp_tools: &[ExtTool]) -> Value {
    let mut defs = anthropic_tool_definitions();
    if !mcp_tools.is_empty() {
        if let Some(arr) = defs.as_array_mut() {
            let d = mcp_call_declaration();
            arr.push(json!({
                "name": d["name"],
                "description": d["description"],
                "input_schema": lowercase_schema(d["parameters"].clone())
            }));
        }
    }
    defs
}

fn openai_tool_definitions_with(mcp_tools: &[ExtTool]) -> Value {
    let mut defs = openai_tool_definitions();
    if !mcp_tools.is_empty() {
        if let Some(arr) = defs.as_array_mut() {
            let d = mcp_call_declaration();
            arr.push(json!({"type": "function", "function": {
                "name": d["name"],
                "description": d["description"],
                "parameters": lowercase_schema(d["parameters"].clone())
            }}));
        }
    }
    defs
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
    /// Slips fixed before decoding (tool name or argument types), one note
    /// each, so the run's events show what was changed.
    repairs: Vec<String>,
}

fn decode_gemini_calls(response: &str) -> Result<DecodedCalls, String> {
    let mut repairs = Vec::new();
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
        let fixed_name = canonical_call_name(name);
        note_name_repair(name, &fixed_name, &mut repairs);
        let name = fixed_name.as_str();
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
                let sites = string_list(&args, "sites");
                calls.push((
                    AgentCall::WebSearch {
                        id,
                        query,
                        max_results,
                        sites,
                    },
                    raw,
                ));
            }
            "web_fetch" => match args.get("url").and_then(Value::as_str) {
                Some(url) => calls.push((
                    AgentCall::WebFetch {
                        id,
                        url: url.into(),
                        offset: args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize,
                        text: args.get("format").and_then(Value::as_str) == Some("text"),
                    },
                    raw,
                )),
                None => calls.push((
                    AgentCall::BadCall {
                        name: "web_fetch".into(),
                        id,
                        error: "web_fetch missing url".into(),
                    },
                    raw,
                )),
            },
            "ask_user" => match parse_ask_user(&args) {
                Ok(questions) => calls.push((AgentCall::AskUser { id, questions }, raw)),
                Err(error) => calls.push((
                    AgentCall::BadCall {
                        name: "ask_user".into(),
                        id,
                        error,
                    },
                    raw,
                )),
            },
            "explore" => match explore::parse_explore_tasks(&args)
                .and_then(|t| explore::parse_explore_kind(&args).map(|k| (t, k)))
            {
                Ok((tasks, kind)) => calls.push((AgentCall::Explore { id, tasks, kind }, raw)),
                Err(error) => calls.push((
                    AgentCall::BadCall {
                        name: "explore".into(),
                        id,
                        error,
                    },
                    raw,
                )),
            },
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
                note_arg_repair(
                    name,
                    rex_tools::repair_tool_args(name, &mut object),
                    &mut repairs,
                );
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
        repairs,
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

/// Calls the loop handles itself rather than through [`ToolRequest`].
const AGENT_CALL_NAMES: &[&str] = &[
    "update_plan",
    "web_search",
    "web_fetch",
    "ask_user",
    "explore",
    "complete_task",
];

/// Map a slightly-off tool name (`Read_File`, `read-file`, ` explore `) to the
/// known name it clearly means. Unknown names come back unchanged so decoding
/// reports them as usual.
fn canonical_call_name(name: &str) -> String {
    let fixed = name.trim().to_ascii_lowercase().replace('-', "_");
    if fixed != name
        && (AGENT_CALL_NAMES.contains(&fixed.as_str())
            || rex_tools::TOOL_NAMES.contains(&fixed.as_str()))
    {
        fixed
    } else {
        name.to_string()
    }
}

fn note_name_repair(original: &str, fixed: &str, repairs: &mut Vec<String>) {
    if original != fixed {
        repairs.push(format!("tool name '{original}' read as '{fixed}'"));
    }
}

fn note_arg_repair(tool: &str, fields: Vec<String>, repairs: &mut Vec<String>) {
    if !fields.is_empty() {
        repairs.push(format!(
            "{tool}: fixed argument types for {}",
            fields.join(", ")
        ));
    }
}

fn decode_named_call(
    name: &str,
    id: String,
    args: Value,
    raw: Value,
    repairs: &mut Vec<String>,
) -> (AgentCall, Option<Value>) {
    let fixed_name = canonical_call_name(name);
    note_name_repair(name, &fixed_name, repairs);
    let name = fixed_name.as_str();
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
                    sites: string_list(&args, "sites"),
                },
                Some(raw),
            ),
            None => bad("web_search missing query".into()),
        },
        "web_fetch" => match args.get("url").and_then(Value::as_str) {
            Some(url) => (
                AgentCall::WebFetch {
                    id,
                    url: url.into(),
                    offset: args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize,
                    text: args.get("format").and_then(Value::as_str) == Some("text"),
                },
                Some(raw),
            ),
            None => bad("web_fetch missing url".into()),
        },
        "ask_user" => match parse_ask_user(&args) {
            Ok(questions) => (AgentCall::AskUser { id, questions }, Some(raw)),
            Err(e) => bad(e),
        },
        "explore" => match explore::parse_explore_tasks(&args)
            .and_then(|t| explore::parse_explore_kind(&args).map(|k| (t, k)))
        {
            Ok((tasks, kind)) => (AgentCall::Explore { id, tasks, kind }, Some(raw)),
            Err(e) => bad(e),
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
            note_arg_repair(
                name,
                rex_tools::repair_tool_args(name, &mut object),
                repairs,
            );
            match serde_json::from_value::<ToolRequest>(object) {
                Ok(request) => (AgentCall::Tool { id, request }, Some(raw)),
                Err(e) => bad(format!("invalid {name} request: {e}")),
            }
        }
    }
}

fn decode_anthropic_calls(response: &str) -> Result<DecodedCalls, String> {
    let mut repairs = Vec::new();
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
                    &mut repairs,
                ));
            }
            _ => {}
        }
    }
    Ok(DecodedCalls {
        texts,
        calls,
        thought_signature: None,
        repairs,
    })
}

fn decode_openai_calls(response: &str) -> Result<DecodedCalls, String> {
    let mut repairs = Vec::new();
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
        calls.push(decode_named_call(
            name,
            id,
            args,
            call.clone(),
            &mut repairs,
        ));
    }
    Ok(DecodedCalls {
        texts,
        calls,
        thought_signature: None,
        repairs,
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
    link: &ProviderLink<'_>,
    calls: Vec<(AgentCall, Option<Value>)>,
    thought_signature: Option<String>,
    cp: &mut Checkpoint,
    started: Instant,
) -> DriveOut {
    let protocol = link.protocol;
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
                sites,
            } => {
                let seeds = search_seeds(&query, &sites);
                let (ok, content) = match &ctx.search {
                    Some(router) if router.active() == SearchProvider::Rex && seeds.is_empty() => {
                        (false, NO_SEED_GUIDANCE.to_string())
                    }
                    Some(router) => {
                        let request: SearchRequest = match serde_json::from_value(json!({
                            "query": query, "seeds": seeds, "max_results": max_results
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
                                        "excerpt": e.excerpt.as_deref().map(|x| &x[..rex_tools::floor_boundary(x, 300)]),
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
                    l.append(
                        "web_search",
                        json!({"query": query, "seeds": seeds, "ok": ok}),
                    );
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
            AgentCall::AskUser { id, questions } => {
                let allowed = ctx
                    .brief
                    .allowed_tools
                    .as_ref()
                    .is_none_or(|a| a.iter().any(|t| t == "ask_user"));
                let single = questions.len() == 1;
                let mut ok = true;
                let mut asked_any = false;
                let mut answers: Vec<Value> = Vec::new();
                let mut single_content: Option<String> = None;
                if !allowed {
                    ok = false;
                    single_content = Some("tool ask_user is not enabled for this run's role; decide yourself and state the assumption".to_string());
                } else if cp.questions_asked >= MAX_QUESTIONS_PER_RUN {
                    ok = false;
                    single_content = Some(format!("question limit reached ({MAX_QUESTIONS_PER_RUN} per run); decide yourself and state the assumption in your completion summary"));
                } else {
                    for (n, (question, choices)) in questions.iter().enumerate() {
                        if cp.questions_asked >= MAX_QUESTIONS_PER_RUN {
                            answers.push(json!({"question": question, "status": "not_asked", "answer": null}));
                            continue;
                        }
                        cp.questions_asked += 1;
                        asked_any = true;
                        let pending = PendingQuestion {
                            call_id: match (id.is_empty(), single) {
                                (true, _) => format!("ask-{}", cp.questions_asked),
                                (false, true) => id.clone(),
                                (false, false) => format!("{id}#{}", n + 1),
                            },
                            question: question.clone(),
                            choices: choices.clone(),
                            batch_index: (!single).then_some(n + 1),
                            batch_total: (!single).then_some(questions.len()),
                            batch: if single {
                                Vec::new()
                            } else {
                                questions
                                    .iter()
                                    .map(|(q, c)| BatchQuestion {
                                        question: q.clone(),
                                        choices: c.clone(),
                                    })
                                    .collect()
                            },
                        };
                        // Parked runs are resumable: persist before blocking.
                        let _ = write_json(&ctx.state_dir.join("checkpoint.json"), &*cp);
                        let (status, answer) = match wait_for_answer(ctx, &pending) {
                            UserAnswer::Text(text) => ("answered", Some(text)),
                            UserAnswer::Declined => ("declined", None),
                            UserAnswer::Timeout => ("timeout", None),
                            UserAnswer::Cancelled => {
                                out.fatal = Some(TerminalReason::Cancelled);
                                return out;
                            }
                        };
                        answers.push(
                            json!({"question": question, "status": status, "answer": answer}),
                        );
                    }
                }
                if let Ok(mut st) = ctx.handle.shared.lock() {
                    st.queued_answers.clear();
                }
                let content = match single_content {
                    Some(c) => c,
                    None if single => match answers[0]["status"].as_str() {
                        Some("answered") => json!({"user_answer": answers[0]["answer"]}).to_string(),
                        Some("declined") => "The user declined to answer. Proceed on your own judgement and state the assumption in your completion summary.".to_string(),
                        _ => "No answer arrived in time. Proceed on your own judgement and state the assumption in your completion summary.".to_string(),
                    },
                    None => json!({
                        "answers": answers,
                        "note": format!("For any question not answered (declined, timeout, or not_asked because of the {MAX_QUESTIONS_PER_RUN}-per-run limit), proceed on your own judgement and state the assumption in your completion summary."),
                    })
                    .to_string(),
                };
                let summary: String = questions
                    .iter()
                    .map(|(q, _)| q.as_str())
                    .collect::<Vec<_>>()
                    .join(" | ")
                    .chars()
                    .take(160)
                    .collect();
                let questions_json: Vec<Value> = questions
                    .iter()
                    .map(|(q, c)| json!({"question": q, "choices": c}))
                    .collect();
                if let Some(l) = ledger.as_mut() {
                    l.append(
                        "ask_user",
                        json!({"questions": questions_json, "asked": asked_any, "ok": ok, "reply": content}),
                    );
                }
                out.digest_actions.push(DigestAction {
                    tool: "ask_user".into(),
                    ok,
                    target: Some(summary),
                    error_kind: (!ok).then(|| "question_refused".to_string()),
                });
                let args = if single {
                    json!({"question": questions[0].0, "choices": questions[0].1})
                } else {
                    json!({"questions": questions_json})
                };
                push_model_part(
                    &mut model_parts,
                    raw,
                    json!({"functionCall":{"name":"ask_user","args": args}}),
                );
                response_parts.push(function_response(protocol, "ask_user", &id, ok, &content));
            }
            AgentCall::WebFetch {
                id,
                url,
                offset,
                text,
            } => {
                let allowed = ctx
                    .brief
                    .allowed_tools
                    .as_ref()
                    .is_none_or(|a| a.iter().any(|t| t == "web_fetch"));
                let (ok, content) = if !allowed {
                    (
                        false,
                        "tool web_fetch is not enabled for this run's role; use only the tools listed in your tool contract".to_string(),
                    )
                } else {
                    match fetch::vet_url(&url) {
                        Err(reason) => (false, format!("refused: {reason}")),
                        Ok(parsed) => {
                            let resp = fetch::fetch(&parsed, text);
                            fetch::render(&resp, offset)
                        }
                    }
                };
                if let Some(l) = ledger.as_mut() {
                    l.append("web_fetch", json!({"url": url, "offset": offset, "ok": ok}));
                }
                crate::memory::record_spill(
                    &mut cp.observations,
                    &mut cp.history_pending,
                    crate::memory::Observation::new(
                        cp.step,
                        "web_fetch",
                        Some(url.clone()),
                        ok,
                        &content,
                    ),
                );
                out.progress |= ok;
                out.digest_actions.push(DigestAction {
                    tool: "web_fetch".into(),
                    ok,
                    target: Some(url.chars().take(160).collect()),
                    error_kind: (!ok).then(|| "fetch_failed".to_string()),
                });
                push_model_part(
                    &mut model_parts,
                    raw,
                    json!({"functionCall":{"name":"web_fetch","args":{"url": url, "offset": offset, "format": if text { "text" } else { "markdown" }}}}),
                );
                response_parts.push(function_response(protocol, "web_fetch", &id, ok, &content));
            }
            AgentCall::Explore { id, tasks, kind } => {
                let mut call_args = if tasks.len() == 1 {
                    json!({"task": tasks[0]})
                } else {
                    json!({"tasks": tasks})
                };
                if kind != explore::ExplorerKind::Explore {
                    call_args["kind"] = json!(kind.label());
                }
                if let Some(allowed) = &ctx.brief.allowed_tools {
                    // A research child fetches web pages, so it needs the
                    // parent's own web_fetch permission as well.
                    let needs_fetch = kind == explore::ExplorerKind::Research;
                    // An edit child writes, so a read-only role (which may
                    // still explore) cannot start one.
                    let needs_write = kind == explore::ExplorerKind::Edit;
                    let can_write = allowed.iter().any(|t| {
                        matches!(
                            t.as_str(),
                            "create_file" | "edit_file" | "apply_patch" | "run_command"
                        )
                    });
                    if !allowed.iter().any(|t| t == "explore")
                        || (needs_fetch && !allowed.iter().any(|t| t == "web_fetch"))
                        || (needs_write && !can_write)
                    {
                        out.digest_actions.push(DigestAction {
                            tool: "explore".into(),
                            ok: false,
                            target: None,
                            error_kind: Some("out_of_scope".into()),
                        });
                        push_model_part(
                            &mut model_parts,
                            raw,
                            json!({"functionCall":{"name":"explore","args": call_args}}),
                        );
                        response_parts.push(function_response(
                            protocol,
                            "explore",
                            &id,
                            false,
                            "tool explore is not enabled for this run's role; use only the tools listed in your tool contract",
                        ));
                        continue;
                    }
                }
                // The parent's remaining budgets are split evenly across the
                // children, so a batch never spends more than one explorer
                // could.
                let budgets = ctx.brief.budgets;
                let n = tasks.len().max(1);
                let limits = ExploreLimits {
                    max_turns: explore::EXPLORE_MAX_TURNS
                        .min(budgets.max_steps.saturating_sub(cp.step).max(1)),
                    max_tool_calls: explore::EXPLORE_MAX_TOOL_CALLS
                        .min(budgets.max_tool_calls.saturating_sub(cp.tool_calls))
                        / n,
                    max_tokens: budgets.max_tokens.saturating_sub(cp.tokens_used) / n as u64,
                    kind,
                };
                for task in &tasks {
                    let preview: String = task.chars().take(120).collect();
                    RunHandle::push_event(
                        &ctx.handle.shared,
                        AgentEvent::Info {
                            message: format!("{} sub-agent started: {preview}", kind.label()),
                        },
                    );
                }
                let transport = ctx.service.transport();
                let allowed = ctx.brief.allowed_tools.as_deref();
                // An edit child's writes and commands wait on the same trusted
                // UI decision as the parent's own calls. There is one approval
                // slot per run, so parallel children take turns at it.
                let approval_turn = std::sync::Mutex::new(());
                let run_handle: &RunHandle = &ctx.handle;
                let approve = |p: &PreparedCall| {
                    let _turn = approval_turn.lock().unwrap_or_else(|e| e.into_inner());
                    if run_handle.cancel.load(Ordering::SeqCst) {
                        return explore::ChildApproval::Stop;
                    }
                    match wait_for_decision_on(run_handle, p) {
                        Decision::Approved => explore::ChildApproval::Approved,
                        Decision::Denied => explore::ChildApproval::Denied,
                        Decision::Timeout | Decision::Cancelled => explore::ChildApproval::Stop,
                    }
                };
                // paths each edit child has written, so two children in one
                // batch never change the same file
                let claims = std::sync::Mutex::new(std::collections::HashMap::new());
                let outcomes: Vec<ExploreOutcome> = if tasks.len() == 1 {
                    let gate = explore::ChildGate {
                        allowed,
                        approve: &approve,
                        claims: &claims,
                        index: 0,
                    };
                    vec![run_explore(
                        transport,
                        link,
                        &ctx.handle,
                        tools,
                        &tasks[0],
                        limits,
                        &gate,
                    )]
                } else {
                    std::thread::scope(|scope| {
                        let workers: Vec<_> = tasks
                            .iter()
                            .enumerate()
                            .map(|(index, task)| {
                                let handle = &ctx.handle;
                                let (approve, claims) = (&approve, &claims);
                                scope.spawn(move || {
                                    let gate = explore::ChildGate {
                                        allowed,
                                        approve,
                                        claims,
                                        index,
                                    };
                                    run_explore(transport, link, handle, tools, task, limits, &gate)
                                })
                            })
                            .collect();
                        workers
                            .into_iter()
                            .map(|w| {
                                w.join().unwrap_or_else(|_| ExploreOutcome {
                                    error: Some("explorer thread panicked".into()),
                                    ..ExploreOutcome::default()
                                })
                            })
                            .collect()
                    })
                };
                let mut cancelled = false;
                let mut entries: Vec<Value> = Vec::new();
                for (task, outcome) in tasks.iter().zip(&outcomes) {
                    cp.tokens_used += outcome.tokens;
                    cp.tool_calls += outcome.tool_calls;
                    cp.total_denials += outcome.denials;
                    cancelled |= outcome.cancelled;
                    if !outcome.files_written.is_empty() {
                        out.mutating_success = true;
                        out.progress = true;
                    }
                    if let Some(l) = ledger.as_mut() {
                        l.append(
                            "explore",
                            json!({
                                "task": task, "finished": outcome.finished,
                                "turns": outcome.turns, "tool_calls": outcome.tool_calls,
                                "tokens": outcome.tokens, "files_read": outcome.files_read,
                                "urls_fetched": outcome.urls_fetched, "child_kind": kind.label(),
                                "files_written": outcome.files_written, "denials": outcome.denials,
                                "overlaps": outcome.overlaps,
                                "error": outcome.error, "batch": tasks.len(),
                            }),
                        );
                    }
                    if outcome.cancelled {
                        continue;
                    }
                    RunHandle::push_event(
                        &ctx.handle.shared,
                        AgentEvent::Info {
                            message: format!(
                                "explorer sub-agent {} after {} turns and {} tool calls",
                                if outcome.finished {
                                    "reported"
                                } else {
                                    "stopped"
                                },
                                outcome.turns,
                                outcome.tool_calls
                            ),
                        },
                    );
                    let preview: String = task.chars().take(120).collect();
                    crate::memory::record_spill(
                        &mut cp.observations,
                        &mut cp.history_pending,
                        crate::memory::Observation::new(
                            cp.step,
                            "explore",
                            Some(preview),
                            outcome.finished,
                            &outcome.report,
                        ),
                    );
                    let (report, clipped) =
                        rex_tools::clip_middle(&outcome.report, explore::EXPLORE_REPORT_CHARS / n);
                    entries.push(json!({
                        "task": task,
                        "finished": outcome.finished,
                        "report": report,
                        "report_truncated": clipped,
                        "files_read": outcome.files_read,
                        "urls_fetched": outcome.urls_fetched,
                        "files_written": outcome.files_written,
                        "denials": outcome.denials,
                        "overlaps": outcome.overlaps,
                        "turns": outcome.turns,
                        "tool_calls": outcome.tool_calls,
                        "error": outcome.error,
                    }));
                }
                if cancelled {
                    out.fatal = Some(TerminalReason::Cancelled);
                    return out;
                }
                // denials inside an edit child count toward the run's limit
                if cp.total_denials >= MAX_DENIALS {
                    out.fatal = Some(TerminalReason::Denied);
                    return out;
                }
                let all_finished = outcomes.iter().all(|o| o.finished);
                let content = if entries.len() == 1 {
                    let mut single = entries.remove(0);
                    if let Some(obj) = single.as_object_mut() {
                        obj.remove("task");
                    }
                    single.to_string()
                } else {
                    json!({"explorers": entries}).to_string()
                };
                out.progress = true;
                out.digest_actions.push(DigestAction {
                    tool: "explore".into(),
                    ok: all_finished,
                    target: Some(format!(
                        "{} explorer(s), {} tool calls",
                        outcomes.len(),
                        outcomes.iter().map(|o| o.tool_calls).sum::<usize>()
                    )),
                    error_kind: outcomes
                        .iter()
                        .any(|o| o.error.is_some())
                        .then(|| "explorer_error".into()),
                });
                push_model_part(
                    &mut model_parts,
                    raw,
                    json!({"functionCall":{"name":"explore","args": call_args}}),
                );
                response_parts.push(function_response(
                    protocol,
                    "explore",
                    &id,
                    all_finished,
                    &content,
                ));
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
                if cp.last_call_sig.as_deref() == Some(sig.as_str()) {
                    cp.same_call_repeats += 1;
                } else {
                    cp.same_call_repeats = 0;
                    cp.last_call_sig = Some(sig.clone());
                }
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
                            | ToolRequest::ApplyPatch { .. }
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
                // Head+tail clipping on char boundaries: the old byte slice
                // panicked on multi-byte output and dropped the tail, where
                // build and test errors are. Failed commands carry their
                // output in the error detail, so that is clipped too.
                let (output, out_clipped) = result
                    .output
                    .as_deref()
                    .map(|o| rex_tools::clip_middle(o, OUTCOME_CHARS))
                    .map_or((None, false), |(o, c)| (Some(o), c));
                let (error, err_clipped) = match &result.error {
                    Some(e) => {
                        let (detail, c) = rex_tools::clip_middle(&e.detail, OUTCOME_CHARS);
                        (Some(json!({"kind": e.kind, "detail": detail})), c)
                    }
                    None => (None, false),
                };
                let mut content = serde_json::to_string(&json!({
                    "ok": result.ok,
                    "output": output,
                    "error": error,
                    "receipt": {
                        "bytes_read": receipt.bytes_read,
                        "bytes_written": receipt.bytes_written,
                        "duration_ms": receipt.duration_ms,
                        "exit_code": receipt.exit_code,
                        "output_truncated": receipt.output_truncated || out_clipped || err_clipped,
                    }
                }))
                .unwrap_or_else(|_| "{\"ok\":false}".into());
                if cp.same_call_repeats >= DOOM_LOOP_REPEATS && result.ok {
                    content.push_str(&format!(
                        " warning: you have made this exact call {} times in a row and it returns the same kind of result; use what you already have or try a different step",
                        cp.same_call_repeats + 1
                    ));
                }
                {
                    let text = match (&result.output, &result.error) {
                        (Some(o), _) => o.as_str(),
                        (None, Some(e)) => e.detail.as_str(),
                        (None, None) => "",
                    };
                    let target = receipt
                        .target
                        .clone()
                        .or(receipt.command.as_ref().map(|c| c.join(" ")));
                    if result.ok {
                        match &request {
                            ToolRequest::CreateFile { .. } | ToolRequest::EditFile { .. } => {
                                crate::memory::supersede(&mut cp.observations, target.as_deref());
                                crate::memory::supersede(
                                    &mut cp.history_pending,
                                    target.as_deref(),
                                );
                            }
                            ToolRequest::ApplyPatch { .. } => {
                                crate::memory::supersede(&mut cp.observations, None);
                                crate::memory::supersede(&mut cp.history_pending, None);
                            }
                            _ => {}
                        }
                    }
                    crate::memory::record_spill(
                        &mut cp.observations,
                        &mut cp.history_pending,
                        crate::memory::Observation::new(
                            cp.step, tool_name, target, result.ok, text,
                        ),
                    );
                }
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
        ToolRequest::GlobFiles { .. } => "glob_files",
        ToolRequest::ApplyPatch { .. } => "apply_patch",
        ToolRequest::RunCommand { .. } => "run_command",
        ToolRequest::McpCall { .. } => "mcp_call",
    }
}

/// Cap on standing approvals in one run, so "allow for this run" stays a
/// handful of named commands and never becomes a blanket pass.
pub const MAX_STANDING_APPROVALS: usize = 8;

/// Record a standing approval, refusing a new one past the cap. Granting a
/// key that is already held is a no-op, not a second slot.
fn grant_standing(standing: &mut Vec<String>, key: String) -> Result<(), String> {
    if standing.contains(&key) {
        return Ok(());
    }
    if standing.len() >= MAX_STANDING_APPROVALS {
        return Err(format!(
            "at most {MAX_STANDING_APPROVALS} standing approvals per run"
        ));
    }
    standing.push(key);
    Ok(())
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
    wait_for_decision_on(&ctx.handle, call)
}

/// The approval wait itself; needs only the run handle, so edit children
/// on worker threads can use it too.
fn wait_for_decision_on(handle: &RunHandle, call: &PreparedCall) -> Decision {
    {
        let mut s = match handle.shared.lock() {
            Ok(s) => s,
            Err(_) => return Decision::Cancelled,
        };
        if handle.cancel.load(Ordering::SeqCst) {
            return Decision::Cancelled;
        }
        // A standing approval the user granted earlier in this run covers
        // this exact command line: run it without parking, and say so in
        // the event log so nothing is approved silently.
        if let Some(key) = call.standing_key.as_ref() {
            if s.standing.contains(key) {
                push_locked(
                    &mut s,
                    AgentEvent::ApprovedByStanding {
                        call_id: call.call_id.clone(),
                        command: call.summary.clone(),
                    },
                );
                return Decision::Approved;
            }
        }
        s.status = AgentStatus::AwaitingApproval;
        s.pending_approval = Some(call.clone());
        push_locked(&mut s, AgentEvent::ApprovalRequired { call: call.clone() });
    }
    let deadline = Instant::now() + Duration::from_millis(APPROVAL_WAIT_MS);
    let mut guard = match handle.shared.lock() {
        Ok(g) => g,
        Err(_) => return Decision::Cancelled,
    };
    loop {
        if handle.cancel.load(Ordering::SeqCst) {
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
        let (g, _timeout) = handle
            .cond
            .wait_timeout(guard, remaining.min(Duration::from_secs(5)))
            .expect("run state poisoned");
        guard = g;
    }
}

enum UserAnswer {
    Text(String),
    Declined,
    Timeout,
    Cancelled,
}

/// Park the loop thread until the trusted UI answers an `ask_user`
/// question. Only `AutonomousRunService::answer` supplies the reply.
fn wait_for_answer<S: SecretStore + 'static, T: Transport + 'static>(
    ctx: &LoopCtx<S, T>,
    question: &PendingQuestion,
) -> UserAnswer {
    {
        let mut s = match ctx.handle.shared.lock() {
            Ok(s) => s,
            Err(_) => return UserAnswer::Cancelled,
        };
        if let Some(queued) = s.queued_answers.pop_front() {
            // answered ahead of time on the batch form: no parking
            push_locked(
                &mut s,
                AgentEvent::QuestionAsked {
                    question: question.clone(),
                },
            );
            push_locked(
                &mut s,
                AgentEvent::QuestionResolved {
                    call_id: question.call_id.clone(),
                    answered: queued.is_some(),
                },
            );
            return match queued {
                Some(text) => UserAnswer::Text(text),
                None => UserAnswer::Declined,
            };
        }
        s.status = AgentStatus::AwaitingAnswer;
        s.pending_question = Some(question.clone());
        s.answer = None;
        push_locked(
            &mut s,
            AgentEvent::QuestionAsked {
                question: question.clone(),
            },
        );
    }
    let deadline = Instant::now() + Duration::from_millis(ANSWER_WAIT_MS);
    let mut guard = match ctx.handle.shared.lock() {
        Ok(g) => g,
        Err(_) => return UserAnswer::Cancelled,
    };
    loop {
        if ctx.handle.cancel.load(Ordering::SeqCst) {
            guard.status = AgentStatus::Running;
            guard.pending_question = None;
            return UserAnswer::Cancelled;
        }
        if let Some(reply) = guard.answer.take() {
            guard.status = AgentStatus::Running;
            guard.pending_question = None;
            return match reply {
                Some(text) => UserAnswer::Text(text),
                None => UserAnswer::Declined,
            };
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            guard.status = AgentStatus::Running;
            guard.pending_question = None;
            let call_id = question.call_id.clone();
            push_locked(
                &mut guard,
                AgentEvent::QuestionResolved {
                    call_id,
                    answered: false,
                },
            );
            return UserAnswer::Timeout;
        }
        let (g, _timeout) = ctx
            .handle
            .cond
            .wait_timeout(guard, remaining.min(Duration::from_secs(5)))
            .expect("run state poisoned");
        guard = g;
    }
}

/// Plan mode: the instruction appended to the plan turn. The request itself
/// declares ONLY `update_plan`, so the model structurally cannot reach any
/// other tool before the human approves the plan.
const PLAN_ONLY_INSTRUCTION: &str = "PLAN MODE. This turn you may ONLY call update_plan. \
    Produce a concrete 2-8 step plan for the task: one in_progress step, the rest pending. \
    The plan IS the tool call - do not describe it in prose instead of calling the tool. \
    No other tools exist in this turn.";

enum PlanGate {
    Approved,
    Denied,
    Failed(TerminalReason),
}

/// The plan-only tool declarations, derived from the same contract as the
/// full toolset so the schema can never drift from it.
fn plan_only_declarations() -> Vec<Value> {
    gemini_tool_definitions()[0]["functionDeclarations"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|d| d.get("name").and_then(|n| n.as_str()) == Some("update_plan"))
        .collect()
}

/// A provider request identical in shape to `build_request` but declaring
/// only `update_plan`: the model cannot call what is not offered.
fn build_plan_request(
    protocol: ProviderProtocol,
    model: &str,
    system: &str,
    state_msg: &str,
) -> String {
    let decls = plan_only_declarations();
    match protocol {
        ProviderProtocol::Gemini => {
            let contents = vec![json!({"role":"user","parts":[{"text": state_msg}]})];
            json!({"contents": contents, "tools": [{"functionDeclarations": decls}],
                "systemInstruction": {"parts":[{"text": system}]},
                "toolConfig":{"functionCallingConfig":{"mode":"AUTO"}},
                "generationConfig":{"temperature":0.2,"maxOutputTokens":4096}})
            .to_string()
        }
        ProviderProtocol::Anthropic => {
            let tools: Vec<Value> = decls
                .into_iter()
                .map(|d| {
                    json!({"name": d["name"], "description": d["description"],
                        "input_schema": lowercase_schema(d["parameters"].clone())})
                })
                .collect();
            json!({"model":model,"max_tokens":4096,"temperature":0.2,
                "system":system,
                "messages":[{"role":"user","content":state_msg}],"tools":tools})
            .to_string()
        }
        ProviderProtocol::OpenAiCompatible => {
            let tools: Vec<Value> = decls
                .into_iter()
                .map(|d| {
                    json!({"type":"function","function":{
                        "name":d["name"],"description":d["description"],
                        "parameters":lowercase_schema(d["parameters"].clone())}})
                })
                .collect();
            json!({"model":model,
                "messages":[{"role":"system","content":system},{"role":"user","content":state_msg}],
                "tools":tools,"tool_choice":"auto","temperature":0.2,"max_tokens":4096})
            .to_string()
        }
    }
}

/// Fold excerpts dropped from working memory into the rolling history
/// summary with one tool-free model call, when enough have piled up and
/// the token budget has room. Any failure leaves the run going without a
/// new summary; the pending excerpts are dropped so a broken provider is
/// not asked again every turn.
#[allow(clippy::too_many_arguments)]
fn summarize_history_step<S: SecretStore + 'static, T: Transport + 'static>(
    ctx: &LoopCtx<S, T>,
    ledger: &mut Option<Ledger>,
    protocol: ProviderProtocol,
    base_url: &str,
    key: &str,
    model: &str,
    budgets: Budgets,
    cp: &mut Checkpoint,
) {
    crate::history::cap_pending(&mut cp.history_pending);
    if !crate::history::should_summarize(&cp.history_pending) {
        return;
    }
    let pending = std::mem::take(&mut cp.history_pending);
    let prompt = crate::history::build_prompt(cp.history_summary.as_deref(), &pending);
    let body = crate::history::build_request(protocol, model, &prompt);
    let estimate = crate::history::estimated_tokens(&body);
    if cp.tokens_used + estimate > budgets.max_tokens {
        RunHandle::push_event(
            &ctx.handle.shared,
            AgentEvent::Info {
                message: format!(
                    "history summary skipped: about {estimate} tokens would pass the token budget"
                ),
            },
        );
        return;
    }
    let step = cp.step;
    let evidence = ctx.state_dir.join("evidence");
    let _ = fs::write(evidence.join(format!("summary-{step}-request.json")), &body);
    let result = generate_with_retry(
        ctx.service.transport(),
        protocol,
        base_url,
        key,
        model,
        &body,
        &ctx.handle,
    );
    let (message, usage) = match result {
        Ok(response) => {
            let _ = fs::write(
                evidence.join(format!("summary-{step}-response.json")),
                &response,
            );
            let usage = usage_tokens(protocol, &response);
            cp.tokens_used += usage.unwrap_or((body.len() + response.len()) as u64 / 4);
            let text = decode_provider_calls(protocol, &response)
                .ok()
                .and_then(|d| crate::history::clip_summary(&d.texts));
            match text {
                Some(t) => {
                    cp.history_summary = Some(t);
                    (
                        format!("summarized {} older tool results", pending.len()),
                        usage,
                    )
                }
                None => (
                    "history summary skipped: the model returned no text".into(),
                    usage,
                ),
            }
        }
        Err(detail) => (format!("history summary skipped: {detail}"), None),
    };
    if let Some(l) = ledger.as_mut() {
        l.append(
            "history_summary",
            json!({"step": step, "items": pending.len(), "usage_tokens": usage,
                "stored": message.starts_with("summarized")}),
        );
    }
    RunHandle::push_event(&ctx.handle.shared, AgentEvent::Info { message });
}

/// Plan gate: one model turn with only `update_plan` declared, then park in
/// `AwaitingPlan` until the trusted UI approves. No tool executes before
/// approval - the plan turn's tool-call accounting is restored afterwards
/// because proposing the plan is pre-work, not execution.
// Runs the single pre-execution plan turn and parks the run at
// AgentStatus::AwaitingPlan until the user approves or denies the plan.
// The parameters are the loop's ambient context; bundling them would hide
// what the gate actually depends on at its single call site.
#[allow(clippy::too_many_arguments)]
fn plan_gate<S: SecretStore + 'static, T: Transport + 'static>(
    ctx: &LoopCtx<S, T>,
    tools: &ToolRuntime,
    ledger: &mut Option<Ledger>,
    protocol: ProviderProtocol,
    base_url: &str,
    key: &str,
    model: &str,
    started: Instant,
    cp: &mut Checkpoint,
) -> PlanGate {
    if let Ok(mut s) = ctx.handle.shared.lock() {
        s.status = AgentStatus::Planning;
    }
    let system = format!("{}\n\n{PLAN_ONLY_INSTRUCTION}", ctx.system_prompt);
    let state_msg = format!(
        "Task: {}\n\nPropose the execution plan now via update_plan. Nothing has run yet; \
         the human approves the plan before any tool executes.",
        ctx.brief.task
    );
    // Two attempts: a model that will not produce a plan fails the gate
    // honestly instead of stalling the run forever.
    for attempt in 1..=2u32 {
        if ctx.handle.cancel.load(Ordering::SeqCst) {
            return PlanGate::Failed(TerminalReason::Cancelled);
        }
        let body = build_plan_request(protocol, model, &system, &state_msg);
        let _ = fs::write(
            ctx.state_dir
                .join("evidence")
                .join(format!("plan-turn-{attempt}-request.json")),
            &body,
        );
        let response = match generate_with_retry(
            ctx.service.transport(),
            protocol,
            base_url,
            key,
            model,
            &body,
            &ctx.handle,
        ) {
            Ok(r) => r,
            Err(detail) => return PlanGate::Failed(TerminalReason::ProviderError { detail }),
        };
        let _ = fs::write(
            ctx.state_dir
                .join("evidence")
                .join(format!("plan-turn-{attempt}-response.json")),
            &response,
        );
        let (usage_total, response_text) =
            (usage_tokens(protocol, &response), response.len() as u64);
        cp.tokens_used += usage_total.unwrap_or((body.len() as u64 + response_text) / 4);
        if let Some(l) = ledger.as_mut() {
            l.append(
                "plan_turn",
                json!({"attempt": attempt, "usage_tokens": usage_total}),
            );
        }
        let decoded = match decode_provider_calls(protocol, &response) {
            Ok(v) => v,
            Err(e) => {
                return PlanGate::Failed(TerminalReason::ProviderError {
                    detail: format!("undecodable plan turn: {e}"),
                })
            }
        };
        for text in &decoded.texts {
            RunHandle::push_event(
                &ctx.handle.shared,
                AgentEvent::ModelText { text: text.clone() },
            );
        }
        for note in &decoded.repairs {
            RunHandle::push_event(
                &ctx.handle.shared,
                AgentEvent::Info {
                    message: format!("repaired call: {note}"),
                },
            );
        }
        // Fail closed: the plan turn offers only update_plan, but the
        // generic decoder turns any tool-shaped part into an executable
        // AgentCall::Tool. Verify every decoded call here, before
        // execute_turn can run anything: only update_plan (and BadCall,
        // which executes nothing) may pass. Anything else aborts the gate
        // before a single tool runs.
        let mut disallowed: Vec<String> = Vec::new();
        for (call, _) in &decoded.calls {
            let name = match call {
                AgentCall::UpdatePlan { .. } => None,
                AgentCall::BadCall { .. } => None,
                AgentCall::Tool { request, .. } => Some(format!(
                    "tool:{}",
                    match request {
                        ToolRequest::ReadFile { .. } => "read_file",
                        ToolRequest::CreateFile { .. } => "create_file",
                        ToolRequest::EditFile { .. } => "edit_file",
                        ToolRequest::SearchFiles { .. } => "search_files",
                        ToolRequest::GlobFiles { .. } => "glob_files",
                        ToolRequest::ApplyPatch { .. } => "apply_patch",
                        ToolRequest::RunCommand { .. } => "run_command",
                        ToolRequest::McpCall { .. } => "mcp_call",
                    }
                )),
                AgentCall::WebSearch { .. } => Some("web_search".to_string()),
                AgentCall::CompleteTask { .. } => Some("complete_task".to_string()),
                AgentCall::Explore { .. } => Some("explore".to_string()),
                AgentCall::WebFetch { .. } => Some("web_fetch".to_string()),
                AgentCall::AskUser { .. } => Some("ask_user".to_string()),
            };
            if let Some(name) = name {
                disallowed.push(name);
            }
        }
        if !disallowed.is_empty() {
            if let Some(l) = ledger.as_mut() {
                l.append(
                    "plan_gate",
                    json!({"ok": false, "disallowed_calls": disallowed}),
                );
            }
            return PlanGate::Failed(TerminalReason::Blocked {
                detail: format!(
                    "plan turn emitted disallowed call(s) before approval: {}",
                    disallowed.join(", ")
                ),
            });
        }
        // The plan turn is pre-work: its tool-call accounting is restored.
        let tool_calls_before = cp.tool_calls;
        let out = execute_turn(
            ctx,
            tools,
            ledger,
            &ProviderLink {
                protocol,
                base_url,
                key,
                model,
            },
            decoded.calls,
            decoded.thought_signature,
            cp,
            started,
        );
        cp.tool_calls = tool_calls_before;
        if let Some(fatal) = out.fatal {
            return PlanGate::Failed(fatal);
        }
        let plan = current_plan(&ctx.handle);
        if !plan.is_empty() {
            if let Some(l) = ledger.as_mut() {
                l.append("plan_proposed", json!({"items": plan}));
            }
            // Parked runs are resumable: persist before blocking on the
            // decision, so process death while awaiting approval still
            // resumes at the gate instead of losing the run.
            let _ = write_json(&ctx.state_dir.join("checkpoint.json"), &*cp);
            return match wait_for_plan_decision(ctx, &plan) {
                Decision::Approved => {
                    // Durable before entering the loop: a resumed run must
                    // never re-enter the gate and must never assume approval.
                    cp.plan_approved = true;
                    let _ = write_json(&ctx.state_dir.join("checkpoint.json"), &*cp);
                    PlanGate::Approved
                }
                Decision::Denied => PlanGate::Denied,
                Decision::Timeout => PlanGate::Failed(TerminalReason::ApprovalTimeout),
                Decision::Cancelled => PlanGate::Failed(TerminalReason::Cancelled),
            };
        }
        RunHandle::push_event(
            &ctx.handle.shared,
            AgentEvent::Info {
                message: format!("plan turn {attempt} produced no plan; asking once more"),
            },
        );
    }
    PlanGate::Failed(TerminalReason::ModelStalled)
}

/// Test-only process-death simulation: persist the checkpoint and report
/// that the run halted silently, without writing a terminal state. Mirrors
/// the main loop's silent-cancel path so `halt_without_terminal` works no
/// matter where the run was parked.
#[cfg(test)]
fn silent_halt<S: SecretStore + 'static, T: Transport + 'static>(
    ctx: &LoopCtx<S, T>,
    cp: &Checkpoint,
) -> bool {
    if ctx.handle.silent_cancel.load(Ordering::SeqCst) {
        let _ = write_json(&ctx.state_dir.join("checkpoint.json"), cp);
        return true;
    }
    false
}

/// Park the loop thread until the trusted UI approves or rejects the plan.
/// Mirrors `wait_for_decision`; the model cannot reach this transition.
fn wait_for_plan_decision<S: SecretStore + 'static, T: Transport + 'static>(
    ctx: &LoopCtx<S, T>,
    plan: &[PlanItem],
) -> Decision {
    {
        let mut s = match ctx.handle.shared.lock() {
            Ok(s) => s,
            Err(_) => return Decision::Cancelled,
        };
        s.status = AgentStatus::AwaitingPlan;
        s.pending_approval = None;
        s.decision = None;
        push_locked(
            &mut s,
            AgentEvent::PlanApprovalRequired {
                items: plan.to_vec(),
            },
        );
    }
    let deadline = Instant::now() + Duration::from_millis(APPROVAL_WAIT_MS);
    let mut guard = match ctx.handle.shared.lock() {
        Ok(g) => g,
        Err(_) => return Decision::Cancelled,
    };
    loop {
        if ctx.handle.cancel.load(Ordering::SeqCst) {
            guard.status = AgentStatus::Planning;
            return Decision::Cancelled;
        }
        if let Some(approved) = guard.decision.take() {
            guard.status = AgentStatus::Running;
            return if approved {
                Decision::Approved
            } else {
                Decision::Denied
            };
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            guard.status = AgentStatus::Planning;
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
        Some(allowed) => ["create_file", "edit_file", "apply_patch", "run_command"]
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
        /// Concurrency probe: each post sleeps `post_delay_ms` while
        /// counted in flight; `max_in_flight` records the peak overlap.
        post_delay_ms: std::sync::atomic::AtomicU64,
        in_flight: std::sync::atomic::AtomicUsize,
        max_in_flight: std::sync::atomic::AtomicUsize,
    }

    impl Script {
        fn new(turns: Vec<String>) -> Self {
            Self {
                turns: Mutex::new(turns.into()),
                fail_posts: Mutex::new((0, 500)),
                posts: Mutex::new(Vec::new()),
                post_delay_ms: Default::default(),
                in_flight: Default::default(),
                max_in_flight: Default::default(),
            }
        }
        fn failing(turns: Vec<String>, count: u32, status: u16) -> Self {
            Self {
                turns: Mutex::new(turns.into()),
                fail_posts: Mutex::new((count, status)),
                posts: Mutex::new(Vec::new()),
                post_delay_ms: Default::default(),
                in_flight: Default::default(),
                max_in_flight: Default::default(),
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
            let delay = self.post_delay_ms.load(Ordering::SeqCst);
            if delay > 0 {
                let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                self.max_in_flight.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(delay));
                self.in_flight.fetch_sub(1, Ordering::SeqCst);
            }
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
            &[],
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
            &[],
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
            &[],
        );

        let openai = build_request(
            ProviderProtocol::OpenAiCompatible,
            "gpt-test",
            "sys",
            "state",
            None,
            &[],
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
            plan_mode: false,
            summarize_history: false,
            name: None,
            continued_from: None,
        };
        write_brief(&dir, &brief).unwrap();
        let mut changed = brief.clone();
        changed.task = "quietly rewritten ask".into();
        assert!(write_brief(&dir, &changed).is_err());
        let on_disk: TaskBrief = read_json(&dir.join("brief.json")).unwrap();
        assert_eq!(on_disk.task, "original ask");
    }

    #[test]
    fn session_meta_is_recorded_on_the_brief() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = Arc::new(service(
            tmp.path(),
            Script::new(vec![call_turn(vec![complete_call("{\"ok\":true}")])]),
        ));
        let snap = svc
            .begin_in_workspace_with_role(
                "do the thing",
                "gemini",
                None,
                Some(budgets()),
                None,
                Role::Worker,
                false,
                SessionMeta {
                    name: Some("alpha".into()),
                    continued_from: Some("agent-0-aaa".into()),
                },
            )
            .unwrap();
        let on_disk: TaskBrief =
            read_json(&svc.run_dir(&snap.id).join("state").join("brief.json")).unwrap();
        assert_eq!(on_disk.name.as_deref(), Some("alpha"));
        assert_eq!(on_disk.continued_from.as_deref(), Some("agent-0-aaa"));
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
    fn repaired_calls_run_and_show_in_events() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = Arc::new(service(
            tmp.path(),
            Script::new(vec![
                call_turn(vec![
                    json!({"functionCall":{"name":"Create_File","args":{"path":"r.txt","content":"x","overwrite":"TRUE"}}}),
                ]),
                text_turn("done"),
                text_turn("done"),
                text_turn("done"),
            ]),
        ));
        let snap = svc.begin("repair", "gemini", Some(budgets())).unwrap();
        auto_approve(svc.clone(), snap.id.clone());
        let done = wait_terminal(&svc, &snap.id, 20_000);
        let infos: Vec<&str> = done
            .events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::Info { message } if message.starts_with("repaired call: ") => {
                    Some(message.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            infos,
            [
                "repaired call: tool name 'Create_File' read as 'create_file'",
                "repaired call: create_file: fixed argument types for overwrite",
            ]
        );
        assert!(done.events.iter().any(
            |e| matches!(e, AgentEvent::ToolFinished { result } if result.ok && result.tool == "create_file")
        ));
    }

    /// Nine 2,100-char reads overflow working memory (12k chars), and the
    /// dropped excerpts pass the 6,000-char summary trigger.
    fn history_run(
        summarize: bool,
        max_tokens: u64,
        turns: Vec<String>,
    ) -> (Arc<Svc>, AgentSnapshot) {
        history_run_with(summarize, max_tokens, 9, vec![], turns)
    }

    fn history_run_with(
        summarize: bool,
        max_tokens: u64,
        n_reads: usize,
        after_reads: Vec<Value>,
        turns: Vec<String>,
    ) -> (Arc<Svc>, AgentSnapshot) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.keep();
        fs::create_dir_all(root.join("runs").join("hist-ws")).unwrap();
        let ws = root.join("runs").join("hist-ws");
        let mut reads = Vec::new();
        for i in 1..=n_reads {
            fs::write(ws.join(format!("f{i}.txt")), format!("{i}").repeat(2_100)).unwrap();
            reads.push(
                json!({"functionCall":{"name":"read_file","args":{"path": format!("f{i}.txt")}}}),
            );
        }
        reads.extend(after_reads);
        let mut script = vec![call_turn(reads)];
        script.extend(turns);
        let svc = Arc::new(service(&root, Script::new(script)));
        let snap = svc
            .begin_in_workspace_with_options(
                "read everything",
                "gemini",
                None,
                Some(Budgets {
                    max_tokens,
                    ..budgets()
                }),
                Some(ws),
                Role::Worker,
                RunOptions {
                    summarize_history: summarize,
                    ..RunOptions::default()
                },
                SessionMeta::default(),
            )
            .unwrap();
        auto_approve(svc.clone(), snap.id.clone());
        let done = wait_terminal(&svc, &snap.id, 20_000);
        (svc, done)
    }

    fn infos(done: &AgentSnapshot, prefix: &str) -> Vec<String> {
        done.events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::Info { message } if message.starts_with(prefix) => {
                    Some(message.clone())
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn history_summary_is_off_by_default_and_makes_no_extra_call() {
        let (svc, done) = history_run(false, 100_000, vec![text_turn("done"); 4]);
        let posts = svc.service.transport().seen();
        assert!(!posts
            .iter()
            .any(|p| p.contains(crate::history::SUMMARY_SYSTEM)));
        assert!(infos(&done, "summarized").is_empty());
        assert!(infos(&done, "history summary").is_empty());
        assert!(!posts.iter().any(|p| p.contains("Model-written summary")));
        let cp: Value = serde_json::from_str(
            &fs::read_to_string(svc.run_dir(&done.id).join("state").join("checkpoint.json"))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(cp["history_pending"], json!([]), "nothing kept when off");
        assert!(cp["history_summary"].is_null());
    }

    #[test]
    fn history_summary_drops_all_pending_reads_after_a_patch() {
        // a patch can touch any file, so every pending read may be stale
        let (svc, done) = history_run_with(
            true,
            100_000,
            10,
            vec![
                json!({"functionCall":{"name":"apply_patch","args":{"patch":"*** Begin Patch\n*** Add File: new.txt\n+n\n*** End Patch"}}}),
            ],
            vec![text_turn("done"); 4],
        );
        assert!(done.events.iter().any(
            |e| matches!(e, AgentEvent::ToolFinished { result } if result.ok && result.tool == "apply_patch")
        ));
        let posts = svc.service.transport().seen();
        assert!(!posts
            .iter()
            .any(|p| p.contains(crate::history::SUMMARY_SYSTEM)));
    }

    #[test]
    fn history_summary_skips_reads_of_files_the_run_then_changed() {
        let (svc, _) = history_run_with(
            true,
            100_000,
            10,
            vec![
                json!({"functionCall":{"name":"create_file","args":{"path":"f1.txt","content":"new","overwrite":true}}}),
            ],
            vec![
                text_turn("S"),
                text_turn("done"),
                text_turn("done"),
                text_turn("done"),
            ],
        );
        let posts = svc.service.transport().seen();
        let req = posts
            .iter()
            .find(|p| p.contains(crate::history::SUMMARY_SYSTEM))
            .expect("summary request made");
        let req: Value = serde_json::from_str(req).unwrap();
        let data: Value =
            serde_json::from_str(req["contents"][0]["parts"][0]["text"].as_str().unwrap()).unwrap();
        let targets: Vec<String> = data["tool_results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["target"].as_str().unwrap_or("").to_string())
            .collect();
        assert!(!targets.is_empty());
        assert!(
            targets
                .iter()
                .all(|t| !t.ends_with("/f1.txt") && t != "f1.txt"),
            "stale read of f1 was summarized: {targets:?}"
        );
        assert!(targets[0].ends_with("f2.txt"), "{targets:?}");
    }

    #[test]
    fn opt_in_history_summary_folds_dropped_results_into_later_turns() {
        let (svc, done) = history_run(
            true,
            100_000,
            vec![
                text_turn("SUMMARY: f1-f3 hold repeated digits"),
                text_turn("done"),
                text_turn("done"),
                text_turn("done"),
            ],
        );
        let posts = svc.service.transport().seen();
        let at = posts
            .iter()
            .position(|p| p.contains(crate::history::SUMMARY_SYSTEM))
            .expect("summary request made");
        assert_eq!(at, 1, "summary runs right after the turn that overflowed");
        let req: Value = serde_json::from_str(&posts[at]).unwrap();
        assert!(req.get("tools").is_none());
        let user = req["contents"][0]["parts"][0]["text"].as_str().unwrap();
        let data: Value = serde_json::from_str(user).unwrap();
        let targets: Vec<&str> = data["tool_results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["target"].as_str().unwrap_or(""))
            .collect();
        // the oldest reads, in order (how many depends on excerpt framing)
        let n = targets.len();
        assert!((3..=5).contains(&n), "{targets:?}");
        for (i, t) in targets.iter().enumerate() {
            assert!(t.ends_with(&format!("f{}.txt", i + 1)), "{targets:?}");
        }
        let next: Value = serde_json::from_str(&posts[at + 1]).unwrap();
        let state: Value = serde_json::from_str(
            next["contents"][0]["parts"][0]["text"]
                .as_str()
                .expect("state message first"),
        )
        .expect("state message is JSON");
        assert_eq!(
            state["history_summary"]["text"],
            "SUMMARY: f1-f3 hold repeated digits"
        );
        assert_eq!(
            state["history_summary"]["note"],
            crate::history::SUMMARY_NOTE
        );
        assert_eq!(
            posts
                .iter()
                .filter(|p| p.contains(crate::history::SUMMARY_SYSTEM))
                .count(),
            1,
            "nothing new dropped, so no second summary"
        );
        assert_eq!(
            infos(&done, "summarized"),
            [format!("summarized {n} older tool results")]
        );
        assert!(done.tokens_used >= 240 + 120 * 2);
    }

    #[test]
    fn history_summary_respects_the_token_budget() {
        // 2,000 tokens (the floor) leave no room for a ~2,500-token summary:
        // it is skipped with a note, and the run goes on without one.
        let (svc, done) = history_run(true, 2_000, vec![text_turn("done"); 4]);
        let posts = svc.service.transport().seen();
        assert!(!posts
            .iter()
            .any(|p| p.contains(crate::history::SUMMARY_SYSTEM)));
        let skipped = infos(&done, "history summary skipped");
        assert_eq!(skipped.len(), 1, "{skipped:?}");
        assert!(skipped[0].contains("token budget"), "{skipped:?}");
        assert!(posts.len() >= 2, "the run kept going");
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
            &[],
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
        let anthropic = build_request(
            ProviderProtocol::Anthropic,
            "m",
            &system,
            "state",
            None,
            &[],
        );
        assert!(anthropic.contains("REX CONSTITUTION"));
        let openai = build_request(
            ProviderProtocol::OpenAiCompatible,
            "m",
            &system,
            "state",
            None,
            &[],
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

    fn fake_ext_tool() -> ExtTool {
        ExtTool {
            server: "fake".into(),
            name: "echo".into(),
            description: "Echo the input text back.".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    #[test]
    fn mcp_call_decodes_from_provider_tool_use() {
        let (call, _) = decode_named_call(
            "mcp_call",
            "call-1".into(),
            serde_json::json!({"server": "fake", "name": "echo", "arguments": {"text": "hi"}}),
            serde_json::json!({}),
            &mut Vec::new(),
        );
        match call {
            AgentCall::Tool { id, request } => {
                assert_eq!(id, "call-1");
                match request {
                    ToolRequest::McpCall {
                        server,
                        name,
                        arguments,
                    } => {
                        assert_eq!(server, "fake");
                        assert_eq!(name, "echo");
                        assert_eq!(arguments["text"], "hi");
                    }
                    other => panic!("wrong variant: {other:?}"),
                }
            }
            _ => panic!("unexpected call variant"),
        }
    }

    #[test]
    fn build_request_declares_mcp_call_when_tools_connected() {
        let tools = vec![fake_ext_tool()];
        for protocol in [
            ProviderProtocol::Gemini,
            ProviderProtocol::Anthropic,
            ProviderProtocol::OpenAiCompatible,
        ] {
            let body = build_request(protocol, "m", "sys", "state", None, &tools);
            assert!(
                body.contains("\"mcp_call\""),
                "protocol {protocol:?} must declare mcp_call"
            );
        }
        for protocol in [
            ProviderProtocol::Gemini,
            ProviderProtocol::Anthropic,
            ProviderProtocol::OpenAiCompatible,
        ] {
            let body = build_request(protocol, "m", "sys", "state", None, &[]);
            assert!(
                !body.contains("mcp_call"),
                "protocol {protocol:?} must not declare mcp_call without servers"
            );
        }
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
    fn state_message_carries_repo_instructions_below_the_contract() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("AGENTS.md"),
            "Run `make check` before completing.",
        )
        .unwrap();
        let project = rex_prompt::project::load(tmp.path());
        let brief = TaskBrief {
            id: "r1".into(),
            task: "fix bug".into(),
            provider: "gemini".into(),
            budgets: Budgets::default(),
            created_at_ms: 1,
            prompt_version: legacy_prompt_marker(),
            prompt_hash: legacy_prompt_marker(),
            role: None,
            allowed_tools: None,
            plan_mode: false,
            summarize_history: false,
            name: None,
            continued_from: None,
        };
        let cp = Checkpoint::default();
        let msg = build_state_message(
            &brief,
            &cp,
            Budgets::default(),
            vec![],
            project.as_ref(),
            None,
        );
        let v: serde_json::Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(v["project_instructions"]["source"], "AGENTS.md");
        assert!(v["project_instructions"]["text"]
            .as_str()
            .unwrap()
            .contains("make check"));
        assert!(v["project_instructions"]["precedence"]
            .as_str()
            .unwrap()
            .contains("never override"));
        // absent file -> null, and the system prompt identity is unaffected
        let none = build_state_message(&brief, &cp, Budgets::default(), vec![], None, None);
        let v: serde_json::Value = serde_json::from_str(&none).unwrap();
        assert!(v["project_instructions"].is_null());
        assert!(v["user_instructions"].is_null());
        // user-level file travels separately with its own label and rank
        let cfg = tempfile::tempdir().unwrap();
        fs::create_dir_all(cfg.path().join("rex")).unwrap();
        fs::write(
            cfg.path().join("rex").join("AGENTS.md"),
            "prefer small diffs",
        )
        .unwrap();
        let user = rex_prompt::project::load_user_from(cfg.path());
        let both = build_state_message(
            &brief,
            &cp,
            Budgets::default(),
            vec![],
            project.as_ref(),
            user.as_ref(),
        );
        let v: serde_json::Value = serde_json::from_str(&both).unwrap();
        assert_eq!(v["project_instructions"]["source"], "AGENTS.md");
        assert_eq!(
            v["user_instructions"]["source"],
            rex_prompt::project::USER_SOURCE
        );
        assert_eq!(v["user_instructions"]["text"], "prefer small diffs");
        assert!(v["user_instructions"]["precedence"]
            .as_str()
            .unwrap()
            .contains("never override"));
    }

    #[test]
    fn explorer_sub_agent_reads_only_and_hands_back_one_report() {
        let tmp = tempfile::tempdir().unwrap();
        let glob = json!({"functionCall":{"name":"glob_files","args":{"pattern":"**/*"}}});
        let explore =
            json!({"functionCall":{"name":"explore","args":{"task":"where is the entry point?"}}});
        let svc = Arc::new(service(
            tmp.path(),
            Script::new(vec![
                // parent turn 1: delegate
                call_turn(vec![explore]),
                // child turn 1: tries to write (refused) and globs
                call_turn(vec![create_call("evil.txt", "x"), glob]),
                // child turn 2: reports
                call_turn(vec![complete_call("ENTRY-REPORT: no source files yet")]),
                // parent turn 2
                call_turn(vec![complete_call("done")]),
            ]),
        ));
        let snap = svc
            .begin("find the entry point", "gemini", Some(budgets()))
            .unwrap();
        auto_approve(svc.clone(), snap.id.clone());
        let done = wait_terminal(&svc, &snap.id, 60_000);
        assert!(done.events.iter().any(|e| matches!(
            e,
            AgentEvent::Info { message } if message.contains("explorer sub-agent reported after 2 turns and 1 tool calls")
        )), "{:?}", done.events);
        let run_dir = tmp.path().join("runs").join(&snap.id);
        assert!(!run_dir.join("workspace").join("evil.txt").exists());
        let posts = svc.service().transport().seen();
        // child requests carry the explorer prompt and only read tools
        let child: Value = serde_json::from_str(&posts[1]).unwrap();
        let sys = child["systemInstruction"]["parts"][0]["text"]
            .as_str()
            .unwrap();
        assert!(sys.contains("explorer sub-agent"));
        let names: Vec<&str> = child["tools"][0]["functionDeclarations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            ["read_file", "search_files", "glob_files", "complete_task"]
        );
        // the refused write reached the child as an error, not the workspace
        assert!(posts[2].contains("create_file is not available to the explorer"));
        // the parent sees the report, not the raw glob output
        assert!(posts[3].contains("ENTRY-REPORT"));
        let ledger = fs::read_to_string(run_dir.join("state").join("ledger.jsonl"))
            .or_else(|_| fs::read_to_string(run_dir.join("ledger.jsonl")))
            .unwrap_or_default();
        assert!(ledger.contains("\"kind\":\"explore\""), "{ledger}");
    }

    #[test]
    fn explore_kind_is_parsed_and_research_gets_web_fetch_only() {
        use explore::ExplorerKind;
        assert_eq!(
            explore::parse_explore_kind(&json!({})),
            Ok(ExplorerKind::Explore)
        );
        assert_eq!(
            explore::parse_explore_kind(&json!({"kind":" research "})),
            Ok(ExplorerKind::Research)
        );
        assert!(explore::parse_explore_kind(&json!({"kind":"writer"})).is_err());
        let bad = decode_provider_calls(
            ProviderProtocol::Gemini,
            &json!({"candidates":[{"content":{"parts":[{"functionCall":{"name":"explore","args":{"task":"q","kind":"writer"}}}]}}]}).to_string(),
        )
        .unwrap();
        assert!(matches!(&bad.calls[0].0, AgentCall::BadCall { .. }));
    }

    #[test]
    fn research_child_fetches_through_vetting_and_explorer_cannot_fetch() {
        let tmp = tempfile::tempdir().unwrap();
        let fetch_local =
            json!({"functionCall":{"name":"web_fetch","args":{"url":"http://127.0.0.1/secret"}}});
        let research = json!({"functionCall":{"name":"explore","args":{
            "task":"what does the upstream doc say?","kind":"research"}}});
        let plain =
            json!({"functionCall":{"name":"explore","args":{"task":"same, workspace only"}}});
        let svc = Arc::new(service(
            tmp.path(),
            Script::new(vec![
                call_turn(vec![research]),
                // research child: a private address is refused by vetting
                call_turn(vec![fetch_local.clone()]),
                call_turn(vec![complete_call("RESEARCH-REPORT")]),
                call_turn(vec![plain]),
                // explore child: web_fetch is not one of its tools
                call_turn(vec![fetch_local]),
                call_turn(vec![complete_call("EXPLORE-REPORT")]),
                call_turn(vec![complete_call("done")]),
            ]),
        ));
        let snap = svc
            .begin("research check", "gemini", Some(budgets()))
            .unwrap();
        auto_approve(svc.clone(), snap.id.clone());
        let done = wait_terminal(&svc, &snap.id, 60_000);
        assert!(
            done.events.iter().any(|e| matches!(
                e,
                AgentEvent::Info { message } if message.starts_with("research sub-agent started")
            )),
            "{:?}",
            done.events
        );
        let posts = svc.service().transport().seen();
        let child: Value = serde_json::from_str(&posts[1]).unwrap();
        assert!(child["systemInstruction"]["parts"][0]["text"]
            .as_str()
            .unwrap()
            .contains("research sub-agent"));
        let names: Vec<&str> = child["tools"][0]["functionDeclarations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "read_file",
                "search_files",
                "glob_files",
                "web_fetch",
                "complete_task"
            ]
        );
        // blocked by vetting or by the fetcher's own address check, never fetched
        assert!(
            posts[2].contains("refused:") || posts[2].contains("unsafe_address"),
            "{}",
            posts[2]
        );
        assert!(posts[3].contains("RESEARCH-REPORT"));
        let explore_child: Value = serde_json::from_str(&posts[4]).unwrap();
        let names: Vec<&str> = explore_child["tools"][0]["functionDeclarations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["name"].as_str().unwrap())
            .collect();
        assert!(!names.contains(&"web_fetch"));
        assert!(
            posts[5].contains("web_fetch is not available to the explorer"),
            "{}",
            posts[5]
        );
    }

    #[test]
    fn edit_child_writes_only_through_the_parents_approval_gate() {
        let tmp = tempfile::tempdir().unwrap();
        let edit = json!({"functionCall":{"name":"explore","args":{
            "task":"add a.txt","kind":"edit"}}});
        let svc = Arc::new(service(
            tmp.path(),
            Script::new(vec![
                call_turn(vec![edit]),
                // child: first write is approved, second is denied
                call_turn(vec![create_call("a.txt", "A")]),
                call_turn(vec![create_call("b.txt", "B")]),
                call_turn(vec![complete_call("EDIT-REPORT")]),
                call_turn(vec![complete_call("done")]),
            ]),
        ));
        let snap = svc.begin("edit check", "gemini", Some(budgets())).unwrap();
        // the trusted UI: approve the first pending call, deny the rest
        let (svc2, id2) = (svc.clone(), snap.id.clone());
        let decided = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = decided.clone();
        std::thread::spawn(move || loop {
            let s = svc2.snapshot(&id2).expect("snapshot");
            if s.terminal_reason.is_some() {
                return;
            }
            if s.status == AgentStatus::AwaitingApproval && s.pending_approval.is_some() {
                let n = seen.fetch_add(1, Ordering::SeqCst);
                let _ = svc2.decide(&id2, n == 0);
            }
            std::thread::sleep(Duration::from_millis(10));
        });
        let done = wait_terminal(&svc, &snap.id, 60_000);
        assert_eq!(decided.load(Ordering::SeqCst), 2, "{:?}", done.events);
        let ws = tmp.path().join("runs").join(&snap.id).join("workspace");
        assert_eq!(fs::read_to_string(ws.join("a.txt")).unwrap(), "A");
        assert!(!ws.join("b.txt").exists());
        let posts = svc.service().transport().seen();
        let child: Value = serde_json::from_str(&posts[1]).unwrap();
        assert!(child["systemInstruction"]["parts"][0]["text"]
            .as_str()
            .unwrap()
            .contains("edit sub-agent"));
        let names: Vec<&str> = child["tools"][0]["functionDeclarations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "read_file",
                "create_file",
                "edit_file",
                "search_files",
                "apply_patch",
                "glob_files",
                "run_command",
                "complete_task"
            ]
        );
        assert!(names.contains(&"create_file") && names.contains(&"run_command"));
        assert!(!names.contains(&"explore") && !names.contains(&"ask_user"));
        assert!(!names.contains(&"web_fetch"));
        // the parent gets the report plus what the child changed
        let back = &posts[4];
        assert!(back.contains("EDIT-REPORT"), "{back}");
        assert!(back.contains("files_written"), "{back}");
        assert!(
            back.contains("\\\"files_written\\\":[\\\"a.txt\\\"]"),
            "{back}"
        );
        assert!(back.contains("\\\"denials\\\":1"), "{back}");
    }

    #[test]
    fn read_only_role_cannot_start_an_edit_child() {
        let tmp = tempfile::tempdir().unwrap();
        let edit = json!({"functionCall":{"name":"explore","args":{
            "task":"write x.txt","kind":"edit"}}});
        let svc = Arc::new(service(
            tmp.path(),
            Script::new(vec![
                call_turn(vec![edit]),
                // if a child were started it would consume this write
                call_turn(vec![create_call("x.txt", "X")]),
                call_turn(vec![complete_call("done")]),
                call_turn(vec![complete_call("done")]),
            ]),
        ));
        let snap = svc
            .begin_in_workspace_with_role(
                "review only",
                "gemini",
                None,
                Some(budgets()),
                None,
                Role::Adversary,
                false,
                SessionMeta::default(),
            )
            .unwrap();
        auto_approve(svc.clone(), snap.id.clone());
        let done = wait_terminal(&svc, &snap.id, 60_000);
        let ws = tmp.path().join("runs").join(&snap.id).join("workspace");
        assert!(!ws.join("x.txt").exists(), "{:?}", done.events);
        assert!(!done.events.iter().any(|e| matches!(
            e,
            AgentEvent::Info { message } if message.starts_with("edit sub-agent started")
        )));
        let posts = svc.service().transport().seen();
        assert!(
            posts[1].contains("tool explore is not enabled for this run's role"),
            "{}",
            posts[1]
        );
    }

    #[test]
    fn parallel_edit_children_take_turns_at_approval_and_never_share_a_file() {
        let tmp = tempfile::tempdir().unwrap();
        let clash = json!({"functionCall":{"name":"explore","args":{
            "tasks":["write same.txt","also write same.txt"],"kind":"edit"}}});
        let split = json!({"functionCall":{"name":"explore","args":{
            "tasks":["write a.txt","write b.txt"],"kind":"edit"}}});
        let script = Script::new(vec![
            call_turn(vec![clash]),
            // both children try the same file in their only turn
            call_turn(vec![create_call("same.txt", "S"), complete_call("R")]),
            call_turn(vec![create_call("./same.txt", "S"), complete_call("R")]),
            call_turn(vec![split]),
            call_turn(vec![create_call("a.txt", "A"), complete_call("R")]),
            call_turn(vec![create_call("b.txt", "B"), complete_call("R")]),
            call_turn(vec![complete_call("done")]),
        ]);
        script.post_delay_ms.store(300, Ordering::SeqCst);
        let svc = Arc::new(service(tmp.path(), script));
        let snap = svc
            .begin("parallel edits", "gemini", Some(budgets()))
            .unwrap();
        let (svc2, id2) = (svc.clone(), snap.id.clone());
        let decided = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = decided.clone();
        std::thread::spawn(move || loop {
            let s = svc2.snapshot(&id2).expect("snapshot");
            if s.terminal_reason.is_some() {
                return;
            }
            if s.status == AgentStatus::AwaitingApproval && s.pending_approval.is_some() {
                seen.fetch_add(1, Ordering::SeqCst);
                let _ = svc2.decide(&id2, true);
            }
            std::thread::sleep(Duration::from_millis(10));
        });
        let done = wait_terminal(&svc, &snap.id, 60_000);
        // one approval for the clash (the loser is refused before asking),
        // two for the split batch, one at a time
        assert_eq!(decided.load(Ordering::SeqCst), 3, "{:?}", done.events);
        assert_eq!(
            svc.service()
                .transport()
                .max_in_flight
                .load(Ordering::SeqCst),
            2
        );
        let ws = tmp.path().join("runs").join(&snap.id).join("workspace");
        assert_eq!(fs::read_to_string(ws.join("same.txt")).unwrap(), "S");
        assert_eq!(fs::read_to_string(ws.join("a.txt")).unwrap(), "A");
        assert_eq!(fs::read_to_string(ws.join("b.txt")).unwrap(), "B");
        let posts = svc.service().transport().seen();
        let wrote = |p: &str, f: &str| p.matches(&format!("\\\"files_written\\\":[{f}]")).count();
        let same = &posts[3];
        assert_eq!(
            wrote(same, "\\\"same.txt\\\"") + wrote(same, "\\\"./same.txt\\\""),
            1,
            "{same}"
        );
        assert_eq!(wrote(same, ""), 1, "{same}");
        let split = &posts[6];
        assert_eq!(wrote(split, "\\\"a.txt\\\""), 1, "{split}");
        assert_eq!(wrote(split, "\\\"b.txt\\\""), 1, "{split}");
    }

    #[test]
    fn standing_approvals_are_capped_per_run_and_never_double_counted() {
        let mut held = Vec::new();
        for n in 0..MAX_STANDING_APPROVALS {
            grant_standing(&mut held, format!("k{n}")).expect("under the cap");
        }
        // re-granting a held key is fine and takes no new slot
        grant_standing(&mut held, "k0".into()).expect("already held");
        assert_eq!(held.len(), MAX_STANDING_APPROVALS);
        let refused = grant_standing(&mut held, "one-more".into()).unwrap_err();
        assert!(refused.contains("at most"), "{refused}");
        assert_eq!(held.len(), MAX_STANDING_APPROVALS);
    }

    #[test]
    fn allow_for_this_run_covers_only_the_exact_command_and_is_logged() {
        let tmp = tempfile::tempdir().unwrap();
        let cp = |to: &str| {
            json!({"functionCall":{"name":"run_command","args":{
            "argv":["cp","seed.txt", to]}}})
        };
        let script = Script::new(vec![
            call_turn(vec![create_call("seed.txt", "SEED")]),
            call_turn(vec![cp("x.txt")]),
            call_turn(vec![cp("x.txt")]),
            call_turn(vec![cp("y.txt")]),
            call_turn(vec![complete_call("done")]),
        ]);
        let svc = Arc::new(service(tmp.path(), script));
        let snap = svc.begin("standing", "gemini", Some(budgets())).unwrap();
        let (svc2, id2) = (svc.clone(), snap.id.clone());
        let refused_for_write = Arc::new(Mutex::new(None::<String>));
        let refused = refused_for_write.clone();
        std::thread::spawn(move || loop {
            let s = svc2.snapshot(&id2).expect("snapshot");
            if s.terminal_reason.is_some() {
                return;
            }
            if s.status == AgentStatus::AwaitingApproval {
                if let Some(call) = s.pending_approval.clone() {
                    if call.tool == "create_file" {
                        // a file write can never get a standing approval
                        *refused.lock().unwrap() = svc2.decide_always(&id2).err();
                        let _ = svc2.decide(&id2, true);
                    } else if call.summary.contains("x.txt") {
                        svc2.decide_always(&id2).expect("standing approval");
                    } else {
                        let _ = svc2.decide(&id2, true);
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        });
        let done = wait_terminal(&svc, &snap.id, 30_000);
        assert_eq!(
            refused_for_write.lock().unwrap().as_deref(),
            Some("this request can only be approved once")
        );
        let asked: Vec<String> = done
            .events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ApprovalRequired { call } => Some(call.tool.clone()),
                _ => None,
            })
            .collect();
        // create, the first cp to x.txt, and the cp to y.txt; the second
        // cp to x.txt ran under the standing approval
        assert_eq!(
            asked,
            vec!["create_file", "run_command", "run_command"],
            "{:?}",
            done.events
        );
        let granted = done
            .events
            .iter()
            .filter(|e| matches!(e, AgentEvent::StandingApprovalGranted { .. }))
            .count();
        let by_standing: Vec<&String> = done
            .events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ApprovedByStanding { command, .. } => Some(command),
                _ => None,
            })
            .collect();
        assert_eq!(granted, 1);
        assert_eq!(by_standing.len(), 1);
        assert!(by_standing[0].contains("x.txt"));
        let ws = tmp.path().join("runs").join(&snap.id).join("workspace");
        assert_eq!(fs::read_to_string(ws.join("x.txt")).unwrap(), "SEED");
        assert_eq!(fs::read_to_string(ws.join("y.txt")).unwrap(), "SEED");
        // not waiting any more: the trusted route refuses
        assert!(svc.decide_always(&snap.id).is_err());
    }

    #[test]
    fn a_command_that_changes_another_childs_file_is_reported_as_overlap() {
        let tmp = tempfile::tempdir().unwrap();
        let clash = json!({"functionCall":{"name":"explore","args":{
            "tasks":["write shared.txt","also write shared.txt"],"kind":"edit"}}});
        let cp = json!({"functionCall":{"name":"run_command","args":{
            "argv":["cp","seed.txt","shared.txt"]}}});
        let script = Script::new(vec![
            call_turn(vec![create_call("seed.txt", "SEED")]),
            call_turn(vec![clash]),
            // both children try shared.txt; one owns it, the other is refused
            call_turn(vec![create_call("shared.txt", "S")]),
            call_turn(vec![create_call("shared.txt", "S")]),
            // then both copy over it: only the non-owner's copy is an overlap
            call_turn(vec![cp.clone()]),
            call_turn(vec![cp]),
            call_turn(vec![complete_call("R")]),
            call_turn(vec![complete_call("R")]),
            call_turn(vec![complete_call("done")]),
        ]);
        script.post_delay_ms.store(200, Ordering::SeqCst);
        let svc = Arc::new(service(tmp.path(), script));
        let snap = svc.begin("overlap", "gemini", Some(budgets())).unwrap();
        let (svc2, id2) = (svc.clone(), snap.id.clone());
        std::thread::spawn(move || loop {
            let s = svc2.snapshot(&id2).expect("snapshot");
            if s.terminal_reason.is_some() {
                return;
            }
            if s.status == AgentStatus::AwaitingApproval && s.pending_approval.is_some() {
                let _ = svc2.decide(&id2, true);
            }
            std::thread::sleep(Duration::from_millis(10));
        });
        let done = wait_terminal(&svc, &snap.id, 60_000);
        let ws = tmp.path().join("runs").join(&snap.id).join("workspace");
        assert_eq!(fs::read_to_string(ws.join("shared.txt")).unwrap(), "SEED");
        let posts = svc.service().transport().seen();
        let batch = posts
            .iter()
            .find(|p| p.contains("explorers"))
            .expect("batch result");
        assert_eq!(
            batch.matches("shared.txt (edit sub-agent ").count(),
            1,
            "{batch} {:?}",
            done.events
        );
        assert!(
            posts
                .iter()
                .any(|p| p.contains("overlap: files another edit sub-agent")),
            "{posts:?}"
        );
        assert_eq!(
            posts
                .iter()
                .filter(|p| p.contains("overlap: files another edit sub-agent"))
                .count(),
            1
        );
    }

    #[test]
    fn a_file_a_command_creates_is_claimed_for_that_child() {
        let tmp = tempfile::tempdir().unwrap();
        let batch = json!({"functionCall":{"name":"explore","args":{
            "task":"make made.txt","kind":"edit"}}});
        let cp = json!({"functionCall":{"name":"run_command","args":{
            "argv":["cp","seed.txt","made.txt"]}}});
        let script = Script::new(vec![
            call_turn(vec![create_call("seed.txt", "SEED")]),
            call_turn(vec![batch]),
            call_turn(vec![cp]),
            call_turn(vec![complete_call("R")]),
            call_turn(vec![complete_call("done")]),
        ]);
        let svc = Arc::new(service(tmp.path(), script));
        let snap = svc.begin("claim", "gemini", Some(budgets())).unwrap();
        let (svc2, id2) = (svc.clone(), snap.id.clone());
        std::thread::spawn(move || loop {
            let s = svc2.snapshot(&id2).expect("snapshot");
            if s.terminal_reason.is_some() {
                return;
            }
            if s.status == AgentStatus::AwaitingApproval && s.pending_approval.is_some() {
                let _ = svc2.decide(&id2, true);
            }
            std::thread::sleep(Duration::from_millis(10));
        });
        let done = wait_terminal(&svc, &snap.id, 60_000);
        let ws = tmp.path().join("runs").join(&snap.id).join("workspace");
        assert_eq!(fs::read_to_string(ws.join("made.txt")).unwrap(), "SEED");
        let posts = svc.service().transport().seen();
        let batch = posts
            .iter()
            .find(|p| p.contains("files_written"))
            .expect("batch result");
        // the command marker plus the file it made (seed.txt was unchanged)
        assert!(
            batch.contains("files_written\\\":[\\\"(run_command)\\\",\\\"made.txt\\\"]"),
            "{batch} {:?}",
            done.events
        );
    }

    #[test]
    fn tree_snapshot_skips_build_dirs_and_finds_new_and_changed_files() {
        use explore::{tree_changes, tree_snapshot, tree_snapshot_capped};
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("node_modules/x")).unwrap();
        fs::write(root.join("src/a.rs"), "a").unwrap();
        fs::write(root.join("keep.txt"), "k").unwrap();
        fs::write(root.join("target/debug/bin"), "b").unwrap();
        fs::write(root.join(".git/HEAD"), "h").unwrap();
        fs::write(root.join("node_modules/x/i.js"), "i").unwrap();
        let before = tree_snapshot(root).unwrap();
        let mut keys: Vec<&String> = before.keys().collect();
        keys.sort();
        assert_eq!(keys, ["keep.txt", "src/a.rs"]);
        assert!(tree_changes(&before, &before).is_empty());
        fs::write(root.join("src/a.rs"), "aa").unwrap();
        fs::write(root.join("src/new.rs"), "n").unwrap();
        fs::write(root.join("target/debug/bin"), "bb").unwrap();
        let after = tree_snapshot(root).unwrap();
        assert_eq!(tree_changes(&before, &after), ["src/a.rs", "src/new.rs"]);
        // removed files are not "changed"
        fs::remove_file(root.join("keep.txt")).unwrap();
        let gone = tree_snapshot(root).unwrap();
        assert!(!tree_changes(&after, &gone).contains(&"keep.txt".to_string()));
        // order is sorted whatever the map order
        for n in 0..8 {
            fs::write(root.join(format!("z{n}.txt")), "z").unwrap();
        }
        let many = tree_snapshot(root).unwrap();
        let listed = tree_changes(&gone, &many);
        let mut sorted = listed.clone();
        sorted.sort();
        assert_eq!(listed.len(), 8);
        assert_eq!(listed, sorted);
        // a size change is seen even when the modified time is put back
        let f = root.join("src/a.rs");
        let t0 = fs::metadata(&f).unwrap().modified().unwrap();
        let snap0 = tree_snapshot(root).unwrap();
        fs::write(&f, "a much longer body").unwrap();
        fs::File::options()
            .write(true)
            .open(&f)
            .unwrap()
            .set_modified(t0)
            .unwrap();
        assert_eq!(fs::metadata(&f).unwrap().modified().unwrap(), t0);
        assert_eq!(
            tree_changes(&snap0, &tree_snapshot(root).unwrap()),
            ["src/a.rs"]
        );
        fs::remove_file(root.join("src/new.rs")).unwrap();
        for n in 0..8 {
            fs::remove_file(root.join(format!("z{n}.txt"))).unwrap();
        }
        // too many files: no snapshot
        assert!(tree_snapshot_capped(root, 1).is_some());
        assert!(tree_snapshot_capped(root, 0).is_none());
    }

    #[test]
    fn claimed_files_are_fingerprinted_and_changes_found() {
        use explore::{changed_since, claimed_by_others};
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(root.join("a.txt"), "a").unwrap();
        fs::write(root.join("b.txt"), "b").unwrap();
        let claims = Mutex::new(std::collections::HashMap::from([
            ("a.txt".to_string(), 0usize),
            ("b.txt".to_string(), 1usize),
            ("gone.txt".to_string(), 1usize),
        ]));
        // child 0 watches only what others own
        let before = claimed_by_others(root, &claims, 0);
        let paths: Vec<&str> = before.iter().map(|(p, _, _)| p.as_str()).collect();
        assert_eq!(paths, ["b.txt", "gone.txt"]);
        assert!(before[0].2.is_some() && before[1].2.is_none());
        assert!(changed_since(root, &before).is_empty());
        fs::write(root.join("b.txt"), "B").unwrap();
        fs::write(root.join("a.txt"), "A").unwrap();
        assert_eq!(changed_since(root, &before), [("b.txt".to_string(), 1)]);
        fs::write(root.join("gone.txt"), "new").unwrap();
        assert_eq!(changed_since(root, &before).len(), 2);
    }

    #[test]
    fn edit_claims_cover_patch_paths_all_or_none() {
        use explore::{claim_paths, written_paths};
        let claims = Mutex::new(std::collections::HashMap::new());
        let patch = ToolRequest::ApplyPatch {
            patch: "*** Begin Patch\n*** Update File: a.txt\n*** Move to: moved.txt\n@@\n-x\n+y\n*** Add File: c.txt\n+c\n*** End Patch".into(),
        };
        assert_eq!(written_paths(&patch), ["a.txt", "moved.txt", "c.txt"]);
        assert!(written_paths(&ToolRequest::ApplyPatch {
            patch: "not a patch".into()
        })
        .is_empty());
        assert_eq!(claim_paths(&claims, &["./a.txt".into()], 0), None);
        // child 1's patch touches a.txt, so nothing in it is claimed
        assert_eq!(
            claim_paths(&claims, &written_paths(&patch), 1),
            Some(("a.txt".into(), 0))
        );
        assert_eq!(claim_paths(&claims, &["c.txt".into()], 1), None);
        assert_eq!(
            claim_paths(&claims, &["moved.txt".into(), "c.txt".into()], 0),
            Some(("c.txt".into(), 1))
        );
        // re-claiming your own path is fine
        assert_eq!(claim_paths(&claims, &["a.txt".into()], 0), None);
    }

    #[test]
    fn edit_children_can_run_as_a_batch() {
        use explore::ExplorerKind;
        assert_eq!(
            explore::parse_explore_kind(&json!({"kind":"edit"})),
            Ok(ExplorerKind::Edit)
        );
        let two = decode_provider_calls(
            ProviderProtocol::Gemini,
            &json!({"candidates":[{"content":{"parts":[{"functionCall":{"name":"explore","args":{"tasks":["a","b"],"kind":"edit"}}}]}}]}).to_string(),
        )
        .unwrap();
        assert!(matches!(
            &two.calls[0].0,
            AgentCall::Explore { kind: ExplorerKind::Edit, tasks, .. } if tasks.len() == 2
        ));
    }

    #[test]
    fn explore_batch_runs_children_in_parallel_and_returns_every_report() {
        let tmp = tempfile::tempdir().unwrap();
        let batch = json!({"functionCall":{"name":"explore","args":{
            "tasks":["where is the config loaded?","which module owns auth?"]}}});
        let script = Script::new(vec![
            call_turn(vec![batch]),
            call_turn(vec![complete_call("REPORT-ONE")]),
            call_turn(vec![complete_call("REPORT-TWO")]),
            call_turn(vec![complete_call("done")]),
        ]);
        // every provider post takes 300ms while counted in flight; parent
        // turns are sequential, so only concurrent children can overlap
        script.post_delay_ms.store(300, Ordering::SeqCst);
        let svc = Arc::new(service(tmp.path(), script));
        let snap = svc
            .begin("parallel explore", "gemini", Some(budgets()))
            .unwrap();
        auto_approve(svc.clone(), snap.id.clone());
        let done = wait_terminal(&svc, &snap.id, 60_000);
        let reported = done
            .events
            .iter()
            .filter(|e| matches!(e, AgentEvent::Info { message } if message.starts_with("explorer sub-agent reported")))
            .count();
        assert_eq!(reported, 2, "{:?}", done.events);
        // both child requests were in flight at the same time
        assert_eq!(
            svc.service()
                .transport()
                .max_in_flight
                .load(Ordering::SeqCst),
            2
        );
        let posts = svc.service().transport().seen();
        assert!(
            posts[3].contains("REPORT-ONE") && posts[3].contains("REPORT-TWO"),
            "{}",
            posts[3]
        );
        assert!(posts[3].contains("explorers"));
        let run_dir = tmp.path().join("runs").join(&snap.id);
        let ledger = fs::read_to_string(run_dir.join("state").join("ledger.jsonl"))
            .or_else(|_| fs::read_to_string(run_dir.join("ledger.jsonl")))
            .unwrap_or_default();
        assert_eq!(ledger.matches("\"batch\":2").count(), 2, "{ledger}");
    }

    #[test]
    fn explore_task_lists_are_normalised_and_capped() {
        use super::explore::{parse_explore_tasks, EXPLORE_MAX_PARALLEL};
        assert!(parse_explore_tasks(&json!({})).is_err());
        assert!(parse_explore_tasks(&json!({"tasks":["  ", ""]})).is_err());
        assert_eq!(
            parse_explore_tasks(&json!({"tasks":["a", "a", " b "], "task":"c"})).unwrap(),
            ["a", "b", "c"]
        );
        let too_many: Vec<String> = (0..=EXPLORE_MAX_PARALLEL)
            .map(|i| format!("q{i}"))
            .collect();
        let err = parse_explore_tasks(&json!({"tasks": too_many})).unwrap_err();
        assert!(err.contains("at most"), "{err}");
    }

    #[test]
    fn older_tool_results_stay_visible_until_superseded() {
        let tmp = tempfile::tempdir().unwrap();
        let glob = json!({"functionCall":{"name":"glob_files","args":{"pattern":"*"}}});
        let svc = Arc::new(service(
            tmp.path(),
            Script::new(vec![
                call_turn(vec![create_call("a.txt", "ALPHA-ONE")]),
                call_turn(vec![read_call("a.txt")]),
                call_turn(vec![glob]),
                call_turn(vec![create_call("a.txt", "ALPHA-TWO")]),
                call_turn(vec![complete_call("done")]),
            ]),
        ));
        let snap = svc
            .begin("memory check", "gemini", Some(budgets()))
            .unwrap();
        auto_approve(svc.clone(), snap.id.clone());
        wait_terminal(&svc, &snap.id, 60_000);
        let posts = svc.service().transport().seen();
        assert!(posts.len() >= 5, "{}", posts.len());
        // turn 4's request: the turn-2 read is no longer in the exact
        // exchange, but its excerpt is still in working memory
        let state4 = posts[3].clone();
        assert!(state4.contains("earlier_observations"));
        assert!(state4.contains("ALPHA-ONE"), "{state4}");
        // turn 5's request: a.txt was rewritten, so the stale read is gone
        assert!(!posts[4].contains("ALPHA-ONE"), "{}", posts[4]);
        // the first request has no memory yet
        let first: Value = serde_json::from_str(&posts[0]).unwrap();
        let state0: Value =
            serde_json::from_str(first["contents"][0]["parts"][0]["text"].as_str().unwrap())
                .unwrap();
        assert!(state0["earlier_observations"].is_null());
    }

    #[test]
    fn web_fetch_refuses_private_and_data_channel_urls() {
        let tmp = tempfile::tempdir().unwrap();
        let local =
            json!({"functionCall":{"name":"web_fetch","args":{"url":"http://127.0.0.1:9/admin"}}});
        let smuggle = json!({"functionCall":{"name":"web_fetch","args":{"url": format!("https://x.org/{}", "c2VjcmV0".repeat(12))}}});
        let svc = Arc::new(service(
            tmp.path(),
            Script::new(vec![
                call_turn(vec![local, smuggle]),
                call_turn(vec![complete_call("done")]),
            ]),
        ));
        let snap = svc.begin("fetch check", "gemini", Some(budgets())).unwrap();
        auto_approve(svc.clone(), snap.id.clone());
        wait_terminal(&svc, &snap.id, 60_000);
        let posts = svc.service().transport().seen();
        assert!(posts[1].contains("unsafe_address"), "{}", posts[1]);
        assert!(posts[1].contains("possible data channel"), "{}", posts[1]);
        assert!(scoped_tools_for(Role::Adversary)
            .0
            .iter()
            .any(|s| s.name == "web_fetch"));
    }

    fn answer_questions(svc: Arc<Svc>, id: String, reply: Option<&'static str>) {
        std::thread::spawn(move || loop {
            let snap = svc.snapshot(&id).expect("snapshot");
            if snap.terminal_reason.is_some() {
                return;
            }
            if snap.status == AgentStatus::AwaitingAnswer && snap.pending_question.is_some() {
                let _ = svc.answer(&id, reply);
            }
            std::thread::sleep(Duration::from_millis(10));
        });
    }

    #[test]
    fn ask_user_parks_the_run_and_returns_the_trusted_answer() {
        let tmp = tempfile::tempdir().unwrap();
        let ask = json!({"functionCall":{"name":"ask_user","args":{
            "question":"Which database should the service use?",
            "choices":["Postgres", {"label":"SQLite"}, "  "]}}});
        let svc = Arc::new(service(
            tmp.path(),
            Script::new(vec![
                call_turn(vec![ask]),
                call_turn(vec![complete_call("done")]),
            ]),
        ));
        let snap = svc.begin("ask check", "gemini", Some(budgets())).unwrap();
        // not waiting yet (or already past): a stray answer is refused
        assert!(svc.answer("no-such-run", Some("x")).is_err());
        answer_questions(svc.clone(), snap.id.clone(), Some("SQLite, it is a demo"));
        let done = wait_terminal(&svc, &snap.id, 60_000);
        // the gates judge the (empty) work; the question flow itself is what matters here
        assert!(done.terminal_reason.is_some());
        assert!(done.pending_question.is_none());
        let asked = done.events.iter().find_map(|e| match e {
            AgentEvent::QuestionAsked { question } => Some(question.clone()),
            _ => None,
        });
        let asked = asked.expect("question event");
        assert_eq!(asked.question, "Which database should the service use?");
        assert_eq!(asked.choices, ["Postgres", "SQLite"]);
        assert!(done
            .events
            .iter()
            .any(|e| matches!(e, AgentEvent::QuestionResolved { answered: true, .. })));
        let posts = svc.service().transport().seen();
        assert!(posts[1].contains("user_answer"), "{}", posts[1]);
        assert!(posts[1].contains("SQLite, it is a demo"), "{}", posts[1]);
        let run_dir = tmp.path().join("runs").join(&snap.id);
        let ledger = fs::read_to_string(run_dir.join("state").join("ledger.jsonl"))
            .or_else(|_| fs::read_to_string(run_dir.join("ledger.jsonl")))
            .unwrap_or_default();
        assert!(ledger.contains("\"kind\":\"ask_user\""), "{ledger}");
    }

    #[test]
    fn ask_user_is_capped_per_run_and_a_decline_means_decide() {
        let tmp = tempfile::tempdir().unwrap();
        let ask = |n: u32| json!({"functionCall":{"name":"ask_user","args":{"question": format!("question {n}?")}}});
        let svc = Arc::new(service(
            tmp.path(),
            Script::new(vec![
                call_turn(vec![ask(1), ask(2), ask(3), ask(4)]),
                call_turn(vec![complete_call("done")]),
            ]),
        ));
        let snap = svc.begin("cap check", "gemini", Some(budgets())).unwrap();
        answer_questions(svc.clone(), snap.id.clone(), None);
        let done = wait_terminal(&svc, &snap.id, 60_000);
        // the gates judge the (empty) work; the question flow itself is what matters here
        assert!(done.terminal_reason.is_some());
        let asked = done
            .events
            .iter()
            .filter(|e| matches!(e, AgentEvent::QuestionAsked { .. }))
            .count();
        assert_eq!(asked, MAX_QUESTIONS_PER_RUN as usize);
        let posts = svc.service().transport().seen();
        assert!(posts[1].contains("declined to answer"), "{}", posts[1]);
        assert!(posts[1].contains("question limit reached"), "{}", posts[1]);
    }

    #[test]
    fn ask_user_parses_a_batch_and_caps_it() {
        let one = parse_ask_user(&json!({"question":" a? ","choices":["x"]})).unwrap();
        assert_eq!(one, vec![("a?".to_string(), vec!["x".to_string()])]);
        let batch = parse_ask_user(&json!({"questions":[
            {"question":"db?","choices":["pg",{"label":"sqlite"}]},
            {"question":"  "},
            "port?",
            {"question":"auth?"},
            {"question":"extra?"}
        ]}))
        .unwrap();
        let qs: Vec<&str> = batch.iter().map(|(q, _)| q.as_str()).collect();
        assert_eq!(qs, ["db?", "port?", "auth?"]);
        assert_eq!(batch[0].1, ["pg", "sqlite"]);
        assert!(parse_ask_user(&json!({"questions":[{"question":""}]})).is_err());
        assert!(parse_ask_user(&json!({})).is_err());
    }

    #[test]
    fn ask_user_batch_returns_every_answer_in_one_response() {
        let tmp = tempfile::tempdir().unwrap();
        let ask = |qs: Value| json!({"functionCall":{"name":"ask_user","args":{"questions": qs}}});
        let svc = Arc::new(service(
            tmp.path(),
            Script::new(vec![
                call_turn(vec![ask(json!([{"question":"db?"},{"question":"port?"}]))]),
                // 2 of 3 used; this batch of 2 gets one asked, one not_asked
                call_turn(vec![ask(
                    json!([{"question":"auth?"},{"question":"cache?"}]),
                )]),
                call_turn(vec![complete_call("done")]),
            ]),
        ));
        let snap = svc.begin("batch check", "gemini", Some(budgets())).unwrap();
        answer_questions(svc.clone(), snap.id.clone(), Some("yes"));
        let done = wait_terminal(&svc, &snap.id, 60_000);
        assert!(done.terminal_reason.is_some());
        let asked: Vec<String> = done
            .events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::QuestionAsked { question } => Some(question.question.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(asked, ["db?", "port?", "auth?"]);
        let first_asked = done.events.iter().find_map(|e| match e {
            AgentEvent::QuestionAsked { question } => Some(question.clone()),
            _ => None,
        });
        let first_asked = first_asked.unwrap();
        assert_eq!(
            (first_asked.batch_index, first_asked.batch_total),
            (Some(1), Some(2))
        );
        let posts = svc.service().transport().seen();
        let first: Vec<_> = posts[1]
            .match_indices(r#"\"status\":\"answered\""#)
            .collect();
        assert_eq!(first.len(), 2, "{}", posts[1]);
        assert!(posts[1].contains("port?"), "{}", posts[1]);
        assert!(posts[2].contains("not_asked"), "{}", posts[2]);
        assert!(posts[2].contains("cache?"), "{}", posts[2]);
    }

    #[test]
    fn answer_many_answers_a_whole_batch_without_parking_again() {
        let tmp = tempfile::tempdir().unwrap();
        let ask = json!({"functionCall":{"name":"ask_user","args":{"questions":[
            {"question":"db?","choices":["pg"]},{"question":"port?"},{"question":"auth?"}]}}});
        let svc = Arc::new(service(
            tmp.path(),
            Script::new(vec![
                call_turn(vec![ask]),
                call_turn(vec![complete_call("done")]),
            ]),
        ));
        let snap = svc.begin("batch form", "gemini", Some(budgets())).unwrap();
        assert!(svc.answer_many(&snap.id, &[Some("x".into())]).is_err());
        let t0 = Instant::now();
        let parked = loop {
            let s = svc.snapshot(&snap.id).unwrap();
            if s.status == AgentStatus::AwaitingAnswer {
                break s;
            }
            assert!(t0.elapsed() < Duration::from_secs(30), "never asked");
            std::thread::sleep(Duration::from_millis(10));
        };
        let q = parked.pending_question.unwrap();
        assert_eq!(q.batch.len(), 3);
        assert_eq!(q.batch[0].choices, ["pg"]);
        assert!(svc.answer_many(&snap.id, &[]).is_err());
        // one extra entry past the batch is ignored
        svc.answer_many(
            &snap.id,
            &[
                Some("pg".into()),
                Some("  ".into()),
                Some("token".into()),
                Some("extra".into()),
            ],
        )
        .unwrap();
        let done = wait_terminal(&svc, &snap.id, 60_000);
        assert!(done.terminal_reason.is_some());
        let parks = done
            .events
            .iter()
            .filter(|e| matches!(e, AgentEvent::QuestionAsked { .. }))
            .count();
        assert_eq!(parks, 3);
        let posts = svc.service().transport().seen();
        let reply = &posts[1];
        assert!(reply.contains(r#"\"answer\":\"pg\""#), "{reply}");
        assert!(reply.contains(r#"\"status\":\"declined\""#), "{reply}");
        assert!(reply.contains(r#"\"answer\":\"token\""#), "{reply}");
        assert!(!reply.contains("extra"), "{reply}");
        assert!(!reply.contains(r#"\"status\":\"timeout\""#), "{reply}");
    }

    #[test]
    fn cancelling_while_a_question_is_open_ends_the_run() {
        let tmp = tempfile::tempdir().unwrap();
        let ask = json!({"functionCall":{"name":"ask_user","args":{"question":"ok?"}}});
        let svc = Arc::new(service(
            tmp.path(),
            Script::new(vec![
                call_turn(vec![ask]),
                call_turn(vec![complete_call("done")]),
            ]),
        ));
        let snap = svc
            .begin("cancel check", "gemini", Some(budgets()))
            .unwrap();
        let t0 = Instant::now();
        while svc.snapshot(&snap.id).unwrap().status != AgentStatus::AwaitingAnswer {
            assert!(t0.elapsed() < Duration::from_secs(30), "never asked");
            std::thread::sleep(Duration::from_millis(10));
        }
        svc.cancel(&snap.id).unwrap();
        let done = wait_terminal(&svc, &snap.id, 60_000);
        assert_eq!(done.status, AgentStatus::Cancelled);
        assert!(svc.answer(&snap.id, Some("late")).is_err());
    }

    #[test]
    fn search_seeds_come_from_sites_and_real_domains_in_the_query() {
        // model-supplied sites: bare domains and URLs, deduped
        let s = search_seeds(
            "tokio spawn blocking",
            &[
                "docs.rs".into(),
                "https://docs.rs/".into(),
                "https://tokio.rs/tokio/tutorial".into(),
            ],
        );
        assert_eq!(s, ["https://docs.rs/", "https://tokio.rs/tokio/tutorial"]);
        // domains in free text need a common ending; file names are not sites
        let q = search_seeds(
            "fix main.rs and config.json per developer.mozilla.org (see https://example.com/a)",
            &[],
        );
        assert_eq!(
            q,
            ["https://developer.mozilla.org/", "https://example.com/a"]
        );
        // private and malformed targets never become seeds
        let bad = search_seeds(
            "x",
            &[
                "http://127.0.0.1/".into(),
                "localhost".into(),
                "e.g.".into(),
                "a@b.com".into(),
                "file:///etc/passwd".into(),
            ],
        );
        assert!(bad.is_empty(), "{bad:?}");
        // capped
        let many: Vec<String> = (0..9).map(|i| format!("site{i}.org")).collect();
        assert_eq!(search_seeds("q", &many).len(), MAX_SEEDS);
        assert!(search_seeds("how do closures work", &[]).is_empty());
    }

    /// Live smoke test (network): the keyless REX engine, seeded the way
    /// the agent seeds it, finds real pages on a public docs site.
    /// `cargo test -p rex-providers live_seeded_search -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_seeded_search_on_public_docs() {
        let seeds = search_seeds(
            "rust closures capture environment doc.rust-lang.org",
            &["https://doc.rust-lang.org/book/ch13-01-closures.html".into()],
        );
        assert!(!seeds.is_empty(), "no seeds");
        let request: SearchRequest = serde_json::from_value(json!({
            "query": "closures capture environment", "seeds": seeds, "max_results": 5
        }))
        .unwrap();
        let resp = rex_search::SearchEngine::default().search(request);
        for e in resp.evidence.iter().take(5) {
            eprintln!("{} | {:?}", e.url, e.title);
        }
        eprintln!("coverage: {:?}", resp.coverage.disclaimer);
        assert!(
            resp.evidence
                .iter()
                .any(|e| e.url.contains("doc.rust-lang.org")),
            "no evidence from the seeded site"
        );
    }

    #[test]
    fn ask_user_args_are_normalised_and_capped() {
        assert!(parse_ask_user(&json!({})).is_err());
        assert!(parse_ask_user(&json!({"question":"   "})).is_err());
        let long = "q".repeat(QUESTION_CHARS + 50);
        let (q, c) = parse_one_question(&json!({"question": long, "choices":
            ["a", {"text":"b"}, 7, "", "c", "d", "e"]}))
        .unwrap();
        assert_eq!(q.chars().count(), QUESTION_CHARS);
        assert_eq!(c, ["a", "b", "c", "d"]);
    }

    #[test]
    fn finished_run_writes_can_be_undone_from_the_journal() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = Arc::new(service(
            tmp.path(),
            Script::new(vec![
                call_turn(vec![create_call("x.txt", "agent text")]),
                call_turn(vec![complete_call("done")]),
            ]),
        ));
        let snap = svc
            .begin("write then finish", "gemini", Some(budgets()))
            .unwrap();
        auto_approve(svc.clone(), snap.id.clone());
        wait_terminal(&svc, &snap.id, 60_000);
        let file = tmp
            .path()
            .join("runs")
            .join(&snap.id)
            .join("workspace")
            .join("x.txt");
        assert!(file.exists());
        let undone = svc.undo_last_write(&snap.id).unwrap();
        assert_eq!(undone.tool, "create_file");
        assert!(!file.exists());
        assert!(svc
            .undo_last_write(&snap.id)
            .unwrap_err()
            .contains("nothing to undo"));
        assert!(svc.undo_last_write("no-such-run").is_err());
    }

    #[test]
    fn finished_run_can_be_rewound_in_one_call() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = Arc::new(service(
            tmp.path(),
            Script::new(vec![
                call_turn(vec![create_call("x.txt", "one")]),
                call_turn(vec![create_call("y.txt", "two")]),
                call_turn(vec![create_call("x.txt", "three")]),
                call_turn(vec![complete_call("done")]),
            ]),
        ));
        let snap = svc
            .begin("three writes", "gemini", Some(budgets()))
            .unwrap();
        auto_approve(svc.clone(), snap.id.clone());
        wait_terminal(&svc, &snap.id, 60_000);
        let ws = tmp.path().join("runs").join(&snap.id).join("workspace");
        assert_eq!(fs::read_to_string(ws.join("x.txt")).unwrap(), "three");
        let undone = svc.undo_to_write(&snap.id, 1).unwrap();
        assert_eq!(undone.len(), 2);
        assert_eq!(fs::read_to_string(ws.join("x.txt")).unwrap(), "one");
        assert!(!ws.join("y.txt").exists());
        assert!(svc
            .undo_after_call(&snap.id, "no-such-call")
            .unwrap_err()
            .contains("no journaled write"));
        // rewind to just after the first write, by its call id
        let first = fs::read_to_string(
            tmp.path()
                .join("runs")
                .join(&snap.id)
                .join("state")
                .join("journal")
                .join("journal.jsonl"),
        )
        .unwrap();
        let first: Value = serde_json::from_str(first.lines().next().unwrap()).unwrap();
        assert!(svc
            .undo_after_call(&snap.id, first["call_id"].as_str().unwrap())
            .unwrap_err()
            .contains("nothing to undo"));
        assert_eq!(svc.undo_to_write(&snap.id, 0).unwrap().len(), 1);
        assert!(!ws.join("x.txt").exists());
        assert!(svc.undo_to_write("no-such-run", 0).is_err());
    }

    #[test]
    fn slightly_off_tool_calls_are_repaired_before_decoding() {
        let got = decode_gemini_calls(&call_turn(vec![
            json!({"functionCall":{"name":"Search_Files","args":{"query":"x","max_results":"5","regex":"TRUE"}}}),
            json!({"functionCall":{"name":"read-file","args":{"path":"a.rs","limit":20.0}}}),
            json!({"functionCall":{"name":"Explore","args":{"task":"q"}}}),
            json!({"functionCall":{"name":"Frobnicate","args":{}}}),
        ]))
        .unwrap();
        assert!(matches!(
            &got.calls[0].0,
            AgentCall::Tool {
                request: ToolRequest::SearchFiles {
                    max_results: Some(5),
                    regex: Some(true),
                    ..
                },
                ..
            }
        ));
        assert!(matches!(
            &got.calls[1].0,
            AgentCall::Tool {
                request: ToolRequest::ReadFile {
                    limit: Some(20),
                    ..
                },
                ..
            }
        ));
        assert!(matches!(&got.calls[2].0, AgentCall::Explore { .. }));
        assert!(matches!(&got.calls[3].0, AgentCall::BadCall { name, .. } if name == "Frobnicate"));
        assert_eq!(
            got.repairs,
            [
                "tool name 'Search_Files' read as 'search_files'",
                "search_files: fixed argument types for max_results, regex",
                "tool name 'read-file' read as 'read_file'",
                "read_file: fixed argument types for limit",
                "tool name 'Explore' read as 'explore'",
            ]
        );
        let openai = json!({"choices":[{"message":{"tool_calls":[
            {"id":"c1","function":{"name":"RUN_COMMAND","arguments":"{\"argv\":\"[\\\"ls\\\",\\\"-a\\\"]\",\"timeout_ms\":\"900\"}"}}
        ]}}]}).to_string();
        let got = decode_openai_calls(&openai).unwrap();
        assert!(
            matches!(&got.calls[0].0, AgentCall::Tool { request: ToolRequest::RunCommand { argv, timeout_ms: Some(900), .. }, .. } if argv == &["ls", "-a"])
        );
        assert_eq!(got.repairs.len(), 2);
        let anthropic = json!({"content":[{"type":"tool_use","id":"t1","name":"Glob_Files","input":{"pattern":"*.rs"}}]}).to_string();
        let got = decode_anthropic_calls(&anthropic).unwrap();
        assert!(matches!(
            &got.calls[0].0,
            AgentCall::Tool {
                request: ToolRequest::GlobFiles { .. },
                ..
            }
        ));
        assert_eq!(got.repairs, ["tool name 'Glob_Files' read as 'glob_files'"]);
        let clean = decode_anthropic_calls(&json!({"content":[{"type":"tool_use","id":"t2","name":"glob_files","input":{"pattern":"*"}}]}).to_string()).unwrap();
        assert!(clean.repairs.is_empty());
    }

    #[test]
    fn explore_decodes_and_rejects_empty_task() {
        let ok = decode_gemini_calls(&call_turn(vec![
            json!({"functionCall":{"name":"explore","args":{"task":"q"}}}),
        ]))
        .unwrap();
        assert!(matches!(&ok.calls[0].0, AgentCall::Explore { tasks, .. } if tasks == &["q"]));
        let bad = decode_gemini_calls(&call_turn(vec![
            json!({"functionCall":{"name":"explore","args":{"task":"  "}}}),
        ]))
        .unwrap();
        assert!(matches!(&bad.calls[0].0, AgentCall::BadCall { .. }));
        let (_, v, h) = assemble_run_prompt(Role::Adversary, &scoped_tools_for(Role::Adversary).0);
        assert!(!v.is_empty() && !h.is_empty());
        assert!(scoped_tools_for(Role::Adversary)
            .0
            .iter()
            .any(|s| s.name == "explore"));
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
                false,
                SessionMeta::default(),
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

    // ---- plan mode --------------------------------------------------------

    #[test]
    fn plan_only_declarations_offer_exactly_update_plan() {
        let decls = plan_only_declarations();
        assert_eq!(decls.len(), 1, "the plan turn must offer exactly one tool");
        assert_eq!(
            decls[0].get("name").and_then(|n| n.as_str()),
            Some("update_plan")
        );
    }

    fn begin_plan_mode(
        svc: &Arc<Svc>,
        root: &Path,
        ws_name: &str,
        task: &str,
    ) -> (String, PathBuf) {
        // The workspace must live under the runs root, which the service
        // only creates lazily; make it exist before begin canonicalizes.
        fs::create_dir_all(root.join("runs")).unwrap();
        let ws = root.join("runs").join(ws_name);
        let snap = svc
            .begin_in_workspace_with_role(
                task,
                "gemini",
                None,
                Some(budgets()),
                Some(ws.clone()),
                Role::Worker,
                true,
                SessionMeta::default(),
            )
            .expect("plan-mode begin");
        (snap.id, ws)
    }

    fn wait_plan_park(svc: &Arc<Svc>, id: &str) -> AgentSnapshot {
        wait_status(svc, id, AgentStatus::AwaitingPlan)
    }

    fn wait_status(svc: &Arc<Svc>, id: &str, want: AgentStatus) -> AgentSnapshot {
        let start = Instant::now();
        loop {
            let s = svc.snapshot(id).expect("snapshot");
            if s.status == want {
                return s;
            }
            assert!(
                start.elapsed().as_secs() < 30,
                "never reached {want:?}: {:?}",
                s.status
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn plan_mode_parks_for_approval_and_denial_mutates_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = Arc::new(service(
            tmp.path(),
            Script::new(vec![call_turn(vec![plan_call(vec![
                ("1", "write the page", "pending"),
                ("2", "verify it", "pending"),
            ])])]),
        ));
        let (id, ws) = begin_plan_mode(&svc, tmp.path(), "plan-deny-ws", "plan then stop");
        let parked = wait_plan_park(&svc, &id);
        assert_eq!(
            parked.plan.len(),
            2,
            "the proposed plan is visible for review"
        );
        assert_eq!(
            parked.tool_calls, 0,
            "the planning turn is pre-work, not execution"
        );
        assert!(
            parked
                .events
                .iter()
                .any(|e| matches!(e, AgentEvent::PlanApprovalRequired { .. })),
            "the UI is told a plan decision is required"
        );
        // Denial ends the run before any tool executes.
        let denied_snap = svc.decide_plan(&id, false).expect("decide plan");
        assert!(
            denied_snap
                .events
                .iter()
                .any(|e| matches!(e, AgentEvent::PlanApprovalResolved { approved: false })),
            "the denial is recorded as an event"
        );
        let done = wait_terminal(&svc, &id, 15_000);
        assert!(matches!(done.terminal_reason, Some(TerminalReason::Denied)));
        assert_eq!(done.status, AgentStatus::Denied);
        assert!(
            !ws.join("index.html").exists(),
            "a denied plan mutates nothing"
        );
        // The gate is single-shot: a second decision fails closed.
        assert!(svc.decide_plan(&id, false).is_err());
    }

    #[test]
    fn plan_mode_approval_continues_into_execution() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = Arc::new(service(
            tmp.path(),
            Script::new(vec![
                // Consumed by the gate, pre-approval: the plan proposal.
                call_turn(vec![plan_call(vec![("1", "write the notes", "pending")])]),
                // Post-approval execution. A plain text file keeps the
                // preview-verification gate out of the picture: it needs a
                // `rex-static-preview` binary the test environment has no
                // business providing, and this test is about plan approval.
                call_turn(vec![create_call("notes.txt", "Fresh leaves.")]),
                call_turn(vec![
                    plan_call(vec![("1", "write the notes", "done")]),
                    complete_call("notes written"),
                ]),
            ]),
        ));
        let (id, ws) = begin_plan_mode(&svc, tmp.path(), "plan-approve-ws", "plan then run");
        auto_approve(svc.clone(), id.clone());
        wait_plan_park(&svc, &id);
        svc.decide_plan(&id, true).expect("approve plan");
        let done = wait_terminal(&svc, &id, 120_000);
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
        assert!(
            ws.join("notes.txt").exists(),
            "the approved run does the work"
        );
    }

    #[test]
    fn plan_mode_rogue_tool_call_in_plan_turn_fails_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = Arc::new(service(
            tmp.path(),
            Script::new(vec![call_turn(vec![
                plan_call(vec![("1", "write file", "pending")]),
                create_call("rogue.txt", "no"),
            ])]),
        ));
        let (id, ws) = begin_plan_mode(&svc, tmp.path(), "plan-rogue-ws", "rogue plan");
        let done = wait_terminal(&svc, &id, 30_000);
        match &done.terminal_reason {
            Some(TerminalReason::Blocked { detail }) => {
                assert!(
                    detail.contains("create_file"),
                    "truthful detail names the disallowed call, got: {detail}"
                );
            }
            other => panic!("expected Blocked, got {other:?}"),
        }
        assert_eq!(done.status, AgentStatus::Blocked);
        assert!(
            !ws.join("rogue.txt").exists(),
            "the rogue call never executed"
        );
    }

    #[test]
    fn plan_mode_cancel_while_awaiting_plan_ends_cancelled_with_no_tool_run() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = Arc::new(service(
            tmp.path(),
            Script::new(vec![
                call_turn(vec![plan_call(vec![("1", "write file", "pending")])]),
                call_turn(vec![create_call("never.txt", "must not exist")]),
            ]),
        ));
        let (id, ws) = begin_plan_mode(&svc, tmp.path(), "plan-cancel-ws", "cancel me");
        wait_plan_park(&svc, &id);
        svc.cancel(&id).expect("cancel while awaiting plan");
        let done = wait_terminal(&svc, &id, 30_000);
        assert!(
            matches!(done.terminal_reason, Some(TerminalReason::Cancelled)),
            "got {:?}",
            done.terminal_reason
        );
        assert!(
            !ws.join("never.txt").exists(),
            "no tool ran after cancellation at the plan gate"
        );
        assert!(
            !done
                .events
                .iter()
                .any(|e| matches!(e, AgentEvent::PlanApprovalResolved { .. })),
            "cancellation records no plan decision"
        );
        svc.teardown(&id).unwrap();
    }

    #[test]
    fn plan_decision_outside_plan_mode_fails_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = Arc::new(service(tmp.path(), Script::new(vec![text_turn("idle")])));
        let snap = svc.begin("plain run", "gemini", Some(budgets())).unwrap();
        assert!(svc.decide_plan(&snap.id, true).is_err());
        assert!(svc.decide_plan(&snap.id, false).is_err());
        assert!(svc.decide_plan("no-such-run", true).is_err());
        svc.cancel(&snap.id).unwrap();
    }

    #[test]
    fn plan_mode_approval_is_checkpointed_and_resume_skips_the_gate() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let run_id;
        let ws;
        {
            // Phase 1 runs the plan gate, does one approved tool call, then
            // parks at a second tool approval that is deliberately never
            // granted: the halt below always lands on a parked run, never in
            // a race with the stall detector.
            let svc = Arc::new(service(
                &root,
                Script::new(vec![
                    call_turn(vec![plan_call(vec![("1", "write the notes", "pending")])]),
                    call_turn(vec![create_call("notes.txt", "Fresh leaves.")]),
                    call_turn(vec![create_call("blocker.txt", "never approved")]),
                ]),
            ));
            let (id, w) = begin_plan_mode(&svc, &root, "plan-resume-ws", "plan then resume");
            run_id = id;
            ws = w;
            wait_plan_park(&svc, &run_id);
            svc.decide_plan(&run_id, true).expect("approve plan");
            // Approve only the first tool call; the second parks forever.
            wait_status(&svc, &run_id, AgentStatus::AwaitingApproval);
            svc.decide(&run_id, true).expect("approve notes.txt");
            // notes.txt existing proves the first approval resolved; the
            // next AwaitingApproval is therefore the blocker park.
            let start = Instant::now();
            loop {
                let snap = svc.snapshot(&run_id).expect("snapshot");
                if ws.join("notes.txt").exists() && snap.status == AgentStatus::AwaitingApproval {
                    break;
                }
                assert!(
                    start.elapsed().as_secs() < 30,
                    "run never parked at the blocker approval"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            std::thread::sleep(Duration::from_millis(200));
            assert_eq!(
                svc.snapshot(&run_id).expect("snapshot").status,
                AgentStatus::AwaitingApproval,
                "the blocker park is stable: the run is not drifting toward a terminal state"
            );
            // Kill the process mid-run: no terminal state is written.
            svc.halt_without_terminal(&run_id);
            std::thread::sleep(Duration::from_millis(100));
            // The approval marker is durable on disk before execution began.
            let cp: serde_json::Value = read_json(
                &root
                    .join("runs")
                    .join(&run_id)
                    .join("state")
                    .join("checkpoint.json"),
            )
            .expect("checkpoint persisted");
            assert_eq!(
                cp.get("plan_approved"),
                Some(&serde_json::Value::Bool(true)),
                "approval is checkpointed, got: {cp}"
            );
        }
        // A new service over the same runs root resumes from disk.
        let svc2 = Arc::new(service(
            &root,
            Script::new(vec![
                call_turn(vec![create_call("notes2.txt", "after resume")]),
                call_turn(vec![
                    plan_call(vec![("1", "write the notes", "done")]),
                    complete_call("notes written"),
                ]),
            ]),
        ));
        let resumed = svc2.resume(&run_id).expect("resume approved run");
        assert!(resumed.step >= 1, "resumed at the checkpointed step");
        auto_approve(svc2.clone(), run_id.clone());
        let done = wait_terminal(&svc2, &run_id, 120_000);
        assert!(
            matches!(done.terminal_reason, Some(TerminalReason::Completed)),
            "got {:?}",
            done.terminal_reason
        );
        assert!(
            !done
                .events
                .iter()
                .any(|e| matches!(e, AgentEvent::PlanApprovalRequired { .. })),
            "the resumed run never re-entered the plan gate"
        );
        svc2.teardown(&run_id).unwrap();
    }

    #[test]
    fn plan_mode_unapproved_run_reparks_on_resume() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let run_id;
        {
            let svc = Arc::new(service(
                &root,
                Script::new(vec![call_turn(vec![plan_call(vec![(
                    "1",
                    "write the notes",
                    "pending",
                )])])]),
            ));
            let (id, _ws) = begin_plan_mode(&svc, &root, "plan-repark-ws", "park then resume");
            run_id = id;
            // Park at the gate, approve nothing, then die.
            wait_plan_park(&svc, &run_id);
            svc.halt_without_terminal(&run_id);
            std::thread::sleep(Duration::from_millis(100));
        }
        // Resume: the gate must run again, never be skipped.
        let svc2 = Arc::new(service(
            &root,
            Script::new(vec![
                call_turn(vec![plan_call(vec![("1", "write the notes", "pending")])]),
                call_turn(vec![create_call("notes.txt", "Fresh leaves.")]),
                call_turn(vec![
                    plan_call(vec![("1", "write the notes", "done")]),
                    complete_call("notes written"),
                ]),
            ]),
        ));
        svc2.resume(&run_id).expect("resume parked run");
        wait_plan_park(&svc2, &run_id);
        let parked = svc2.snapshot(&run_id).expect("snapshot");
        assert!(
            parked
                .events
                .iter()
                .any(|e| matches!(e, AgentEvent::PlanApprovalRequired { .. })),
            "the resumed run re-parked for plan approval instead of bypassing it"
        );
        auto_approve(svc2.clone(), run_id.clone());
        svc2.decide_plan(&run_id, true).expect("approve plan");
        let done = wait_terminal(&svc2, &run_id, 120_000);
        assert!(
            matches!(done.terminal_reason, Some(TerminalReason::Completed)),
            "got {:?}",
            done.terminal_reason
        );
        svc2.teardown(&run_id).unwrap();
    }
}
