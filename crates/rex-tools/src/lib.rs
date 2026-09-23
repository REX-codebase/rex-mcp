//! Capability-scoped local tools for REX agents.
//!
//! Models submit normalized requests. They never receive an OS handle. Every
//! request is prepared against a canonical workspace, classified, and bound to
//! an unguessable pending call. Risky calls require a separate user decision.

pub mod sandbox;

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
    if text.len() <= max {
        return text.to_string();
    }
    format!("{}…\n[output truncated at {max} bytes]", &text[..max])
}
const MAX_WRITE_BYTES: usize = 2 * 1024 * 1024;
const MAX_SEARCH_RESULTS: usize = 200;
const MAX_OUTPUT_BYTES: usize = 512 * 1024;
const MAX_TIMEOUT_MS: u64 = 120_000;
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
        })
    }

    /// Attach the run's live MCP sessions. Without this, `mcp_call` requests
    /// prepare fine but fail at execution with a clear error.
    pub fn with_mcp_caller(mut self, caller: Arc<dyn McpCaller>) -> Self {
        self.mcp = Some(caller);
        self
    }

    pub fn workspace_root(&self) -> &Path {
        &self.root
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
                let new = if *replace_all {
                    old.replace(expected, replacement)
                } else {
                    old.replacen(expected, replacement, 1)
                };
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
                &e.detail,
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
        let outcome = self.execute_inner(&call.request);
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
            ToolRequest::ReadFile { path } => self.read_file(path),
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
            } => self.search_files(query, path.as_deref(), max_results.unwrap_or(50)),
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

    fn read_file(&self, path: &str) -> Result<ExecData, ToolError> {
        let target = self.resolve_existing(path, false)?;
        let meta = fs::metadata(&target).map_err(io_err)?;
        if !meta.is_file() {
            return Err(err(
                ErrorKind::InvalidRequest,
                "target is not a regular file",
            ));
        }
        if meta.len() > MAX_FILE_BYTES {
            return Err(err(ErrorKind::TooLarge, "file exceeds 2 MiB read limit"));
        }
        let mut bytes = Vec::with_capacity(meta.len() as usize);
        File::open(&target)
            .and_then(|mut f| f.read_to_end(&mut bytes))
            .map_err(io_err)?;
        let text = String::from_utf8(bytes).map_err(|_| {
            err(
                ErrorKind::InvalidRequest,
                "binary or non-UTF-8 files are not supported",
            )
        })?;
        Ok(exec(
            Some(text),
            receipt(&self.root, Some(&target), meta.len(), 0),
        ))
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
        let target = self.resolve_for_write(path)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(io_err)?;
            self.reject_symlinks(parent)?;
        }
        let previous = if target.exists() {
            if !overwrite {
                return Err(err(
                    ErrorKind::AlreadyExists,
                    "file exists and overwrite is false",
                ));
            }
            let meta = fs::metadata(&target).map_err(io_err)?;
            if meta.len() > MAX_FILE_BYTES {
                return Err(err(ErrorKind::TooLarge, "existing file exceeds diff limit"));
            }
            fs::read_to_string(&target).map_err(io_err)?
        } else {
            String::new()
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
        Ok(exec(
            Some(format!(
                "wrote {} bytes to {}",
                content.len(),
                relative(&self.root, &target)
            )),
            r,
        ))
    }

    fn edit_file(
        &self,
        path: &str,
        expected: &str,
        replacement: &str,
        replace_all: bool,
    ) -> Result<ExecData, ToolError> {
        if expected.is_empty() {
            return Err(err(
                ErrorKind::InvalidRequest,
                "expected text must not be empty",
            ));
        }
        let target = self.resolve_existing(path, false)?;
        let old = fs::read_to_string(&target).map_err(io_err)?;
        if old.len() > MAX_FILE_BYTES as usize {
            return Err(err(ErrorKind::TooLarge, "file exceeds 2 MiB edit limit"));
        }
        let count = old.matches(expected).count();
        if count == 0 {
            return Err(err(
                ErrorKind::Conflict,
                "expected text was not found; file may have changed",
            ));
        }
        if count > 1 && !replace_all {
            return Err(err(
                ErrorKind::Conflict,
                "expected text is not unique; set replace_all explicitly",
            ));
        }
        let new = if replace_all {
            old.replace(expected, replacement)
        } else {
            old.replacen(expected, replacement, 1)
        };
        if new.len() > MAX_WRITE_BYTES {
            return Err(err(ErrorKind::TooLarge, "edited file exceeds 2 MiB limit"));
        }
        fs::write(&target, new.as_bytes()).map_err(io_err)?;
        let mut r = receipt(
            &self.root,
            Some(&target),
            old.len() as u64,
            new.len() as u64,
        );
        r.diff = Some(simple_diff(&old, &new, &relative(&self.root, &target)));
        Ok(exec(
            Some(format!(
                "replaced {} occurrence(s) in {}",
                if replace_all { count } else { 1 },
                relative(&self.root, &target)
            )),
            r,
        ))
    }

    fn search_files(
        &self,
        query: &str,
        path: Option<&str>,
        max_results: usize,
    ) -> Result<ExecData, ToolError> {
        let base = self.resolve_existing(path.unwrap_or("."), true)?;
        let limit = max_results.clamp(1, MAX_SEARCH_RESULTS);
        let mut files = Vec::new();
        collect_files(&base, &mut files, 0)?;
        let mut hits = Vec::new();
        let mut bytes_read = 0u64;
        for file in files {
            if hits.len() >= limit {
                break;
            }
            let meta = match fs::metadata(&file) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
                continue;
            }
            bytes_read += meta.len();
            let f = match File::open(&file) {
                Ok(f) => f,
                Err(_) => continue,
            };
            for (idx, line) in BufReader::new(f).lines().take(20_000).enumerate() {
                let line = match line {
                    Ok(v) => v,
                    Err(_) => break,
                };
                if line.to_lowercase().contains(&query.to_lowercase()) {
                    hits.push(format!(
                        "{}:{}:{}",
                        relative(&self.root, &file),
                        idx + 1,
                        line.trim()
                    ));
                    if hits.len() >= limit {
                        break;
                    }
                }
            }
        }
        Ok(exec(
            Some(hits.join("\n")),
            receipt(&self.root, Some(&base), bytes_read, 0),
        ))
    }

    fn run_command(
        &self,
        argv: &[String],
        cwd: Option<&str>,
        timeout_ms: u64,
    ) -> Result<ExecData, ToolError> {
        command_policy(argv)?;
        self.run_command_impl(argv, cwd, timeout_ms)
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
            return Err(err(
                ErrorKind::InvalidRequest,
                "command cwd is not a directory",
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
                let _ = out_handle.join();
                let _ = err_handle.join();
                return Err(err(
                    ErrorKind::Timeout,
                    "command exceeded its time limit and its process group was terminated",
                ));
            }
            thread::sleep(Duration::from_millis(15));
        };
        let (stdout, out_truncated) = out_handle.join().unwrap_or_default();
        let (stderr, err_truncated) = err_handle.join().unwrap_or_default();
        let mut combined = String::new();
        if !stdout.is_empty() {
            combined.push_str("stdout:\n");
            combined.push_str(&String::from_utf8_lossy(&stdout));
        }
        if !stderr.is_empty() {
            if !combined.is_empty() {
                combined.push('\n');
            }
            combined.push_str("stderr:\n");
            combined.push_str(&String::from_utf8_lossy(&stderr));
        }
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
                &format!("command exited with {:?}: {}", status.code(), combined),
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
            let hint = if e.kind() == std::io::ErrorKind::NotFound {
                "; suite checks must use an interpreter-prefixed argv such as [\"python3\", \"-m\", \"pytest\", ...]"
            } else {
                ""
            };
            let message = format!("{e}");
            if message.starts_with(sandbox::SANDBOX_ERROR_PREFIX) {
                message
            } else {
                format!("failed to spawn scoring command '{}' ({e}){hint}", argv[0])
            }
        })
    }

    fn resolve_existing(&self, raw: &str, allow_dir: bool) -> Result<PathBuf, ToolError> {
        let joined = checked_join(&self.root, raw)?;
        let canonical = fs::canonicalize(&joined).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                err(ErrorKind::NotFound, "path not found")
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
            return Err(err(ErrorKind::InvalidRequest, "directory not allowed here"));
        }
        Ok(canonical)
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
        ToolRequest::RunCommand { .. } => "run_command",
        ToolRequest::McpCall { .. } => "mcp_call",
    }
}
fn summarize(r: &ToolRequest) -> String {
    match r {
        ToolRequest::ReadFile { path } => format!("Read {path}"),
        ToolRequest::CreateFile {
            path, overwrite, ..
        } => format!("{} {path}", if *overwrite { "Write" } else { "Create" }),
        ToolRequest::EditFile { path, .. } => format!("Edit {path}"),
        ToolRequest::SearchFiles { query, path, .. } => format!(
            "Search {:?} for {:?}",
            path.as_deref().unwrap_or("."),
            query
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
        ToolRequest::ReadFile { .. } | ToolRequest::SearchFiles { .. } => (
            RiskClass::Read,
            "bounded read inside the selected workspace",
        ),
        ToolRequest::CreateFile { .. } | ToolRequest::EditFile { .. } => (
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
        ToolRequest::ReadFile { path }
        | ToolRequest::CreateFile { path, .. }
        | ToolRequest::EditFile { path, .. }
            if path.trim().is_empty() =>
        {
            Err(err(ErrorKind::InvalidRequest, "path is empty"))
        }
        ToolRequest::SearchFiles { query, .. } if query.trim().is_empty() => {
            Err(err(ErrorKind::InvalidRequest, "search query is empty"))
        }
        ToolRequest::RunCommand { argv, .. } if argv.is_empty() || argv[0].trim().is_empty() => {
            Err(err(ErrorKind::InvalidRequest, "command argv is empty"))
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
        return Err(err(ErrorKind::PolicyDenied,"shells, interpreters, network clients, privilege tools, destructive commands, and process-control commands are blocked"));
    }
    if argv.iter().skip(1).any(|a| a.contains('\0')) {
        return Err(err(ErrorKind::InvalidRequest, "NUL byte in argument"));
    }
    Ok(())
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
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) -> Result<(), ToolError> {
    if depth > 20 || out.len() > 10_000 {
        return Ok(());
    }
    let entries = fs::read_dir(dir).map_err(io_err)?;
    for entry in entries {
        let entry = entry.map_err(io_err)?;
        let ft = entry.file_type().map_err(io_err)?;
        if ft.is_symlink() {
            continue;
        }
        let p = entry.path();
        if ft.is_dir() {
            collect_files(&p, out, depth + 1)?;
        } else if ft.is_file() {
            out.push(p);
        }
    }
    Ok(())
}
fn read_capped<R: Read>(mut r: R, limit: usize) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    let mut truncated = false;
    loop {
        match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let room = limit.saturating_sub(out.len());
                out.extend_from_slice(&buf[..n.min(room)]);
                if n > room {
                    truncated = true;
                }
            }
            Err(_) => break,
        }
    }
    (out, truncated)
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn temp() -> PathBuf {
        let p = std::env::temp_dir().join(new_call_id());
        fs::create_dir_all(&p).unwrap();
        p
    }
    #[test]
    fn traversal_refused() {
        let rt = ToolRuntime::new(temp()).unwrap();
        let p = rt
            .prepare(ToolRequest::ReadFile {
                path: "../secret".into(),
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
            .prepare(ToolRequest::ReadFile { path: "x".into() })
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
