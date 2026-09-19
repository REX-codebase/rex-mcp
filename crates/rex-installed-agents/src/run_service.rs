//! Durable, asynchronous installed-agent run lifecycle.
//!
//! The synchronous `run()` API buffers a whole child session and returns one
//! final value. The desktop product needs the real lifecycle instead: a run
//! starts immediately, every child event becomes visible while the child is
//! still working, the operator can cancel, and any change the child staged is
//! promoted into the source workspace only after an explicit reviewed
//! approval. All state transitions are persisted under the REX config dir so
//! a run record survives a reload.
//!
//! Permission truth: the retained vendor CLI runs with its safest documented
//! permission mode. OpenAI Codex `exec` runs in its read-only sandbox and
//! never receives a skip-permissions or full-auto flag; its stream is gated
//! by the documented JSONL contract, so a failed turn or an unknown event is
//! a failure, never a success. REX's own approval boundary is the staging
//! review below: the child only ever writes an isolated copy, and promotion
//! applies the reviewed diff.

use crate::{
    collect_events, enforce_contract, safe_args_with_options, spec, validate_codex_stream,
    AgentEvent, InstalledAgentError, InstalledAgentId, RunOptions,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    AwaitingReview,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromotionState {
    /// Fresh task workspace: nothing pre-existed, so there is nothing to review.
    NotRequired,
    /// Run finished against a staging copy and the diff waits on the operator.
    Pending,
    Promoted,
    Discarded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffEntry {
    pub path: String,
    pub kind: DiffKind,
    pub bytes: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffSummary {
    pub entries: Vec<DiffEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSnapshot {
    pub id: String,
    pub backend: InstalledAgentId,
    pub status: RunStatus,
    pub prompt: String,
    /// Source workspace the operator targeted ("" when REX created a fresh one).
    pub workspace: String,
    /// Directory the child actually worked in (staging copy or fresh workspace).
    pub staging_workspace: String,
    /// Directory the preview should open on: staging while review is pending,
    /// the source workspace after promotion, else the working directory.
    pub preview_dir: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub exit_code: Option<i32>,
    pub events: Vec<AgentEvent>,
    pub stderr_tail: String,
    pub diff: Option<DiffSummary>,
    pub promotion: PromotionState,
    /// Truthful completion-gate verdict ("completed" or the failure reason).
    pub completion: Option<String>,
    pub error: Option<String>,
}

struct RunRecord {
    snapshot: RunSnapshot,
    child: Option<Arc<Mutex<Option<Child>>>>,
    source: Option<PathBuf>,
    staging: PathBuf,
}

pub struct RunManager {
    runs: Mutex<HashMap<String, RunRecord>>,
    runs_dir: PathBuf,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn slugify(prompt: &str) -> String {
    let mut out = String::new();
    for c in prompt.chars() {
        if out.len() >= 32 {
            break;
        }
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "task".into()
    } else {
        trimmed.to_string()
    }
}

fn hash_file(path: &Path) -> std::io::Result<u64> {
    let bytes = fs::read(path)?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    Ok(hasher.finish())
}

fn walk_files(root: &Path, base: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for item in fs::read_dir(root)? {
        let item = item?;
        let name = item.file_name();
        if matches!(
            name.to_str(),
            Some(".git" | "target" | "node_modules" | ".rex")
        ) {
            continue;
        }
        let path = item.path();
        let ty = item.file_type()?;
        if ty.is_dir() {
            walk_files(&path, base, out)?;
        } else if ty.is_file() {
            if let Ok(rel) = path.strip_prefix(base) {
                out.push(rel.to_path_buf());
            }
        }
    }
    Ok(())
}

/// Compare the staging copy against the source workspace. Only regular files
/// count; a file whose bytes differ is a modification.
pub fn diff_workspaces(source: &Path, staging: &Path) -> Result<DiffSummary, InstalledAgentError> {
    let mut source_files = Vec::new();
    let mut staging_files = Vec::new();
    walk_files(source, source, &mut source_files).map_err(|e| InstalledAgentError::Io(e.to_string()))?;
    walk_files(staging, staging, &mut staging_files).map_err(|e| InstalledAgentError::Io(e.to_string()))?;
    let mut entries = Vec::new();
    for rel in &staging_files {
        let staged = staging.join(rel);
        let original = source.join(rel);
        let bytes = fs::metadata(&staged).map(|m| m.len()).unwrap_or(0);
        let rel_text = rel.to_string_lossy().into_owned();
        if !original.is_file() {
            entries.push(DiffEntry { path: rel_text, kind: DiffKind::Added, bytes });
        } else {
            let same = match (hash_file(&original), hash_file(&staged)) {
                (Ok(a), Ok(b)) => a == b,
                _ => false,
            };
            if !same {
                entries.push(DiffEntry { path: rel_text, kind: DiffKind::Modified, bytes });
            }
        }
    }
    for rel in &source_files {
        if !staging.join(rel).exists() {
            entries.push(DiffEntry {
                path: rel.to_string_lossy().into_owned(),
                kind: DiffKind::Deleted,
                bytes: 0,
            });
        }
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(DiffSummary { entries })
}

fn apply_promotion(source: &Path, staging: &Path, diff: &DiffSummary) -> Result<(), InstalledAgentError> {
    for entry in &diff.entries {
        let rel = Path::new(&entry.path);
        if rel.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return Err(InstalledAgentError::Invalid(format!(
                "refusing to promote path escape: {}",
                entry.path
            )));
        }
        let from = staging.join(rel);
        let to = source.join(rel);
        match entry.kind {
            DiffKind::Added | DiffKind::Modified => {
                if let Some(parent) = to.parent() {
                    fs::create_dir_all(parent).map_err(|e| InstalledAgentError::Io(e.to_string()))?;
                }
                fs::copy(&from, &to).map_err(|e| InstalledAgentError::Io(e.to_string()))?;
            }
            DiffKind::Deleted => {
                if to.is_file() {
                    fs::remove_file(&to).map_err(|e| InstalledAgentError::Io(e.to_string()))?;
                }
            }
        }
    }
    Ok(())
}

const STDERR_TAIL_LIMIT: usize = 4000;

impl RunManager {
    pub fn new(runs_dir: PathBuf) -> Result<Self, InstalledAgentError> {
        fs::create_dir_all(&runs_dir).map_err(|e| InstalledAgentError::Io(e.to_string()))?;
        Ok(Self { runs: Mutex::new(HashMap::new()), runs_dir })
    }

    fn persist(&self, snapshot: &RunSnapshot) {
        let path = self.runs_dir.join(format!("{}.json", snapshot.id));
        if let Ok(json) = serde_json::to_string_pretty(snapshot) {
            let _ = fs::write(path, json);
        }
    }

    /// Start a run. An empty `workspace` creates a fresh task workspace that
    /// the child owns outright (nothing pre-existed, so no review is needed);
    /// a real workspace is always staged first.
    pub fn begin(
        self: &Arc<Self>,
        backend: InstalledAgentId,
        prompt: &str,
        workspace: &str,
        options: RunOptions,
    ) -> Result<RunSnapshot, InstalledAgentError> {
        // Fail closed before spawning anything: the model/effort flags are
        // only passed through where the vendor's documented interface is
        // verified to accept them.
        let s = spec(backend);
        let exe = crate::find_on_path(s.executable)
            .ok_or_else(|| InstalledAgentError::Missing(format!("{} is not installed", s.name)))?;
        enforce_contract(&exe, s)?;
        self.begin_with(backend, prompt, workspace, options, &exe)
    }

    /// Same lifecycle with an explicitly resolved executable. Production
    /// callers go through `begin`, which discovers the vendor CLI on PATH;
    /// tests point at hermetic fixtures without mutating process-wide PATH.
    pub(crate) fn begin_with(
        self: &Arc<Self>,
        backend: InstalledAgentId,
        prompt: &str,
        workspace: &str,
        options: RunOptions,
        exe: &Path,
    ) -> Result<RunSnapshot, InstalledAgentError> {
        if prompt.trim().is_empty() {
            return Err(InstalledAgentError::Invalid("task is empty".into()));
        }
        let (args, input) = safe_args_with_options(backend, prompt, &options)?;

        let id = format!("run-{}", now_ms());
        let (source, staging, promotion) = if workspace.trim().is_empty() {
            let fresh = self
                .runs_dir
                .join("workspaces")
                .join(format!("{}-{}", slugify(prompt), now_ms()));
            fs::create_dir_all(&fresh).map_err(|e| InstalledAgentError::Io(e.to_string()))?;
            (None, fresh, PromotionState::NotRequired)
        } else {
            let source = PathBuf::from(workspace);
            if !source.is_dir() {
                return Err(InstalledAgentError::Invalid(
                    "workspace is not a directory".into(),
                ));
            }
            let staging = crate::stage_workspace(&source)?;
            (Some(source), staging, PromotionState::Pending)
        };

        let created = now_ms();
        let snapshot = RunSnapshot {
            id: id.clone(),
            backend,
            status: RunStatus::Running,
            prompt: prompt.to_string(),
            workspace: source
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            staging_workspace: staging.to_string_lossy().into_owned(),
            preview_dir: staging.to_string_lossy().into_owned(),
            model: options.model.clone(),
            effort: options.effort.clone(),
            created_at_ms: created,
            updated_at_ms: created,
            exit_code: None,
            events: Vec::new(),
            stderr_tail: String::new(),
            diff: None,
            promotion,
            completion: None,
            error: None,
        };

        let child = Command::new(&exe)
            .args(args)
            .current_dir(&staging)
            .env("REX_INSTALLED_AGENT", "1")
            .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| InstalledAgentError::Child(e.to_string()))?;
        let shared_child = Arc::new(Mutex::new(Some(child)));

        {
            let mut runs = self.runs.lock().expect("runs lock");
            runs.insert(
                id.clone(),
                RunRecord {
                    snapshot: snapshot.clone(),
                    child: Some(shared_child.clone()),
                    source: source.clone(),
                    staging: staging.clone(),
                },
            );
        }
        self.persist(&snapshot);

        let manager = Arc::clone(self);
        let run_id = id.clone();
        thread::spawn(move || {
            manager.work(run_id, backend, input, shared_child, source, staging);
        });
        Ok(snapshot)
    }

    fn work(
        &self,
        run_id: String,
        backend: InstalledAgentId,
        input: Option<String>,
        shared_child: Arc<Mutex<Option<Child>>>,
        source: Option<PathBuf>,
        staging: PathBuf,
    ) {
        let _ = &source;
        let _ = &staging;
        let outcome = drive_child(backend, input, &shared_child, |event| {
            self.append_event(&run_id, event);
        });
        // Events were already appended incrementally; only finalize state.
        let (stderr, exit_code, child_error) = match outcome {
            Ok((_events, stderr, exit_code)) => (stderr, exit_code, None),
            Err(error) => (String::new(), None, Some(error)),
        };

        let finalize;
        {
            let mut runs = self.runs.lock().expect("runs lock");
            let Some(record) = runs.get_mut(&run_id) else { return };
            if record.snapshot.status == RunStatus::Cancelled {
                return; // cancel() already owns the terminal state and cleanup.
            }
            record.child = None;
            record.snapshot.exit_code = exit_code;
            record.snapshot.stderr_tail = tail(&stderr, STDERR_TAIL_LIMIT);

            if let Some(error) = child_error {
                record.snapshot.status = RunStatus::Failed;
                record.snapshot.error = Some(error.to_string());
                record.snapshot.completion = Some(error.to_string());
            } else {
                let gate = validate_codex_stream(&record.snapshot.events, exit_code)
                    .map(|_| "completed".to_string());
                match gate {
                    Ok(verdict) => {
                        record.snapshot.completion = Some(verdict);
                        if let Some(source) = &record.source {
                            match diff_workspaces(source, &record.staging) {
                                Ok(diff) if diff.entries.is_empty() => {
                                    record.snapshot.status = RunStatus::Completed;
                                    record.snapshot.promotion = PromotionState::NotRequired;
                                }
                                Ok(diff) => {
                                    record.snapshot.diff = Some(diff);
                                    record.snapshot.status = RunStatus::AwaitingReview;
                                    record.snapshot.promotion = PromotionState::Pending;
                                }
                                Err(error) => {
                                    record.snapshot.status = RunStatus::Failed;
                                    record.snapshot.error = Some(error.to_string());
                                }
                            }
                        } else {
                            record.snapshot.status = RunStatus::Completed;
                        }
                    }
                    Err(error) => {
                        record.snapshot.status = RunStatus::Failed;
                        record.snapshot.error = Some(error.to_string());
                        record.snapshot.completion = Some(error.to_string());
                    }
                }
            }
            record.snapshot.updated_at_ms = now_ms();
            finalize = Some(record.snapshot.clone());
        }
        if let Some(snapshot) = finalize {
            self.persist(&snapshot);
        }
    }

    fn append_event(&self, run_id: &str, event: AgentEvent) {
        let snapshot = {
            let mut runs = self.runs.lock().expect("runs lock");
            let Some(record) = runs.get_mut(run_id) else { return };
            record.snapshot.events.push(event);
            record.snapshot.updated_at_ms = now_ms();
            record.snapshot.clone()
        };
        self.persist(&snapshot);
    }

    pub fn snapshot(&self, run_id: &str) -> Option<RunSnapshot> {
        self.runs
            .lock()
            .expect("runs lock")
            .get(run_id)
            .map(|record| record.snapshot.clone())
    }

    /// Trusted operator decision on the staged diff. This is the only path
    /// that writes child output back into the source workspace.
    pub fn decide(&self, run_id: &str, approved: bool) -> Result<RunSnapshot, InstalledAgentError> {
        let cleanup;
        let snapshot = {
            let mut runs = self.runs.lock().expect("runs lock");
            let record = runs
                .get_mut(run_id)
                .ok_or_else(|| InstalledAgentError::Invalid("unknown run".into()))?;
            if record.snapshot.status != RunStatus::AwaitingReview {
                return Err(InstalledAgentError::Invalid(
                    "run is not awaiting review".into(),
                ));
            }
            if approved {
                let source = record
                    .source
                    .clone()
                    .ok_or_else(|| InstalledAgentError::Invalid("run has no source workspace".into()))?;
                let diff = record.snapshot.diff.clone().unwrap_or_default();
                apply_promotion(&source, &record.staging, &diff)?;
                record.snapshot.promotion = PromotionState::Promoted;
                record.snapshot.preview_dir = source.to_string_lossy().into_owned();
            } else {
                record.snapshot.promotion = PromotionState::Discarded;
                // Discarded work leaves no previewable result in the source;
                // keep pointing at staging until cleanup, then the source.
                if let Some(source) = &record.source {
                    record.snapshot.preview_dir = source.to_string_lossy().into_owned();
                }
            }
            record.snapshot.status = RunStatus::Completed;
            record.snapshot.updated_at_ms = now_ms();
            cleanup = Some(record.staging.clone());
            record.snapshot.clone()
        };
        if let Some(staging) = cleanup {
            let _ = fs::remove_dir_all(staging);
        }
        self.persist(&snapshot);
        Ok(snapshot)
    }

    pub fn cancel(&self, run_id: &str) -> Result<RunSnapshot, InstalledAgentError> {
        let staging_cleanup;
        let snapshot = {
            let mut runs = self.runs.lock().expect("runs lock");
            let record = runs
                .get_mut(run_id)
                .ok_or_else(|| InstalledAgentError::Invalid("unknown run".into()))?;
            if record.snapshot.status != RunStatus::Running
                && record.snapshot.status != RunStatus::AwaitingReview
            {
                return Err(InstalledAgentError::Invalid(
                    "run is already terminal".into(),
                ));
            }
            if let Some(shared) = record.child.take() {
                if let Some(child) = shared.lock().expect("child lock").as_mut() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
            record.snapshot.status = RunStatus::Cancelled;
            record.snapshot.updated_at_ms = now_ms();
            staging_cleanup = if record.source.is_some() {
                Some(record.staging.clone())
            } else {
                None
            };
            record.snapshot.clone()
        };
        if let Some(staging) = staging_cleanup {
            let _ = fs::remove_dir_all(staging);
        }
        self.persist(&snapshot);
        Ok(snapshot)
    }
}

fn tail(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let mut start = text.len() - limit;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_string()
}

/// Spawn-side driver: streams child stdout into per-line events as they
/// arrive, drains stderr concurrently, and returns the full capture when the
/// child exits. The event callback fires from the reader thread so snapshots
/// show live progress instead of one buffered result.
fn drive_child(
    backend: InstalledAgentId,
    input: Option<String>,
    shared_child: &Arc<Mutex<Option<Child>>>,
    on_event: impl Fn(AgentEvent),
) -> Result<(Vec<AgentEvent>, String, Option<i32>), InstalledAgentError> {
    let (mut stdin, stdout, stderr) = {
        let mut guard = shared_child.lock().expect("child lock");
        let child = guard
            .as_mut()
            .ok_or_else(|| InstalledAgentError::Child("child already gone".into()))?;
        (
            child.stdin.take(),
            child.stdout.take(),
            child.stderr.take(),
        )
    };
    if let (Some(mut pipe), Some(line)) = (stdin.take(), input) {
        match writeln!(pipe, "{line}") {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
            Err(e) => return Err(InstalledAgentError::Io(e.to_string())),
        }
        // Closing stdin is the documented graceful end-of-session signal.
        drop(pipe);
    }
    let stdout = stdout.ok_or_else(|| InstalledAgentError::Child("child stdout unavailable".into()))?;
    let stderr = stderr.ok_or_else(|| InstalledAgentError::Child("child stderr unavailable".into()))?;
    let stderr_thread = thread::spawn(move || {
        let mut out = String::new();
        let mut reader = BufReader::new(stderr);
        std::io::Read::read_to_string(&mut reader, &mut out).map(|_| out)
    });
    let mut events = Vec::new();
    for line in BufReader::new(stdout).lines() {
        let line = line.map_err(|e| InstalledAgentError::Io(e.to_string()))?;
        if line.trim().is_empty() {
            continue;
        }
        match collect_events::parse_line(backend, &line) {
            Ok(event) => {
                on_event(event.clone());
                events.push(event);
            }
            Err(error) => {
                let _ = shared_child
                    .lock()
                    .expect("child lock")
                    .as_mut()
                    .map(|c| c.kill());
                return Err(error);
            }
        }
    }
    let exit_code = {
        let mut guard = shared_child.lock().expect("child lock");
        match guard.as_mut() {
            Some(child) => child
                .wait()
                .map_err(|e| InstalledAgentError::Child(e.to_string()))?
                .code(),
            None => None, // cancelled while waiting
        }
    };
    let stderr = stderr_thread
        .join()
        .map_err(|_| InstalledAgentError::Child("stderr reader panicked".into()))?
        .map_err(|e| InstalledAgentError::Io(e.to_string()))?;
    Ok((events, stderr, exit_code))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn manager() -> (Arc<RunManager>, PathBuf) {
        let root = std::env::temp_dir().join(format!("rex-runsvc-{}", std::process::id()));
        let dir = root.join(format!("runs-{}", now_ms()));
        (Arc::new(RunManager::new(dir.clone()).unwrap()), dir)
    }

    #[cfg(unix)]
    fn fake_cli(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        fs::write(&path, body).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    fn wait_terminal(m: &RunManager, id: &str, timeout_ms: u64) -> RunSnapshot {
        let start = now_ms();
        loop {
            let snap = m.snapshot(id).expect("run exists");
            if snap.status != RunStatus::Running {
                return snap;
            }
            assert!(now_ms() - start < timeout_ms, "run did not finish in time");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[test]
    fn codex_fails_closed_on_model_selection() {
        let options = RunOptions {
            model: Some("anything".into()),
            effort: Some("high".into()),
        };
        assert!(safe_args_with_options(InstalledAgentId::Codex, "x", &options).is_err());
    }

    #[test]
    fn diff_detects_added_modified_deleted() {
        let root = std::env::temp_dir().join(format!("rex-diff-{}", now_ms()));
        let source = root.join("src");
        let staging = root.join("stg");
        fs::create_dir_all(source.join("sub")).unwrap();
        fs::create_dir_all(staging.join("sub")).unwrap();
        fs::write(source.join("same.txt"), "same").unwrap();
        fs::write(staging.join("same.txt"), "same").unwrap();
        fs::write(source.join("changed.txt"), "old").unwrap();
        fs::write(staging.join("changed.txt"), "new content").unwrap();
        fs::write(source.join("gone.txt"), "bye").unwrap();
        fs::write(staging.join("new.txt"), "hello").unwrap();
        let diff = diff_workspaces(&source, &staging).unwrap();
        let kinds: Vec<(&str, DiffKind)> = diff
            .entries
            .iter()
            .map(|e| (e.path.as_str(), e.kind))
            .collect();
        assert!(kinds.contains(&("new.txt", DiffKind::Added)));
        assert!(kinds.contains(&("changed.txt", DiffKind::Modified)));
        assert!(kinds.contains(&("gone.txt", DiffKind::Deleted)));
        assert!(!kinds.iter().any(|(p, _)| *p == "same.txt"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn promotion_applies_only_reviewed_changes() {
        let root = std::env::temp_dir().join(format!("rex-promo-{}", now_ms()));
        let source = root.join("src");
        let staging = root.join("stg");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&staging).unwrap();
        fs::write(source.join("keep.txt"), "keep").unwrap();
        fs::write(staging.join("keep.txt"), "keep").unwrap();
        fs::write(staging.join("added.txt"), "new").unwrap();
        fs::write(source.join("edit.txt"), "old").unwrap();
        fs::write(staging.join("edit.txt"), "edited").unwrap();
        let diff = diff_workspaces(&source, &staging).unwrap();
        apply_promotion(&source, &staging, &diff).unwrap();
        assert_eq!(fs::read_to_string(source.join("added.txt")).unwrap(), "new");
        assert_eq!(fs::read_to_string(source.join("edit.txt")).unwrap(), "edited");
        assert_eq!(fs::read_to_string(source.join("keep.txt")).unwrap(), "keep");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn fresh_workspace_run_streams_events_and_completes() {
        let root = std::env::temp_dir().join(format!("rex-live-{}", now_ms()));
        fs::create_dir_all(&root).unwrap();
        let script = fake_cli(
            &root,
            "codex",
            "#!/bin/sh\nprintf '%s\\n' '{\"type\":\"thread.started\",\"thread_id\":\"t-1\"}'\nprintf '%s\\n' '{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}'\n",
        );
        let (m, dir) = manager();
        let snap = m
            .begin_with(
                InstalledAgentId::Codex,
                "make a page",
                "",
                RunOptions::default(),
                &script,
            )
            .unwrap();
        assert_eq!(snap.status, RunStatus::Running);
        assert_eq!(snap.promotion, PromotionState::NotRequired);
        let final_snap = wait_terminal(&m, &snap.id, 10_000);
        assert_eq!(final_snap.status, RunStatus::Completed);
        assert_eq!(final_snap.completion.as_deref(), Some("completed"));
        assert_eq!(final_snap.events.len(), 2);
        assert_eq!(final_snap.events[0].event, "thread.started");
        assert!(Path::new(&final_snap.preview_dir).is_dir());
        assert!(dir.join(format!("{}.json", final_snap.id)).is_file());
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn staged_run_requires_review_and_promotes_on_approval() {
        let root = std::env::temp_dir().join(format!("rex-review-{}", now_ms()));
        let fixture = root.join("bin");
        let source = root.join("project");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("existing.txt"), "original").unwrap();
        let script = fake_cli(
            &fixture,
            "codex",
            "#!/bin/sh\nprintf '%s\\n' '{\"type\":\"thread.started\",\"thread_id\":\"t-1\"}'\necho created > created.txt\nprintf '%s\\n' '{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}'\n",
        );
        let (m, _dir) = manager();
        let snap = m
            .begin_with(
                InstalledAgentId::Codex,
                "change the project",
                source.to_str().unwrap(),
                RunOptions::default(),
                &script,
            )
            .unwrap();
        let reviewed = wait_terminal(&m, &snap.id, 10_000);
        assert_eq!(reviewed.status, RunStatus::AwaitingReview);
        assert_eq!(reviewed.promotion, PromotionState::Pending);
        let diff = reviewed.diff.clone().unwrap();
        assert!(diff.entries.iter().any(|e| e.path == "created.txt" && e.kind == DiffKind::Added));
        // Source is untouched until the operator approves.
        assert!(!source.join("created.txt").exists());
        // A second decision path is rejected while pending is required first.
        let promoted = m.decide(&snap.id, true).unwrap();
        assert_eq!(promoted.status, RunStatus::Completed);
        assert_eq!(promoted.promotion, PromotionState::Promoted);
        assert_eq!(fs::read_to_string(source.join("created.txt")).unwrap(), "created\n");
        assert_eq!(fs::read_to_string(source.join("existing.txt")).unwrap(), "original");
        assert_eq!(promoted.preview_dir, source.to_string_lossy());
        // Terminal runs cannot be decided or cancelled again.
        assert!(m.decide(&snap.id, true).is_err());
        assert!(m.cancel(&snap.id).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn discarded_staging_never_touches_source() {
        let root = std::env::temp_dir().join(format!("rex-discard-{}", now_ms()));
        let fixture = root.join("bin");
        let source = root.join("project");
        fs::create_dir_all(&source).unwrap();
        let script = fake_cli(
            &fixture,
            "codex",
            "#!/bin/sh\nprintf '%s\\n' '{\"type\":\"thread.started\",\"thread_id\":\"t-1\"}'\necho nope > rejected.txt\nprintf '%s\\n' '{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}'\n",
        );
        let (m, _dir) = manager();
        let snap = m
            .begin_with(InstalledAgentId::Codex, "try", source.to_str().unwrap(), RunOptions::default(), &script)
            .unwrap();
        let reviewed = wait_terminal(&m, &snap.id, 10_000);
        assert_eq!(reviewed.status, RunStatus::AwaitingReview);
        let staging = reviewed.staging_workspace.clone();
        let discarded = m.decide(&snap.id, false).unwrap();
        assert_eq!(discarded.promotion, PromotionState::Discarded);
        assert!(!source.join("rejected.txt").exists());
        assert!(!Path::new(&staging).exists(), "staging is cleaned after discard");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn cancel_kills_the_child_and_marks_the_run() {
        let root = std::env::temp_dir().join(format!("rex-cancel-{}", now_ms()));
        let fixture = root.join("bin");
        let script = fake_cli(
            &fixture,
            "codex",
            "#!/bin/sh\nsleep 60\n",
        );
        let (m, _dir) = manager();
        let snap = m
            .begin_with(InstalledAgentId::Codex, "long task", "", RunOptions::default(), &script)
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        let cancelled = m.cancel(&snap.id).unwrap();
        assert_eq!(cancelled.status, RunStatus::Cancelled);
        // The worker thread must not resurrect the run afterwards.
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(m.snapshot(&snap.id).unwrap().status, RunStatus::Cancelled);
        let _ = fs::remove_dir_all(root);
    }
}
