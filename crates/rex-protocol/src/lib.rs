//! rex-protocol: the versioned contract between host agents and REX.
//!
//! Everything a host agent (Claude Code, Antigravity, the REX UI) can ask
//! REX to do is one of these typed tools. The types here are the single
//! source of truth for the daemon and the MCP server: no transport logic,
//! no policy, only the versioned shape of requests, responses, events and
//! errors.
//!
//! Protocol rules:
//! - `PROTOCOL_VERSION` is negotiated at handshake; a mismatched major
//!   version is rejected before any task state is touched.
//! - Every mutating call carries an idempotency key. Repeating a call with
//!   the same key returns the original outcome; repeating it with a
//!   different payload is a conflict.
//! - Every response carries the task's durable state so a fresh session
//!   can resume from nothing but the task id.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Wire version. Major bumps break; minor bumps add optional fields only.
pub const PROTOCOL_VERSION: &str = "2.0";

pub mod packets;
pub mod schema;

/// The complete v1 tool surface. MCP names are the snake_case strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolName {
    Execute,
    Next,
    Read,
    Edit,
    Search,
    Run,
    Test,
    Submit,
    Status,
    Events,
    Result,
    Cancel,
    UltraOpen,
    UltraSubmit,
    UltraPromote,
    Proof,
    ProofVerify,
    HumanStop,
    ArtifactPut,
}

impl ToolName {
    pub fn wire_name(self) -> &'static str {
        match self {
            ToolName::Execute => "rex_execute",
            ToolName::Next => "rex_next",
            ToolName::Read => "rex_read",
            ToolName::Edit => "rex_edit",
            ToolName::Search => "rex_search",
            ToolName::Run => "rex_run",
            ToolName::Test => "rex_test",
            ToolName::Submit => "rex_submit",
            ToolName::Status => "rex_status",
            ToolName::Events => "rex_events",
            ToolName::Result => "rex_result",
            ToolName::Cancel => "rex_cancel",
            ToolName::UltraOpen => "rex_ultra_open",
            ToolName::UltraSubmit => "rex_ultra_submit",
            ToolName::UltraPromote => "rex_ultra_promote",
            ToolName::Proof => "rex_proof",
            ToolName::ProofVerify => "rex_proof_verify",
            ToolName::HumanStop => "rex_human_stop",
            ToolName::ArtifactPut => "rex_artifact_put",
        }
    }

    pub fn from_wire_name(name: &str) -> Option<Self> {
        Some(match name {
            "rex_execute" => ToolName::Execute,
            "rex_next" => ToolName::Next,
            "rex_read" => ToolName::Read,
            "rex_edit" => ToolName::Edit,
            "rex_search" => ToolName::Search,
            "rex_run" => ToolName::Run,
            "rex_test" => ToolName::Test,
            "rex_submit" => ToolName::Submit,
            "rex_status" => ToolName::Status,
            "rex_events" => ToolName::Events,
            "rex_result" => ToolName::Result,
            "rex_cancel" => ToolName::Cancel,
            "rex_ultra_open" => ToolName::UltraOpen,
            "rex_ultra_submit" => ToolName::UltraSubmit,
            "rex_ultra_promote" => ToolName::UltraPromote,
            "rex_proof" => ToolName::Proof,
            "rex_proof_verify" => ToolName::ProofVerify,
            "rex_human_stop" => ToolName::HumanStop,
            "rex_artifact_put" => ToolName::ArtifactPut,
            _ => return None,
        })
    }

    pub fn all() -> &'static [ToolName] {
        &[
            ToolName::Execute,
            ToolName::Next,
            ToolName::Read,
            ToolName::Edit,
            ToolName::Search,
            ToolName::Run,
            ToolName::Test,
            ToolName::Submit,
            ToolName::Status,
            ToolName::Events,
            ToolName::Result,
            ToolName::Cancel,
            ToolName::UltraOpen,
            ToolName::UltraSubmit,
            ToolName::UltraPromote,
            ToolName::Proof,
            ToolName::ProofVerify,
            ToolName::HumanStop,
            ToolName::ArtifactPut,
        ]
    }
}

/// Who is at the other end of the connection. The daemon never learns
/// anything about the host's upstream account or credentials; a declared
/// host kind is enough to adapt capability negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostKind {
    Human,
    ClaudeCode,
    Codex,
    OpenCode,
    Hermes,
    Antigravity,
    GenericAgent,
}

/// Stable machine-readable error codes. New codes may be added; existing
/// meanings never change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    VersionMismatch,
    UnknownTool,
    MalformedRequest,
    Unauthorized,
    TaskNotFound,
    TaskTerminal,
    IdempotencyConflict,
    StaleLease,
    LeaseConflict,
    ScopeDenied,
    ApprovalRequired,
    BudgetExceeded,
    GateFailed,
    NoResult,
    Internal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolError {
    pub code: ErrorCode,
    pub message: String,
    /// Carry the task id when known so hosts can reattach.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

impl ProtocolError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            task_id: None,
        }
    }
    pub fn for_task(mut self, task_id: &str) -> Self {
        self.task_id = Some(task_id.to_string());
        self
    }
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}
impl std::error::Error for ProtocolError {}

/// Durable task lifecycle. Distinct from custody's internal phases: this
/// is the host-visible summary the daemon projects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// Created, intent frozen, waiting for the first action.
    Created,
    /// An action is issued and being worked.
    Active,
    /// REX is verifying submitted evidence.
    Verifying,
    /// Terminal: evidence gates passed.
    Completed,
    /// Terminal: declared or detected failure.
    Failed,
    /// Terminal: cancelled or human-stopped.
    Cancelled,
}

impl TaskState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskState::Completed | TaskState::Failed | TaskState::Cancelled
        )
    }
}

/// The one unit of work REX hands out. Exactly one is open at a time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionSpec {
    pub action_id: String,
    pub seq: u64,
    /// What the host must do, in concrete terms.
    pub instructions: String,
    /// Tool names the host may use for THIS action, subset of the task
    /// scope. Empty means reasoning only, then submit.
    pub permitted_tools: Vec<ToolName>,
    /// The acceptance test REX will apply to the submission.
    pub acceptance: String,
    /// Hard ceilings for this action.
    pub max_wall_ms: u64,
}

/// Creator-supplied, frozen, literal mobile first-view assertions. This is not
/// an AI interpretation of the brief or proof of spatial/semantic correctness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MobileResultField {
    pub name: String,
    /// Literal visible text, or one of the literal alternatives.
    pub alternatives: Vec<String>,
    /// `text` or `control` (button/link/form control).
    pub kind: String,
    /// `exact` (default, including existing tasks) or `contains`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_mode: Option<String>,
}

/// rex_execute request: create or resume one durable task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecuteRequest {
    /// Idempotency key. Same key + same task text resumes the same task.
    pub request_id: String,
    pub task: String,
    /// Optional execute-time acceptance fields frozen before any builder action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mobile_result_fields: Option<Vec<MobileResultField>>,
    /// Optional explicit task id for resume-by-id from a fresh session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Host resume handle issued at creation; required on every resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_handle: Option<String>,
    /// Optional human follow-up recorded against the same durable task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up: Option<String>,
    pub host: HostKind,
    /// Operator identity declaration: human at the keyboard, or an agent
    /// operating under its own authority.
    pub operator_is_agent: bool,
    /// Selects the durable Ultra proof route for this task.
    #[serde(default, skip_serializing_if = "is_false")]
    pub ultra: bool,
    /// Requested budget ceilings; the daemon clamps to policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budgets: Option<BudgetRequest>,
    /// Named proof recipe the task must satisfy before completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof: Option<String>,
    /// Host-proposed decomposition. REX freezes it verbatim and issues one
    /// step at a time; the host cannot edit it mid-task (freeze hash is in
    /// the audit chain). Absent means one action: the whole task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<Vec<PlanStep>>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// One host-proposed step, frozen at execute time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    pub instructions: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_calls: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_wall_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecuteResponse {
    pub task_id: String,
    pub state: TaskState,
    /// True when this call resumed an existing task instead of creating.
    pub resumed: bool,
    /// Issued at creation and rotated on every accepted resume. The host
    /// must persist the latest value; losing it leaves the task to the
    /// trusted human launcher path. Never logged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_resume_handle: Option<String>,
    /// Per-task unforgeable capability, issued at creation and re-issued on
    /// every verified resume. Required on every operational call (next,
    /// read, edit, search, run, test, submit, cancel, ultra_*). The daemon
    /// stores only its SHA-256. Never logged. Knowledge of the task id and
    /// lease epoch alone authorizes nothing (protocol 2.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_capability: Option<String>,
    pub next: Option<ActionSpec>,
    pub lease: LeaseView,
    /// Frozen creator contract, returned so builders cannot silently omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mobile_result_fields: Option<Vec<MobileResultField>>,
    /// The Fable completion discipline every operator must honor. Host
    /// agents never see REX's system prompt, so the rules travel here.
    /// Added in protocol 1.0; absent means a pre-discipline daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discipline: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseView {
    pub epoch: u64,
    pub expires_ms_from_now: u64,
    pub heartbeat_interval_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NextRequest {
    pub task_id: String,
    /// Per-task capability issued by rex_execute. Required: task id plus
    /// lease epoch is sequencing information, not authorization.
    pub capability: String,
    /// Heartbeat: keeps the lease alive and proves the host is still
    /// working. The daemon rejects calls on a lapsed lease.
    pub lease_epoch: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NextResponse {
    pub state: TaskState,
    /// None when the task is terminal or waiting on verification.
    pub next: Option<ActionSpec>,
    pub lease: LeaseView,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadRequest {
    pub task_id: String,
    /// Per-task capability issued by rex_execute. Required: task id plus
    /// lease epoch is sequencing information, not authorization.
    pub capability: String,
    pub lease_epoch: u64,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_range: Option<(u64, u64)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadResponse {
    pub content: String,
    pub truncated: bool,
    pub bytes: u64,
    /// Harness receipt id; cite it as evidence at submit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditRequest {
    pub task_id: String,
    /// Per-task capability issued by rex_execute. Required: task id plus
    /// lease epoch is sequencing information, not authorization.
    pub capability: String,
    pub lease_epoch: u64,
    pub path: String,
    /// Create when absent, else exact expected-content replacement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    pub replacement: String,
    #[serde(default)]
    pub create: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditResponse {
    pub receipt: String,
    pub bytes_written: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRequest {
    pub task_id: String,
    /// Per-task capability issued by rex_execute. Required: task id plus
    /// lease epoch is sequencing information, not authorization.
    pub capability: String,
    pub lease_epoch: u64,
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_results: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub path: String,
    pub line: u64,
    pub excerpt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    pub hits: Vec<SearchHit>,
    /// Harness receipt id; cite it as evidence at submit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRequest {
    pub task_id: String,
    /// Per-task capability issued by rex_execute. Required: task id plus
    /// lease epoch is sequencing information, not authorization.
    pub capability: String,
    pub lease_epoch: u64,
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunResponse {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub output_truncated: bool,
    /// Harness receipt id; cite it as evidence at submit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestRequest {
    pub task_id: String,
    /// Per-task capability issued by rex_execute. Required: task id plus
    /// lease epoch is sequencing information, not authorization.
    pub capability: String,
    pub lease_epoch: u64,
    /// A named recipe from the task's allowlist (e.g. "cargo-test").
    pub recipe: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResponse {
    pub recipe: String,
    pub passed: bool,
    pub summary: String,
    pub evidence_id: String,
}

/// rex_submit: deliver one action's outcome for verification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitRequest {
    pub task_id: String,
    /// Per-task capability issued by rex_execute. Required: task id plus
    /// lease epoch is sequencing information, not authorization.
    pub capability: String,
    pub lease_epoch: u64,
    pub action_id: String,
    /// Free-form account of what was done.
    pub narrative: String,
    /// Machine-readable evidence pointers (receipts, captures).
    #[serde(default)]
    pub evidence: BTreeMap<String, String>,
}

/// Informational only: matching bytes do not establish whether a transition was tested.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VisualCaptureCoverage {
    pub action_id: String,
    pub cited_slots: u8,
    pub unique_frames: u8,
    pub duplicate_slots: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitResponse {
    pub state: TaskState,
    pub accepted: bool,
    /// Present on rejection: what to repair before resubmitting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair: Option<String>,
    pub next: Option<ActionSpec>,
    /// Present for an accepted Standard visual action, never a quality verdict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visual_capture_coverage: Option<VisualCaptureCoverage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRefRequest {
    pub task_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResponse {
    pub task_id: String,
    pub state: TaskState,
    pub task: String,
    pub operator_is_agent: bool,
    pub host: HostKind,
    pub lease: LeaseView,
    pub open_action: Option<ActionSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mobile_result_fields: Option<Vec<MobileResultField>>,
    pub budgets: BudgetView,
    pub last_event_seq: u64,
    /// Latest accepted Standard visual action only; none before a visual submission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visual_capture_coverage: Option<VisualCaptureCoverage>,
    #[serde(default)]
    pub operation: packets::OperationStatus,
    #[serde(default)]
    pub packet: packets::PacketIdentity,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetView {
    pub max_tool_calls: u64,
    pub used_tool_calls: u64,
    pub max_wall_ms: u64,
    pub used_wall_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventsRequest {
    pub task_id: String,
    /// Return events with seq strictly greater than this cursor.
    #[serde(default)]
    pub after_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskEvent {
    pub seq: u64,
    pub ts_ms: u128,
    pub kind: String,
    pub detail: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventsResponse {
    pub events: Vec<TaskEvent>,
    pub last_seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultResponse {
    pub task_id: String,
    pub state: TaskState,
    /// Present only when evidence gates passed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof_bundle: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelRequest {
    pub task_id: String,
    /// Operator cancellation requires the per-task capability. The distinct
    /// final human Stop is rex_human_stop, a separate authority.
    pub capability: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelResponse {
    pub task_id: String,
    pub state: TaskState,
    pub final_reason: String,
}

/// rex_human_stop: the final human Stop. A separate authority from operator
/// cancel: it requires the daemon's human-stop token, which exists only in
/// the trusted local launcher's state dir (0600), never in MCP payloads an
/// agent can mint. It fences the task terminally in every phase, even
/// against an agent operator, and its effect cannot be rolled back.
/// rex_artifact_put: store evidence bytes in the daemon's content-addressed
/// immutable artifact store, bound to this task (and optionally a candidate
/// and round). The returned digest is what later evidence citations must
/// resolve to; bytes cannot be altered after this call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactPutRequest {
    pub task_id: String,
    /// Per-task capability issued by rex_execute. Required: task id plus
    /// lease epoch is sequencing information, not authorization.
    pub capability: String,
    pub lease_epoch: u64,
    /// Evidence kind, e.g. "screenshot", "log", "verdict".
    pub kind: String,
    /// Base64-encoded artifact bytes (standard alphabet, padding required).
    pub bytes_base64: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactPutResponse {
    pub sha256: String,
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round: Option<u64>,
    /// False when this exact binding already existed (idempotent replay).
    pub fresh: bool,
    pub recorded_ms: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HumanStopRequest {
    pub task_id: String,
    pub human_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// What the host is submitting into the Ultra external-host loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UltraSubmissionKind {
    Candidate,
    Adversary,
    Verifier,
    /// Pixel-level taste gate evidence for visual contracts.
    Visual,
}

/// rex_ultra_promote request: promote the qualified candidate's sealed
/// bundle into the task workspace with verified rollback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UltraPromoteRequest {
    pub task_id: String,
    /// Per-task capability issued by rex_execute. Required: task id plus
    /// lease epoch is sequencing information, not authorization.
    pub capability: String,
    pub lease_epoch: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UltraPromoteResponse {
    pub task_id: String,
    /// committed | rolled_back | corrupt_state
    pub state: String,
    pub candidate_id: String,
    pub bundle_hash: String,
    pub destination_hash_before: String,
    pub staging_hash: String,
    pub gates_rerun: Vec<String>,
    pub gates_not_rerun: Vec<String>,
    pub detail: String,
}

/// rex_ultra_open request: fetch the open Ultra requests for a live task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UltraOpenRequest {
    pub task_id: String,
    /// Per-task capability issued by rex_execute. Required: task id plus
    /// lease epoch is sequencing information, not authorization.
    pub capability: String,
    pub lease_epoch: u64,
    /// Host-drafted acceptance contract (JSON text; may be embedded in
    /// prose). Required the first time an Ultra task is opened: the daemon
    /// parses and validates it deterministically, freezes it, and executes
    /// its proofs itself. On later opens a supplied draft must hash-match
    /// the frozen contract. Obligations whose proof is host-judged
    /// behavior prose are rejected: the external path executes every proof.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract_draft: Option<String>,
}

/// rex_ultra_submit request: one candidate response or one evidence item.
/// For candidates the answered candidate id goes in `request_id` (and
/// `candidate_id` mirrors it); for adversary/verifier evidence `request_id`
/// is the evidence request id being answered and `candidate_id` names the
/// candidate under attack or verification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UltraSubmitRequest {
    pub task_id: String,
    /// Per-task capability issued by rex_execute. Required: task id plus
    /// lease epoch is sequencing information, not authorization.
    pub capability: String,
    pub lease_epoch: u64,
    pub kind: UltraSubmissionKind,
    pub request_id: String,
    pub candidate_id: String,
    pub response_hash: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UltraCandidateRequestView {
    /// "general" or "visual"; visual contracts demand pixel evidence.
    pub work_kind: String,
    pub candidate_id: String,
    pub contract_hash: String,
    pub task: String,
    pub obligation_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UltraEvidenceRequestView {
    pub request_id: String,
    pub candidate_id: String,
    pub contract_hash: String,
    pub kind: String,
    pub obligation_ids: Vec<String>,
    pub candidate_response_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillGateView {
    pub pack: String,
    pub id: String,
    pub command_hint: String,
    pub required: bool,
    /// True when the daemon executes this gate at promotion; false means
    /// the gate is advisory or honestly unenforceable, never a silent pass.
    #[serde(default)]
    pub executable: bool,
}

/// The compiled, version-pinned skill plan bound to a task: what was
/// selected, which executable gates apply, and what is honestly unsupported.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillPlanView {
    pub plan_hash: String,
    pub compiler_version: String,
    pub selected: Vec<String>,
    pub gates: Vec<SkillGateView>,
    pub unsupported: Vec<String>,
}

/// Host-visible Ultra loop state: truthful kernel status plus open requests.
/// Strings, not rex-ultra types, keep the protocol crate dependency-free of
/// the Ultra subsystem.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UltraViewResponse {
    pub task_id: String,
    pub status: String,
    pub kernel_state: String,
    pub candidate_requests: Vec<UltraCandidateRequestView>,
    pub evidence_requests: Vec<UltraEvidenceRequestView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_plan: Option<SkillPlanView>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_names_round_trip() {
        let names: Vec<&str> = ToolName::all().iter().map(|t| t.wire_name()).collect();
        assert_eq!(
            names,
            vec![
                "rex_execute",
                "rex_next",
                "rex_read",
                "rex_edit",
                "rex_search",
                "rex_run",
                "rex_test",
                "rex_submit",
                "rex_status",
                "rex_events",
                "rex_result",
                "rex_cancel",
                "rex_ultra_open",
                "rex_ultra_submit",
                "rex_ultra_promote",
                "rex_proof",
                "rex_proof_verify",
                "rex_human_stop",
                "rex_artifact_put",
            ]
        );
        for t in ToolName::all() {
            assert_eq!(ToolName::from_wire_name(t.wire_name()), Some(*t));
        }
        assert_eq!(ToolName::from_wire_name("rex_hack"), None);
        assert_eq!(ToolName::from_wire_name("execute"), None);
    }

    #[test]
    fn wire_names_stay_snake_case() {
        // The tool surface is a frozen contract: hosts hard-code these.
        for t in ToolName::all() {
            let w = t.wire_name();
            assert!(w.starts_with("rex_"));
            assert_eq!(w, w.to_lowercase());
        }
    }

    #[test]
    fn serde_stability_of_states_and_codes() {
        assert_eq!(
            serde_json::to_string(&TaskState::Created).unwrap(),
            "\"created\""
        );
        assert_eq!(
            serde_json::to_string(&TaskState::Verifying).unwrap(),
            "\"verifying\""
        );
        assert_eq!(
            serde_json::to_string(&TaskState::Completed).unwrap(),
            "\"completed\""
        );
        assert_eq!(
            serde_json::to_string(&ErrorCode::StaleLease).unwrap(),
            "\"stale_lease\""
        );
        assert_eq!(
            serde_json::to_string(&ErrorCode::GateFailed).unwrap(),
            "\"gate_failed\""
        );
        assert_eq!(
            serde_json::to_string(&HostKind::ClaudeCode).unwrap(),
            "\"claude_code\""
        );
        assert_eq!(
            serde_json::to_string(&HostKind::GenericAgent).unwrap(),
            "\"generic_agent\""
        );
        // Round trip every variant of each enum.
        for s in [
            TaskState::Created,
            TaskState::Active,
            TaskState::Verifying,
            TaskState::Completed,
            TaskState::Failed,
            TaskState::Cancelled,
        ] {
            let back: TaskState =
                serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
            assert_eq!(back, s);
        }
        for c in [
            ErrorCode::VersionMismatch,
            ErrorCode::UnknownTool,
            ErrorCode::MalformedRequest,
            ErrorCode::Unauthorized,
            ErrorCode::TaskNotFound,
            ErrorCode::TaskTerminal,
            ErrorCode::IdempotencyConflict,
            ErrorCode::StaleLease,
            ErrorCode::LeaseConflict,
            ErrorCode::ScopeDenied,
            ErrorCode::ApprovalRequired,
            ErrorCode::BudgetExceeded,
            ErrorCode::GateFailed,
            ErrorCode::NoResult,
            ErrorCode::Internal,
        ] {
            let back: ErrorCode =
                serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
            assert_eq!(back, c);
        }
    }

    #[test]
    fn optional_fields_omit_and_default() {
        // Optional fields omitted on the wire must deserialize as None
        // (forward compatibility for minor additions).
        let json =
            r#"{"request_id":"r1","task":"do it","host":"claude_code","operator_is_agent":true}"#;
        let req: ExecuteRequest = serde_json::from_str(json).unwrap();
        assert!(
            req.task_id.is_none()
                && req.budgets.is_none()
                && req.proof.is_none()
                && req.plan.is_none()
        );
        // ...and serializing a None-heavy request omits them.
        let out = serde_json::to_string(&req).unwrap();
        assert!(!out.contains("task_id"));
        assert!(!out.contains("budgets"));
        assert!(!out.contains("plan"));

        let ev: EventsRequest = serde_json::from_str(r#"{"task_id":"task-1"}"#).unwrap();
        assert_eq!(ev.after_seq, 0);
        assert!(ev.limit.is_none());

        let sub: SubmitRequest = serde_json::from_str(concat!(
            r#"{"task_id":"task-1","capability":"cap-0123456789abcdef0123456789abcdef0123456789abcdef","#,
            r#""lease_epoch":1,"action_id":"a","narrative":"done"}"#
        ))
        .unwrap();
        assert!(sub.evidence.is_empty());
        // Protocol 2.0 fails closed: capability-bearing calls MUST NOT
        // deserialize into a usable request without the capability.
        let no_cap = r#"{"task_id":"task-1","lease_epoch":1,"action_id":"a","narrative":"done"}"#;
        assert!(serde_json::from_str::<SubmitRequest>(no_cap).is_err());
    }

    #[test]
    fn protocol_version_is_v2_0() {
        assert_eq!(PROTOCOL_VERSION, "2.0");
    }
}
