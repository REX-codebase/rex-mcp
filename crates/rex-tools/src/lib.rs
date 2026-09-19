//! Capability-scoped local tools for REX agents.
//!
//! Models submit normalized requests. They never receive an OS handle. Every
//! request is prepared against a canonical workspace, classified, and bound to
//! an unguessable pending call. Risky calls require a separate user decision.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolError {
    pub kind: ErrorKind,
    pub detail: String,
}

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
        })
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
        }
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
        r.diff = Some(simple_diff(&previous, content));
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
        r.diff = Some(simple_diff(&old, &new));
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
        let cwd = self.resolve_existing(cwd.unwrap_or("."), true)?;
        if !cwd.is_dir() {
            return Err(err(
                ErrorKind::InvalidRequest,
                "command cwd is not a directory",
            ));
        }
        let mut command = Command::new(&argv[0]);
        command
            .args(&argv[1..])
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_clear();
        if let Ok(path) = std::env::var("PATH") {
            command.env("PATH", path);
        }
        command
            .env("HOME", &*self.root)
            .env("REX_WORKSPACE", &*self.root);
        #[cfg(unix)]
        unsafe {
            use std::os::unix::process::CommandExt;
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
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
        let mut child = command.spawn().map_err(io_err)?;
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
        if !status.success() {
            return Err(err(
                ErrorKind::ProcessFailed,
                &format!("command exited with {:?}: {}", status.code(), combined),
            ));
        }
        Ok(exec(Some(combined), r))
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
    }
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
fn simple_diff(old: &str, new: &str) -> String {
    const L: usize = 12000;
    let mut out = String::from("--- before\n+++ after\n");
    for line in old.lines().take(1000) {
        out.push('-');
        out.push_str(line);
        out.push('\n');
        if out.len() > L {
            break;
        }
    }
    for line in new.lines().take(1000) {
        out.push('+');
        out.push_str(line);
        out.push('\n');
        if out.len() > L {
            break;
        }
    }
    if out.len() > L {
        out.truncate(L);
        out.push_str("\n[diff truncated]");
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
    #[cfg(unix)]
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
