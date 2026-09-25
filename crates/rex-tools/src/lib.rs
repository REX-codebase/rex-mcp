//! Capability-scoped local tools for REX agents.
//!
//! Models submit normalized requests. They never receive an OS handle. Every
//! request is prepared against a canonical workspace, classified, and bound to
//! an unguessable pending call. Risky calls require a separate user decision.

pub mod check;
pub mod fuzzy;
pub mod journal;
pub mod patch;
pub mod sandbox;
pub mod term;
pub mod walk;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Files up to this size and `DEFAULT_READ_LINES` lines are returned raw
/// (exact bytes, easy to copy into an edit). Larger files are paged.
const RAW_READ_BYTES: u64 = 256 * 1024;
const DEFAULT_READ_LINES: usize = 2000;
const MAX_READ_LINES: usize = 5000;
const MAX_LINE_CHARS: usize = 2000;
const MAX_PAGED_READ_BYTES: u64 = 64 * 1024 * 1024;
const MAX_LIST_ENTRIES: usize = 500;
/// MCP tool output is capped so a chatty server cannot flood the model
/// context; the truncation marker tells the model the output was cut.
const MAX_MCP_OUTPUT: usize = 64 * 1024;

/// Extract human-readable text from an MCP `tools/call` result: join
/// `type: "text"` content parts; fall back to the compact JSON.
fn mcp_text(result: &Value) -> String {
    let parts = result
        .get("content")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    if item.get("type").and_then(Value::as_str) == Some("text") {
                        item.get("text").and_then(Value::as_str)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    if parts.is_empty() {
        serde_json::to_string(result).unwrap_or_default()
    } else {
        parts
    }
}

fn truncate(text: &str, max: usize) -> String {
    clip_middle(text, max).0
}

/// Largest char boundary at or below `at`.
pub fn floor_boundary(text: &str, at: usize) -> usize {
    let mut i = at.min(text.len());
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Smallest char boundary at or above `at`.
fn ceil_boundary(text: &str, at: usize) -> usize {
    let mut i = at.min(text.len());
    while i < text.len() && !text.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Clip `text` to about `max` bytes keeping the head and the tail (where
/// build errors, test failures and exit summaries usually are), with an
/// explicit marker saying how much was omitted. Always cuts on char
/// boundaries, so multi-byte output can never panic. Returns whether
/// anything was cut.
pub fn clip_middle(text: &str, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text.to_string(), false);
    }
    let head_end = floor_boundary(text, max * 3 / 5);
    let tail_start = ceil_boundary(text, text.len() - (max - max * 3 / 5));
    let tail_start = tail_start.max(head_end);
    let omitted = tail_start - head_end;
    (
        format!(
            "{}\n[... {omitted} bytes omitted from the middle; narrow the command or read a smaller range ...]\n{}",
            &text[..head_end],
            &text[tail_start..]
        ),
        true,
    )
}

const MAX_WRITE_BYTES: usize = 2 * 1024 * 1024;
const MAX_SEARCH_RESULTS: usize = 200;
const MAX_OUTPUT_BYTES: usize = 512 * 1024;
/// Longest a model-requested command may run; larger requests are cut to it.
pub const MAX_TIMEOUT_MS: u64 = 120_000;
const MAX_PENDING: usize = 256;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RiskClass {
    Read,
    Write,
    Execute,
    Denied,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "tool", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolRequest {
    ReadFile {
        path: String,
        /// 1-based first line to return. Setting `offset` or `limit` (or
        /// reading a large file) switches to paged, line-numbered output.
        #[serde(default)]
        offset: Option<usize>,
        /// Maximum lines to return in paged mode (default 2000, max 5000).
        #[serde(default)]
        limit: Option<usize>,
    },
    CreateFile {
        path: String,
        content: String,
        overwrite: bool,
    },
    EditFile {
        path: String,
        expected: String,
        replacement: String,
        replace_all: bool,
    },
    SearchFiles {
        query: String,
        path: Option<String>,
        max_results: Option<usize>,
        /// Treat `query` as a regular expression (default: literal,
        /// case-insensitive substring).
        #[serde(default)]
        regex: Option<bool>,
        /// Only search files whose path matches this glob, e.g. `*.rs` or
        /// `src/**/*.{ts,tsx}`.
        #[serde(default)]
        include: Option<String>,
    },
    /// Atomic multi-file patch (`*** Begin Patch` envelope): add, update,
    /// move and delete files; every hunk must apply or nothing is written.
    ApplyPatch { patch: String },
    /// Find files by glob pattern, newest first.
    GlobFiles {
        pattern: String,
        path: Option<String>,
        max_results: Option<usize>,
    },
    RunCommand {
        argv: Vec<String>,
        cwd: Option<String>,
        timeout_ms: Option<u64>,
    },
    McpCall {
        server: String,
        name: String,
        arguments: Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparedCall {
    pub call_id: String,
    pub tool: String,
    pub summary: String,
    pub risk: RiskClass,
    pub approval_required: bool,
    pub policy_reason: String,
    /// Set only for a command that may get a standing approval for the
    /// rest of a run: the exact argv and working directory, as a stable
    /// key. `None` for file writes, MCP calls and any command that runs a
    /// path (a script the model could rewrite between runs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub standing_key: Option<String>,
}

/// Original vs proposed content for a pending file write, for the diff UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDiff {
    pub path: String,
    pub original: String,
    pub modified: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallState {
    PendingApproval,
    Cancelled,
    Ready,
    Denied,
    Executed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub ok: bool,
    pub tool: String,
    pub state: CallState,
    pub output: Option<String>,
    pub error: Option<ToolError>,
    pub receipt: AuditReceipt,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditReceipt {
    pub started_at_ms: u128,
    pub duration_ms: u128,
    pub workspace_root: String,
    pub target: Option<String>,
    pub command: Option<Vec<String>>,
    pub exit_code: Option<i32>,
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub output_truncated: bool,
    pub diff: Option<String>,
    pub redactions: usize,
    /// OS sandbox outcome for spawned commands: "applied: ..." or
    /// "unavailable: <reason>". Never absent for run commands.
    #[serde(default)]
    pub sandbox: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolError {
    pub kind: ErrorKind,
    pub detail: String,
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.detail)
    }
}

impl std::error::Error for ToolError {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    InvalidRequest,
    OutsideWorkspace,
    SymlinkRefused,
    NotFound,
    AlreadyExists,
    TooLarge,
    ApprovalRequired,
    UserDenied,
    Cancelled,
    PolicyDenied,
    Conflict,
    Timeout,
    OutputLimit,
    ProcessFailed,
    Io,
    AlreadyExecuted,
    UnknownCall,
}

#[derive(Clone)]
pub struct ToolRuntime {
    root: Arc<PathBuf>,
    pending: Arc<Mutex<HashMap<String, Pending>>>,
    order: Arc<Mutex<VecDeque<String>>>,
    mcp: Option<Arc<dyn McpCaller>>,
    /// Content fingerprint of each file as this runtime last read or wrote
    /// it. A write to a file whose bytes changed since then (another
    /// process, the user, a concurrent run) is refused as stale, so a model
    /// never overwrites edits it has not seen.
    seen: Arc<Mutex<HashMap<PathBuf, u64>>>,
    /// Optional on-disk write journal enabling undo (see `journal`).
    journal: Option<Arc<journal::Journal>>,
    /// When set, `read_file` on a PNG/JPEG/GIF/WebP image succeeds and
    /// queues the image for the run host to attach to the next model turn
    /// (see [`ToolRuntime::take_images`]). Off by default: a caller that
    /// cannot show images to a model gets the plain binary-file error.
    images: Option<Arc<Mutex<Vec<ImageRead>>>>,
}

/// One image read through `read_file`, waiting to be shown to the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRead {
    /// Workspace-relative path as the model asked for it.
    pub path: String,
    /// `image/png`, `image/jpeg`, `image/gif` or `image/webp`.
    pub mime: &'static str,
    pub bytes: Vec<u8>,
}

/// Largest image `read_file` will queue. Kept so the base64 form stays
/// under 5 MB, the per-image limit Anthropic documents.
pub const MAX_IMAGE_BYTES: u64 = 3_750_000;

/// Most images queued for one model turn; a further read in the same turn
/// is refused so the model knows it was not attached.
pub const MAX_QUEUED_IMAGES: usize = 3;

/// Image kind from the file's magic bytes, never from its name, so a text
/// file called `x.png` is still read as text.
pub fn sniff_image(head: &[u8]) -> Option<&'static str> {
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// How a run reaches third-party MCP servers. Implemented by the run host
/// (rex-providers), which owns the live stdio sessions; rex-tools only
/// knows this interface so the tool sandbox stays decoupled from MCP.
pub trait McpCaller: Send + Sync {
    fn call(&self, server: &str, tool: &str, arguments: &Value) -> Result<Value, String>;
}

#[derive(Clone)]
struct Pending {
    request: ToolRequest,
    state: CallState,
    prepared: PreparedCall,
}

impl ToolRuntime {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, ToolError> {
        fs::create_dir_all(root.as_ref()).map_err(io_err)?;
        let root = fs::canonicalize(root.as_ref()).map_err(io_err)?;
        if !root.is_dir() {
            return Err(err(
                ErrorKind::InvalidRequest,
                "workspace root is not a directory",
            ));
        }
        Ok(Self {
            root: Arc::new(root),
            pending: Default::default(),
            order: Default::default(),
            mcp: None,
            seen: Arc::new(Mutex::new(HashMap::new())),
            journal: None,
            images: None,
        })
    }

    /// Let `read_file` return images for the run host to attach to the next
    /// model turn instead of refusing them as binary.
    pub fn with_image_reads(mut self) -> Self {
        self.images = Some(Arc::new(Mutex::new(Vec::new())));
        self
    }

    /// Images queued by `read_file` since the last call, oldest first.
    pub fn take_images(&self) -> Vec<ImageRead> {
        match &self.images {
            Some(q) => std::mem::take(&mut *q.lock().expect("image queue poisoned")),
            None => Vec::new(),
        }
    }

    /// Attach the run's live MCP sessions. Without this, `mcp_call` requests
    /// prepare fine but fail at execution with a clear error.
    pub fn with_mcp_caller(mut self, caller: Arc<dyn McpCaller>) -> Self {
        self.mcp = Some(caller);
        self
    }

    /// Journal every successful file write under `dir` so it can be undone
    /// with [`ToolRuntime::undo_last_write`] or [`journal::Journal::undo_last`].
    pub fn with_journal(mut self, dir: impl Into<PathBuf>) -> Result<Self, ToolError> {
        self.journal = Some(Arc::new(journal::Journal::open(dir).map_err(io_err)?));
        Ok(self)
    }

    /// Undo the newest journaled write. Refuses (changing nothing) if a
    /// touched file changed after the agent wrote it.
    pub fn undo_last_write(&self) -> Result<journal::JournalEntry, ToolError> {
        let j = self.journal.as_ref().ok_or_else(|| {
            err(
                ErrorKind::InvalidRequest,
                "no write journal for this runtime",
            )
        })?;
        let entry = j
            .undo_last(&self.root)
            .map_err(|e| err(ErrorKind::Conflict, &e))?;
        for f in &entry.files {
            self.note_seen(&self.root.join(&f.path));
        }
        Ok(entry)
    }

    /// Workspace-relative paths a write request will touch, for journaling.
    fn write_targets(&self, request: &ToolRequest) -> Option<Vec<String>> {
        let rels = match request {
            ToolRequest::CreateFile { path, .. } => {
                vec![relative(&self.root, &self.resolve_for_write(path).ok()?)]
            }
            ToolRequest::EditFile { path, .. } => {
                vec![relative(
                    &self.root,
                    &self.resolve_existing(path, false).ok()?,
                )]
            }
            ToolRequest::ApplyPatch { patch } => self
                .plan_patch(patch)
                .ok()?
                .iter()
                .map(|(t, _)| relative(&self.root, t))
                .collect(),
            _ => return None,
        };
        Some(rels)
    }

    fn fingerprint(bytes: &[u8]) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut h);
        h.finish()
    }

    /// Record the current on-disk bytes of `target` as seen by the model.
    fn note_seen(&self, target: &Path) {
        if let Ok(bytes) = fs::read(target) {
            self.seen
                .lock()
                .expect("seen lock poisoned")
                .insert(target.to_path_buf(), Self::fingerprint(&bytes));
        }
    }

    /// Refuse a write when the file changed on disk since this runtime last
    /// read or wrote it. Files never read are allowed: an edit's expected
    /// text is itself a check, and creating new files needs no read.
    fn check_not_stale(&self, target: &Path) -> Result<(), ToolError> {
        let known = self
            .seen
            .lock()
            .expect("seen lock poisoned")
            .get(target)
            .copied();
        if let Some(fp) = known {
            let now = fs::read(target).map(|b| Self::fingerprint(&b)).ok();
            if now != Some(fp) {
                return Err(err(
                    ErrorKind::Conflict,
                    "file changed on disk since it was last read; read it again before writing",
                ));
            }
        }
        Ok(())
    }

    /// Refuse to overwrite a whole existing file this runtime never read
    /// or wrote: replacing content the model has not seen would drop it
    /// silently. Hermes asks for the same before `write_file` replaces a
    /// file (`full_write_baselines`, `tools/file_tools.py`); opencode's
    /// write relies on the diff shown for approval. `edit_file` and
    /// `apply_patch` need no read: their expected text is the check.
    fn check_seen_before_overwrite(&self, target: &Path) -> Result<(), ToolError> {
        if self
            .seen
            .lock()
            .expect("seen lock poisoned")
            .contains_key(target)
        {
            return Ok(());
        }
        Err(err(
            ErrorKind::Conflict,
            "file exists and was not read in this run; read it first (or change it with edit_file) before overwriting the whole file",
        ))
    }

    pub fn workspace_root(&self) -> &Path {
        &self.root
    }

    /// The checks a write would fail at execution, run before anyone is
    /// asked to approve it: an overwrite of a file not read in this run or
    /// changed since, an edit whose expected text is not in the file, a
    /// patch that does not apply. Execution repeats every check, so this
    /// only saves the user from approving a call that cannot succeed.
    /// opencode's edit tool likewise works out the replacement before it
    /// asks (`src/tool/edit.ts`). Other calls pass.
    pub fn preflight(&self, request: &ToolRequest) -> Result<(), ToolError> {
        match request {
            ToolRequest::CreateFile {
                path,
                content,
                overwrite,
            } => {
                if content.len() > MAX_WRITE_BYTES {
                    return Err(err(ErrorKind::TooLarge, "write exceeds 2 MiB limit"));
                }
                refuse_text_into_container(path)?;
                let target = self.resolve_for_write(path)?;
                if target.is_dir() {
                    return Err(folder_not_file(&relative(&self.root, &target)));
                }
                if target.exists() {
                    if !overwrite {
                        return Err(err(ErrorKind::AlreadyExists, EXISTS_NO_OVERWRITE));
                    }
                    self.check_not_stale(&target)?;
                    self.check_seen_before_overwrite(&target)?;
                }
                Ok(())
            }
            ToolRequest::EditFile {
                path,
                expected,
                replacement,
                replace_all,
            } => {
                if expected.is_empty() {
                    return Err(self.empty_expected_error(path));
                }
                let target = self.resolve_existing(path, false)?;
                let old = fs::read_to_string(&target).map_err(io_err)?;
                if old.len() > MAX_FILE_BYTES as usize {
                    return Err(err(ErrorKind::TooLarge, "file exceeds 2 MiB edit limit"));
                }
                self.check_not_stale(&target)?;
                plan_edit_checked(&old, expected, replacement, *replace_all).map(|_| ())
            }
            ToolRequest::ApplyPatch { patch } => self.plan_patch(patch).map(|_| ()),
            _ => Ok(()),
        }
    }

    pub fn prepare(&self, request: ToolRequest) -> Result<PreparedCall, ToolError> {
        validate_request(&request)?;
        let (risk, reason) = classify(&request);
        let call_id = new_call_id();
        let approval_required = matches!(risk, RiskClass::Write | RiskClass::Execute);
        let state = if matches!(risk, RiskClass::Denied) {
            CallState::Denied
        } else if approval_required {
            CallState::PendingApproval
        } else {
            CallState::Ready
        };
        let prepared = PreparedCall {
            call_id: call_id.clone(),
            tool: tool_name(&request).into(),
            summary: summarize(&request),
            risk,
            approval_required,
            policy_reason: reason.into(),
            standing_key: if approval_required {
                standing_key(&request)
            } else {
                None
            },
        };
        let mut pending = self.pending.lock().expect("pending lock poisoned");
        let mut order = self.order.lock().expect("order lock poisoned");
        while order.len() >= MAX_PENDING {
            if let Some(old) = order.pop_front() {
                pending.remove(&old);
            }
        }
        order.push_back(call_id.clone());
        pending.insert(
            call_id,
            Pending {
                request,
                state,
                prepared: prepared.clone(),
            },
        );
        Ok(prepared)
    }

    /// A file diff for a pending call, for the approval UI.
    /// Returns the original and proposed content so the UI can render a
    /// Monaco diff. Only meaningful for CreateFile/EditFile; others return
    /// None.
    pub fn pending_diff(&self, call_id: &str) -> Result<Option<FileDiff>, ToolError> {
        let pending = self.pending.lock().expect("pending lock poisoned");
        let entry = pending
            .get(call_id)
            .ok_or_else(|| err(ErrorKind::NotFound, "unknown call id"))?;
        match &entry.request {
            ToolRequest::CreateFile {
                path,
                content,
                overwrite,
            } => {
                // For a new file, original is empty. If overwrite is set and
                // the file exists, show the existing content as original.
                let original = if *overwrite {
                    self.resolve_existing(path, false)
                        .ok()
                        .and_then(|p| fs::read_to_string(p).ok())
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                Ok(Some(FileDiff {
                    path: path.clone(),
                    original,
                    modified: content.clone(),
                }))
            }
            ToolRequest::ApplyPatch { patch } => {
                let changes = self.plan_patch(patch)?;
                let mut original = String::new();
                let mut modified = String::new();
                let mut names = Vec::new();
                for (_, change) in &changes {
                    let (path, before, after) = match change {
                        patch::Change::Write {
                            path,
                            original,
                            content,
                        } => (path, original.clone().unwrap_or_default(), content.clone()),
                        patch::Change::Delete { path, original } => {
                            (path, original.clone(), String::new())
                        }
                    };
                    names.push(path.clone());
                    original.push_str(&format!("=== {path} ===\n{before}\n"));
                    modified.push_str(&format!("=== {path} ===\n{after}\n"));
                }
                Ok(Some(FileDiff {
                    path: names.join(", "),
                    original,
                    modified,
                }))
            }
            ToolRequest::EditFile {
                path,
                expected,
                replacement,
                replace_all,
            } => {
                let target = self.resolve_existing(path, false)?;
                let old = fs::read_to_string(&target).map_err(io_err)?;
                if old.len() > MAX_FILE_BYTES as usize {
                    return Err(err(ErrorKind::TooLarge, "file exceeds 2 MiB edit limit"));
                }
                // Apply the edit in memory (same logic as edit_file, without
                // writing) to produce the proposed content.
                // Same planner as execution, so the approved diff is exactly
                // what would be written if the file is unchanged.
                let new = plan_edit_checked(&old, expected, replacement, *replace_all)?.new_content;
                Ok(Some(FileDiff {
                    path: path.clone(),
                    original: old,
                    modified: new,
                }))
            }
            _ => Ok(None),
        }
    }

    /// This method must be called only from a trusted UI action, never from a model tool payload.
    pub fn resolve_approval(&self, call_id: &str, approved: bool) -> Result<CallState, ToolError> {
        let mut pending = self.pending.lock().expect("pending lock poisoned");
        let call = pending
            .get_mut(call_id)
            .ok_or_else(|| err(ErrorKind::UnknownCall, "unknown or expired call"))?;
        if call.state != CallState::PendingApproval {
            return Err(err(
                ErrorKind::InvalidRequest,
                "call is not waiting for approval",
            ));
        }
        call.state = if approved {
            CallState::Ready
        } else {
            CallState::Denied
        };
        Ok(call.state.clone())
    }

    /// Cancel a pending call from trusted controller state. Cancellation is
    /// terminal and never comes from model content.
    pub fn cancel(&self, call_id: &str) -> Result<CallState, ToolError> {
        let mut pending = self.pending.lock().expect("pending lock poisoned");
        let call = pending
            .get_mut(call_id)
            .ok_or_else(|| err(ErrorKind::UnknownCall, "unknown or expired call"))?;
        match call.state {
            CallState::PendingApproval | CallState::Ready => {
                call.state = CallState::Cancelled;
                Ok(CallState::Cancelled)
            }
            _ => Err(err(
                ErrorKind::InvalidRequest,
                "call cannot be cancelled in its current state",
            )),
        }
    }

    /// Execute a suite-authored scoring command under the same sandbox as
    /// model-approved commands (env_clear, rlimits, kill-tree timeout,
    /// capped output, redaction, audit receipt), but with argv gated by
    /// `allowed_executables` instead of the model-facing command_policy.
    /// NOT reachable from model tool requests: the only caller is the
    /// benchmark verifier, which runs pinned suite checks after a run.
    pub fn execute_trusted_scoring(
        &self,
        argv: &[String],
        cwd: Option<&str>,
        timeout_ms: u64,
        allowed_executables: &[&str],
    ) -> ToolResult {
        let started = Instant::now();
        let started_at_ms = now_ms();
        let call_id = new_call_id();
        if let Err(e) = scoring_policy(argv, allowed_executables) {
            return failure(
                &call_id,
                "trusted_scoring",
                e.kind,
                &e.detail,
                started_at_ms,
                started,
                &self.root,
            );
        }
        match self.run_command_impl(argv, cwd, timeout_ms) {
            Ok(mut data) => {
                let (clean, redactions) = redact(&data.output.unwrap_or_default());
                data.receipt.redactions += redactions;
                data.receipt.started_at_ms = started_at_ms;
                data.receipt.duration_ms = started.elapsed().as_millis();
                ToolResult {
                    call_id,
                    ok: true,
                    tool: "trusted_scoring".into(),
                    state: CallState::Executed,
                    output: Some(clean),
                    error: None,
                    receipt: data.receipt,
                }
            }
            Err(e) => failure(
                &call_id,
                "trusted_scoring",
                e.kind,
                &if e.detail.starts_with("failed to start ") && e.detail.contains(NOT_ON_PATH) {
                    format!("{}; suite checks must use an interpreter-prefixed argv such as [\"python3\", \"-m\", \"pytest\", ...]", e.detail)
                } else {
                    e.detail
                },
                started_at_ms,
                started,
                &self.root,
            ),
        }
    }

    pub fn execute(&self, call_id: &str) -> ToolResult {
        let started = Instant::now();
        let started_at_ms = now_ms();
        let call = {
            let mut pending = self.pending.lock().expect("pending lock poisoned");
            match pending.get_mut(call_id) {
                None => {
                    return failure(
                        call_id,
                        "unknown",
                        ErrorKind::UnknownCall,
                        "unknown or expired call",
                        started_at_ms,
                        started,
                        &self.root,
                    )
                }
                Some(call) if call.state == CallState::PendingApproval => {
                    return failure(
                        call_id,
                        &call.prepared.tool,
                        ErrorKind::ApprovalRequired,
                        "user approval is required",
                        started_at_ms,
                        started,
                        &self.root,
                    )
                }
                Some(call) if call.state == CallState::Cancelled => {
                    return failure(
                        call_id,
                        &call.prepared.tool,
                        ErrorKind::Cancelled,
                        "call was cancelled",
                        started_at_ms,
                        started,
                        &self.root,
                    )
                }
                Some(call) if call.state == CallState::Denied => {
                    return failure(
                        call_id,
                        &call.prepared.tool,
                        if call.prepared.risk == RiskClass::Denied {
                            ErrorKind::PolicyDenied
                        } else {
                            ErrorKind::UserDenied
                        },
                        &call.prepared.policy_reason,
                        started_at_ms,
                        started,
                        &self.root,
                    )
                }
                Some(call) if call.state == CallState::Executed => {
                    return failure(
                        call_id,
                        &call.prepared.tool,
                        ErrorKind::AlreadyExecuted,
                        "call has already executed",
                        started_at_ms,
                        started,
                        &self.root,
                    )
                }
                Some(call) => {
                    call.state = CallState::Executed;
                    call.clone()
                }
            }
        };
        let before = match &self.journal {
            Some(_) => self
                .write_targets(&call.request)
                .map(|rels| journal::Pending::capture(&self.root, &rels)),
            None => None,
        };
        let outcome = self.execute_inner(&call.request);
        if let (Ok(_), Some(j), Some(pending)) = (&outcome, &self.journal, before) {
            let _ = j.record(&self.root, call_id, &call.prepared.tool, pending);
        }
        match outcome {
            Ok(mut data) => {
                let (clean, redactions) = redact(&data.output.unwrap_or_default());
                data.output = Some(clean);
                data.receipt.redactions += redactions;
                data.receipt.started_at_ms = started_at_ms;
                data.receipt.duration_ms = started.elapsed().as_millis();
                ToolResult {
                    call_id: call_id.into(),
                    ok: true,
                    tool: call.prepared.tool,
                    state: CallState::Executed,
                    output: data.output,
                    error: None,
                    receipt: data.receipt,
                }
            }
            Err(e) => failure(
                call_id,
                &call.prepared.tool,
                e.kind,
                &e.detail,
                started_at_ms,
                started,
                &self.root,
            ),
        }
    }

    fn execute_inner(&self, request: &ToolRequest) -> Result<ExecData, ToolError> {
        match request {
            ToolRequest::ReadFile {
                path,
                offset,
                limit,
            } => self.read_file(path, *offset, *limit),
            ToolRequest::CreateFile {
                path,
                content,
                overwrite,
            } => self.create_file(path, content, *overwrite),
            ToolRequest::EditFile {
                path,
                expected,
                replacement,
                replace_all,
            } => self.edit_file(path, expected, replacement, *replace_all),
            ToolRequest::SearchFiles {
                query,
                path,
                max_results,
                regex,
                include,
            } => self.search_files(
                query,
                path.as_deref(),
                max_results.unwrap_or(50),
                regex.unwrap_or(false),
                include.as_deref(),
            ),
            ToolRequest::ApplyPatch { patch } => self.apply_patch(patch),
            ToolRequest::GlobFiles {
                pattern,
                path,
                max_results,
            } => self.glob_files(pattern, path.as_deref(), max_results.unwrap_or(100)),
            ToolRequest::RunCommand {
                argv,
                cwd,
                timeout_ms,
            } => self.run_command(argv, cwd.as_deref(), timeout_ms.unwrap_or(30_000)),
            ToolRequest::McpCall {
                server,
                name,
                arguments,
            } => self.mcp_call(server, name, arguments),
        }
    }

    /// Call a third-party MCP tool through the run's attached caller.
    /// The server's text content is returned as model-visible output;
    /// an MCP-level error (`isError`) becomes a tool error so the model
    /// sees the failure instead of a silent success.
    fn mcp_call(&self, server: &str, name: &str, arguments: &Value) -> Result<ExecData, ToolError> {
        let caller = self.mcp.as_ref().ok_or_else(|| {
            err(
                ErrorKind::InvalidRequest,
                "no MCP servers connected for this run",
            )
        })?;
        let result = caller.call(server, name, arguments).map_err(|detail| {
            err(
                ErrorKind::ProcessFailed,
                &format!("MCP {server}.{name}: {detail}"),
            )
        })?;
        let text = mcp_text(&result);
        if result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Err(err(
                ErrorKind::ProcessFailed,
                &format!("MCP {server}.{name} reported an error: {text}"),
            ));
        }
        let mut r = receipt(&self.root, None, text.len() as u64, 0);
        r.target = Some(format!("mcp:{server}.{name}"));
        Ok(exec(Some(truncate(&text, MAX_MCP_OUTPUT)), r))
    }

    fn read_file(
        &self,
        path: &str,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> Result<ExecData, ToolError> {
        let target = self.resolve_existing(path, true)?;
        let meta = fs::metadata(&target).map_err(io_err)?;
        if meta.is_dir() {
            return self.list_dir_output(&target);
        }
        if !meta.is_file() {
            return Err(err(
                ErrorKind::InvalidRequest,
                "target is not a regular file",
            ));
        }
        if let Some(done) = self.read_image(path, &target, meta.len())? {
            return Ok(done);
        }
        // An empty file reads as a plain note, not as "" (which looks like
        // a broken tool) or "[lines 1-0 of 0]". Hermes names it the same
        // way ("File is empty (0 bytes)", `tools/file_operations.py`). A
        // file holding only a byte order mark counts as empty.
        if meta.len() <= fuzzy::BOM.len_utf8() as u64 {
            let bytes = fs::read(&target).map_err(io_err)?;
            if bytes.is_empty() || bytes == b"\xEF\xBB\xBF" {
                self.note_seen(&target);
                let note = if bytes.is_empty() {
                    "[empty file: 0 bytes]"
                } else {
                    "[empty file: only a byte order mark]"
                };
                return Ok(exec(
                    Some(note.into()),
                    receipt(&self.root, Some(&target), meta.len(), 0),
                ));
            }
        }
        let paged = offset.is_some() || limit.is_some() || meta.len() > RAW_READ_BYTES;
        if !paged {
            let mut bytes = Vec::with_capacity(meta.len() as usize);
            File::open(&target)
                .and_then(|mut f| f.read_to_end(&mut bytes))
                .map_err(io_err)?;
            if looks_binary(&bytes) {
                return Err(binary_error(meta.len()));
            }
            let mut text = String::from_utf8(bytes).map_err(|_| {
                err(
                    ErrorKind::InvalidRequest,
                    "binary or non-UTF-8 files are not supported",
                )
            })?;
            // The byte order mark is not text: the model never sees it, and
            // edits and overwrites keep it (see `fuzzy::plan_edit`).
            if text.starts_with(fuzzy::BOM) {
                text.drain(..fuzzy::BOM.len_utf8());
            }
            let line_count = text.lines().count();
            if line_count <= DEFAULT_READ_LINES {
                self.note_seen(&target);
                return Ok(exec(
                    Some(text),
                    receipt(&self.root, Some(&target), meta.len(), 0),
                ));
            }
        }
        if meta.len() > MAX_PAGED_READ_BYTES {
            return Err(err(
                ErrorKind::TooLarge,
                "file exceeds 64 MiB paged read limit; use search_files to find the region",
            ));
        }
        let start = offset.unwrap_or(1).max(1);
        let want = limit.unwrap_or(DEFAULT_READ_LINES).clamp(1, MAX_READ_LINES);
        let mut head = [0u8; 8192];
        let n = File::open(&target)
            .and_then(|mut f| f.read(&mut head))
            .map_err(io_err)?;
        if looks_binary(&head[..n]) {
            return Err(binary_error(meta.len()));
        }
        let reader = BufReader::new(File::open(&target).map_err(io_err)?);
        let mut out = String::new();
        let mut total = 0usize;
        let mut shown = 0usize;
        let mut cut_lines = 0usize;
        for line in reader.split(b'\n') {
            let mut line = line.map_err(io_err)?;
            total += 1;
            if total < start || shown >= want {
                continue;
            }
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if total == 1 && line.starts_with(b"\xEF\xBB\xBF") {
                line.drain(..3);
            }
            let text = String::from_utf8_lossy(&line);
            let text = if text.chars().count() > MAX_LINE_CHARS {
                cut_lines += 1;
                let mut t: String = text.chars().take(MAX_LINE_CHARS).collect();
                t.push_str(" … [line truncated]");
                t
            } else {
                text.into_owned()
            };
            out.push_str(&format!("{total:>6}\t{text}\n"));
            shown += 1;
        }
        if start > total.max(1) {
            return Err(err(
                ErrorKind::InvalidRequest,
                &format!("offset {start} is past the end of the file ({total} lines)"),
            ));
        }
        let last = start + shown - 1;
        if last < total {
            out.push_str(&format!(
                "[lines {start}-{last} of {total}; call read_file with offset={} to continue]",
                last + 1
            ));
        } else {
            out.push_str(&format!("[lines {start}-{last} of {total}; end of file]"));
        }
        if cut_lines > 0 {
            out.push_str(&format!(
                "\n[{cut_lines} line(s) longer than {MAX_LINE_CHARS} chars were cut]"
            ));
        }
        self.note_seen(&target);
        let mut r = receipt(&self.root, Some(&target), meta.len(), 0);
        r.output_truncated = last < total || cut_lines > 0;
        Ok(exec(Some(out), r))
    }

    /// Directory listing for `read_file` on a directory: sorted, one entry
    /// per line, directories suffixed with `/`, symlinks marked, capped.
    fn list_dir_output(&self, dir: &Path) -> Result<ExecData, ToolError> {
        let mut entries: Vec<String> = fs::read_dir(dir)
            .map_err(io_err)?
            .filter_map(Result::ok)
            .map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                match e.file_type() {
                    Ok(t) if t.is_symlink() => format!("{name}@"),
                    Ok(t) if t.is_dir() => format!("{name}/"),
                    _ => name,
                }
            })
            .collect();
        entries.sort();
        let total = entries.len();
        let mut out = entries
            .into_iter()
            .take(MAX_LIST_ENTRIES)
            .collect::<Vec<_>>()
            .join("\n");
        if total > MAX_LIST_ENTRIES {
            out.push_str(&format!(
                "\n[{} more entries not shown; use glob_files with a pattern]",
                total - MAX_LIST_ENTRIES
            ));
        }
        let mut r = receipt(&self.root, Some(dir), 0, 0);
        r.output_truncated = total > MAX_LIST_ENTRIES;
        Ok(exec(Some(out), r))
    }

    fn create_file(
        &self,
        path: &str,
        content: &str,
        overwrite: bool,
    ) -> Result<ExecData, ToolError> {
        if content.len() > MAX_WRITE_BYTES {
            return Err(err(ErrorKind::TooLarge, "write exceeds 2 MiB limit"));
        }
        refuse_text_into_container(path)?;
        let target = self.resolve_for_write(path)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(io_err)?;
            self.reject_symlinks(parent)?;
        }
        if target.is_dir() {
            return Err(folder_not_file(&relative(&self.root, &target)));
        }
        let previous = if target.exists() {
            if !overwrite {
                return Err(err(ErrorKind::AlreadyExists, EXISTS_NO_OVERWRITE));
            }
            let meta = fs::metadata(&target).map_err(io_err)?;
            if meta.len() > MAX_FILE_BYTES {
                return Err(err(ErrorKind::TooLarge, "existing file exceeds diff limit"));
            }
            self.check_not_stale(&target)?;
            self.check_seen_before_overwrite(&target)?;
            fs::read_to_string(&target).map_err(io_err)?
        } else {
            String::new()
        };
        // Replacing a file that starts with a byte order mark keeps the
        // mark (opencode's write tool does the same); the model never
        // sees or writes it.
        // Replacing a CRLF file with bare-LF text keeps CRLF, as Hermes's
        // write_file does (`tools/file_operations.py`): read_file hides the
        // endings and models write LF, so a round trip would otherwise
        // rewrite every line. Text that has any CRLF of its own is written
        // as given.
        let with_crlf;
        let content = if fuzzy::uses_crlf(&previous) && !content.contains("\r\n") {
            with_crlf = fuzzy::to_crlf(content);
            with_crlf.as_str()
        } else {
            content
        };
        let with_bom;
        let content = if previous.starts_with(fuzzy::BOM) && !content.starts_with(fuzzy::BOM) {
            with_bom = format!("{}{content}", fuzzy::BOM);
            with_bom.as_str()
        } else {
            content
        };
        let mut options = OpenOptions::new();
        options.write(true).truncate(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&target).map_err(io_err)?;
        file.write_all(content.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(io_err)?;
        drop(file);
        self.note_seen(&target);
        let mut r = receipt(
            &self.root,
            Some(&target),
            previous.len() as u64,
            content.len() as u64,
        );
        r.diff = Some(simple_diff(
            &previous,
            content,
            &relative(&self.root, &target),
        ));
        let mut text = with_check(
            format!(
                "wrote {} bytes to {}",
                content.len(),
                relative(&self.root, &target)
            ),
            &relative(&self.root, &target),
            (!previous.is_empty()).then_some(previous.as_str()),
            content,
        );
        if let Some(hint) = rewrite_hint(&previous, content) {
            text.push('\n');
            text.push_str(&hint);
        }
        Ok(exec(Some(text), r))
    }

    /// An edit with no text to find. opencode treats an empty `oldString`
    /// as "create this file" and refuses it on an existing file with a
    /// pointer to its write tool (`tool/edit.ts`). REX keeps creating in
    /// `create_file`, and says which tool fits.
    fn empty_expected_error(&self, path: &str) -> ToolError {
        let exists = checked_join(&self.root, path).is_ok_and(|p| p.exists());
        let detail = if exists {
            "expected text must not be empty: give the exact text to replace, or use create_file with overwrite: true to replace the whole file"
        } else {
            "expected text must not be empty, and the file does not exist: use create_file to make a new file"
        };
        err(ErrorKind::InvalidRequest, detail)
    }

    fn edit_file(
        &self,
        path: &str,
        expected: &str,
        replacement: &str,
        replace_all: bool,
    ) -> Result<ExecData, ToolError> {
        if expected.is_empty() {
            return Err(self.empty_expected_error(path));
        }
        let target = self.resolve_existing(path, false)?;
        let old = fs::read_to_string(&target).map_err(io_err)?;
        if old.len() > MAX_FILE_BYTES as usize {
            return Err(err(ErrorKind::TooLarge, "file exceeds 2 MiB edit limit"));
        }
        self.check_not_stale(&target)?;
        let plan = plan_edit_checked(&old, expected, replacement, replace_all)?;
        let new = plan.new_content;
        if new.len() > MAX_WRITE_BYTES {
            return Err(err(ErrorKind::TooLarge, "edited file exceeds 2 MiB limit"));
        }
        fs::write(&target, new.as_bytes()).map_err(io_err)?;
        self.note_seen(&target);
        let mut r = receipt(
            &self.root,
            Some(&target),
            old.len() as u64,
            new.len() as u64,
        );
        r.diff = Some(simple_diff(&old, &new, &relative(&self.root, &target)));
        let rel = relative(&self.root, &target);
        Ok(exec(
            Some(with_check(
                if plan.strategy == "exact" {
                    format!(
                        "replaced {} occurrence(s) in {}",
                        plan.replaced,
                        relative(&self.root, &target)
                    )
                } else {
                    format!(
                    "replaced {} occurrence(s) in {} (matched via {}; indentation taken from the file)",
                    plan.replaced,
                    relative(&self.root, &target),
                    plan.strategy
                )
                },
                &rel,
                Some(&old),
                &new,
            )),
            r,
        ))
    }

    fn search_files(
        &self,
        query: &str,
        path: Option<&str>,
        max_results: usize,
        regex: bool,
        include: Option<&str>,
    ) -> Result<ExecData, ToolError> {
        let base = self.resolve_existing(path.unwrap_or("."), true)?;
        let limit = max_results.clamp(1, MAX_SEARCH_RESULTS);
        // The matcher returns the char offset of the first match so a long
        // line can be shown around the match instead of from its start.
        let matcher: LineMatcher = if regex {
            let re = regex::RegexBuilder::new(query)
                .size_limit(1 << 20)
                .build()
                .map_err(|e| err(ErrorKind::InvalidRequest, &regex_error_detail(&e)))?;
            Box::new(move |line: &str| re.find(line).map(|m| line[..m.start()].chars().count()))
        } else {
            let needle = query.to_lowercase();
            Box::new(move |line: &str| {
                let lower = line.to_lowercase();
                lower.find(&needle).map(|at| lower[..at].chars().count())
            })
        };
        let include = match include.map(str::trim).filter(|g| !g.is_empty()) {
            Some(g) => Some(walk::Glob::new(g).ok_or_else(|| {
                err(
                    ErrorKind::InvalidRequest,
                    &format!("invalid include glob: {g}"),
                )
            })?),
            None => None,
        };
        let ignore = walk::Ignore::load(&self.root);
        // A file named as the path is searched itself, as ripgrep does for
        // an explicit file (opencode's grep passes it straight to rg).
        // Walking it as a folder found nothing and said "[no matches]".
        let (files, capped) = if base.is_file() {
            (vec![base.clone()], false)
        } else {
            walk::walk_files(&self.root, &base, &ignore, 20_000)
        };
        let mut hits = Vec::new();
        let mut bytes_read = 0u64;
        let mut more = false;
        'files: for file in files {
            if let Some(g) = &include {
                // for a named file, match the glob against its own name
                let from = if base.is_file() {
                    base.parent().unwrap_or(&base)
                } else {
                    &base
                };
                if !g.matches(&walk::rel_path(from, &file)) {
                    continue;
                }
            }
            let meta = match fs::metadata(&file) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
                continue;
            }
            let Ok(bytes) = fs::read(&file) else { continue };
            bytes_read += meta.len();
            if looks_binary(&bytes) {
                continue;
            }
            let text = String::from_utf8_lossy(&bytes);
            for (idx, line) in text.lines().take(50_000).enumerate() {
                // A byte-order mark is not content; hide it as read_file does.
                let line = if idx == 0 {
                    line.strip_prefix('\u{feff}').unwrap_or(line)
                } else {
                    line
                };
                if let Some(at) = matcher(line) {
                    if hits.len() >= limit {
                        more = true;
                        break 'files;
                    }
                    let shown = match_window(line, at);
                    hits.push(format!(
                        "{}:{}:{}",
                        relative(&self.root, &file),
                        idx + 1,
                        shown
                    ));
                }
            }
        }
        let mut out = hits.join("\n");
        if hits.is_empty() {
            out = "[no matches]".to_string();
        }
        if more {
            out.push_str(&format!(
                "\n[more than {limit} matches; narrow with path, include or a more specific query]"
            ));
        }
        if capped {
            out.push_str("\n[file walk stopped at 20000 files; narrow with path or include]");
        }
        let mut r = receipt(&self.root, Some(&base), bytes_read, 0);
        r.output_truncated = more || capped;
        Ok(exec(Some(out), r))
    }

    /// Parse and plan a patch against the workspace without writing.
    fn plan_patch(&self, text: &str) -> Result<Vec<(PathBuf, patch::Change)>, ToolError> {
        if text.len() > MAX_WRITE_BYTES {
            return Err(err(ErrorKind::TooLarge, "patch exceeds 2 MiB limit"));
        }
        let ops = patch::parse(text).map_err(|e| err(ErrorKind::InvalidRequest, &e))?;
        let mut tool_error: Option<ToolError> = None;
        let planned = patch::plan(&ops, |p| {
            let res = (|| {
                let target = self.resolve_for_write(p)?;
                if !target.exists() {
                    return Ok(None);
                }
                let meta = fs::metadata(&target).map_err(io_err)?;
                if !meta.is_file() {
                    return Err(err(
                        ErrorKind::InvalidRequest,
                        "target is not a regular file",
                    ));
                }
                if meta.len() > MAX_FILE_BYTES {
                    return Err(err(ErrorKind::TooLarge, "file exceeds 2 MiB edit limit"));
                }
                fs::read_to_string(&target).map(Some).map_err(io_err)
            })();
            res.map_err(|e: ToolError| {
                let msg = format!("{p}: {}", e.detail);
                tool_error = Some(err(e.kind, &msg));
                msg
            })
        });
        let changes = match planned {
            Ok(c) => c,
            Err(e) => return Err(tool_error.unwrap_or_else(|| err(ErrorKind::Conflict, &e))),
        };
        let mut out = Vec::with_capacity(changes.len());
        for change in changes {
            let path = match &change {
                patch::Change::Write { path, content, .. } => {
                    if content.len() > MAX_WRITE_BYTES {
                        return Err(err(ErrorKind::TooLarge, "patched file exceeds 2 MiB limit"));
                    }
                    refuse_text_into_container(path)?;
                    path
                }
                patch::Change::Delete { path, .. } => path,
            };
            out.push((self.resolve_for_write(path)?, change));
        }
        Ok(out)
    }

    /// Apply a patch all-or-nothing: plan everything, refuse stale files,
    /// then write each file via a temp file + rename; if any step fails,
    /// already-applied files are restored to their original bytes.
    fn apply_patch(&self, text: &str) -> Result<ExecData, ToolError> {
        let changes = self.plan_patch(text)?;
        if changes.is_empty() {
            return Err(err(ErrorKind::InvalidRequest, "patch makes no changes"));
        }
        for (target, _) in &changes {
            if target.exists() {
                self.check_not_stale(target)?;
            }
        }
        let mut applied: Vec<usize> = Vec::new();
        let mut failure: Option<ToolError> = None;
        for (idx, (target, change)) in changes.iter().enumerate() {
            let step = match change {
                patch::Change::Write { content, .. } => write_atomic(target, content),
                patch::Change::Delete { .. } => fs::remove_file(target).map_err(io_err),
            };
            match step {
                Ok(()) => applied.push(idx),
                Err(e) => {
                    failure = Some(e);
                    break;
                }
            }
        }
        if let Some(e) = failure {
            for idx in applied.into_iter().rev() {
                let (target, change) = &changes[idx];
                let _ = match change {
                    patch::Change::Write {
                        original: Some(o), ..
                    }
                    | patch::Change::Delete { original: o, .. } => write_atomic(target, o),
                    patch::Change::Write { original: None, .. } => {
                        fs::remove_file(target).map_err(io_err)
                    }
                };
            }
            return Err(err(
                e.kind,
                &format!("patch rolled back, nothing changed: {}", e.detail),
            ));
        }
        let mut summary = Vec::new();
        let mut notes: Vec<String> = Vec::new();
        let mut diffs = String::new();
        let (mut read_bytes, mut written) = (0u64, 0u64);
        for (target, change) in &changes {
            let rel = relative(&self.root, target);
            match change {
                patch::Change::Write {
                    original, content, ..
                } => {
                    self.note_seen(target);
                    summary.push(format!(
                        "{} {rel}",
                        if original.is_some() { "M" } else { "A" }
                    ));
                    let before = original.as_deref().unwrap_or("");
                    if let Some(n) = check::delta_note(&rel, original.as_deref(), content) {
                        notes.push(format!("{rel}: {n}"));
                    }
                    read_bytes += before.len() as u64;
                    written += content.len() as u64;
                    diffs.push_str(&simple_diff(before, content, &rel));
                }
                patch::Change::Delete { original, .. } => {
                    self.seen.lock().expect("seen lock poisoned").remove(target);
                    summary.push(format!("D {rel}"));
                    read_bytes += original.len() as u64;
                    diffs.push_str(&simple_diff(original, "", &rel));
                }
            }
        }
        let mut r = receipt(&self.root, None, read_bytes, written);
        r.target = Some(
            summary
                .iter()
                .map(|s| s[2..].to_string())
                .collect::<Vec<_>>()
                .join(", "),
        );
        r.diff = Some(diffs);
        Ok(exec(
            Some({
                let mut msg = format!(
                    "applied patch to {} file(s):\n{}",
                    summary.len(),
                    summary.join("\n")
                );
                for n in notes {
                    msg.push('\n');
                    msg.push_str(&n);
                }
                msg
            }),
            r,
        ))
    }

    fn glob_files(
        &self,
        pattern: &str,
        path: Option<&str>,
        max_results: usize,
    ) -> Result<ExecData, ToolError> {
        let base = self.resolve_existing(path.unwrap_or("."), true)?;
        // A file is not a folder to list. opencode refuses it too
        // (`tool/glob.ts`); REX also names the folder to use instead of
        // answering "[no files match]", which read as "nothing there".
        if base.is_file() {
            let parent = base
                .parent()
                .map(|p| relative(&self.root, p))
                .unwrap_or_else(|| ".".into());
            let parent = if parent.is_empty() {
                ".".into()
            } else {
                parent
            };
            return Err(err(
                ErrorKind::InvalidRequest,
                &format!(
                    "glob_files path must be a folder, and {} is a file; use path \"{parent}\" to list its folder, or search_files / read_file to look inside it",
                    relative(&self.root, &base)
                ),
            ));
        }
        let limit = max_results.clamp(1, MAX_SEARCH_RESULTS);
        let glob = walk::Glob::new(pattern).ok_or_else(|| {
            err(
                ErrorKind::InvalidRequest,
                &format!("invalid glob: {pattern}"),
            )
        })?;
        let ignore = walk::Ignore::load(&self.root);
        let (files, capped) = walk::walk_files(&self.root, &base, &ignore, 50_000);
        let mut found: Vec<(SystemTime, String)> = files
            .into_iter()
            .filter(|f| glob.matches(&walk::rel_path(&base, f)))
            .map(|f| {
                let mtime = fs::metadata(&f)
                    .and_then(|m| m.modified())
                    .unwrap_or(UNIX_EPOCH);
                (mtime, relative(&self.root, &f))
            })
            .collect();
        // Newest first: the file the task is about is usually the one
        // touched most recently. Ties break by path for determinism.
        found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let total = found.len();
        let mut out = found
            .into_iter()
            .take(limit)
            .map(|(_, p)| p)
            .collect::<Vec<_>>()
            .join("\n");
        if total == 0 {
            out = "[no files match]".to_string();
        }
        if total > limit {
            out.push_str(&format!("\n[{} more files not shown]", total - limit));
        }
        if capped {
            out.push_str("\n[file walk stopped at 50000 files; narrow with path]");
        }
        let mut r = receipt(&self.root, Some(&base), 0, 0);
        r.output_truncated = total > limit || capped;
        Ok(exec(Some(out), r))
    }

    fn run_command(
        &self,
        argv: &[String],
        cwd: Option<&str>,
        timeout_ms: u64,
    ) -> Result<ExecData, ToolError> {
        command_policy(argv)?;
        // A zero limit would start the command and kill it at once, after
        // any side effects began; refuse it before anything runs.
        if timeout_ms == 0 {
            return Err(err(
                ErrorKind::InvalidRequest,
                &format!(
                    "timeout_ms must be a positive number of milliseconds (got 0); omit it for the 30000 ms default or give up to {MAX_TIMEOUT_MS}"
                ),
            ));
        }
        let mut data = self.run_command_impl(argv, cwd, timeout_ms)?;
        // A timeout means the cut is reported by timeout_detail; a command
        // that finished in time must still hear that its request was cut,
        // or it keeps asking for a limit it never gets.
        if timeout_ms > MAX_TIMEOUT_MS {
            let out = data.output.get_or_insert_with(String::new);
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&format!(
                "note: timeout_ms {timeout_ms} is above the {MAX_TIMEOUT_MS} ms cap, so it ran with {MAX_TIMEOUT_MS}"
            ));
        }
        Ok(data)
    }

    /// Bounded process execution shared by model-approved commands and
    /// trusted scoring. Callers are responsible for policy; this applies
    /// the sandbox (env_clear, rlimits, kill-tree timeout, capped output).
    fn run_command_impl(
        &self,
        argv: &[String],
        cwd: Option<&str>,
        timeout_ms: u64,
    ) -> Result<ExecData, ToolError> {
        let cwd = self.resolve_existing(cwd.unwrap_or("."), true)?;
        if !cwd.is_dir() {
            let parent = cwd
                .parent()
                .map(|p| relative(&self.root, p))
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| ".".into());
            return Err(err(
                ErrorKind::InvalidRequest,
                &format!(
                    "command cwd {} is a file, not a folder; use cwd \"{parent}\"",
                    relative(&self.root, &cwd)
                ),
            ));
        }
        // The real boundary is the OS sandbox: every child enters fresh
        // user/network/IPC/UTS namespaces. If the kernel refuses, degrade
        // honestly - the command still runs under rlimits and process-group
        // isolation, and the receipt says exactly why confinement is off.
        let (mut child, sandbox_status) = match self.spawn_command(argv, &cwd, true) {
            Ok(child) => (child, sandbox::SandboxStatus::Applied),
            Err(setup) => {
                let reason = setup
                    .strip_prefix(sandbox::SANDBOX_ERROR_PREFIX)
                    .unwrap_or(&setup)
                    .to_string();
                let child = self
                    .spawn_command(argv, &cwd, false)
                    .map_err(|e| err(ErrorKind::Io, &e))?;
                (child, sandbox::SandboxStatus::Unavailable(reason))
            }
        };
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let out_handle = thread::spawn(move || read_capped(stdout, MAX_OUTPUT_BYTES));
        let err_handle = thread::spawn(move || read_capped(stderr, MAX_OUTPUT_BYTES));
        let deadline = Instant::now() + Duration::from_millis(timeout_ms.min(MAX_TIMEOUT_MS));
        let status = loop {
            if let Some(status) = child.try_wait().map_err(io_err)? {
                break status;
            }
            if Instant::now() >= deadline {
                kill_tree(&mut child);
                let _ = child.wait();
                let (stdout, _) = out_handle.join().unwrap_or_default();
                let (stderr, _) = err_handle.join().unwrap_or_default();
                return Err(err(
                    ErrorKind::Timeout,
                    &timeout_detail(timeout_ms, &command_output(&stdout, &stderr)),
                ));
            }
            thread::sleep(Duration::from_millis(15));
        };
        let (stdout, out_truncated) = out_handle.join().unwrap_or_default();
        let (stderr, err_truncated) = err_handle.join().unwrap_or_default();
        let combined = command_output(&stdout, &stderr);
        let mut r = receipt(
            &self.root,
            Some(&cwd),
            (stdout.len() + stderr.len()) as u64,
            0,
        );
        r.command = Some(argv.to_vec());
        r.exit_code = status.code();
        r.output_truncated = out_truncated || err_truncated;
        r.sandbox = Some(sandbox_status.label());
        if !status.success() {
            return Err(err(
                ErrorKind::ProcessFailed,
                &failed_command_detail(status, &combined),
            ));
        }
        Ok(exec(Some(combined), r))
    }

    /// Spawn one bounded command. With `with_sandbox` the child first
    /// enters the namespace sandbox; a setup failure surfaces as an
    /// error carrying sandbox::SANDBOX_ERROR_PREFIX so the caller can
    /// retry without confinement and report the degradation.
    fn spawn_command(
        &self,
        argv: &[String],
        cwd: &Path,
        with_sandbox: bool,
    ) -> Result<std::process::Child, String> {
        let mut command = Command::new(&argv[0]);
        command
            .args(&argv[1..])
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_clear();
        if let Ok(path) = std::env::var("PATH") {
            command.env("PATH", path);
        }
        // Suite checks run with HOME pointed at the disposable workspace, so
        // Python user-site installs (pip --user, the default under
        // --break-system-packages) are invisible unless the bench host
        // exports PYTHONPATH to the real user site-packages. Pass it through
        // like PATH; it only affects Python subprocesses of suite checks.
        if let Ok(pythonpath) = std::env::var("PYTHONPATH") {
            command.env("PYTHONPATH", pythonpath);
        }
        command
            .env("HOME", &*self.root)
            .env("REX_WORKSPACE", &*self.root);
        #[cfg(unix)]
        unsafe {
            use std::os::unix::process::CommandExt;
            command.pre_exec(move || {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if with_sandbox {
                    if let Err(reason) = sandbox::apply_in_child() {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::PermissionDenied,
                            format!("{}{reason}", sandbox::SANDBOX_ERROR_PREFIX),
                        ));
                    }
                }
                let cpu = libc::rlimit {
                    rlim_cur: 120,
                    rlim_max: 120,
                };
                let mem = libc::rlimit {
                    rlim_cur: 1024 * 1024 * 1024,
                    rlim_max: 1024 * 1024 * 1024,
                };
                let files = libc::rlimit {
                    rlim_cur: 256,
                    rlim_max: 256,
                };
                if libc::setrlimit(libc::RLIMIT_CPU, &cpu) != 0
                    || libc::setrlimit(libc::RLIMIT_AS, &mem) != 0
                    || libc::setrlimit(libc::RLIMIT_NOFILE, &files) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        command.spawn().map_err(|e| {
            let message = format!("{e}");
            if message.starts_with(sandbox::SANDBOX_ERROR_PREFIX) {
                message
            } else {
                // execvp reports EACCES rather than ENOENT when a PATH entry
                // cannot be searched, so a bare name missing from every
                // PATH folder counts as missing too.
                let missing = e.kind() == std::io::ErrorKind::NotFound
                    || (!argv[0].contains('/') && !on_path(&argv[0]));
                let hint = if missing {
                    missing_program_hint(&argv[0])
                } else {
                    String::new()
                };
                format!("failed to start '{}' ({e}){hint}", argv[0])
            }
        })
    }

    /// `read_file` of an image when image reads are on: queue the bytes and
    /// tell the model where they will show up. `None` means "not an image
    /// (or image reads are off); read it as text".
    fn read_image(
        &self,
        path: &str,
        target: &Path,
        len: u64,
    ) -> Result<Option<ExecData>, ToolError> {
        let Some(queue) = &self.images else {
            return Ok(None);
        };
        let mut head = [0u8; 12];
        let n = File::open(target)
            .and_then(|mut f| f.read(&mut head))
            .map_err(io_err)?;
        let Some(mime) = sniff_image(&head[..n]) else {
            return Ok(None);
        };
        if len > MAX_IMAGE_BYTES {
            return Err(err(
                ErrorKind::TooLarge,
                &format!("{mime} image is {len} bytes; images over {MAX_IMAGE_BYTES} bytes are not attached"),
            ));
        }
        let mut bytes = Vec::with_capacity(len as usize);
        File::open(target)
            .and_then(|mut f| f.read_to_end(&mut bytes))
            .map_err(io_err)?;
        // re-check what was actually read, not the earlier peek
        if sniff_image(&bytes) != Some(mime) || bytes.len() as u64 > MAX_IMAGE_BYTES {
            return Err(err(
                ErrorKind::InvalidRequest,
                "image changed while it was being read",
            ));
        }
        let read = bytes.len() as u64;
        let mut queued = queue.lock().expect("image queue poisoned");
        if queued.len() >= MAX_QUEUED_IMAGES {
            return Err(err(
                ErrorKind::InvalidRequest,
                &format!("{MAX_QUEUED_IMAGES} images are already attached to the next turn; read more after it"),
            ));
        }
        queued.push(ImageRead {
            path: path.to_string(),
            mime,
            bytes,
        });
        Ok(Some(exec(
            Some(format!(
                "{mime} image, {read} bytes. The image itself is attached to your next turn."
            )),
            receipt(&self.root, Some(target), read, 0),
        )))
    }

    fn resolve_existing(&self, raw: &str, allow_dir: bool) -> Result<PathBuf, ToolError> {
        let joined = checked_join(&self.root, raw)?;
        let canonical = fs::canonicalize(&joined).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                err(ErrorKind::NotFound, &self.not_found_detail(&joined))
            } else {
                io_err(e)
            }
        })?;
        if !canonical.starts_with(&*self.root) {
            return Err(err(
                ErrorKind::OutsideWorkspace,
                "path resolves outside the workspace",
            ));
        }
        self.reject_symlinks(&joined)?;
        if !allow_dir && canonical.is_dir() {
            return Err(folder_not_file(&relative(&self.root, &canonical)));
        }
        Ok(canonical)
    }

    /// "path not found", plus up to three names from the same folder that
    /// look like what was meant (other case, other extension, one name
    /// inside the other, or a near spelling), so the model can retry
    /// without a separate listing. opencode's read tool and Hermes's
    /// `_suggest_similar_files` (`tools/file_operations.py`) do the same.
    /// Only a folder that exists inside the workspace, reached without
    /// symlinks, is looked at, and at most `MAX_LIST_ENTRIES` names.
    fn not_found_detail(&self, joined: &Path) -> String {
        let shown = relative(&self.root, joined);
        let mut detail = format!("path not found: {shown}");
        let (Some(dir), Some(wanted)) = (joined.parent(), joined.file_name()) else {
            return detail;
        };
        let inside = fs::canonicalize(dir).is_ok_and(|c| c.starts_with(&*self.root));
        if !inside {
            if let Some(hint) = self.missing_folder_hint(joined) {
                detail.push_str(&format!("; did you mean {hint}?"));
            }
            return detail;
        }
        if self.reject_symlinks(dir).is_err() {
            return detail;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            return detail;
        };
        let names = entries
            .take(MAX_LIST_ENTRIES)
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().to_str()?.to_string();
                let is_dir = e.file_type().ok()?.is_dir();
                Some((name, is_dir))
            });
        let picks = similar_names(&wanted.to_string_lossy(), names);
        if !picks.is_empty() {
            let shown: Vec<String> = picks
                .iter()
                .map(|(name, is_dir)| {
                    let rel = relative(&self.root, &dir.join(name));
                    if *is_dir {
                        format!("{rel}/")
                    } else {
                        rel
                    }
                })
                .collect();
            detail.push_str(&format!("; did you mean {}?", shown.join(", ")));
        }
        detail
    }

    /// When a folder on the way is what is missing (`scr/main.rs`), look
    /// in the deepest folder that does exist for one folder named like the
    /// missing one. Offer the corrected full path when it exists, else the
    /// folder. Neither competitor looks past the last folder; both only
    /// list the file's own folder. Same limits: inside the workspace, no
    /// symlinks, at most `MAX_LIST_ENTRIES` names, and only one clear pick.
    fn missing_folder_hint(&self, joined: &Path) -> Option<String> {
        let mut base = joined.parent()?;
        let mut missing = joined.file_name()?;
        while !base.exists() {
            missing = base.file_name()?;
            base = base.parent()?;
        }
        let inside = fs::canonicalize(base).is_ok_and(|c| c.starts_with(&*self.root));
        if !inside || self.reject_symlinks(base).is_err() || !base.is_dir() {
            return None;
        }
        let rest = joined.strip_prefix(base.join(missing)).ok()?;
        let names = fs::read_dir(base)
            .ok()?
            .take(MAX_LIST_ENTRIES)
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|e| Some((e.file_name().to_str()?.to_string(), true)));
        let picks = similar_names(&missing.to_string_lossy(), names);
        let [(name, _)] = picks.as_slice() else {
            return None;
        };
        let folder = base.join(name);
        let full = folder.join(rest);
        if full.symlink_metadata().is_ok() && self.reject_symlinks(&full).is_ok() {
            Some(relative(&self.root, &full))
        } else {
            Some(format!("{}/", relative(&self.root, &folder)))
        }
    }

    fn resolve_for_write(&self, raw: &str) -> Result<PathBuf, ToolError> {
        let joined = checked_join(&self.root, raw)?;
        let mut ancestor = joined.as_path();
        while !ancestor.exists() {
            ancestor = ancestor
                .parent()
                .ok_or_else(|| err(ErrorKind::OutsideWorkspace, "invalid path"))?;
        }
        let canonical = fs::canonicalize(ancestor).map_err(io_err)?;
        if !canonical.starts_with(&*self.root) {
            return Err(err(
                ErrorKind::OutsideWorkspace,
                "write parent resolves outside workspace",
            ));
        }
        self.reject_symlinks(ancestor)?;
        if joined.exists() {
            self.reject_symlinks(&joined)?;
        }
        Ok(joined)
    }

    fn reject_symlinks(&self, path: &Path) -> Result<(), ToolError> {
        let rel = path.strip_prefix(&*self.root).unwrap_or(path);
        let mut cursor = (*self.root).clone();
        for component in rel.components() {
            cursor.push(component);
            if let Ok(meta) = fs::symlink_metadata(&cursor) {
                if meta.file_type().is_symlink() {
                    return Err(err(
                        ErrorKind::SymlinkRefused,
                        "symlink path components are refused",
                    ));
                }
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Git integration: user-initiated from the Git panel UI, not model
    // tool calls. These do not go through the approval gate — the user
    // clicks Commit themselves. All run under the sandbox via
    // run_command_impl (env_clear, rlimits, kill-tree, capped output).
    // ------------------------------------------------------------------

    /// Validate that `workspace` is a git repo under the tool root.
    fn git_repo(&self, workspace: &Path) -> Result<PathBuf, ToolError> {
        let ws = workspace
            .canonicalize()
            .map_err(|_| err(ErrorKind::NotFound, "workspace not found"))?;
        let root = self.root.canonicalize().map_err(io_err)?;
        if !ws.starts_with(&root) {
            return Err(err(
                ErrorKind::OutsideWorkspace,
                "workspace must sit under the tool root",
            ));
        }
        if !ws.join(".git").exists() {
            return Err(err(ErrorKind::NotFound, "not a git repository"));
        }
        Ok(ws)
    }

    fn git_run(&self, ws: &Path, args: &[&str]) -> Result<String, ToolError> {
        let argv: Vec<String> = std::iter::once("git".to_string())
            .chain(args.iter().map(|s| s.to_string()))
            .collect();
        // run_command_impl resolves cwd relative to the tool root, so pass
        // the workspace as a root-relative path.
        let root = self.root.canonicalize().map_err(io_err)?;
        let rel = ws.strip_prefix(&root).map_err(|_| {
            err(
                ErrorKind::OutsideWorkspace,
                "workspace must sit under the tool root",
            )
        })?;
        let cwd = if rel.as_os_str().is_empty() {
            ".".to_string()
        } else {
            rel.to_string_lossy().to_string()
        };
        // run_command_impl returns Err on non-zero exit, so Ok means success.
        // The output has "stdout:\n" / "stderr:\n" labels; strip them for git.
        let data = self.run_command_impl(&argv, Some(&cwd), 30_000)?;
        let raw = data.output.unwrap_or_default();
        let stripped = raw.strip_prefix("stdout:\n").unwrap_or(&raw).to_string();
        // If stderr was included, cut it off.
        Ok(stripped
            .split("\nstderr:\n")
            .next()
            .unwrap_or(&stripped)
            .to_string())
    }

    /// `git status --porcelain`: changed files.
    pub fn git_status(&self, workspace: &Path) -> Result<Vec<GitFile>, ToolError> {
        let ws = self.git_repo(workspace)?;
        let out = self.git_run(&ws, &["status", "--porcelain=v1", "--untracked-files=all"])?;
        let mut files = Vec::new();
        for line in out.lines() {
            if line.len() < 4 {
                continue;
            }
            let status = line[..2].to_string();
            let path = line[3..].to_string();
            // Renames show as "R  old -> new"; take the new path.
            let path = path.split(" -> ").last().unwrap_or(&path).to_string();
            files.push(GitFile { status, path });
        }
        Ok(files)
    }

    /// `git diff` for a single path (staged + unstaged).
    pub fn git_diff(&self, workspace: &Path, path: &str) -> Result<String, ToolError> {
        let ws = self.git_repo(workspace)?;
        // Refuse path escapes; git would too, but fail fast with a clear error.
        if path.contains("..") || path.starts_with('/') {
            return Err(err(
                ErrorKind::OutsideWorkspace,
                "path escapes the workspace",
            ));
        }
        let mut out = self.git_run(&ws, &["diff", "--", path])?;
        let staged = self.git_run(&ws, &["diff", "--cached", "--", path])?;
        if !staged.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&staged);
        }
        Ok(out)
    }

    /// `git add -A` + `git commit`. The message is the user's own.
    pub fn git_commit(&self, workspace: &Path, message: &str) -> Result<String, ToolError> {
        let ws = self.git_repo(workspace)?;
        let message = message.trim();
        if message.is_empty() {
            return Err(err(ErrorKind::InvalidRequest, "commit message is empty"));
        }
        if message.len() > 1000 {
            return Err(err(
                ErrorKind::InvalidRequest,
                "commit message exceeds 1000 chars",
            ));
        }
        self.git_run(&ws, &["add", "-A"])?;
        let out = self.git_run(&ws, &["commit", "-m", message])?;
        // Return the new commit hash.
        let hash = self
            .git_run(&ws, &["rev-parse", "HEAD"])?
            .trim()
            .to_string();
        Ok(format!("{hash}\n{out}"))
    }
}

/// A changed file from `git status --porcelain`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitFile {
    /// Two-letter porcelain status (e.g. " M", "A ", "??").
    pub status: String,
    pub path: String,
}

struct ExecData {
    output: Option<String>,
    receipt: AuditReceipt,
}
fn exec(output: Option<String>, receipt: AuditReceipt) -> ExecData {
    ExecData { output, receipt }
}
fn receipt(
    root: &Path,
    target: Option<&Path>,
    bytes_read: u64,
    bytes_written: u64,
) -> AuditReceipt {
    AuditReceipt {
        started_at_ms: 0,
        duration_ms: 0,
        workspace_root: root.display().to_string(),
        target: target.map(|p| relative(root, p)),
        command: None,
        exit_code: None,
        bytes_read,
        bytes_written,
        output_truncated: false,
        sandbox: None,
        diff: None,
        redactions: 0,
    }
}
fn failure(
    call_id: &str,
    tool: &str,
    kind: ErrorKind,
    detail: &str,
    started_at_ms: u128,
    started: Instant,
    root: &Path,
) -> ToolResult {
    let (detail, redactions) = redact(detail);
    ToolResult {
        call_id: call_id.into(),
        ok: false,
        tool: tool.into(),
        state: CallState::Executed,
        output: None,
        error: Some(ToolError { kind, detail }),
        receipt: AuditReceipt {
            started_at_ms,
            duration_ms: started.elapsed().as_millis(),
            workspace_root: root.display().to_string(),
            target: None,
            command: None,
            exit_code: None,
            bytes_read: 0,
            bytes_written: 0,
            output_truncated: false,
            diff: None,
            redactions,
            sandbox: None,
        },
    }
}
fn err(kind: ErrorKind, detail: &str) -> ToolError {
    ToolError {
        kind,
        detail: detail.into(),
    }
}
fn io_err(e: std::io::Error) -> ToolError {
    err(ErrorKind::Io, &e.to_string())
}
fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
fn new_call_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(1);
    format!(
        "tool-{:x}-{:x}-{:x}",
        now_ms(),
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}
fn tool_name(r: &ToolRequest) -> &'static str {
    match r {
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
fn summarize(r: &ToolRequest) -> String {
    match r {
        ToolRequest::ReadFile { path, .. } => format!("Read {path}"),
        ToolRequest::CreateFile {
            path, overwrite, ..
        } => format!("{} {path}", if *overwrite { "Write" } else { "Create" }),
        ToolRequest::EditFile { path, .. } => format!("Edit {path}"),
        ToolRequest::SearchFiles { query, path, .. } => format!(
            "Search {:?} for {:?}",
            path.as_deref().unwrap_or("."),
            query
        ),
        ToolRequest::ApplyPatch { patch } => {
            let files = patch::parse(patch)
                .map(|ops| patch_paths(&ops).join(", "))
                .unwrap_or_else(|_| "unparseable patch".into());
            format!("Apply patch: {files}")
        }
        ToolRequest::GlobFiles { pattern, path, .. } => format!(
            "Find files {:?} under {:?}",
            pattern,
            path.as_deref().unwrap_or(".")
        ),
        ToolRequest::RunCommand { argv, cwd, .. } => {
            format!("Run {:?} in {}", argv, cwd.as_deref().unwrap_or("."))
        }
        ToolRequest::McpCall { server, name, .. } => format!("MCP {server}.{name}"),
    }
}
/// Trusted scoring policy for benchmark verification. This list is ONLY
/// consulted by `execute_trusted_scoring` - model-issued RunCommand
/// requests keep going through `command_policy`, which denies the same
/// interpreter names. Suite executable checks are task-author
/// infrastructure run by the harness verifier inside the disposable
/// workspace; argv comes from the pinned suite file, never from model
/// output. Executables must be bare names resolved via PATH so a suite
/// cannot smuggle an absolute binary or a traversal path.
fn scoring_policy(argv: &[String], allowed: &[&str]) -> Result<(), ToolError> {
    if argv.is_empty() || argv[0].trim().is_empty() {
        return Err(err(ErrorKind::InvalidRequest, "empty command"));
    }
    let raw = &argv[0];
    if raw.contains('/') || raw.contains('\\') {
        return Err(err(
            ErrorKind::PolicyDenied,
            "scoring executables must be bare names resolved via PATH",
        ));
    }
    let exe = raw.to_ascii_lowercase();
    if !allowed.iter().any(|a| a.eq_ignore_ascii_case(&exe)) {
        return Err(err(
            ErrorKind::PolicyDenied,
            "executable is not in the suite scoring allowlist",
        ));
    }
    if argv.iter().skip(1).any(|a| a.contains('\0')) {
        return Err(err(ErrorKind::InvalidRequest, "NUL byte in argument"));
    }
    Ok(())
}

fn classify(r: &ToolRequest) -> (RiskClass, &'static str) {
    match r {
        ToolRequest::ReadFile { .. }
        | ToolRequest::SearchFiles { .. }
        | ToolRequest::GlobFiles { .. } => (
            RiskClass::Read,
            "bounded read inside the selected workspace",
        ),
        ToolRequest::CreateFile { .. }
        | ToolRequest::EditFile { .. }
        | ToolRequest::ApplyPatch { .. } => (
            RiskClass::Write,
            "changes workspace files and requires user approval",
        ),
        ToolRequest::RunCommand { argv, .. } if command_policy(argv).is_err() => {
            (RiskClass::Denied, "command is blocked by hard policy")
        }
        ToolRequest::RunCommand { .. } => (
            RiskClass::Execute,
            "runs a bounded process and requires user approval",
        ),
        ToolRequest::McpCall { .. } => (
            RiskClass::Execute,
            "calls a third-party MCP server and requires user approval",
        ),
    }
}
fn validate_request(r: &ToolRequest) -> Result<(), ToolError> {
    match r {
        ToolRequest::ReadFile { path, .. }
        | ToolRequest::CreateFile { path, .. }
        | ToolRequest::EditFile { path, .. }
            if path.trim().is_empty() =>
        {
            Err(err(ErrorKind::InvalidRequest, "path is empty"))
        }
        ToolRequest::SearchFiles { query, .. } if query.trim().is_empty() => {
            Err(err(ErrorKind::InvalidRequest, "search query is empty"))
        }
        ToolRequest::ApplyPatch { patch } if patch.trim().is_empty() => {
            Err(err(ErrorKind::InvalidRequest, "patch is empty"))
        }
        ToolRequest::GlobFiles { pattern, .. } if pattern.trim().is_empty() => {
            Err(err(ErrorKind::InvalidRequest, "glob pattern is empty"))
        }
        ToolRequest::RunCommand { argv, .. } if argv.is_empty() || argv[0].trim().is_empty() => {
            Err(err(ErrorKind::InvalidRequest, "command argv is empty"))
        }
        // A zero timeout cannot run. Reject it before asking for approval,
        // rather than approving a call that is guaranteed to fail.
        ToolRequest::RunCommand { timeout_ms: Some(0), .. } => Err(err(
            ErrorKind::InvalidRequest,
            &format!(
                "timeout_ms must be a positive number of milliseconds (got 0); omit it for the 30000 ms default or give up to {MAX_TIMEOUT_MS}"
            ),
        )),
        // There is no shell: `&&`, `|`, `>` and the like would reach the
        // program as plain words and fail in a confusing way, or, worse,
        // run the first command only. opencode and Hermes run a shell line,
        // so these work there; REX refuses before anyone is asked to
        // approve, and says what to do instead.
        // `cd` and friends are shell builtins: there is no such program,
        // and even a shell could not carry the change to the next call.
        // opencode tells its model to use `workdir` instead of `cd`
        // (`tool/shell/prompt.ts`); REX points at `cwd`.
        ToolRequest::RunCommand { argv, .. } if SHELL_BUILTINS.contains(&argv[0].trim()) => {
            Err(err(
                ErrorKind::InvalidRequest,
                &format!(
                    "'{}' is a shell builtin and run_command has no shell; to run a command in a folder, set the cwd field (e.g. cwd: \"crates/app\") on that command",
                    argv[0].trim()
                ),
            ))
        }
        ToolRequest::RunCommand { argv, .. } => {
            match argv.iter().skip(1).find(|a| SHELL_OPERATORS.contains(&a.as_str())) {
                Some(op) => Err(err(
                    ErrorKind::InvalidRequest,
                    &format!(
                        "run_command runs one program with no shell, so '{op}' would be passed to it as a plain argument; run each command as its own call, and read output with read_output instead of redirecting it"
                    ),
                )),
                None => Ok(()),
            }
        }
        ToolRequest::McpCall { server, name, .. }
            if server.trim().is_empty() || name.trim().is_empty() =>
        {
            Err(err(
                ErrorKind::InvalidRequest,
                "mcp_call needs a server and a tool name",
            ))
        }
        _ => Ok(()),
    }
}
/// Programs that exist only inside a shell.
const SHELL_BUILTINS: &[&str] = &[
    "cd", "chdir", "pushd", "popd", "export", "source", ".", "alias", "set", "unset",
];

/// Whole argv items that only mean something to a shell.
const SHELL_OPERATORS: &[&str] = &[
    "&&", "||", "|", ";", "&", ">", ">>", "<", "2>", "2>&1", "&>", "|&",
];

fn checked_join(root: &Path, raw: &str) -> Result<PathBuf, ToolError> {
    let path = Path::new(raw);
    if path.is_absolute() {
        return Err(err(
            ErrorKind::OutsideWorkspace,
            "absolute paths are refused",
        ));
    }
    for c in path.components() {
        if matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            return Err(err(
                ErrorKind::OutsideWorkspace,
                "parent traversal is refused",
            ));
        }
    }
    Ok(root.join(path))
}
/// `create_file` on an existing file without `overwrite`. opencode's
/// write tool replaces files outright; REX keeps the guard and says how to
/// get past it on purpose.
const EXISTS_NO_OVERWRITE: &str = "file exists and overwrite is false; to change part of it use edit_file, or to replace all of it read it first and send create_file again with overwrite: true";

/// A file tool pointed at a folder. opencode says "Path is a directory,
/// not a file" (`tool/edit.ts`); REX also names the tool that lists it.
fn folder_not_file(rel: &str) -> ToolError {
    err(
        ErrorKind::InvalidRequest,
        &format!("{rel} is a folder, not a file; read_file on it lists what is inside"),
    )
}

const NOT_ON_PATH: &str = "is not installed or not on PATH";

/// True when some folder on PATH holds a file named `program`.
fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(program).is_file()))
}

/// Why a program could not be found, in words the model can act on. The
/// model-facing hint used to be the scoring one ("use python3 -m ..."),
/// which names an interpreter `command_policy` blocks. opencode and Hermes
/// run a shell line, so the shell's "command not found" does this there;
/// REX takes an argv, so it says it itself, including the common slip of
/// a whole command line in `argv[0]`.
fn missing_program_hint(program: &str) -> String {
    let words: Vec<&str> = program.split_whitespace().collect();
    if words.len() > 1 {
        let split = words
            .iter()
            .map(|w| format!("\"{w}\""))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "; argv[0] holds a whole command line - put each word in its own item, e.g. [{split}]"
        )
    } else if program.contains('/') {
        "; no such program at that path".to_string()
    } else {
        format!("; '{program}' {NOT_ON_PATH}")
    }
}

/// Key for a run-scoped standing approval of one exact command.
///
/// Only a command that already passes `command_policy` qualifies (so no
/// shells, interpreters or launchers), and only when the program is a bare
/// name looked up on PATH. A program given as a path (`./build.sh`,
/// `bin/tool`) is left out: the model can edit that file between two runs of
/// the same command line. The key is the exact argv plus the working
/// directory, so `npm test` never covers `npm test -- --update`.
pub fn standing_key(request: &ToolRequest) -> Option<String> {
    let ToolRequest::RunCommand { argv, cwd, .. } = request else {
        return None;
    };
    if command_policy(argv).is_err() {
        return None;
    }
    let program = argv.first()?;
    if program.is_empty() || program.contains('/') || program.contains('\\') {
        return None;
    }
    let dir = match cwd.as_deref().map(str::trim) {
        None | Some("") | Some(".") => ".".to_string(),
        Some(d) => d.trim_end_matches('/').to_string(),
    };
    serde_json::to_string(&(argv, dir)).ok()
}

fn command_policy(argv: &[String]) -> Result<(), ToolError> {
    if argv.is_empty() {
        return Err(err(ErrorKind::InvalidRequest, "empty command"));
    }
    let exe = Path::new(&argv[0])
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let denied = [
        "sh",
        "bash",
        "zsh",
        "fish",
        "cmd",
        "cmd.exe",
        "powershell",
        "pwsh",
        "sudo",
        "su",
        "doas",
        "rm",
        "rmdir",
        "shutdown",
        "reboot",
        "halt",
        "mkfs",
        "dd",
        "mount",
        "umount",
        "kill",
        "pkill",
        "taskkill",
        "curl",
        "wget",
        "nc",
        "ncat",
        "ssh",
        "scp",
        "python",
        "python3",
        "node",
        "ruby",
        "perl",
        // Launcher indirection: these exist to run something else, so a
        // basename list can never cover what they invoke.
        "env",
        "busybox",
        "xargs",
        "find",
        "nohup",
        "stdbuf",
        "timeout",
        "nice",
        "ionice",
        "setsid",
        "taskset",
        "unshare",
        "nsenter",
        "chroot",
        "bwrap",
        "firejail",
        "script",
        "watch",
    ];
    if denied.contains(&exe.as_str()) {
        return Err(err(ErrorKind::PolicyDenied, &denied_detail(&exe)));
    }
    if argv.iter().skip(1).any(|a| a.contains('\0')) {
        return Err(err(ErrorKind::InvalidRequest, "NUL byte in argument"));
    }
    Ok(())
}
/// Why `exe` is blocked, and what to do instead. A bare "blocked" makes
/// the model retry the same thing through another blocked program (`bash`
/// then `sh` then `env bash`); naming the tool or field that does the job
/// ends that loop.
fn denied_detail(exe: &str) -> String {
    let (why, instead) = match exe {
        "sh" | "bash" | "zsh" | "fish" | "cmd" | "cmd.exe" | "powershell" | "pwsh" => (
            "run_command has no shell",
            "put the program and its arguments in argv directly (e.g. [\"cargo\", \"test\"]), one command per call; set cwd for the folder",
        ),
        "python" | "python3" | "node" | "ruby" | "perl" => (
            "interpreters can run any code",
            "use the project's own commands (cargo, npm, make, pytest ...) and the file tools to read or change files",
        ),
        "curl" | "wget" | "nc" | "ncat" | "ssh" | "scp" => (
            "commands have no network access",
            "work from the files in the workspace",
        ),
        "rm" | "rmdir" => (
            "deleting through run_command is not allowed",
            "remove a file with apply_patch (*** Delete File: <path>)",
        ),
        "find" => (
            "find can run other programs with -exec",
            "use glob_files to list files by pattern, or search_files to search their contents",
        ),
        "timeout" => (
            "timeout runs another program",
            "set the timeout_ms field on run_command instead",
        ),
        "nohup" | "setsid" => (
            "there is no background mode",
            "run the command in the foreground; its process group is stopped when it ends or times out",
        ),
        "env" | "busybox" | "xargs" | "stdbuf" | "nice" | "ionice" | "taskset" | "unshare"
        | "nsenter" | "chroot" | "bwrap" | "firejail" | "script" | "watch" => (
            "it runs another program the policy cannot check",
            "run that program directly; commands start with a clean environment",
        ),
        _ => (
            "privilege, system and process-control commands are not allowed",
            "do the work with the file tools or the project's own commands",
        ),
    };
    format!("'{exe}' is blocked: {why}; {instead}")
}
fn relative(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).display().to_string()
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum DiffOp {
    Same,
    Del,
    Ins,
}

/// Line-oriented diff via the Myers O(ND) greedy algorithm.
/// Inputs are capped by the caller; fine for audit-sized diffs.
fn myers_ops(a: &[&str], b: &[&str]) -> Vec<DiffOp> {
    use DiffOp::{Del, Ins, Same};
    let n = a.len() as i32;
    let m = b.len() as i32;
    let max = (n + m) as usize;
    if max == 0 {
        return Vec::new();
    }
    let off = max as i32;
    let idx = |k: i32| (k + off) as usize;
    let mut v = vec![0i32; 2 * max + 1];
    let mut trace: Vec<Vec<i32>> = Vec::new();
    let mut d_final = 0i32;
    'outer: for d in 0..=(n + m) {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let mut x = if k == -d || (k != d && v[idx(k - 1)] < v[idx(k + 1)]) {
                v[idx(k + 1)]
            } else {
                v[idx(k - 1)] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[idx(k)] = x;
            if x >= n && y >= m {
                d_final = d;
                break 'outer;
            }
            k += 2;
        }
    }
    let mut ops = Vec::new();
    let (mut x, mut y) = (n, m);
    let mut d = d_final;
    while d > 0 {
        let vt = &trace[d as usize];
        let k = x - y;
        let prev_k = if k == -d || (k != d && vt[idx(k - 1)] < vt[idx(k + 1)]) {
            k + 1
        } else {
            k - 1
        };
        let (prev_x, prev_y) = (vt[idx(prev_k)], vt[idx(prev_k)] - prev_k);
        while x > prev_x && y > prev_y {
            ops.push(Same);
            x -= 1;
            y -= 1;
        }
        ops.push(if x == prev_x { Ins } else { Del });
        x = prev_x;
        y = prev_y;
        d -= 1;
    }
    while x > 0 && y > 0 {
        ops.push(Same);
        x -= 1;
        y -= 1;
    }
    ops.reverse();
    ops
}

/// Unified diff with `@@` hunk headers, capped in size. Replaces the old
/// before/after dump; the desktop receipt card renders this string as-is,
/// and the CLI parses the hunk headers for diff-hunk provenance.
fn simple_diff(old: &str, new: &str, label: &str) -> String {
    const MAX_CHARS: usize = 12000;
    const MAX_LINES: usize = 2000;
    const CONTEXT: usize = 3;

    let a: Vec<&str> = old.lines().take(MAX_LINES).collect();
    let b: Vec<&str> = new.lines().take(MAX_LINES).collect();
    let ops = myers_ops(&a, &b);

    // Group ops into hunks: a change plus CONTEXT lines each side,
    // merged when they overlap.
    let mut hunks: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < ops.len() {
        if ops[i] == DiffOp::Same {
            i += 1;
            continue;
        }
        let start = i.saturating_sub(CONTEXT);
        let mut last_change = i;
        let mut j = i;
        while j < ops.len() {
            if ops[j] != DiffOp::Same {
                last_change = j;
            }
            if j - last_change > 2 * CONTEXT {
                break;
            }
            j += 1;
        }
        let end = (last_change + CONTEXT + 1).min(ops.len());
        match hunks.last_mut() {
            Some(prev) if start <= prev.1 => prev.1 = end,
            _ => hunks.push((start, end)),
        }
        i = end;
    }

    let mut out = format!("--- {label}\n+++ {label}\n");
    // Line numbers consumed before each op index.
    let mut a_line = vec![0usize; ops.len() + 1];
    let mut b_line = vec![0usize; ops.len() + 1];
    for (k, op) in ops.iter().enumerate() {
        a_line[k + 1] = a_line[k] + usize::from(*op != DiffOp::Ins);
        b_line[k + 1] = b_line[k] + usize::from(*op != DiffOp::Del);
    }
    for (start, end) in hunks {
        let (al, bl) = (a_line[start], b_line[start]);
        let (ac, bc) = (a_line[end] - al, b_line[end] - bl);
        // Unified convention: empty range starts at the line before.
        let (as_, ac_) = if ac == 0 {
            (al.saturating_sub(1), 0)
        } else {
            (al + 1, ac)
        };
        let (bs_, bc_) = if bc == 0 {
            (bl.saturating_sub(1), 0)
        } else {
            (bl + 1, bc)
        };
        out.push_str(&format!("@@ -{as_},{ac_} +{bs_},{bc_} @@\n"));
        for (k, op) in ops[start..end].iter().enumerate() {
            let line = match op {
                DiffOp::Same => a[a_line[start + k]],
                DiffOp::Del => a[a_line[start + k]],
                DiffOp::Ins => b[b_line[start + k]],
            };
            out.push(match op {
                DiffOp::Same => ' ',
                DiffOp::Del => '-',
                DiffOp::Ins => '+',
            });
            out.push_str(line);
            out.push('\n');
            if out.len() > MAX_CHARS {
                out.truncate(MAX_CHARS);
                out.push_str("\n[diff truncated]");
                return out;
            }
        }
    }
    out
}
/// Drain a pipe keeping the first `limit/2` and the last `limit/2` bytes,
/// so a long build log keeps both its start and the final error. The
/// reader is always drained to EOF so the child never blocks on a full
/// pipe.
fn read_capped<R: Read>(mut r: R, limit: usize) -> (Vec<u8>, bool) {
    let head_cap = limit / 2;
    let tail_cap = limit - head_cap;
    let mut head = Vec::new();
    let mut tail: VecDeque<u8> = VecDeque::new();
    let mut dropped = 0usize;
    let mut buf = [0u8; 8192];
    loop {
        match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let mut chunk = &buf[..n];
                if head.len() < head_cap {
                    let take = chunk.len().min(head_cap - head.len());
                    head.extend_from_slice(&chunk[..take]);
                    chunk = &chunk[take..];
                }
                tail.extend(chunk.iter().copied());
                while tail.len() > tail_cap {
                    tail.pop_front();
                    dropped += 1;
                }
            }
            Err(_) => break,
        }
    }
    let truncated = dropped > 0;
    if truncated {
        head.extend_from_slice(
            format!("\n[... {dropped} bytes of output omitted ...]\n").as_bytes(),
        );
    }
    head.extend(tail);
    (head, truncated)
}
/// A command's two streams as the model sees them: labelled, decoded
/// lossily and cleaned of terminal codes; empty streams are left out.
fn command_output(stdout: &[u8], stderr: &[u8]) -> String {
    let mut combined = String::new();
    for (label, bytes) in [("stdout", stdout), ("stderr", stderr)] {
        if bytes.is_empty() {
            continue;
        }
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(label);
        combined.push_str(":\n");
        combined.push_str(&term::clean_terminal_text(&String::from_utf8_lossy(bytes)));
    }
    combined
}

/// A failed command's status is actionable even when it printed nothing.
/// On Unix, a signal has no exit code: do not tell the model it exited
/// with `None` when it was actually killed by the OS.
fn failed_command_detail(status: std::process::ExitStatus, output: &str) -> String {
    let cause = if let Some(code) = status.code() {
        format!("command exited with code {code}")
    } else {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            match status.signal() {
                Some(signal) => format!("command was terminated by signal {signal}"),
                None => "command ended without an exit code".to_string(),
            }
        }
        #[cfg(not(unix))]
        {
            "command ended without an exit code".to_string()
        }
    };
    if output.is_empty() {
        format!("{cause}; it printed nothing")
    } else {
        format!("{cause}; output:\n{output}")
    }
}

/// The error for a command stopped at its time limit. What it printed
/// before being stopped is kept (a hung test run has usually already
/// named its failures), with a hint to retry with a larger limit, as
/// opencode's shell tool gives.
///
/// At the cap there is no larger limit to ask for, and a model told to
/// "retry with a larger timeout" loops; so it is told the limit was the
/// most allowed (and that its own larger request was cut down), and to
/// split the work instead.
fn timeout_detail(requested_ms: u64, output: &str) -> String {
    let limit_ms = requested_ms.min(MAX_TIMEOUT_MS);
    let mut detail = format!(
        "command exceeded its time limit of {limit_ms} ms and its process group was terminated; "
    );
    if limit_ms < MAX_TIMEOUT_MS {
        detail.push_str(&format!(
            "if it is not waiting for input, retry with a larger timeout_ms (at most {MAX_TIMEOUT_MS})"
        ));
    } else {
        if requested_ms > MAX_TIMEOUT_MS {
            detail.push_str(&format!(
                "the {requested_ms} ms asked for was cut to the maximum; "
            ));
        }
        detail.push_str(
            "that is the longest run_command allows, so split the work into smaller commands (one test file, package or target at a time)",
        );
    }
    if output.is_empty() {
        detail.push_str("; it printed nothing");
    } else {
        detail.push_str("; output before it was stopped:\n");
        detail.push_str(output);
    }
    detail
}

fn kill_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
}
/// True when `text` holds something the output redactor would hide
/// (API keys, tokens, bearer credentials, `password=` values).
pub fn contains_secret(text: &str) -> bool {
    redact(text).1 > 0
}

fn redact(input: &str) -> (String, usize) {
    let patterns = [
        r"(?i)(api[_-]?key|token|secret|password)\s*[:=]\s*[^\s,;]+",
        r"\bgh[pousr]_[A-Za-z0-9_]{20,}\b",
        r"\bsk-[A-Za-z0-9_-]{16,}\b",
        r"\bAIza[A-Za-z0-9_-]{20,}\b",
        r"(?i)bearer\s+[A-Za-z0-9._~-]{12,}",
    ];
    let mut text = input.to_string();
    let mut total = 0;
    for p in patterns {
        let re = Regex::new(p).expect("valid redaction regex");
        let n = re.find_iter(&text).count();
        if n > 0 {
            text = re.replace_all(&text, "[REDACTED]").into_owned();
            total += n;
        }
    }
    (text, total)
}

/// Append the post-write syntax note, if any, to a write result message.
/// Smallest old and new file size, in bytes, for [`rewrite_hint`].
const REWRITE_HINT_MIN_BYTES: usize = 12_000;

/// A note for a whole-file overwrite that mostly re-sends lines already in
/// the file: at least 80% of lines kept (counted as a line multiset, so
/// the cost stays linear), with both versions at least
/// `REWRITE_HINT_MIN_BYTES`. Re-sending a large file to change a few lines
/// costs output tokens for every unchanged line; `edit_file` and
/// `apply_patch` send only the change. Hermes adds the same kind of hint
/// to `write_file` (`_whole_file_rewrite_hint`, `tools/file_tools.py`).
fn rewrite_hint(old: &str, new: &str) -> Option<String> {
    if old.len() < REWRITE_HINT_MIN_BYTES || new.len() < REWRITE_HINT_MIN_BYTES {
        return None;
    }
    let mut left: HashMap<&str, usize> = HashMap::new();
    let mut old_lines = 0usize;
    for line in old.lines() {
        *left.entry(line).or_default() += 1;
        old_lines += 1;
    }
    let mut new_lines = 0usize;
    let mut kept = 0usize;
    for line in new.lines() {
        new_lines += 1;
        if let Some(n) = left.get_mut(line).filter(|n| **n > 0) {
            *n -= 1;
            kept += 1;
        }
    }
    let most = old_lines.max(new_lines);
    if most == 0 || kept * 5 < most * 4 {
        return None;
    }
    Some(format!(
        "note: {kept} of {new_lines} lines were already in the file, so about {} changed; \
         for a change like this use edit_file or apply_patch, which send only the changed lines",
        most - kept
    ))
}

fn with_check(msg: String, rel: &str, before: Option<&str>, after: &str) -> String {
    match check::delta_note(rel, before, after) {
        Some(note) => format!("{msg}\n{note}"),
        None => msg,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn temp() -> PathBuf {
        let p = std::env::temp_dir().join(new_call_id());
        fs::create_dir_all(&p).unwrap();
        p
    }
    fn approved(rt: &ToolRuntime, req: ToolRequest) -> ToolResult {
        let p = rt.prepare(req).unwrap();
        if p.approval_required {
            rt.resolve_approval(&p.call_id, true).unwrap();
        }
        rt.execute(&p.call_id)
    }

    #[test]
    fn repair_tool_args_fixes_only_clear_slips() {
        let mut a = json!({"path":"p","offset":" 3 ","limit":"null","max_results":7.0,"regex":"False","overwrite":"null","replace_all":"yes","timeout_ms":-1.0});
        let mut fixed = repair_tool_args("read_file", &mut a);
        fixed.sort();
        assert_eq!(fixed, ["limit", "max_results", "offset", "regex"]);
        assert_eq!(a["offset"], json!(3));
        assert_eq!(a["limit"], Value::Null);
        assert_eq!(a["max_results"], json!(7));
        assert_eq!(a["regex"], json!(false));
        // required flags keep "null"; unclear strings and negatives stay
        assert_eq!(a["overwrite"], json!("null"));
        assert_eq!(a["replace_all"], json!("yes"));
        assert_eq!(a["timeout_ms"], json!(-1.0));
        for bad in ["", "3.5", "-2", "1e3", "12a", "+5"] {
            let mut v = json!({ "limit": bad });
            assert!(repair_tool_args("read_file", &mut v).is_empty(), "{bad}");
        }
        let mut v = json!({"limit": 2.5, "path": "5"});
        assert!(repair_tool_args("read_file", &mut v).is_empty());
        let mut v = json!({"overwrite": "true", "replace_all": " FALSE "});
        assert_eq!(repair_tool_args("edit_file", &mut v).len(), 2);
        assert_eq!(v, json!({"overwrite": true, "replace_all": false}));
    }

    #[test]
    fn repair_tool_args_decodes_argv_and_mcp_arguments_strings() {
        let mut v = json!({"argv": " [\"git\", \"status\"]"});
        assert_eq!(repair_tool_args("run_command", &mut v), ["argv"]);
        assert_eq!(v["argv"], json!(["git", "status"]));
        for bad in ["git status", "[]", "[1]", "[\"a\", 2]", "[oops"] {
            let mut v = json!({ "argv": bad });
            assert!(repair_tool_args("run_command", &mut v).is_empty(), "{bad}");
        }
        // only run_command's argv and mcp_call's arguments are decoded
        let mut v = json!({"argv": "[\"a\"]", "arguments": "{\"k\":1}"});
        assert!(repair_tool_args("read_file", &mut v).is_empty());
        let mut v = json!({"arguments": "{\"k\":1}"});
        assert_eq!(repair_tool_args("mcp_call", &mut v), ["arguments"]);
        assert_eq!(v["arguments"], json!({"k": 1}));
        for bad in ["[1]", "{bad", "k=1"] {
            let mut v = json!({ "arguments": bad });
            assert!(repair_tool_args("mcp_call", &mut v).is_empty(), "{bad}");
        }
        assert!(repair_tool_args("read_file", &mut json!("x")).is_empty());
        assert_eq!(TOOL_NAMES.len(), 8);
    }

    #[test]
    fn journaled_runtime_undoes_agent_writes_but_keeps_user_edits() {
        let root = temp();
        let jdir = temp();
        let rt = ToolRuntime::new(&root)
            .unwrap()
            .with_journal(&jdir)
            .unwrap();
        fs::write(root.join("a.txt"), "orig\n").unwrap();
        assert!(
            approved(
                &rt,
                ToolRequest::EditFile {
                    path: "a.txt".into(),
                    expected: "orig".into(),
                    replacement: "agent".into(),
                    replace_all: false,
                }
            )
            .ok
        );
        assert!(approved(&rt, ToolRequest::ApplyPatch {
            patch: "*** Begin Patch\n*** Add File: b.txt\n+new\n*** Update File: a.txt\n@@\n-agent\n+agent2\n*** End Patch".into(),
        }).ok);
        // a failed write is not journaled
        assert!(
            !approved(
                &rt,
                ToolRequest::EditFile {
                    path: "a.txt".into(),
                    expected: "no such text".into(),
                    replacement: "x".into(),
                    replace_all: false,
                }
            )
            .ok
        );
        assert_eq!(rt.undo_last_write().unwrap().tool, "apply_patch");
        assert!(!root.join("b.txt").exists());
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "agent\n");
        // the runtime still accepts writes after undo (seen state updated)
        fs::write(root.join("a.txt"), "user\n").unwrap();
        let e = rt.undo_last_write().unwrap_err();
        assert_eq!(e.kind, ErrorKind::Conflict);
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "user\n");
        // an un-journaled runtime says so
        let plain = ToolRuntime::new(temp()).unwrap();
        assert!(plain.undo_last_write().is_err());
    }

    #[test]
    fn writes_report_syntax_breakage_but_never_block() {
        let root = temp();
        let rt = ToolRuntime::new(&root).unwrap();
        let r = approved(
            &rt,
            ToolRequest::CreateFile {
                path: "src/a.rs".into(),
                content: "fn a() {\n    let x = (1 + 2;\n}\n".into(),
                overwrite: false,
            },
        );
        assert!(r.ok);
        let out = r.output.unwrap();
        assert!(
            out.contains("syntax check: this write left the file with: 3:1"),
            "{out}"
        );
        assert!(root.join("src/a.rs").exists());
        // fixing it: edit result is clean, no note
        let r = approved(
            &rt,
            ToolRequest::EditFile {
                path: "src/a.rs".into(),
                expected: "(1 + 2;".into(),
                replacement: "(1 + 2);".into(),
                replace_all: false,
            },
        );
        assert!(r.ok);
        assert!(!r.output.unwrap().contains("syntax check"));
        // a patch that breaks JSON is reported per file
        let r = approved(
            &rt,
            ToolRequest::ApplyPatch {
                patch: "*** Begin Patch\n*** Add File: cfg.json\n+{\"a\": \n*** End Patch".into(),
            },
        );
        assert!(r.ok, "{:?}", r.error);
        let out = r.output.unwrap();
        assert!(out.contains("cfg.json: syntax check"), "{out}");
        // unchecked types stay silent
        let r = approved(
            &rt,
            ToolRequest::CreateFile {
                path: "notes.md".into(),
                content: "((( unbalanced prose".into(),
                overwrite: false,
            },
        );
        assert!(!r.output.unwrap().contains("syntax check"));
    }

    #[test]
    fn traversal_refused() {
        let rt = ToolRuntime::new(temp()).unwrap();
        let p = rt
            .prepare(ToolRequest::ReadFile {
                path: "../secret".into(),
                offset: None,
                limit: None,
            })
            .unwrap();
        let r = rt.execute(&p.call_id);
        assert_eq!(r.error.unwrap().kind, ErrorKind::OutsideWorkspace);
    }
    #[test]
    fn writes_need_distinct_approval() {
        let root = temp();
        let rt = ToolRuntime::new(&root).unwrap();
        let p = rt
            .prepare(ToolRequest::CreateFile {
                path: "a.txt".into(),
                content: "hi".into(),
                overwrite: false,
            })
            .unwrap();
        assert!(p.approval_required);
        assert_eq!(
            rt.execute(&p.call_id).error.unwrap().kind,
            ErrorKind::ApprovalRequired
        );
        rt.resolve_approval(&p.call_id, true).unwrap();
        assert!(rt.execute(&p.call_id).ok);
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "hi");
    }
    #[test]
    fn mcp_call_needs_approval_and_round_trips_through_caller() {
        struct Fake;
        impl McpCaller for Fake {
            fn call(&self, server: &str, name: &str, arguments: &Value) -> Result<Value, String> {
                assert_eq!(server, "fake");
                assert_eq!(name, "echo");
                Ok(json!({
                    "content": [{"type": "text", "text": format!("echo:{}", arguments["text"].as_str().unwrap_or(""))}]
                }))
            }
        }
        let rt = ToolRuntime::new(temp())
            .unwrap()
            .with_mcp_caller(Arc::new(Fake));
        let p = rt
            .prepare(ToolRequest::McpCall {
                server: "fake".into(),
                name: "echo".into(),
                arguments: json!({"text": "hello"}),
            })
            .unwrap();
        assert!(p.approval_required);
        assert_eq!(p.risk, RiskClass::Execute);
        // Approval gate still applies: executing before approval fails.
        assert_eq!(
            rt.execute(&p.call_id).error.unwrap().kind,
            ErrorKind::ApprovalRequired
        );
        rt.resolve_approval(&p.call_id, true).unwrap();
        let r = rt.execute(&p.call_id);
        assert!(r.ok, "mcp_call failed: {:?}", r.error);
        assert_eq!(r.output.as_deref(), Some("echo:hello"));
        assert_eq!(r.tool, "mcp_call");
    }

    #[test]
    fn mcp_call_without_caller_fails_honestly() {
        let rt = ToolRuntime::new(temp()).unwrap();
        let p = rt
            .prepare(ToolRequest::McpCall {
                server: "fake".into(),
                name: "echo".into(),
                arguments: json!({}),
            })
            .unwrap();
        assert!(p.approval_required);
        rt.resolve_approval(&p.call_id, true).unwrap();
        let r = rt.execute(&p.call_id);
        assert!(!r.ok);
        assert!(r.error.unwrap().detail.contains("no MCP servers connected"));
    }

    #[test]
    fn mcp_call_server_error_is_not_a_silent_success() {
        struct Failing;
        impl McpCaller for Failing {
            fn call(&self, _s: &str, _n: &str, _a: &Value) -> Result<Value, String> {
                Ok(json!({
                    "content": [{"type": "text", "text": "boom"}],
                    "isError": true
                }))
            }
        }
        let rt = ToolRuntime::new(temp())
            .unwrap()
            .with_mcp_caller(Arc::new(Failing));
        let p = rt
            .prepare(ToolRequest::McpCall {
                server: "s".into(),
                name: "t".into(),
                arguments: json!({}),
            })
            .unwrap();
        rt.resolve_approval(&p.call_id, true).unwrap();
        let r = rt.execute(&p.call_id);
        assert!(!r.ok);
        assert!(r.error.unwrap().detail.contains("boom"));
    }

    #[test]
    fn mcp_call_rejects_empty_server_or_tool() {
        let rt = ToolRuntime::new(temp()).unwrap();
        assert!(rt
            .prepare(ToolRequest::McpCall {
                server: "".into(),
                name: "t".into(),
                arguments: json!({}),
            })
            .is_err());
        assert!(rt
            .prepare(ToolRequest::McpCall {
                server: "s".into(),
                name: "  ".into(),
                arguments: json!({}),
            })
            .is_err());
    }

    fn numbered(n: usize) -> Vec<String> {
        (0..n)
            .map(|i| format!("let value_{i} = compute({i}); // padding text"))
            .collect()
    }
    #[test]
    fn rewrite_hint_needs_a_big_mostly_unchanged_file() {
        let old = numbered(1000);
        let with_changes = |k: usize| {
            let mut v = old.clone();
            for line in v.iter_mut().take(k) {
                line.push_str(" changed");
            }
            v.join("\n")
        };
        let old_text = old.join("\n");
        let h = rewrite_hint(&old_text, &with_changes(10)).unwrap();
        assert!(h.contains("990 of 1000 lines"), "{h}");
        assert!(h.contains("about 10 changed"), "{h}");
        assert!(h.contains("edit_file or apply_patch"), "{h}");
        // dropped lines count as changed
        let h = rewrite_hint(&old_text, &old[..900].join("\n")).unwrap();
        assert!(h.contains("900 of 900 lines"), "{h}");
        assert!(h.contains("about 100 changed"), "{h}");
        // 80% kept is the edge
        assert!(rewrite_hint(&old_text, &with_changes(200)).is_some());
        assert!(rewrite_hint(&old_text, &with_changes(201)).is_none());
        // growth counts against the ratio: 1000 kept of 1300
        let mut grown = old.clone();
        grown.extend(numbered(1300).into_iter().skip(1000).map(|l| l + " new"));
        assert!(rewrite_hint(&old_text, &grown.join("\n")).is_none());
        // lines are counted as a multiset, not a set
        let same = "x".repeat(20);
        let one = vec![same.as_str(); 1000].join("\n");
        let two = vec![same.as_str(); 2000].join("\n");
        assert!(rewrite_hint(&one, &two).is_none());
        assert!(rewrite_hint(&two, &one).is_none());
        // small files never get the note, on either side
        let small_old = numbered(100).join("\n");
        let mut small_new = numbered(100);
        small_new[0].push('!');
        assert!(small_old.len() < REWRITE_HINT_MIN_BYTES);
        assert!(rewrite_hint(&small_old, &small_new.join("\n")).is_none());
        let big_new = format!("{small_old}\n{}", "y".repeat(REWRITE_HINT_MIN_BYTES));
        assert!(rewrite_hint(&small_old, &big_new).is_none());
        assert!(rewrite_hint(&big_new, &small_old).is_none());
        // exactly at the size floor still counts
        let floor = "a".repeat(REWRITE_HINT_MIN_BYTES);
        assert!(rewrite_hint(&floor, &floor).is_some());
    }
    #[test]
    fn overwrite_of_a_big_file_suggests_edit_tools() {
        let root = temp();
        let old = numbered(1000).join("\n");
        fs::write(root.join("big.rs"), &old).unwrap();
        let rt = ToolRuntime::new(&root).unwrap();
        let read = ToolRequest::ReadFile {
            path: "big.rs".into(),
            offset: None,
            limit: None,
        };
        assert!(approved(&rt, read).ok);
        let write = |path: &str, content: String| {
            approved(
                &rt,
                ToolRequest::CreateFile {
                    path: path.into(),
                    content,
                    overwrite: true,
                },
            )
        };
        let r = write("big.rs", old.replacen("value_5 ", "value_five ", 1));
        assert!(r.ok, "{:?}", r.error);
        let out = r.output.unwrap();
        assert!(out.starts_with("wrote "), "{out}");
        assert!(out.contains("note: 999 of 1000 lines"), "{out}");
        // a new file gets no note
        let r = write("fresh.rs", old.clone());
        assert!(!r.output.unwrap().contains("note:"));
    }
    #[test]
    fn read_file_hides_the_byte_order_mark() {
        let root = temp();
        fs::write(root.join("b.txt"), "\u{FEFF}one\n\u{FEFF}two\n").unwrap();
        let rt = ToolRuntime::new(&root).unwrap();
        let read = |offset: Option<usize>, limit: Option<usize>| {
            let r = approved(
                &rt,
                ToolRequest::ReadFile {
                    path: "b.txt".into(),
                    offset,
                    limit,
                },
            );
            r.output.unwrap()
        };
        // whole file: only the leading mark goes
        assert_eq!(read(None, None), "one\n\u{FEFF}two\n");
        // paged: the same, line numbers unchanged
        let out = read(Some(1), Some(2));
        assert!(
            out.starts_with("     1\tone\n     2\t\u{FEFF}two\n"),
            "{out:?}"
        );
        assert!(read(Some(2), Some(1)).starts_with("     2\t\u{FEFF}two\n"));
        // the file itself is untouched
        assert!(fs::read(root.join("b.txt"))
            .unwrap()
            .starts_with(b"\xEF\xBB\xBF"));
    }
    #[test]
    fn byte_order_mark_survives_edits_and_overwrites() {
        let root = temp();
        fs::write(root.join("b.cs"), "\u{FEFF}using System;\nclass A {}\n").unwrap();
        fs::write(root.join("plain.cs"), "class P {}\n").unwrap();
        let rt = ToolRuntime::new(&root).unwrap();
        let bytes = |p: &str| fs::read(root.join(p)).unwrap();
        let r = approved(
            &rt,
            ToolRequest::EditFile {
                path: "b.cs".into(),
                expected: "using System;".into(),
                replacement: "using System.IO;".into(),
                replace_all: false,
            },
        );
        assert!(r.ok, "{:?}", r.error);
        assert_eq!(
            bytes("b.cs"),
            "\u{FEFF}using System.IO;\nclass A {}\n".as_bytes()
        );
        let write = |path: &str, content: &str| {
            approved(
                &rt,
                ToolRequest::CreateFile {
                    path: path.into(),
                    content: content.into(),
                    overwrite: true,
                },
            )
        };
        // overwrite without the mark keeps it; with the mark it is not doubled
        assert!(write("b.cs", "class B {}\n").ok);
        assert_eq!(bytes("b.cs"), "\u{FEFF}class B {}\n".as_bytes());
        assert!(write("b.cs", "\u{FEFF}class C {}\n").ok);
        assert_eq!(bytes("b.cs"), "\u{FEFF}class C {}\n".as_bytes());
        // files without one never gain one
        let read = ToolRequest::ReadFile {
            path: "plain.cs".into(),
            offset: None,
            limit: None,
        };
        assert!(approved(&rt, read).ok);
        assert!(write("plain.cs", "class Q {}\n").ok);
        assert_eq!(bytes("plain.cs"), b"class Q {}\n");
        assert!(write("new.cs", "class N {}\n").ok);
        assert_eq!(bytes("new.cs"), b"class N {}\n");
    }
    #[test]
    fn overwrite_keeps_crlf_line_endings() {
        let root = temp();
        fs::write(root.join("w.bat"), "@echo off\r\necho hi\r\n").unwrap();
        fs::write(root.join("m.txt"), "a\r\nb\nc\nd\n").unwrap();
        fs::write(root.join("bc.cs"), "\u{FEFF}class A {}\r\n").unwrap();
        let rt = ToolRuntime::new(&root).unwrap();
        let bytes = |p: &str| fs::read(root.join(p)).unwrap();
        for p in ["w.bat", "m.txt", "bc.cs"] {
            let read = ToolRequest::ReadFile {
                path: p.into(),
                offset: None,
                limit: None,
            };
            assert!(approved(&rt, read).ok);
        }
        let write = |path: &str, content: &str| {
            let r = approved(
                &rt,
                ToolRequest::CreateFile {
                    path: path.into(),
                    content: content.into(),
                    overwrite: true,
                },
            );
            assert!(r.ok, "{:?}", r.error);
        };
        // bare LF over a CRLF file becomes CRLF
        write("w.bat", "@echo off\necho bye\n");
        assert_eq!(bytes("w.bat"), b"@echo off\r\necho bye\r\n");
        // text with its own CRLF is written as given
        write("w.bat", "one\r\ntwo\n");
        assert_eq!(bytes("w.bat"), b"one\r\ntwo\n");
        // a mostly-LF file stays LF
        write("m.txt", "x\ny\n");
        assert_eq!(bytes("m.txt"), b"x\ny\n");
        // mark and CRLF are both kept
        write("bc.cs", "class B {}\nclass C {}\n");
        assert_eq!(
            bytes("bc.cs"),
            "\u{FEFF}class B {}\r\nclass C {}\r\n".as_bytes()
        );
        // a new file is written as given
        write("n.txt", "p\nq\n");
        assert_eq!(bytes("n.txt"), b"p\nq\n");
    }
    #[test]
    fn folder_given_to_a_file_tool_says_so() {
        let root = temp();
        fs::create_dir_all(root.join("src/sub")).unwrap();
        fs::write(root.join("src/a.rs"), "x\n").unwrap();
        let rt = ToolRuntime::new(&root).unwrap();
        let want = "src/sub is a folder, not a file; read_file on it lists what is inside";
        let r = approved(
            &rt,
            ToolRequest::EditFile {
                path: "src/sub".into(),
                expected: "x".into(),
                replacement: "y".into(),
                replace_all: false,
            },
        );
        assert_eq!(r.error.unwrap().detail, want);
        let r = approved(
            &rt,
            ToolRequest::CreateFile {
                path: "src/sub".into(),
                content: "y".into(),
                overwrite: true,
            },
        );
        assert_eq!(r.error.unwrap().detail, want);
        assert!(root.join("src/sub").is_dir());
        // the pre-approval check says the same
        let e = rt
            .preflight(&ToolRequest::CreateFile {
                path: "src/sub".into(),
                content: "y".into(),
                overwrite: false,
            })
            .unwrap_err();
        assert_eq!(e.detail, want);
        let e = rt
            .preflight(&ToolRequest::CreateFile {
                path: "src/a.rs".into(),
                content: "y".into(),
                overwrite: false,
            })
            .unwrap_err();
        assert_eq!(e.kind, ErrorKind::AlreadyExists);
        assert_eq!(e.detail, "file exists and overwrite is false; to change part of it use edit_file, or to replace all of it read it first and send create_file again with overwrite: true");
        // a file that appears between approval and running gets it too
        let p = rt
            .prepare(ToolRequest::CreateFile {
                path: "late.txt".into(),
                content: "y".into(),
                overwrite: false,
            })
            .unwrap();
        fs::write(root.join("late.txt"), "first").unwrap();
        let _ = rt.resolve_approval(&p.call_id, true);
        let r = rt.execute(&p.call_id);
        assert_eq!(r.error.unwrap().detail, e.detail);
        assert_eq!(fs::read_to_string(root.join("late.txt")).unwrap(), "first");
        let run = |cwd: &str| {
            approved(
                &rt,
                ToolRequest::RunCommand {
                    argv: vec!["ls".into()],
                    cwd: Some(cwd.into()),
                    timeout_ms: None,
                },
            )
            .error
            .unwrap()
            .detail
        };
        assert_eq!(
            run("src/a.rs"),
            "command cwd src/a.rs is a file, not a folder; use cwd \"src\""
        );
        fs::write(root.join("top.txt"), "").unwrap();
        assert_eq!(
            run("top.txt"),
            "command cwd top.txt is a file, not a folder; use cwd \".\""
        );
    }
    #[test]
    fn empty_expected_text_names_the_right_tool() {
        let root = temp();
        fs::write(root.join("a.txt"), "x\n").unwrap();
        let rt = ToolRuntime::new(&root).unwrap();
        let edit = |path: &str| ToolRequest::EditFile {
            path: path.into(),
            expected: String::new(),
            replacement: "y".into(),
            replace_all: false,
        };
        let e = rt.preflight(&edit("a.txt")).unwrap_err();
        assert_eq!(e.kind, ErrorKind::InvalidRequest);
        assert_eq!(e.detail, "expected text must not be empty: give the exact text to replace, or use create_file with overwrite: true to replace the whole file");
        let e = rt.preflight(&edit("new.txt")).unwrap_err();
        assert_eq!(e.detail, "expected text must not be empty, and the file does not exist: use create_file to make a new file");
        // the execute path says the same
        let r = approved(&rt, edit("new.txt"));
        assert_eq!(r.error.unwrap().detail, e.detail);
        let r = approved(&rt, edit("a.txt"));
        assert!(r.error.unwrap().detail.contains("overwrite: true"));
    }
    #[test]
    fn edit_that_changes_nothing_is_refused() {
        let root = temp();
        let text = "fn main() {\n    let x = 1;\n}\n";
        fs::write(root.join("m.rs"), text).unwrap();
        let rt = ToolRuntime::new(&root).unwrap();
        let edit = |expected: &str, replacement: &str| ToolRequest::EditFile {
            path: "m.rs".into(),
            expected: expected.into(),
            replacement: replacement.into(),
            replace_all: false,
        };
        let e = rt.preflight(&edit("let x = 1;", "let x = 1;")).unwrap_err();
        assert_eq!(e.kind, ErrorKind::InvalidRequest);
        assert_eq!(
            e.detail,
            "edit makes no change: expected and replacement are identical"
        );
        let r = approved(&rt, edit("let x = 1;", "let x = 1;"));
        assert!(!r.ok);
        assert!(r.error.unwrap().detail.starts_with("edit makes no change"));
        // a loose match whose replacement is already what the file holds
        let e = rt
            .preflight(&edit("let  x = 1;", "let x = 1;"))
            .unwrap_err();
        assert_eq!(e.kind, ErrorKind::InvalidRequest);
        assert!(
            e.detail.starts_with(
                "edit makes no change: the file already has the replacement text (matched "
            ),
            "{}",
            e.detail
        );
        assert!(
            e.detail.contains("(matched whitespace_normalized)"),
            "{}",
            e.detail
        );
        assert!(e
            .detail
            .ends_with("read the file again if you expected something else"));
        assert_eq!(fs::read_to_string(root.join("m.rs")).unwrap(), text);
        // a real edit still goes through
        let r = approved(&rt, edit("let x = 1;", "let x = 2;"));
        assert!(r.ok, "{:?}", r.error);
        assert!(fs::read_to_string(root.join("m.rs"))
            .unwrap()
            .contains("x = 2"));
        // a change to trailing whitespace alone is still a change
        let r = approved(&rt, edit("}\n", "}"));
        assert!(r.ok, "{:?}", r.error);
        assert!(fs::read_to_string(root.join("m.rs"))
            .unwrap()
            .ends_with("2;\n}"));
    }
    #[test]
    fn similar_names_ranks_likely_meanings() {
        let names = |v: &[(&str, bool)]| {
            v.iter()
                .map(|(n, d)| (n.to_string(), *d))
                .collect::<Vec<_>>()
                .into_iter()
        };
        let pick = |wanted: &str, v: &[(&str, bool)]| {
            similar_names(wanted, names(v))
                .into_iter()
                .map(|(n, _)| n)
                .collect::<Vec<_>>()
        };
        let dir = [
            ("config.yaml", false),
            ("Config.yml", false),
            ("config.yml.bak", false),
            ("zzz.txt", false),
            (".git", true),
        ];
        // other case beats other extension beats containment
        assert_eq!(
            pick("config.yml", &dir),
            ["Config.yml", "config.yaml", "config.yml.bak"]
        );
        // near spelling
        assert_eq!(
            pick("AGENT.md", &[("AGENTS.md", false), ("b.md", false)]),
            ["AGENTS.md"]
        );
        // one name inside the other, both ways, 3+ characters
        assert_eq!(
            pick("main", &[("domain.rs", false), ("ma", false)]),
            ["domain.rs"]
        );
        assert_eq!(
            pick("src_utils_old", &[("utils", true), ("x", false)]),
            ["utils"]
        );
        // .git and unrelated names are never offered; at most three, ties alphabetical
        assert!(pick("git", &[(".git", true), ("qqq", false)]).is_empty());
        assert_eq!(
            pick(
                "a.rs",
                &[
                    ("a.md", false),
                    ("a.c", false),
                    ("a.h", false),
                    ("a.go", false)
                ]
            ),
            ["a.c", "a.go", "a.h"]
        );
        // the directory flag is kept
        assert_eq!(
            similar_names("Src", names(&[("src", true)])),
            [("src".to_string(), true)]
        );
        // rank decides before the alphabet does
        assert_eq!(
            pick("B.md", &[("B.txt", false), ("b.md", false)]),
            ["b.md", "B.txt"]
        );
        assert_eq!(
            pick("lib.rs", &[("alib.rs.old", false), ("lib.go", false)]),
            ["lib.go", "alib.rs.old"]
        );
        // short names are not matched by containment, and dotfiles have no empty stem
        assert!(pick("ab", &[("xaby.rs", false)]).is_empty());
        assert!(pick(".env", &[(".zshrc", false)]).is_empty());
        // the exact name itself is not a suggestion
        assert!(pick("x.rs", &[("x.rs", false)]).is_empty());
    }
    #[cfg(unix)]
    #[test]
    fn missing_path_suggests_nearby_names() {
        let root = temp();
        fs::create_dir_all(root.join("src/tests")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        let rt = ToolRuntime::new(&root).unwrap();
        let read = |path: &str| {
            approved(
                &rt,
                ToolRequest::ReadFile {
                    path: path.into(),
                    offset: None,
                    limit: None,
                },
            )
            .error
            .unwrap()
        };
        let e = read("src/Main.rs");
        assert_eq!(e.kind, ErrorKind::NotFound);
        assert_eq!(
            e.detail,
            "path not found: src/Main.rs; did you mean src/main.rs?"
        );
        let e = read("src/test");
        assert_eq!(
            e.detail,
            "path not found: src/test; did you mean src/tests/?"
        );
        // nothing alike, or no such folder: the plain message
        assert_eq!(read("src/zzzz.py").detail, "path not found: src/zzzz.py");
        assert_eq!(read("nope/main.rs").detail, "path not found: nope/main.rs");
        // a misspelt folder: the fixed path when it exists, else the folder
        assert_eq!(
            read("srcs/main.rs").detail,
            "path not found: srcs/main.rs; did you mean src/main.rs?"
        );
        assert_eq!(
            read("SRC/tests/a/b.rs").detail,
            "path not found: SRC/tests/a/b.rs; did you mean src/?"
        );
        assert_eq!(
            read("src/test/x.rs").detail,
            "path not found: src/test/x.rs; did you mean src/tests/?"
        );
        // a file with a folder's name is not a folder
        fs::write(root.join("docs"), "x").unwrap();
        assert_eq!(read("doc/a.md").detail, "path not found: doc/a.md");
        // two folders alike: no guess
        fs::create_dir_all(root.join("lib1")).unwrap();
        fs::create_dir_all(root.join("lib2")).unwrap();
        assert_eq!(read("lib/x.rs").detail, "path not found: lib/x.rs");
        // a folder reached through a symlink is not listed
        let outside = temp();
        fs::write(outside.join("secret.txt"), "x").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        assert_eq!(
            read("link/secrets.txt").detail,
            "path not found: link/secrets.txt"
        );
        std::os::unix::fs::symlink(root.join("src"), root.join("alias")).unwrap();
        assert_eq!(
            read("alias/Main.rs").detail,
            "path not found: alias/Main.rs"
        );
        // nor a missing folder under a symlinked one
        assert_eq!(
            read("alias/test/x.rs").detail,
            "path not found: alias/test/x.rs"
        );
    }
    #[test]
    fn container_names_are_refused_for_text_writes() {
        for bad in [
            "report.docx",
            "out/Q3.XLSX",
            "deck.pptx",
            "old.doc",
            "sheet.ods",
            "book.epub",
            "tmpl.dotx",
            "data.db-wal",
            "app.sqlite-shm",
            "x.sqlite3-journal",
            "v2.final.docx",
        ] {
            let e = refuse_text_into_container(bad).unwrap_err();
            assert_eq!(e.kind, ErrorKind::InvalidRequest, "{bad}");
            assert!(e.detail.starts_with(bad), "{bad}: {}", e.detail);
        }
        let e = refuse_text_into_container("r.docx").unwrap_err();
        assert!(e.detail.contains(".docx document"), "{}", e.detail);
        assert!(
            e.detail.contains(".md, .txt, .html or .csv"),
            "{}",
            e.detail
        );
        let e = refuse_text_into_container("a.db-wal").unwrap_err();
        assert!(e.detail.contains("SQLite journal"), "{}", e.detail);
        for ok in [
            "notes.rtf",
            "a.md",
            "docx",
            "store.db",
            "x.docx-wal",
            "notes.txt-journal",
            "docs.docx/readme.md",
            "Makefile",
            "archive.docx.md",
        ] {
            assert!(refuse_text_into_container(ok).is_ok(), "{ok}");
        }
    }
    #[test]
    fn text_is_not_written_into_documents() {
        let root = temp();
        let rt = ToolRuntime::new(&root).unwrap();
        let create = ToolRequest::CreateFile {
            path: "report.docx".into(),
            content: "Quarterly report".into(),
            overwrite: false,
        };
        assert_eq!(
            rt.preflight(&create).unwrap_err().kind,
            ErrorKind::InvalidRequest
        );
        let r = approved(&rt, create);
        assert!(!r.ok);
        assert!(r.error.unwrap().detail.contains(".docx document"));
        assert!(!root.join("report.docx").exists());
        // a patch that adds a document, or moves text into one, writes nothing
        let add =
            "*** Begin Patch\n*** Add File: a.md\n+x\n*** Add File: b.xlsx\n+y\n*** End Patch\n";
        let r = approved(&rt, ToolRequest::ApplyPatch { patch: add.into() });
        assert!(!r.ok);
        assert!(r.error.unwrap().detail.contains("b.xlsx"));
        assert!(!root.join("a.md").exists() && !root.join("b.xlsx").exists());
        fs::write(root.join("n.txt"), "one\n").unwrap();
        let mv = "*** Begin Patch\n*** Update File: n.txt\n*** Move to: n.odt\n@@\n-one\n+two\n*** End Patch\n";
        let r = approved(&rt, ToolRequest::ApplyPatch { patch: mv.into() });
        assert!(!r.ok);
        assert_eq!(fs::read_to_string(root.join("n.txt")).unwrap(), "one\n");
        // deleting a stray document is still allowed
        fs::write(root.join("junk.docx"), "not really a doc").unwrap();
        let del = "*** Begin Patch\n*** Delete File: junk.docx\n*** End Patch\n";
        let r = approved(&rt, ToolRequest::ApplyPatch { patch: del.into() });
        assert!(r.ok, "{:?}", r.error);
        assert!(!root.join("junk.docx").exists());
    }
    #[test]
    fn preflight_catches_doomed_writes_before_approval() {
        let root = temp();
        fs::write(root.join("old.txt"), "keep me").unwrap();
        let rt = ToolRuntime::new(&root).unwrap();
        let create = |path: &str, overwrite: bool| ToolRequest::CreateFile {
            path: path.into(),
            content: "new".into(),
            overwrite,
        };
        let edit = |expected: &str| ToolRequest::EditFile {
            path: "old.txt".into(),
            expected: expected.into(),
            replacement: "x".into(),
            replace_all: false,
        };
        let kind = |r: Result<(), ToolError>| r.unwrap_err().kind;
        // new files and reads pass
        assert!(rt.preflight(&create("fresh.txt", false)).is_ok());
        assert!(rt
            .preflight(&ToolRequest::ReadFile {
                path: "old.txt".into(),
                offset: None,
                limit: None
            })
            .is_ok());
        // existing file: no overwrite flag, then an unread overwrite
        assert_eq!(
            kind(rt.preflight(&create("old.txt", false))),
            ErrorKind::AlreadyExists
        );
        assert_eq!(
            kind(rt.preflight(&create("old.txt", true))),
            ErrorKind::Conflict
        );
        // too large is caught too
        let big = ToolRequest::CreateFile {
            path: "fresh.txt".into(),
            content: "a".repeat(MAX_WRITE_BYTES + 1),
            overwrite: false,
        };
        assert_eq!(kind(rt.preflight(&big)), ErrorKind::TooLarge);
        // edits: empty, missing and present expected text; missing file
        assert_eq!(kind(rt.preflight(&edit(""))), ErrorKind::InvalidRequest);
        assert!(rt.preflight(&edit("not there")).is_err());
        assert!(rt.preflight(&edit("keep")).is_ok());
        assert!(rt
            .preflight(&ToolRequest::EditFile {
                path: "nope.txt".into(),
                expected: "a".into(),
                replacement: "b".into(),
                replace_all: false,
            })
            .is_err());
        // an edit of a file over the edit limit is caught
        fs::write(
            root.join("huge.txt"),
            "a".repeat(MAX_FILE_BYTES as usize + 1),
        )
        .unwrap();
        let huge = ToolRequest::EditFile {
            path: "huge.txt".into(),
            expected: "a".into(),
            replacement: "b".into(),
            replace_all: false,
        };
        assert_eq!(kind(rt.preflight(&huge)), ErrorKind::TooLarge);
        // a bad patch is caught
        assert!(rt
            .preflight(&ToolRequest::ApplyPatch {
                patch: "not a patch".into()
            })
            .is_err());
        // after a read the overwrite passes; after an outside change it and
        // the edit are stale again
        assert!(
            approved(
                &rt,
                ToolRequest::ReadFile {
                    path: "old.txt".into(),
                    offset: None,
                    limit: None
                }
            )
            .ok
        );
        assert!(rt.preflight(&create("old.txt", true)).is_ok());
        fs::write(root.join("old.txt"), "keep me, changed").unwrap();
        assert_eq!(
            kind(rt.preflight(&create("old.txt", true))),
            ErrorKind::Conflict
        );
        assert_eq!(kind(rt.preflight(&edit("keep"))), ErrorKind::Conflict);
        // preflight never writes
        assert!(!root.join("fresh.txt").exists());
        assert_eq!(
            fs::read_to_string(root.join("old.txt")).unwrap(),
            "keep me, changed"
        );
    }

    #[test]
    fn denial_is_final() {
        let rt = ToolRuntime::new(temp()).unwrap();
        let p = rt
            .prepare(ToolRequest::EditFile {
                path: "a".into(),
                expected: "x".into(),
                replacement: "y".into(),
                replace_all: false,
            })
            .unwrap();
        rt.resolve_approval(&p.call_id, false).unwrap();
        assert_eq!(
            rt.execute(&p.call_id).error.unwrap().kind,
            ErrorKind::UserDenied
        );
    }
    #[test]
    fn edit_conflict_is_truthful() {
        let root = temp();
        fs::write(root.join("a"), "same same").unwrap();
        let rt = ToolRuntime::new(root).unwrap();
        let p = rt
            .prepare(ToolRequest::EditFile {
                path: "a".into(),
                expected: "same".into(),
                replacement: "new".into(),
                replace_all: false,
            })
            .unwrap();
        rt.resolve_approval(&p.call_id, true).unwrap();
        assert_eq!(
            rt.execute(&p.call_id).error.unwrap().kind,
            ErrorKind::Conflict
        );
    }
    fn run_edit(rt: &ToolRuntime, path: &str, expected: &str, replacement: &str) -> ToolResult {
        let p = rt
            .prepare(ToolRequest::EditFile {
                path: path.into(),
                expected: expected.into(),
                replacement: replacement.into(),
                replace_all: false,
            })
            .unwrap();
        rt.resolve_approval(&p.call_id, true).unwrap();
        rt.execute(&p.call_id)
    }
    #[test]
    fn fuzzy_edit_reports_strategy_and_keeps_indent() {
        let root = temp();
        fs::write(root.join("m.rs"), "fn f() {\n    let a = 1;\n}\n").unwrap();
        let rt = ToolRuntime::new(&root).unwrap();
        // The model dropped the block's indentation.
        let r = run_edit(
            &rt,
            "m.rs",
            "fn f() {\nlet a = 1;\n}\n",
            "fn f() {\nlet a = 2;\nlet b = 3;\n}\n",
        );
        assert!(r.ok, "{:?}", r.error);
        assert!(r.output.unwrap().contains("matched via line_trimmed"));
        assert_eq!(
            fs::read_to_string(root.join("m.rs")).unwrap(),
            "fn f() {\n    let a = 2;\n    let b = 3;\n}\n"
        );
    }
    #[test]
    fn approved_diff_equals_written_bytes_for_fuzzy_edit() {
        let root = temp();
        fs::write(root.join("d.txt"), "  alpha\n  beta\n").unwrap();
        let rt = ToolRuntime::new(&root).unwrap();
        let p = rt
            .prepare(ToolRequest::EditFile {
                path: "d.txt".into(),
                expected: "beta".into(),
                replacement: "gamma".into(),
                replace_all: false,
            })
            .unwrap();
        let diff = rt.pending_diff(&p.call_id).unwrap().unwrap();
        rt.resolve_approval(&p.call_id, true).unwrap();
        assert!(rt.execute(&p.call_id).ok);
        assert_eq!(
            fs::read_to_string(root.join("d.txt")).unwrap(),
            diff.modified
        );
    }
    #[test]
    fn write_after_external_change_is_refused_as_stale() {
        let root = temp();
        fs::write(root.join("s.txt"), "one\ntwo\n").unwrap();
        let rt = ToolRuntime::new(&root).unwrap();
        let p = rt
            .prepare(ToolRequest::ReadFile {
                path: "s.txt".into(),
                offset: None,
                limit: None,
            })
            .unwrap();
        assert!(rt.execute(&p.call_id).ok);
        // Someone else edits the file after the model read it.
        fs::write(root.join("s.txt"), "one\ntwo\nthree\n").unwrap();
        let r = run_edit(&rt, "s.txt", "two", "TWO");
        let e = r.error.unwrap();
        assert_eq!(e.kind, ErrorKind::Conflict);
        assert!(e.detail.contains("changed on disk"));
        // Re-reading clears the stale state.
        let p = rt
            .prepare(ToolRequest::ReadFile {
                path: "s.txt".into(),
                offset: None,
                limit: None,
            })
            .unwrap();
        assert!(rt.execute(&p.call_id).ok);
        assert!(run_edit(&rt, "s.txt", "two", "TWO").ok);
        // A second edit right after our own write is not stale.
        assert!(run_edit(&rt, "s.txt", "three", "3").ok);
        assert_eq!(
            fs::read_to_string(root.join("s.txt")).unwrap(),
            "one\nTWO\n3\n"
        );
    }
    #[test]
    fn overwrite_after_external_change_is_refused_as_stale() {
        let root = temp();
        fs::write(root.join("o.txt"), "v1").unwrap();
        let rt = ToolRuntime::new(&root).unwrap();
        let p = rt
            .prepare(ToolRequest::ReadFile {
                path: "o.txt".into(),
                offset: None,
                limit: None,
            })
            .unwrap();
        assert!(rt.execute(&p.call_id).ok);
        fs::write(root.join("o.txt"), "v2 by user").unwrap();
        let p = rt
            .prepare(ToolRequest::CreateFile {
                path: "o.txt".into(),
                content: "v3".into(),
                overwrite: true,
            })
            .unwrap();
        rt.resolve_approval(&p.call_id, true).unwrap();
        assert_eq!(
            rt.execute(&p.call_id).error.unwrap().kind,
            ErrorKind::Conflict
        );
        assert_eq!(
            fs::read_to_string(root.join("o.txt")).unwrap(),
            "v2 by user"
        );
    }
    #[test]
    fn overwriting_a_file_never_read_is_refused() {
        let root = temp();
        fs::write(root.join("keep.txt"), "user notes").unwrap();
        let rt = ToolRuntime::new(&root).unwrap();
        let write = |path: &str, content: &str| {
            let p = rt
                .prepare(ToolRequest::CreateFile {
                    path: path.into(),
                    content: content.into(),
                    overwrite: true,
                })
                .unwrap();
            rt.resolve_approval(&p.call_id, true).unwrap();
            rt.execute(&p.call_id)
        };
        let r = write("keep.txt", "replaced");
        let e = r.error.unwrap();
        assert_eq!(e.kind, ErrorKind::Conflict);
        assert!(
            e.detail.contains("was not read in this run"),
            "{}",
            e.detail
        );
        assert_eq!(
            fs::read_to_string(root.join("keep.txt")).unwrap(),
            "user notes"
        );
        // a partial read is enough
        let p = rt
            .prepare(ToolRequest::ReadFile {
                path: "keep.txt".into(),
                offset: Some(1),
                limit: Some(1),
            })
            .unwrap();
        assert!(rt.execute(&p.call_id).ok);
        assert!(write("keep.txt", "replaced").ok);
        assert_eq!(
            fs::read_to_string(root.join("keep.txt")).unwrap(),
            "replaced"
        );
        // new files need no read, and a file this run wrote can be rewritten
        assert!(write("new.txt", "a").ok);
        assert!(write("new.txt", "b").ok);
        assert_eq!(fs::read_to_string(root.join("new.txt")).unwrap(), "b");
        // edits need no read: the expected text is the check
        fs::write(root.join("e.txt"), "alpha\n").unwrap();
        assert!(run_edit(&rt, "e.txt", "alpha", "beta").ok);
    }
    #[test]
    fn not_found_edit_tells_model_where_to_look() {
        let root = temp();
        fs::write(root.join("h.rs"), "fn alpha() {}\nfn compute(x: u8) {}\n").unwrap();
        let rt = ToolRuntime::new(&root).unwrap();
        let r = run_edit(&rt, "h.rs", "fn compute(x: u16) {}", "fn c() {}");
        let e = r.error.unwrap();
        assert_eq!(e.kind, ErrorKind::Conflict);
        assert!(e.detail.contains("line 2"), "{}", e.detail);
    }
    fn read(
        rt: &ToolRuntime,
        path: &str,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> ToolResult {
        let p = rt
            .prepare(ToolRequest::ReadFile {
                path: path.into(),
                offset,
                limit,
            })
            .unwrap();
        rt.execute(&p.call_id)
    }
    #[test]
    fn small_file_reads_raw_bytes() {
        let root = temp();
        fs::write(root.join("r.txt"), "a\n  b\n").unwrap();
        let rt = ToolRuntime::new(&root).unwrap();
        assert_eq!(read(&rt, "r.txt", None, None).output.unwrap(), "a\n  b\n");
    }
    #[test]
    fn paged_read_is_numbered_and_says_how_to_continue() {
        let root = temp();
        let body: String = (1..=10).map(|i| format!("line{i}\n")).collect();
        fs::write(root.join("p.txt"), body).unwrap();
        let rt = ToolRuntime::new(&root).unwrap();
        let r = read(&rt, "p.txt", Some(4), Some(3));
        let out = r.output.unwrap();
        assert!(
            out.starts_with("     4\tline4\n     5\tline5\n     6\tline6\n"),
            "{out}"
        );
        assert!(out.contains("[lines 4-6 of 10; call read_file with offset=7 to continue]"));
        assert!(r.receipt.output_truncated);
        let tail = read(&rt, "p.txt", Some(9), None).output.unwrap();
        assert!(tail.contains("end of file"), "{tail}");
        let past = read(&rt, "p.txt", Some(50), None).error.unwrap();
        assert_eq!(past.kind, ErrorKind::InvalidRequest);
    }
    #[test]
    fn long_file_pages_by_default_instead_of_flooding_context() {
        let root = temp();
        let body: String = (1..=2500).map(|i| format!("{i}\n")).collect();
        fs::write(root.join("big.txt"), body).unwrap();
        let rt = ToolRuntime::new(&root).unwrap();
        let out = read(&rt, "big.txt", None, None).output.unwrap();
        assert!(
            out.contains("[lines 1-2000 of 2500; call read_file with offset=2001"),
            "{}",
            &out[out.len() - 120..]
        );
    }
    #[test]
    fn file_over_old_2mib_cap_is_now_pageable() {
        let root = temp();
        let line = "x".repeat(99);
        let body: String = (0..30_000).map(|_| format!("{line}\n")).collect();
        assert!(body.len() > 2 * 1024 * 1024);
        fs::write(root.join("huge.log"), body).unwrap();
        let rt = ToolRuntime::new(&root).unwrap();
        let out = read(&rt, "huge.log", Some(29_999), None).output.unwrap();
        assert!(
            out.contains("[lines 29999-30000 of 30000; end of file]"),
            "{out}"
        );
    }
    #[test]
    fn binary_and_long_lines_are_handled() {
        let root = temp();
        fs::write(root.join("b.bin"), [0u8, 1, 2, 3]).unwrap();
        fs::write(root.join("l.txt"), format!("{}\nshort\n", "y".repeat(5000))).unwrap();
        let rt = ToolRuntime::new(&root).unwrap();
        let e = read(&rt, "b.bin", None, None).error.unwrap();
        assert!(e.detail.contains("binary file"));
        let out = read(&rt, "l.txt", Some(1), None).output.unwrap();
        assert!(out.contains("[line truncated]"));
        assert!(out.contains("1 line(s) longer than 2000 chars were cut"));
    }
    #[test]
    fn empty_file_reads_as_a_note() {
        let root = temp();
        fs::write(root.join("e.txt"), "").unwrap();
        fs::write(root.join("b.txt"), "\u{feff}").unwrap();
        fs::write(root.join("x.txt"), "x").unwrap();
        fs::write(root.join("xyz.txt"), "xyz").unwrap();
        let rt = ToolRuntime::new(&root).unwrap();
        assert_eq!(
            read(&rt, "e.txt", None, None).output.unwrap(),
            "[empty file: 0 bytes]"
        );
        assert_eq!(
            read(&rt, "e.txt", Some(1), None).output.unwrap(),
            "[empty file: 0 bytes]"
        );
        assert_eq!(
            read(&rt, "b.txt", None, None).output.unwrap(),
            "[empty file: only a byte order mark]"
        );
        // short files with content read as themselves
        assert_eq!(read(&rt, "x.txt", None, None).output.unwrap(), "x");
        assert_eq!(read(&rt, "xyz.txt", None, None).output.unwrap(), "xyz");
        // reading an empty file counts as seen, so it can be overwritten
        let p = rt
            .prepare(ToolRequest::CreateFile {
                path: "e.txt".into(),
                content: "now\n".into(),
                overwrite: true,
            })
            .unwrap();
        let _ = rt.resolve_approval(&p.call_id, true);
        let r = rt.execute(&p.call_id);
        assert!(r.ok, "{:?}", r.error);
    }
    #[test]
    fn reading_a_directory_lists_it() {
        let root = temp();
        fs::create_dir_all(root.join("d/sub")).unwrap();
        fs::write(root.join("d/z.txt"), "").unwrap();
        fs::write(root.join("d/a.txt"), "").unwrap();
        let rt = ToolRuntime::new(&root).unwrap();
        assert_eq!(
            read(&rt, "d", None, None).output.unwrap(),
            "a.txt\nsub/\nz.txt"
        );
    }
    fn search(rt: &ToolRuntime, q: &str, regex: bool, include: Option<&str>) -> String {
        let p = rt
            .prepare(ToolRequest::SearchFiles {
                query: q.into(),
                path: None,
                max_results: Some(3),
                regex: Some(regex),
                include: include.map(Into::into),
            })
            .unwrap();
        let r = rt.execute(&p.call_id);
        r.output.unwrap_or_else(|| format!("ERR {:?}", r.error))
    }
    #[test]
    fn search_shows_long_lines_around_the_match() {
        let root = temp();
        let far = format!("{}needle_far{}", "x".repeat(1000), "y".repeat(1000));
        let near = format!("needle_near{}", "z".repeat(500));
        let short = format!("  {}needle_short  ", "w".repeat(20));
        let edge = format!("{}needle_end", "v".repeat(400));
        fs::write(
            root.join("a.txt"),
            format!("{far}\n{near}\n{short}\n{edge}\n"),
        )
        .unwrap();
        fs::write(root.join("b.txt"), "\u{feff}needle_bom here\n").unwrap();
        let rt = ToolRuntime::new(&root).unwrap();
        let hit = |q: &str, re: bool| {
            let out = search(&rt, q, re, None);
            let line = out.lines().next().unwrap().to_string();
            line.splitn(3, ':').nth(2).unwrap().to_string()
        };
        // match deep in a long line: window around it, both ends marked
        let f = hit("NEEDLE_FAR", false);
        assert!(f.contains("needle_far"), "{f}");
        assert!(f.starts_with('…') && f.ends_with('…'), "{f}");
        assert_eq!(f.chars().count(), 302);
        assert!(
            f.starts_with(&format!("…{}needle_far", "x".repeat(100))),
            "{f}"
        );
        // the same through a regex
        let r = hit("needle_f[a]r", true);
        assert_eq!(r, f);
        // match near the start: no leading mark
        let n = hit("needle_near", false);
        assert!(n.starts_with("needle_near") && n.ends_with('…'), "{n}");
        assert_eq!(n.chars().count(), 301);
        // short line: trimmed, unmarked
        assert_eq!(
            hit("needle_short", false),
            format!("{}needle_short", "w".repeat(20))
        );
        // match at the very end: window ends at the line end, no trailing mark
        let e = hit("needle_end", false);
        assert!(e.starts_with('…') && e.ends_with("needle_end"), "{e}");
        assert_eq!(e.chars().count(), 301);
        // one char cut at either end is still marked
        fs::write(
            root.join("c.txt"),
            format!(
                "{}needle_one{}\nneedle_two{}\n",
                "u".repeat(101),
                "t".repeat(300),
                "s".repeat(291)
            ),
        )
        .unwrap();
        let o = hit("needle_one", false);
        assert!(
            o.starts_with(&format!("…{}needle_one", "u".repeat(100))),
            "{o}"
        );
        let t = hit("needle_two", false);
        assert_eq!(t.chars().count(), 301, "{t}");
        assert!(t.ends_with('…'), "{t}");
        // a byte-order mark is not shown
        assert_eq!(hit("needle_bom", false), "needle_bom here");
    }
    #[test]
    fn search_in_one_named_file() {
        let root = temp();
        fs::create_dir_all(root.join("gen")).unwrap();
        fs::write(root.join(".gitignore"), "gen/\n").unwrap();
        fs::write(root.join("a.rs"), "fn one() {}\nfn needle() {}\n").unwrap();
        fs::write(root.join("b.rs"), "fn needle() {}\n").unwrap();
        fs::write(root.join("gen/g.rs"), "needle\n").unwrap();
        let rt = ToolRuntime::new(&root).unwrap();
        let run = |path: &str| {
            let p = rt
                .prepare(ToolRequest::SearchFiles {
                    query: "needle".into(),
                    path: Some(path.into()),
                    max_results: None,
                    regex: None,
                    include: None,
                })
                .unwrap();
            rt.execute(&p.call_id).output.unwrap()
        };
        assert_eq!(run("a.rs"), "a.rs:2:fn needle() {}");
        // named on purpose, so an ignored file is still searched
        assert_eq!(run("gen/g.rs"), "gen/g.rs:1:needle");
        let with = |path: &str, inc: &str| {
            let p = rt
                .prepare(ToolRequest::SearchFiles {
                    query: "needle".into(),
                    path: Some(path.into()),
                    max_results: None,
                    regex: None,
                    include: Some(inc.into()),
                })
                .unwrap();
            rt.execute(&p.call_id).output.unwrap()
        };
        assert_eq!(with("a.rs", "*.rs"), "a.rs:2:fn needle() {}");
        assert_eq!(with("gen/g.rs", "g.rs"), "gen/g.rs:1:needle");
        assert_eq!(with("a.rs", "*.ts"), "[no matches]");
        // a folder still walks, skipping ignored files
        assert_eq!(run("."), "a.rs:2:fn needle() {}\nb.rs:1:fn needle() {}");
    }
    #[test]
    fn glob_with_a_file_path_names_the_folder() {
        let root = temp();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/a.rs"), "").unwrap();
        fs::write(root.join("top.rs"), "").unwrap();
        let rt = ToolRuntime::new(&root).unwrap();
        let run = |path: &str| {
            let p = rt
                .prepare(ToolRequest::GlobFiles {
                    pattern: "*.rs".into(),
                    path: Some(path.into()),
                    max_results: None,
                })
                .unwrap();
            rt.execute(&p.call_id)
        };
        let e = run("src/a.rs").error.unwrap();
        assert_eq!(e.kind, ErrorKind::InvalidRequest);
        assert_eq!(
            e.detail,
            "glob_files path must be a folder, and src/a.rs is a file; use path \"src\" to list its folder, or search_files / read_file to look inside it"
        );
        let e = run("top.rs").error.unwrap();
        assert!(e.detail.contains("use path \".\""), "{}", e.detail);
        assert_eq!(run("src").output.unwrap(), "src/a.rs");
    }
    fn search_repo() -> PathBuf {
        let root = temp();
        fs::create_dir_all(root.join("src/deep")).unwrap();
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::create_dir_all(root.join("node_modules/x")).unwrap();
        fs::create_dir_all(root.join("gen")).unwrap();
        fs::write(root.join(".gitignore"), "gen/\n*.log\n").unwrap();
        fs::write(root.join("src/lib.rs"), "fn needle_one() {}\n").unwrap();
        fs::write(root.join("src/deep/mod.ts"), "const needle_two = 2;\n").unwrap();
        fs::write(root.join("target/debug/out.rs"), "fn needle_junk() {}\n").unwrap();
        fs::write(root.join("node_modules/x/i.js"), "needle_junk\n").unwrap();
        fs::write(root.join("gen/g.rs"), "needle_junk\n").unwrap();
        fs::write(root.join("app.log"), "needle_junk\n").unwrap();
        fs::write(root.join("blob.bin"), b"needle_junk\0\0").unwrap();
        root
    }
    #[test]
    fn search_skips_build_output_ignored_and_binary_files() {
        let rt = ToolRuntime::new(search_repo()).unwrap();
        let out = search(&rt, "needle", false, None);
        assert!(out.contains("src/lib.rs:1:fn needle_one() {}"), "{out}");
        assert!(out.contains("src/deep/mod.ts:1:"), "{out}");
        assert!(!out.contains("junk"), "{out}");
    }
    #[test]
    fn search_supports_regex_and_include_glob() {
        let rt = ToolRuntime::new(search_repo()).unwrap();
        let out = search(&rt, r"needle_(one|two)\b", true, Some("*.ts"));
        assert_eq!(out, "src/deep/mod.ts:1:const needle_two = 2;");
        let out = search(&rt, "needle", false, Some("src/**/*.{rs,ts}"));
        assert!(out.contains("lib.rs") && out.contains("mod.ts"), "{out}");
        assert_eq!(
            search(&rt, "fn foo(", true, None),
            "ERR Some(ToolError { kind: InvalidRequest, detail: \"invalid regex: unclosed group; to find the text exactly as written, send regex: false\" })"
        );
        assert!(search(&rt, "(", true, None).contains("invalid regex"));
        assert_eq!(search(&rt, "zzz_absent", false, None), "[no matches]");
    }
    #[test]
    fn search_says_when_results_were_cut() {
        let root = temp();
        fs::write(root.join("m.txt"), "hit\nhit\nhit\nhit\n").unwrap();
        let rt = ToolRuntime::new(root).unwrap();
        let out = search(&rt, "hit", false, None);
        assert_eq!(out.lines().filter(|l| l.starts_with("m.txt:")).count(), 3);
        assert!(out.contains("[more than 3 matches"), "{out}");
    }
    #[test]
    fn glob_files_finds_by_pattern_and_respects_ignores() {
        let rt = ToolRuntime::new(search_repo()).unwrap();
        let p = rt
            .prepare(ToolRequest::GlobFiles {
                pattern: "**/*.rs".into(),
                path: None,
                max_results: None,
            })
            .unwrap();
        assert!(!p.approval_required);
        let out = rt.execute(&p.call_id).output.unwrap();
        assert_eq!(out, "src/lib.rs");
    }
    #[test]
    fn glob_translation_is_anchored_and_segment_aware() {
        let g = walk::Glob::new("src/*.rs").unwrap();
        assert!(g.matches("src/a.rs"));
        assert!(!g.matches("src/x/a.rs"));
        assert!(!g.matches("xsrc/a.rs"));
        let g = walk::Glob::new("**/test_?.py").unwrap();
        assert!(g.matches("test_a.py") && g.matches("a/b/test_b.py"));
        assert!(walk::Glob::new("{a,b").is_none());
        let g = walk::Glob::new("*.[ch]").unwrap();
        assert!(g.matches("x/y.c") && g.matches("y.h") && !g.matches("y.o"));
    }
    #[test]
    fn clip_middle_keeps_head_and_tail_on_char_boundaries() {
        let text = format!("START{}é-END-ERROR", "é".repeat(5000));
        let (clipped, cut) = clip_middle(&text, 1000);
        assert!(cut);
        assert!(clipped.starts_with("START"));
        assert!(clipped.ends_with("-END-ERROR"));
        assert!(clipped.contains("bytes omitted from the middle"));
        assert!(clipped.len() < 1200);
        for max in 0..40 {
            let _ = clip_middle("ééééé€€€€€𝄞𝄞𝄞", max);
        }
        assert_eq!(clip_middle("short", 100), ("short".to_string(), false));
        assert_eq!(floor_boundary("é", 1), 0);
    }
    #[test]
    fn read_capped_keeps_the_final_error_line() {
        let mut log = "compiling...\n".repeat(10_000);
        log.push_str("error[E0308]: mismatched types\n");
        let (bytes, cut) = read_capped(log.as_bytes(), 4096);
        let text = String::from_utf8_lossy(&bytes);
        assert!(cut);
        assert!(text.starts_with("compiling..."));
        assert!(text.ends_with("error[E0308]: mismatched types\n"));
        assert!(text.contains("bytes of output omitted"));
        let (small, cut) = read_capped(&b"ok\n"[..], 4096);
        assert_eq!((small.as_slice(), cut), (&b"ok\n"[..], false));
    }
    fn patch_call(rt: &ToolRuntime, patch: &str) -> ToolResult {
        let p = rt
            .prepare(ToolRequest::ApplyPatch {
                patch: patch.into(),
            })
            .unwrap();
        assert!(p.approval_required, "patches are writes");
        rt.resolve_approval(&p.call_id, true).unwrap();
        rt.execute(&p.call_id)
    }
    #[test]
    fn apply_patch_changes_several_files_at_once() {
        let root = temp();
        fs::write(root.join("a.rs"), "fn a() {\n    1\n}\n").unwrap();
        fs::write(root.join("old.txt"), "bye\n").unwrap();
        let rt = ToolRuntime::new(&root).unwrap();
        let r = patch_call(
            &rt,
            "*** Begin Patch\n*** Update File: a.rs\n@@\n fn a() {\n-    1\n+    2\n }\n*** Add File: src/new.rs\n+pub fn n() {}\n*** Delete File: old.txt\n*** End Patch\n",
        );
        assert!(r.ok, "{:?}", r.error);
        assert_eq!(
            fs::read_to_string(root.join("a.rs")).unwrap(),
            "fn a() {\n    2\n}\n"
        );
        assert_eq!(
            fs::read_to_string(root.join("src/new.rs")).unwrap(),
            "pub fn n() {}\n"
        );
        assert!(!root.join("old.txt").exists());
        let out = r.output.unwrap();
        assert!(
            out.contains("M a.rs") && out.contains("A src/new.rs") && out.contains("D old.txt"),
            "{out}"
        );
        assert!(r.receipt.diff.unwrap().contains("+    2"));
    }
    #[test]
    fn apply_patch_rolls_back_when_a_later_write_fails() {
        let root = temp();
        fs::write(root.join("a.txt"), "one\n").unwrap();
        fs::write(root.join("blocker"), "i am a file\n").unwrap();
        let rt = ToolRuntime::new(&root).unwrap();
        let r = patch_call(
            &rt,
            "*** Begin Patch\n*** Update File: a.txt\n@@\n-one\n+two\n*** Add File: blocker/x.txt\n+x\n*** End Patch",
        );
        let e = r.error.unwrap();
        assert!(e.detail.contains("rolled back"), "{}", e.detail);
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "one\n");
        let leftovers: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".rex-patch-"))
            .collect();
        assert!(leftovers.is_empty());
    }
    #[test]
    fn apply_patch_refuses_escape_stale_and_bad_hunks_without_writing() {
        let root = temp();
        fs::write(root.join("s.txt"), "v1\n").unwrap();
        let rt = ToolRuntime::new(&root).unwrap();
        let r = patch_call(
            &rt,
            "*** Begin Patch\n*** Add File: ../evil.txt\n+x\n*** End Patch",
        );
        assert_eq!(r.error.unwrap().kind, ErrorKind::OutsideWorkspace);
        let p = rt
            .prepare(ToolRequest::ReadFile {
                path: "s.txt".into(),
                offset: None,
                limit: None,
            })
            .unwrap();
        assert!(rt.execute(&p.call_id).ok);
        fs::write(root.join("s.txt"), "v1 edited by user\n").unwrap();
        let r = patch_call(
            &rt,
            "*** Begin Patch\n*** Update File: s.txt\n@@\n-v1 edited by user\n+v2\n*** End Patch",
        );
        assert!(r.error.unwrap().detail.contains("changed on disk"));
        let r = patch_call(&rt, "*** Begin Patch\n*** Add File: fresh.txt\n+ok\n*** Update File: s.txt\n@@\n-nope\n+v3\n*** End Patch");
        assert_eq!(r.error.unwrap().kind, ErrorKind::Conflict);
        assert!(!root.join("fresh.txt").exists());
        assert_eq!(
            fs::read_to_string(root.join("s.txt")).unwrap(),
            "v1 edited by user\n"
        );
    }
    #[test]
    fn apply_patch_diff_preview_matches_result() {
        let root = temp();
        fs::write(root.join("p.txt"), "a\nb\n").unwrap();
        let rt = ToolRuntime::new(&root).unwrap();
        let p = rt
            .prepare(ToolRequest::ApplyPatch {
                patch: "*** Begin Patch\n*** Update File: p.txt\n@@\n a\n-b\n+c\n*** End Patch"
                    .into(),
            })
            .unwrap();
        let d = rt.pending_diff(&p.call_id).unwrap().unwrap();
        assert_eq!(d.path, "p.txt");
        assert!(d.modified.contains("a\nc\n"));
        rt.resolve_approval(&p.call_id, true).unwrap();
        assert!(rt.execute(&p.call_id).ok);
        assert_eq!(fs::read_to_string(root.join("p.txt")).unwrap(), "a\nc\n");
    }
    #[test]
    fn shell_operators_in_argv_are_refused_up_front() {
        let root = temp();
        let rt = ToolRuntime::new(&root).unwrap();
        let prep = |argv: &[&str]| {
            rt.prepare(ToolRequest::RunCommand {
                argv: argv.iter().map(|s| s.to_string()).collect(),
                cwd: None,
                timeout_ms: None,
            })
        };
        for op in ["&&", "|", ">", "2>&1", ";"] {
            let e = prep(&["cargo", "build", op, "x"]).unwrap_err();
            assert_eq!(e.kind, ErrorKind::InvalidRequest);
            assert_eq!(
                e.detail,
                format!("run_command runs one program with no shell, so '{op}' would be passed to it as a plain argument; run each command as its own call, and read output with read_output instead of redirecting it")
            );
        }
        for b in ["cd", "pushd", "export", "source"] {
            let e = prep(&[b, "x"]).unwrap_err();
            assert_eq!(
                e.detail,
                format!("'{b}' is a shell builtin and run_command has no shell; to run a command in a folder, set the cwd field (e.g. cwd: \"crates/app\") on that command")
            );
        }
        // a builtin name as an argument is fine
        assert!(prep(&["git", "cd"]).is_ok());
        // operators inside a word, and ordinary args, are fine
        assert!(prep(&["grep", "-E", "a|b", "f.txt"]).is_ok());
        assert!(prep(&["git", "log", "--format=%h > %s"]).is_ok());
        assert!(prep(&["cargo", "test"]).is_ok());
    }
    #[test]
    fn failed_command_reports_exit_code_or_signal_and_output() {
        let root = temp();
        let rt = ToolRuntime::new(&root).unwrap();
        let p = rt
            .prepare(ToolRequest::RunCommand {
                argv: vec!["false".into()],
                cwd: None,
                timeout_ms: None,
            })
            .unwrap();
        rt.resolve_approval(&p.call_id, true).unwrap();
        let e = rt.execute(&p.call_id).error.unwrap();
        assert_eq!(e.kind, ErrorKind::ProcessFailed);
        assert_eq!(e.detail, "command exited with code 1; it printed nothing");

        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            let killed = std::process::ExitStatus::from_raw(libc::SIGKILL);
            assert_eq!(
                failed_command_detail(killed, "stderr:\nOOM"),
                "command was terminated by signal 9; output:\nstderr:\nOOM"
            );
            assert_eq!(
                failed_command_detail(killed, ""),
                "command was terminated by signal 9; it printed nothing"
            );
        }
    }
    #[test]
    fn timeout_at_the_cap_says_to_split_the_work() {
        let below = timeout_detail(5_000, "");
        assert_eq!(below, "command exceeded its time limit of 5000 ms and its process group was terminated; if it is not waiting for input, retry with a larger timeout_ms (at most 120000); it printed nothing");
        let at = timeout_detail(MAX_TIMEOUT_MS, "x");
        assert_eq!(at, "command exceeded its time limit of 120000 ms and its process group was terminated; that is the longest run_command allows, so split the work into smaller commands (one test file, package or target at a time); output before it was stopped:\nx");
        let over = timeout_detail(600_000, "");
        assert!(
            over.starts_with("command exceeded its time limit of 120000 ms"),
            "{over}"
        );
        assert!(
            over.contains("; the 600000 ms asked for was cut to the maximum; that is the longest"),
            "{over}"
        );
        assert!(!at.contains("asked for"));
    }
    #[test]
    fn zero_timeout_is_refused_before_the_command_runs() {
        let root = temp();
        let rt = ToolRuntime::new(&root).unwrap();
        let run = |timeout_ms: Option<u64>| {
            let p = rt
                .prepare(ToolRequest::RunCommand {
                    argv: vec!["touch".into(), "made".into()],
                    cwd: None,
                    timeout_ms,
                })
                .unwrap();
            let _ = rt.resolve_approval(&p.call_id, true);
            rt.execute(&p.call_id)
        };
        let e = rt
            .prepare(ToolRequest::RunCommand {
                argv: vec!["touch".into(), "made".into()],
                cwd: None,
                timeout_ms: Some(0),
            })
            .unwrap_err();
        assert_eq!(e.kind, ErrorKind::InvalidRequest);
        assert_eq!(e.detail, "timeout_ms must be a positive number of milliseconds (got 0); omit it for the 30000 ms default or give up to 120000");
        assert!(!root.join("made").exists(), "the command must not run");
        assert!(run(None).error.is_none());
        assert!(root.join("made").exists());
    }
    #[test]
    fn a_cut_timeout_is_reported_even_when_the_command_finishes() {
        let root = temp();
        let rt = ToolRuntime::new(&root).unwrap();
        let run = |argv: &[&str], timeout_ms: Option<u64>| {
            let p = rt
                .prepare(ToolRequest::RunCommand {
                    argv: argv.iter().map(|s| s.to_string()).collect(),
                    cwd: None,
                    timeout_ms,
                })
                .unwrap();
            let _ = rt.resolve_approval(&p.call_id, true);
            let r = rt.execute(&p.call_id);
            assert!(r.error.is_none(), "{:?}", r.error);
            r.output.unwrap_or_default()
        };
        let note = "note: timeout_ms 600000 is above the 120000 ms cap, so it ran with 120000";
        assert_eq!(
            run(&["echo", "hi"], Some(600_000)),
            format!("stdout:\nhi\n\n{note}")
        );
        assert_eq!(run(&["true"], Some(600_000)), note);
        // at or under the cap, nothing is added
        assert_eq!(run(&["echo", "hi"], Some(MAX_TIMEOUT_MS)), "stdout:\nhi\n");
        assert_eq!(run(&["true"], None), "");
    }
    #[test]
    fn a_blocked_program_is_told_what_to_use_instead() {
        let deny = |argv0: &str| {
            let e = command_policy(&[argv0.to_string(), "x".to_string()]).unwrap_err();
            assert_eq!(e.kind, ErrorKind::PolicyDenied, "{argv0}");
            e.detail
        };
        assert_eq!(deny("bash"), "'bash' is blocked: run_command has no shell; put the program and its arguments in argv directly (e.g. [\"cargo\", \"test\"]), one command per call; set cwd for the folder");
        // the basename, lowercased, is what gets named
        assert!(deny("/usr/bin/PWSH").starts_with("'pwsh' is blocked: run_command has no shell;"));
        assert!(deny("python3").contains("interpreters can run any code"));
        assert!(deny("curl").contains("no network access"));
        assert!(deny("rm").ends_with("remove a file with apply_patch (*** Delete File: <path>)"));
        assert!(deny("find").contains("use glob_files to list files by pattern, or search_files"));
        assert!(deny("timeout").contains("set the timeout_ms field"));
        assert!(deny("nohup").contains("there is no background mode"));
        assert!(deny("xargs").contains("run that program directly"));
        assert_eq!(deny("sudo"), "'sudo' is blocked: privilege, system and process-control commands are not allowed; do the work with the file tools or the project's own commands");
        // every denied program gets a named message
        for exe in [
            "sh", "node", "scp", "rmdir", "setsid", "watch", "kill", "dd",
        ] {
            assert!(
                deny(exe).starts_with(&format!("'{exe}' is blocked: ")),
                "{exe}"
            );
        }
    }
    #[test]
    fn missing_program_says_what_to_fix() {
        let root = temp();
        let rt = ToolRuntime::new(&root).unwrap();
        let run = |argv: &[&str]| {
            let p = rt
                .prepare(ToolRequest::RunCommand {
                    argv: argv.iter().map(|s| s.to_string()).collect(),
                    cwd: None,
                    timeout_ms: None,
                })
                .unwrap();
            let _ = rt.resolve_approval(&p.call_id, true);
            rt.execute(&p.call_id).error.unwrap().detail
        };
        let d = run(&["rex-no-such-tool-xyz", "--v"]);
        assert!(
            d.starts_with("failed to start 'rex-no-such-tool-xyz' ("),
            "{d}"
        );
        assert!(
            d.ends_with("; 'rex-no-such-tool-xyz' is not installed or not on PATH"),
            "{d}"
        );
        assert!(!d.contains("python3") && !d.contains("scoring"), "{d}");
        let d = run(&["cargo  test --quiet"]);
        assert!(
            d.ends_with("; argv[0] holds a whole command line - put each word in its own item, e.g. [\"cargo\", \"test\", \"--quiet\"]"),
            "{d}"
        );
        let d = run(&["make build"]);
        assert!(d.ends_with("e.g. [\"make\", \"build\"]"), "{d}");
        let d = run(&["./bin/nothing-here"]);
        assert!(d.ends_with("; no such program at that path"), "{d}");
        // the scoring path keeps its own hint
        let r = rt.execute_trusted_scoring(
            &["rex-no-such-tool-xyz".into()],
            None,
            5_000,
            &["rex-no-such-tool-xyz"],
        );
        let d = r.error.unwrap().detail;
        assert!(d.contains("is not installed or not on PATH; suite checks must use an interpreter-prefixed argv"), "{d}");
    }
    #[test]
    fn shell_is_hard_denied() {
        let rt = ToolRuntime::new(temp()).unwrap();
        let p = rt
            .prepare(ToolRequest::RunCommand {
                argv: vec!["sh".into(), "-c".into(), "echo bad".into()],
                cwd: None,
                timeout_ms: None,
            })
            .unwrap();
        assert_eq!(p.risk, RiskClass::Denied);
        assert_eq!(
            rt.execute(&p.call_id).error.unwrap().kind,
            ErrorKind::PolicyDenied
        );
    }
    #[test]
    fn output_redacts_secrets() {
        let (s, n) = redact("token=abc123456789 secret\nsk-abcdefghijklmnopqrstuvwxyz");
        assert!(n >= 2);
        assert!(!s.contains("abcdefghijklmnopqrstuvwxyz"));
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn command_output_has_no_escape_codes() {
        let rt = ToolRuntime::new(temp()).unwrap();
        let r = run_approved(
            &rt,
            &[
                "printf",
                "\\033[1;31mFAILED\\033[0m t\\007\\033]0;title\\007 done\\n",
            ],
        );
        assert!(r.ok, "{:?}", r.error);
        let out = r.output.unwrap();
        assert!(out.contains("FAILED t done"), "{out:?}");
        // stderr is cleaned too
        let root = rt.workspace_root().to_path_buf();
        fs::write(root.join("e.txt"), "\u{1b}[33mwarn\u{1b}[0m\n").unwrap();
        let r = run_approved(&rt, &["sed", "-n", "w /dev/stderr", "e.txt"]);
        assert!(r.ok, "{:?}", r.error);
        let out = r.output.unwrap();
        assert!(out.contains("stderr:\nwarn"), "{out:?}");
        assert!(!out.contains('\u{1b}'), "{out:?}");
    }
    #[cfg(unix)]
    #[test]
    fn timed_out_command_keeps_what_it_printed() {
        let rt = ToolRuntime::new(temp()).unwrap();
        let root = rt.workspace_root().to_path_buf();
        fs::write(
            root.join("log.txt"),
            "test a ... FAILED token=abc123456789\n",
        )
        .unwrap();
        let run = |argv: &[&str]| {
            let p = rt
                .prepare(ToolRequest::RunCommand {
                    argv: argv.iter().map(|s| s.to_string()).collect(),
                    cwd: None,
                    timeout_ms: Some(1_500),
                })
                .unwrap();
            rt.resolve_approval(&p.call_id, true).unwrap();
            rt.execute(&p.call_id)
        };
        // tail -F prints the file, complains about the missing one on
        // stderr, then waits forever
        let r = run(&["tail", "-F", "log.txt", "gone.txt"]);
        assert!(!r.ok);
        let e = r.error.unwrap();
        assert_eq!(e.kind, ErrorKind::Timeout);
        assert!(e.detail.contains("time limit of 1500 ms"), "{}", e.detail);
        assert!(
            e.detail.contains("larger timeout_ms (at most 120000)"),
            "{}",
            e.detail
        );
        assert!(
            e.detail
                .contains("stopped:\nstdout:\n==> log.txt <==\ntest a ... FAILED"),
            "{}",
            e.detail
        );
        assert!(e.detail.contains("\nstderr:\ntail: "), "{}", e.detail);
        // the kept output is still redacted
        assert!(!e.detail.contains("abc123456789"), "{}", e.detail);
        let e = run(&["sleep", "30"]).error.unwrap();
        assert_eq!(e.kind, ErrorKind::Timeout);
        assert!(e.detail.ends_with("; it printed nothing"), "{}", e.detail);
    }
    #[test]
    fn command_output_labels_and_skips_empty_streams() {
        assert_eq!(command_output(b"", b""), "");
        assert_eq!(command_output(b"a\n", b""), "stdout:\na\n");
        assert_eq!(command_output(b"", b"\x1b[31me\x1b[0m"), "stderr:\ne");
        assert_eq!(command_output(b"a", b"b"), "stdout:\na\nstderr:\nb");
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn shell_writes_land_inside_workspace() {
        // Checkpoint/rewind covers shell-made changes because every tool
        // (shell included) is confined to the workspace root: the default
        // cwd "." resolves inside it, and anything outside is refused.
        let root = temp();
        let rt = ToolRuntime::new(&root).unwrap();
        let r = run_approved(&rt, &["touch", "made-by-shell"]);
        assert!(r.ok, "touch failed: {:?}", r.error);
        assert!(
            root.join("made-by-shell").exists(),
            "shell side effect escaped the workspace root"
        );
        let p = rt
            .prepare(ToolRequest::RunCommand {
                argv: vec!["true".into()],
                cwd: Some("/tmp".into()),
                timeout_ms: Some(10_000),
            })
            .unwrap();
        rt.resolve_approval(&p.call_id, true).unwrap();
        let r2 = rt.execute(&p.call_id);
        assert_eq!(r2.error.unwrap().kind, ErrorKind::OutsideWorkspace);
        let _ = std::fs::remove_dir_all(&root);
    }
    #[cfg(unix)]
    #[test]
    fn standing_key_covers_only_exact_bare_commands() {
        let run = |argv: &[&str], cwd: Option<&str>| ToolRequest::RunCommand {
            argv: argv.iter().map(|s| s.to_string()).collect(),
            cwd: cwd.map(str::to_string),
            timeout_ms: None,
        };
        let npm = standing_key(&run(&["npm", "test"], None)).expect("bare command qualifies");
        // cwd spellings of the workspace root are one key; timeout is ignored
        assert_eq!(
            standing_key(&run(&["npm", "test"], Some("."))),
            Some(npm.clone())
        );
        assert_eq!(
            standing_key(&run(&["npm", "test"], Some(""))),
            Some(npm.clone())
        );
        let mut timed = run(&["npm", "test"], None);
        if let ToolRequest::RunCommand { timeout_ms, .. } = &mut timed {
            *timeout_ms = Some(5);
        }
        assert_eq!(standing_key(&timed), Some(npm.clone()));
        // any other argv or directory is a different key
        assert_ne!(
            standing_key(&run(&["npm", "test", "--", "-u"], None)),
            Some(npm.clone())
        );
        assert_ne!(
            standing_key(&run(&["npm", "test"], Some("web"))),
            Some(npm.clone())
        );
        assert_eq!(
            standing_key(&run(&["npm", "test"], Some("web/"))),
            standing_key(&run(&["npm", "test"], Some("web")))
        );
        // programs given as a path can be rewritten by the model
        assert_eq!(standing_key(&run(&["./build.sh"], None)), None);
        assert_eq!(standing_key(&run(&["bin/tool", "x"], None)), None);
        assert_eq!(standing_key(&run(&["tools\\x.exe"], None)), None);
        // anything command_policy refuses never qualifies
        assert_eq!(standing_key(&run(&["bash", "-c", "true"], None)), None);
        assert_eq!(standing_key(&run(&["rm", "-rf", "x"], None)), None);
        assert_eq!(standing_key(&run(&[], None)), None);
        // file writes never qualify
        let write = ToolRequest::CreateFile {
            path: "a.txt".into(),
            content: "x".into(),
            overwrite: false,
        };
        assert_eq!(standing_key(&write), None);
    }

    #[test]
    fn image_reads_queue_real_images_only_when_turned_on() {
        let dir = temp();
        let png = [b"\x89PNG\r\n\x1a\n".as_slice(), &[0u8, 0, 0, 13, 1, 2, 3]].concat();
        fs::write(dir.join("shot.png"), &png).unwrap();
        fs::write(dir.join("fake.png"), "just text\n").unwrap();
        let read = |rt: &ToolRuntime, path: &str| {
            let p = rt
                .prepare(ToolRequest::ReadFile {
                    path: path.into(),
                    offset: None,
                    limit: None,
                })
                .unwrap();
            rt.execute(&p.call_id)
        };

        // off by default: still the binary-file refusal, nothing queued
        let plain = ToolRuntime::new(&dir).unwrap();
        let r = read(&plain, "shot.png");
        assert!(!r.ok);
        assert!(r.error.unwrap().detail.contains("binary file"));
        assert!(plain.take_images().is_empty());

        let rt = ToolRuntime::new(&dir).unwrap().with_image_reads();
        let r = read(&rt, "shot.png");
        assert!(r.ok, "{:?}", r.error);
        let out = r.output.unwrap();
        assert!(out.starts_with("image/png image, 15 bytes"), "{out}");
        assert_eq!(r.receipt.bytes_read, 15);
        // a text file named .png is read as text: magic bytes decide
        let r = read(&rt, "fake.png");
        assert_eq!(r.output.as_deref(), Some("just text\n"));
        let queued = rt.take_images();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].path, "shot.png");
        assert_eq!(queued[0].mime, "image/png");
        assert_eq!(queued[0].bytes, png);
        // taking drains the queue
        assert!(rt.take_images().is_empty());

        // over the cap: refused, not queued
        let mut big = png.clone();
        big.resize(MAX_IMAGE_BYTES as usize + 1, 0);
        fs::write(dir.join("big.png"), &big).unwrap();
        let r = read(&rt, "big.png");
        assert!(!r.ok);
        assert!(r.error.unwrap().detail.contains("not attached"));
        assert!(rt.take_images().is_empty());
        // exactly at the cap is fine
        big.truncate(MAX_IMAGE_BYTES as usize);
        fs::write(dir.join("big.png"), &big).unwrap();
        assert!(read(&rt, "big.png").ok);
        assert_eq!(rt.take_images().len(), 1);

        // at most MAX_QUEUED_IMAGES per turn; the next read says so
        for _ in 0..MAX_QUEUED_IMAGES {
            assert!(read(&rt, "shot.png").ok);
        }
        let r = read(&rt, "shot.png");
        assert!(!r.ok);
        assert!(r.error.unwrap().detail.contains("already attached"));
        assert_eq!(rt.take_images().len(), MAX_QUEUED_IMAGES);
        assert!(read(&rt, "shot.png").ok, "a new turn starts empty");
    }

    #[test]
    fn sniff_image_uses_magic_bytes() {
        assert_eq!(sniff_image(b"\x89PNG\r\n\x1a\nrest"), Some("image/png"));
        assert_eq!(sniff_image(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(sniff_image(b"GIF87a..."), Some("image/gif"));
        assert_eq!(sniff_image(b"GIF89a..."), Some("image/gif"));
        assert_eq!(sniff_image(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff_image(b"RIFF\0\0\0\0WAVEfmt "), None);
        assert_eq!(sniff_image(b"RIFF"), None);
        assert_eq!(sniff_image(b"\x89PNG\r\n"), None);
        assert_eq!(sniff_image(&[0xFF, 0xD8]), None);
        assert_eq!(sniff_image(b"<svg xmlns"), None);
        assert_eq!(sniff_image(b""), None);
    }

    #[test]
    fn prepare_sets_standing_key_only_for_approval_commands() {
        let rt = ToolRuntime::new(temp()).unwrap();
        let cmd = rt
            .prepare(ToolRequest::RunCommand {
                argv: vec!["cargo".into(), "test".into()],
                cwd: None,
                timeout_ms: None,
            })
            .unwrap();
        assert!(cmd.approval_required);
        assert!(cmd.standing_key.is_some());
        let write = rt
            .prepare(ToolRequest::CreateFile {
                path: "a.txt".into(),
                content: "x".into(),
                overwrite: false,
            })
            .unwrap();
        assert!(write.approval_required);
        assert_eq!(write.standing_key, None);
        // absent from the JSON the UI sees when not set
        let json = serde_json::to_value(&write).unwrap();
        assert!(json.get("standing_key").is_none());
    }

    #[test]
    fn trusted_scoring_runs_allowlisted_bare_names_only() {
        let dir = std::env::temp_dir().join(format!("rex-scoring-test-{}", std::process::id()));
        let rt = ToolRuntime::new(&dir).expect("runtime");
        // allowlisted executable runs
        let ok = rt.execute_trusted_scoring(&["true".to_string()], None, 5_000, &["true"]);
        assert!(ok.ok, "true should pass: {:?}", ok.error);
        // allowlisted executable that exits nonzero is a failure, not a policy block
        let bad = rt.execute_trusted_scoring(&["false".to_string()], None, 5_000, &["false"]);
        assert!(!bad.ok);
        assert!(matches!(
            bad.error.as_ref().map(|e| &e.kind),
            Some(ErrorKind::ProcessFailed)
        ));
        // non-allowlisted interpreter stays blocked even though it exists
        let denied = rt.execute_trusted_scoring(
            &["sh".to_string(), "-c".to_string(), "true".to_string()],
            None,
            5_000,
            &["true"],
        );
        assert!(!denied.ok);
        assert!(matches!(
            denied.error.as_ref().map(|e| &e.kind),
            Some(ErrorKind::PolicyDenied)
        ));
        // path traversal as executable name refused
        let trav = rt.execute_trusted_scoring(&["/bin/true".to_string()], None, 5_000, &["true"]);
        assert!(!trav.ok);
        assert!(matches!(
            trav.error.as_ref().map(|e| &e.kind),
            Some(ErrorKind::PolicyDenied)
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn env_launcher_is_hard_denied() {
        // Audit finding 5 exploit: absolute-path launcher indirection used
        // to walk past the argv[0] basename denylist.
        let rt = ToolRuntime::new(temp()).unwrap();
        for argv0 in ["/usr/bin/env", "/bin/busybox", "xargs", "timeout"] {
            let p = rt
                .prepare(ToolRequest::RunCommand {
                    argv: vec![argv0.into(), "bash".into(), "-c".into(), "id".into()],
                    cwd: None,
                    timeout_ms: None,
                })
                .unwrap();
            assert_eq!(p.risk, RiskClass::Denied, "{argv0} must be denied");
            assert_eq!(
                rt.execute(&p.call_id).error.unwrap().kind,
                ErrorKind::PolicyDenied
            );
        }
    }
    #[cfg(target_os = "linux")]
    fn run_approved(rt: &ToolRuntime, argv: &[&str]) -> ToolResult {
        let p = rt
            .prepare(ToolRequest::RunCommand {
                argv: argv.iter().map(|s| s.to_string()).collect(),
                cwd: None,
                timeout_ms: Some(10_000),
            })
            .unwrap();
        assert_eq!(p.risk, RiskClass::Execute);
        rt.resolve_approval(&p.call_id, true).unwrap();
        rt.execute(&p.call_id)
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn sandbox_gives_child_a_fresh_network_namespace() {
        let rt = ToolRuntime::new(temp()).unwrap();
        let r = run_approved(&rt, &["readlink", "/proc/self/ns/net"]);
        assert!(r.ok, "readlink failed: {:?}", r.error);
        let status = r
            .receipt
            .sandbox
            .clone()
            .expect("receipt must carry the sandbox status");
        let child_ns = r
            .output
            .unwrap()
            .replace("stdout:\n", "")
            .trim()
            .to_string();
        let parent_ns = std::fs::read_link("/proc/self/ns/net")
            .unwrap()
            .display()
            .to_string();
        if status.starts_with("applied") {
            assert_ne!(
                child_ns, parent_ns,
                "sandboxed child must not share the host network namespace"
            );
        } else {
            assert!(
                status.starts_with("unavailable: "),
                "degradation must be explicit, got: {status}"
            );
            assert_eq!(child_ns, parent_ns);
        }
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn sandboxed_child_has_no_default_route() {
        let rt = ToolRuntime::new(temp()).unwrap();
        let r = run_approved(&rt, &["cat", "/proc/net/route"]);
        assert!(r.ok, "cat failed: {:?}", r.error);
        let status = r.receipt.sandbox.clone().unwrap_or_default();
        let table = r.output.unwrap();
        if status.starts_with("applied") {
            // Fresh netns: loopback is down and no route was ever added,
            // so there is no default route (destination 00000000).
            assert!(
                !table.contains("00000000"),
                "sandboxed child must have no network route: {table}"
            );
        }
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn sandboxed_child_maps_root_and_keeps_workspace_writes() {
        let root = temp();
        let rt = ToolRuntime::new(&root).unwrap();
        let id = run_approved(&rt, &["id", "-u"]);
        assert!(id.ok, "id failed: {:?}", id.error);
        let status = id.receipt.sandbox.clone().unwrap_or_default();
        if status.starts_with("applied") {
            let uid = id
                .output
                .unwrap()
                .replace("stdout:\n", "")
                .trim()
                .to_string();
            assert_eq!(uid, "0", "userns root maps back to the real uid");
        }
        let t = run_approved(&rt, &["touch", "probe.txt"]);
        assert!(t.ok, "touch failed: {:?}", t.error);
        assert!(root.join("probe.txt").exists());
    }

    #[test]
    fn symlink_escape_refused() {
        use std::os::unix::fs::symlink;
        let root = temp();
        symlink("/tmp", root.join("link")).unwrap();
        let rt = ToolRuntime::new(root).unwrap();
        let p = rt
            .prepare(ToolRequest::ReadFile {
                path: "link/x".into(),
                offset: None,
                limit: None,
            })
            .unwrap();
        let r = rt.execute(&p.call_id);
        assert!(matches!(
            r.error.unwrap().kind,
            ErrorKind::OutsideWorkspace | ErrorKind::SymlinkRefused | ErrorKind::NotFound
        ));
    }
}

#[cfg(test)]
mod diff_tests {
    use super::*;

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rex-diff-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn pending_diff_for_edit_file() {
        let root = temp();
        std::fs::write(root.join("a.txt"), "hello world").unwrap();
        let rt = ToolRuntime::new(root).unwrap();
        let p = rt
            .prepare(ToolRequest::EditFile {
                path: "a.txt".into(),
                expected: "world".into(),
                replacement: "rust".into(),
                replace_all: false,
            })
            .unwrap();
        let diff = rt.pending_diff(&p.call_id).unwrap().unwrap();
        assert_eq!(diff.path, "a.txt");
        assert_eq!(diff.original, "hello world");
        assert_eq!(diff.modified, "hello rust");
    }

    #[test]
    fn pending_diff_for_create_file() {
        let root = temp();
        let rt = ToolRuntime::new(root).unwrap();
        let p = rt
            .prepare(ToolRequest::CreateFile {
                path: "new.txt".into(),
                content: "fresh".into(),
                overwrite: false,
            })
            .unwrap();
        let diff = rt.pending_diff(&p.call_id).unwrap().unwrap();
        assert_eq!(diff.original, "");
        assert_eq!(diff.modified, "fresh");
    }

    #[test]
    fn pending_diff_none_for_read() {
        let root = temp();
        let rt = ToolRuntime::new(root).unwrap();
        let p = rt
            .prepare(ToolRequest::ReadFile {
                path: "x".into(),
                offset: None,
                limit: None,
            })
            .unwrap();
        assert!(rt.pending_diff(&p.call_id).unwrap().is_none());
    }
}

#[cfg(test)]
mod git_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_git() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("rex-git-test-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Init a git repo.
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&dir)
            .status()
            .unwrap();
        assert!(status.success());
        std::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&dir)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(&dir)
            .status()
            .unwrap();
        dir
    }

    #[test]
    fn git_status_and_commit() {
        let root = temp_git();
        std::fs::write(root.join("a.txt"), "hello").unwrap();
        let rt = ToolRuntime::new(root.clone()).unwrap();

        // Status shows the untracked file.
        let files = rt.git_status(&root).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "a.txt");
        assert_eq!(files[0].status.trim(), "??");

        // Commit it.
        let out = rt.git_commit(&root, "test commit").unwrap();
        assert!(out.contains("test commit") || !out.is_empty());

        // Status is clean now.
        let files = rt.git_status(&root).unwrap();
        assert!(files.is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn git_diff_shows_changes() {
        let root = temp_git();
        std::fs::write(root.join("a.txt"), "hello").unwrap();
        let rt = ToolRuntime::new(root.clone()).unwrap();
        rt.git_commit(&root, "initial").unwrap();

        std::fs::write(root.join("a.txt"), "hello world").unwrap();
        let diff = rt.git_diff(&root, "a.txt").unwrap();
        assert!(diff.contains("hello world"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn git_rejects_non_repo() {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("rex-nogit-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let rt = ToolRuntime::new(dir.clone()).unwrap();
        let err = rt.git_status(&dir).unwrap_err();
        assert!(matches!(err.kind, ErrorKind::NotFound));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unified_diff_single_hunk() {
        let d = simple_diff("a\nb\nc\n", "a\nB\nc\n", "f.txt");
        assert!(d.starts_with("--- f.txt\n+++ f.txt\n"), "headers:\n{d}");
        assert!(d.contains("@@ -1,3 +1,3 @@"), "hunk header:\n{d}");
        assert!(d.contains(" a\n-b\n+B\n c\n"), "body:\n{d}");
    }

    #[test]
    fn unified_diff_empty_old_is_pure_addition() {
        let d = simple_diff("", "x\ny\n", "new.txt");
        assert!(d.contains("@@ -0,0 +1,2 @@"), "header:\n{d}");
        assert!(d.contains("+x\n+y\n"), "body:\n{d}");
    }

    #[test]
    fn unified_diff_identical_has_no_hunks() {
        let d = simple_diff("a\nb\n", "a\nb\n", "same.txt");
        assert!(!d.contains("@@"), "no hunks expected:\n{d}");
    }

    #[test]
    fn unified_diff_two_hunks() {
        let old: String = (1..=20).map(|i| format!("line{i}\n")).collect();
        let mut new = old.clone();
        new = new.replacen("line2\n", "LINE2\n", 1);
        new = new.replacen("line18\n", "LINE18\n", 1);
        let d = simple_diff(&old, &new, "m.txt");
        assert_eq!(d.matches("@@").count() / 2, 2, "two hunks:\n{d}");
        assert!(d.contains("-line2\n+LINE2\n"), "first change:\n{d}");
        assert!(d.contains("-line18\n+LINE18\n"), "second change:\n{d}");
    }

    #[test]
    fn unified_diff_roundtrip_applies() {
        // Apply the diff hunks to `old` and check we get `new`.
        let old = "one\ntwo\nthree\nfour\nfive\n";
        let new = "one\nTWO\nthree\nfour\nFIVE\n";
        let d = simple_diff(old, new, "r.txt");
        let mut rebuilt: Vec<&str> = Vec::new();
        for line in d.lines() {
            if line.starts_with("@@") || line.starts_with("---") || line.starts_with("+++") {
                continue;
            }
            if let Some(rest) = line.strip_prefix(' ') {
                rebuilt.push(rest);
            } else if let Some(rest) = line.strip_prefix('+') {
                rebuilt.push(rest);
            }
        }
        assert_eq!(rebuilt.join("\n") + "\n", new, "diff:\n{d}");
    }
}

/// Plan an edit with the tolerant matcher and map failures to model-facing
/// tool errors that say what to do next.
fn plan_edit_checked(
    old: &str,
    expected: &str,
    replacement: &str,
    replace_all: bool,
) -> Result<fuzzy::EditPlan, ToolError> {
    fuzzy::plan_edit(old, expected, replacement, replace_all).map_err(|e| match e {
        fuzzy::PlanError::NotFound { hint } => err(
            ErrorKind::Conflict,
            &format!("expected text was not found; {hint}"),
        ),
        fuzzy::PlanError::Ambiguous {
            strategy,
            count,
            lines,
        } => err(
            ErrorKind::Conflict,
            &format!(
                "expected text is not unique: {count} matches ({strategy}) starting at lines {lines:?}; include more surrounding lines, or set replace_all explicitly"
            ),
        ),
    })
    .and_then(|plan| {
        // An edit that leaves the file as it was would still ask for
        // approval and journal a no-op. opencode refuses identical
        // old/new strings (`tool/edit.ts`); REX also catches a loose match
        // whose replacement turns out to equal what is already there.
        if plan.new_content != old {
            Ok(plan)
        } else if expected == replacement {
            Err(err(
                ErrorKind::InvalidRequest,
                "edit makes no change: expected and replacement are identical",
            ))
        } else {
            Err(err(
                ErrorKind::InvalidRequest,
                &format!(
                    "edit makes no change: the file already has the replacement text (matched {}); read the file again if you expected something else",
                    plan.strategy
                ),
            ))
        }
    })
}

/// NUL bytes in the first 8 KiB mark a file as binary (the same heuristic
/// git and grep use).
/// Office, OpenDocument and EPUB files are zip or OLE containers, and
/// SQLite's `-wal`/`-shm`/`-journal` files are raw database pages: no text
/// the file tools write can be a valid one, so a "report.docx" made of
/// plain text is a file its owner cannot open. Hermes refuses the same
/// writes (`tools/file_tools_write_guards.py`). RTF is left out on
/// purpose: it is plain text. Only the file name is checked, so the
/// refusal comes before anything is touched.
fn refuse_text_into_container(path: &str) -> Result<(), ToolError> {
    const CONTAINERS: &[&str] = &[
        "doc", "docx", "docm", "dot", "dotx", "xls", "xlsx", "xlsm", "xlsb", "ppt", "pptx", "pptm",
        "pps", "ppsx", "ppsm", "pot", "potx", "odt", "ods", "odp", "odg", "epub",
    ];
    let name = Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    let sidecar = ["-wal", "-shm", "-journal"].iter().any(|m| {
        ext.strip_suffix(m)
            .is_some_and(|db| matches!(db, "db" | "sqlite" | "sqlite3"))
    });
    if sidecar {
        return Err(err(
            ErrorKind::InvalidRequest,
            &format!("{path} is a SQLite journal file; it holds raw database pages and must not be written as text"),
        ));
    }
    if CONTAINERS.contains(&ext) {
        return Err(err(
            ErrorKind::InvalidRequest,
            &format!(
                "{path} is a .{ext} document, a binary container that plain text cannot produce; \
                 write the content as .md, .txt, .html or .csv instead, or build the document with a tool the project already has"
            ),
        ));
    }
    Ok(())
}

/// Rank folder entries against a name that was not found: same name in
/// another case first, then the same stem with another extension, then
/// one name containing the other (3+ characters), then a near spelling
/// (edit similarity of at least 0.75). Ties go alphabetically; `.git` is
/// never offered. Returns at most three `(name, is_dir)` pairs.
fn similar_names(wanted: &str, names: impl Iterator<Item = (String, bool)>) -> Vec<(String, bool)> {
    let want = wanted.to_lowercase();
    let stem = |s: &str| -> String {
        match s.rsplit_once('.') {
            Some((a, _)) if !a.is_empty() => a.to_string(),
            _ => s.to_string(),
        }
    };
    let want_stem = stem(&want);
    let mut ranked: Vec<(u8, String, bool)> = names
        .filter(|(name, _)| name != ".git" && name != wanted)
        .filter_map(|(name, is_dir)| {
            let have = name.to_lowercase();
            let rank = if have == want {
                0
            } else if stem(&have) == want_stem {
                1
            } else if (want.len() >= 3 && have.contains(&want))
                || (have.len() >= 3 && want.contains(&have))
            {
                2
            } else if fuzzy::similarity(&want, &have) >= 0.75 {
                3
            } else {
                return None;
            };
            Some((rank, name, is_dir))
        })
        .collect();
    ranked.sort();
    ranked
        .into_iter()
        .take(3)
        .map(|(_, name, is_dir)| (name, is_dir))
        .collect()
}

/// A bad pattern, in one line. The regex crate's message spans several
/// lines with a caret diagram; the last line holds the reason. opencode
/// reports a bad ripgrep pattern as its own error (`InvalidPatternError`,
/// `packages/core/src/ripgrep.ts`); REX also says how to search the text
/// as written, since most bad patterns are code with `(` or `[` in it.
fn regex_error_detail(e: &regex::Error) -> String {
    let text = e.to_string();
    let reason = text
        .lines()
        .last()
        .map(str::trim)
        .unwrap_or("bad pattern")
        .trim_start_matches("error: ");
    format!("invalid regex: {reason}; to find the text exactly as written, send regex: false")
}

/// Finds the char offset of the first match in a line.
type LineMatcher = Box<dyn Fn(&str) -> Option<usize>>;

/// Chars of a matching line shown by `search_files`.
const SEARCH_LINE_CHARS: usize = 300;

/// The part of a matching line to show: the whole trimmed line when it is
/// short, otherwise a window that starts a little before the match, with
/// `…` marking each cut end so the model knows the line goes on.
fn match_window(line: &str, at: usize) -> String {
    let chars: Vec<char> = line.chars().collect();
    let first = chars
        .iter()
        .position(|c| !c.is_whitespace())
        .unwrap_or(chars.len());
    let last = chars
        .iter()
        .rposition(|c| !c.is_whitespace())
        .map_or(first, |i| i + 1);
    if last - first <= SEARCH_LINE_CHARS {
        return chars[first..last].iter().collect();
    }
    let at = at.clamp(first, last);
    let start = if at - first <= SEARCH_LINE_CHARS / 3 {
        first
    } else {
        (at - SEARCH_LINE_CHARS / 3).min(last - SEARCH_LINE_CHARS)
    };
    let end = start + SEARCH_LINE_CHARS;
    let mut out = String::new();
    if start > first {
        out.push('…');
    }
    out.extend(&chars[start..end]);
    if end < last {
        out.push('…');
    }
    out
}

fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8192).any(|b| *b == 0)
}

fn binary_error(len: u64) -> ToolError {
    err(
        ErrorKind::InvalidRequest,
        &format!("binary file ({len} bytes); it cannot be read as text"),
    )
}

/// Names of the tools decoded into [`ToolRequest`].
pub const TOOL_NAMES: &[&str] = &[
    "read_file",
    "create_file",
    "edit_file",
    "search_files",
    "apply_patch",
    "glob_files",
    "run_command",
    "mcp_call",
];

/// Repair common argument slips before a tool call is decoded: `"50"` or
/// `50.0` for a count, `"true"` for a flag, `"null"` for an optional count or
/// flag, a JSON-encoded array for `argv` and a JSON-encoded object for MCP
/// `arguments`. Only fields whose type is known are touched, and a value is
/// changed only when the repair is unambiguous; anything else is left for
/// decoding to reject with its usual error. Returns the repaired field names.
pub fn repair_tool_args(tool: &str, args: &mut Value) -> Vec<String> {
    enum Kind {
        Count,
        Flag,
        Argv,
        Object,
    }
    let kind = |field: &str| match (tool, field) {
        (_, "offset" | "limit" | "max_results" | "timeout_ms") => Some(Kind::Count),
        (_, "overwrite" | "replace_all" | "regex") => Some(Kind::Flag),
        ("run_command", "argv") => Some(Kind::Argv),
        ("mcp_call", "arguments") => Some(Kind::Object),
        _ => None,
    };
    let mut fixed = Vec::new();
    let Some(map) = args.as_object_mut() else {
        return fixed;
    };
    let optional = |field: &str| !matches!(field, "overwrite" | "replace_all");
    let keys: Vec<String> = map.keys().cloned().collect();
    for key in keys {
        let Some(kind) = kind(&key) else { continue };
        let value = &map[&key];
        let repaired = match (&kind, value) {
            (Kind::Count | Kind::Flag, Value::String(s))
                if s.trim().eq_ignore_ascii_case("null") && optional(&key) =>
            {
                Some(Value::Null)
            }
            (Kind::Count, Value::String(s)) => {
                let t = s.trim();
                (!t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()))
                    .then(|| t.parse::<u64>().ok())
                    .flatten()
                    .map(Value::from)
            }
            (Kind::Count, Value::Number(n)) if n.as_u64().is_none() => n
                .as_f64()
                .filter(|f| *f >= 0.0 && f.fract() == 0.0 && *f <= 9.0e15)
                .map(|f| Value::from(f as u64)),
            (Kind::Flag, Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
                "true" => Some(Value::Bool(true)),
                "false" => Some(Value::Bool(false)),
                _ => None,
            },
            (Kind::Argv, Value::String(s)) if s.trim_start().starts_with('[') => {
                serde_json::from_str::<Value>(s.trim()).ok().filter(|v| {
                    v.as_array()
                        .is_some_and(|a| !a.is_empty() && a.iter().all(Value::is_string))
                })
            }
            (Kind::Object, Value::String(s)) if s.trim_start().starts_with('{') => {
                // a JSON text that starts with `{` can only be an object
                serde_json::from_str::<Value>(s.trim()).ok()
            }
            _ => None,
        };
        if let Some(v) = repaired {
            map.insert(key.clone(), v);
            fixed.push(key);
        }
    }
    fixed
}

/// Every path a patch touches, including move destinations.
pub fn patch_paths(ops: &[patch::Op]) -> Vec<String> {
    let mut out = Vec::new();
    for op in ops {
        match op {
            patch::Op::Add { path, .. } | patch::Op::Delete { path } => out.push(path.clone()),
            patch::Op::Update { path, move_to, .. } => {
                out.push(path.clone());
                if let Some(d) = move_to {
                    out.push(d.clone());
                }
            }
        }
    }
    out
}

/// Write via a sibling temp file and rename, keeping an existing file's
/// permissions (new files are 0600, like `create_file`).
fn write_atomic(target: &Path, content: &str) -> Result<(), ToolError> {
    let parent = target
        .parent()
        .ok_or_else(|| err(ErrorKind::InvalidRequest, "target has no parent"))?;
    fs::create_dir_all(parent).map_err(io_err)?;
    let perms = fs::metadata(target).ok().map(|m| m.permissions());
    let tmp = parent.join(format!(".rex-patch-{}.tmp", new_call_id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut f = options.open(&tmp)?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
        if let Some(p) = perms {
            fs::set_permissions(&tmp, p)?;
        }
        fs::rename(&tmp, target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map_err(io_err)
}
