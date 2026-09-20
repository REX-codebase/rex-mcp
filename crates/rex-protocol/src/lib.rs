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
pub const PROTOCOL_VERSION: &str = "1.0";

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
        matches!(self, TaskState::Completed | TaskState::Failed | TaskState::Cancelled)
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

/// rex_execute request: create or resume one durable task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecuteRequest {
    /// Idempotency key. Same key + same task text resumes the same task.
    pub request_id: String,
    pub task: String,
    /// Optional explicit task id for resume-by-id from a fresh session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub host: HostKind,
    /// Operator identity declaration: human at the keyboard, or an agent
    /// operating under its own authority.
    pub operator_is_agent: bool,
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
    pub next: Option<ActionSpec>,
    pub lease: LeaseView,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditRequest {
    pub task_id: String,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRequest {
    pub task_id: String,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestRequest {
    pub task_id: String,
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
    pub lease_epoch: u64,
    pub action_id: String,
    /// Free-form account of what was done.
    pub narrative: String,
    /// Machine-readable evidence pointers (receipts, captures).
    #[serde(default)]
    pub evidence: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitResponse {
    pub state: TaskState,
    pub accepted: bool,
    /// Present on rejection: what to repair before resubmitting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair: Option<String>,
    pub next: Option<ActionSpec>,
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
    pub budgets: BudgetView,
    pub last_event_seq: u64,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelResponse {
    pub task_id: String,
    pub state: TaskState,
    pub final_reason: String,
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
                "rex_execute", "rex_next", "rex_read", "rex_edit", "rex_search", "rex_run",
                "rex_test", "rex_submit", "rex_status", "rex_events", "rex_result", "rex_cancel",
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
        assert_eq!(serde_json::to_string(&TaskState::Created).unwrap(), "\"created\"");
        assert_eq!(serde_json::to_string(&TaskState::Verifying).unwrap(), "\"verifying\"");
        assert_eq!(serde_json::to_string(&TaskState::Completed).unwrap(), "\"completed\"");
        assert_eq!(serde_json::to_string(&ErrorCode::StaleLease).unwrap(), "\"stale_lease\"");
        assert_eq!(serde_json::to_string(&ErrorCode::GateFailed).unwrap(), "\"gate_failed\"");
        assert_eq!(serde_json::to_string(&HostKind::ClaudeCode).unwrap(), "\"claude_code\"");
        assert_eq!(serde_json::to_string(&HostKind::GenericAgent).unwrap(), "\"generic_agent\"");
        // Round trip every variant of each enum.
        for s in [
            TaskState::Created, TaskState::Active, TaskState::Verifying, TaskState::Completed,
            TaskState::Failed, TaskState::Cancelled,
        ] {
            let back: TaskState = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
            assert_eq!(back, s);
        }
        for c in [
            ErrorCode::VersionMismatch, ErrorCode::UnknownTool, ErrorCode::MalformedRequest,
            ErrorCode::Unauthorized, ErrorCode::TaskNotFound, ErrorCode::TaskTerminal,
            ErrorCode::IdempotencyConflict, ErrorCode::StaleLease, ErrorCode::LeaseConflict,
            ErrorCode::ScopeDenied, ErrorCode::ApprovalRequired, ErrorCode::BudgetExceeded,
            ErrorCode::GateFailed, ErrorCode::NoResult, ErrorCode::Internal,
        ] {
            let back: ErrorCode = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
            assert_eq!(back, c);
        }
    }

    #[test]
    fn optional_fields_omit_and_default() {
        // Optional fields omitted on the wire must deserialize as None
        // (forward compatibility for minor additions).
        let json = r#"{"request_id":"r1","task":"do it","host":"claude_code","operator_is_agent":true}"#;
        let req: ExecuteRequest = serde_json::from_str(json).unwrap();
        assert!(req.task_id.is_none() && req.budgets.is_none() && req.proof.is_none() && req.plan.is_none());
        // ...and serializing a None-heavy request omits them.
        let out = serde_json::to_string(&req).unwrap();
        assert!(!out.contains("task_id"));
        assert!(!out.contains("budgets"));
        assert!(!out.contains("plan"));

        let ev: EventsRequest = serde_json::from_str(r#"{"task_id":"task-1"}"#).unwrap();
        assert_eq!(ev.after_seq, 0);
        assert!(ev.limit.is_none());

        let sub: SubmitRequest = serde_json::from_str(
            r#"{"task_id":"task-1","lease_epoch":1,"action_id":"a","narrative":"done"}"#,
        ).unwrap();
        assert!(sub.evidence.is_empty());
    }

    #[test]
    fn protocol_version_is_v1() {
        assert_eq!(PROTOCOL_VERSION, "1.0");
    }
}
