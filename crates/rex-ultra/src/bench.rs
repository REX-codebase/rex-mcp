//! Benchmark instrumentation: the same worker model, raw vs Simple vs Ultra,
//! scored blindly.
//!
//! A suite is JSONL: one task per line with deterministic checks. The runner
//! executes each task in a fresh disposable workspace and scores only through
//! the verifier - the checker never sees the model's prose, so a confident
//! answer that does not compile scores zero. Results append to a JSONL report
//! with tokens, wall time, steps and per-check outcomes, so uplift claims are
//! reproducible from the artifacts in this repo.

use crate::contract::Proof;
use crate::evidence::EvidenceStore;
use crate::verify::{ObligationOutcome, VerificationReport};
use crate::contract::AcceptanceContract;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BenchTask {
    pub id: String,
    pub prompt: String,
    pub checks: Vec<Proof>,
}

pub fn load_suite(path: &Path) -> Result<Vec<BenchTask>, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("suite unreadable: {e}"))?;
    let mut tasks = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let task: BenchTask = serde_json::from_str(line)
            .map_err(|e| format!("suite line {}: {e}", n + 1))?;
        if task.id.trim().is_empty() || task.prompt.trim().is_empty() || task.checks.is_empty() {
            return Err(format!("suite line {}: id, prompt and checks are required", n + 1));
        }
        tasks.push(task);
    }
    if tasks.is_empty() {
        return Err("suite has no tasks".into());
    }
    Ok(tasks)
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BenchMode {
    /// One single-shot completion, no tools, no verification loop.
    Raw,
    /// The Simple autonomous loop.
    Simple,
    /// The full Ultra pipeline.
    Ultra,
}

fn legacy_prompt_marker() -> String {
    "legacy-unknown".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    pub task_id: String,
    pub mode: BenchMode,
    /// Prompt-architecture identity the run executed under. Result files
    /// from before prompt versioning deserialize as "legacy-unknown" and
    /// fail closed against any versioned record at comparison time.
    #[serde(default = "legacy_prompt_marker")]
    pub prompt_version: String,
    #[serde(default = "legacy_prompt_marker")]
    pub prompt_hash: String,
    pub passed: bool,
    pub checks: Vec<ObligationOutcome>,
    pub tokens_used: u64,
    pub wall_ms: u64,
    pub steps: usize,
    pub approvals: u32,
    pub terminal: String,
}

/// Score a finished workspace against a task's checks. This is the blind
/// scorer: only fresh execution counts.
pub fn score_workspace(task: &BenchTask, workspace: &Path, evidence_dir: &Path) -> VerificationReport {
    score_workspace_with_scoring(task, workspace, evidence_dir, None)
}

/// Score with an optional trusted-scoring allowlist for suite-authored
/// command checks (see verify::verify_contract_with_scoring).
pub fn score_workspace_with_scoring(
    task: &BenchTask,
    workspace: &Path,
    evidence_dir: &Path,
    scoring: Option<&[&str]>,
) -> VerificationReport {
    let mut evidence = EvidenceStore::open(evidence_dir).expect("evidence store");
    let contract = AcceptanceContract {
        task: task.prompt.clone(),
        obligations: task
            .checks
            .iter()
            .enumerate()
            .map(|(i, proof)| crate::contract::Obligation {
                id: format!("check-{}", i + 1),
                statement: format!("{:?}", proof),
                proof: proof.clone(),
            })
            .collect(),
        forbidden_regressions: Vec::new(),
    };
    crate::verify::verify_contract_with_scoring(&contract, workspace, &mut evidence, scoring)
}

/// Stage hidden suite files (e.g. tests) into the workspace AFTER a run,
/// before scoring. Files the model wrote at the same paths are overwritten -
/// scoring must only ever see suite-authored checks. Symlinks in the stage
/// tree are refused. Returns the staged relative paths for the audit trail.
pub fn stage_hidden_files(stage_root: &Path, task_id: &str, workspace: &Path) -> Result<Vec<String>, String> {
    let src = stage_root.join(task_id);
    if !src.is_dir() {
        return Ok(Vec::new());
    }
    let mut staged = Vec::new();
    let mut stack = vec![src.clone()];
    while let Some(dir) = stack.pop() {
        let entries = fs::read_dir(&dir).map_err(|e| format!("stage unreadable {}: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("stage entry: {e}"))?;
            let path = entry.path();
            let rel = path
                .strip_prefix(&src)
                .map_err(|e| format!("stage relative: {e}"))?
                .to_path_buf();
            let ft = entry.file_type().map_err(|e| format!("stage type: {e}"))?;
            if ft.is_symlink() {
                return Err(format!("stage symlink refused: {}", path.display()));
            }
            if ft.is_dir() {
                stack.push(path);
                continue;
            }
            let dest = workspace.join(&rel);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("stage mkdir: {e}"))?;
            }
            fs::copy(&path, &dest).map_err(|e| format!("stage copy {}: {e}", path.display()))?;
            staged.push(rel.display().to_string());
        }
    }
    staged.sort();
    Ok(staged)
}

/// Extract files from a raw-mode answer. The format the prompt demands:
/// one fenced block per file, info string `file:<relative path>`.
pub fn extract_raw_files(text: &str) -> Vec<(String, String)> {
    let mut files = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("```") {
        let after = &rest[start + 3..];
        let Some(nl) = after.find('\n') else { break };
        let info = after[..nl].trim();
        let body_start = start + 3 + nl + 1;
        let Some(end) = rest[body_start..].find("```") else { break };
        let body = &rest[body_start..body_start + end];
        if let Some(path) = info.strip_prefix("file:") {
            let path = path.trim();
            if !path.is_empty()
                && !path.starts_with('/')
                && !path.contains("..")
                && files.len() < 64
            {
                files.push((path.to_string(), body.to_string()));
            }
        }
        rest = &rest[body_start + end + 3..];
    }
    files
}

/// The fixed raw-mode output contract. Pinned verbatim: raw stays raw, and
/// its identity hash covers this exact text.
pub const RAW_FORMAT_SUFFIX: &str = "Produce the complete solution as fenced code blocks, one per file, \
each opened with an info string of exactly `file:<relative path>`. No prose between blocks.";

pub fn raw_prompt(task: &BenchTask) -> String {
    format!("{}\n\n{RAW_FORMAT_SUFFIX}", task.prompt)
}

pub fn write_files(workspace: &Path, files: &[(String, String)]) -> Result<(), String> {
    for (rel, content) in files {
        let full = workspace.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::write(&full, content).map_err(|e| format!("{rel}: {e}"))?;
    }
    Ok(())
}

pub fn append_result(out: &PathBuf, result: &TaskResult) -> Result<(), String> {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(out)
        .map_err(|e| e.to_string())?;
    writeln!(
        file,
        "{}",
        serde_json::to_string(result).map_err(|e| e.to_string())?
    )
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suite_loads_and_rejects_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("suite.jsonl");
        fs::write(
            &path,
            r#"{"id":"t1","prompt":"make a file","checks":[{"kind":"file_exists","path":"out.txt"}]}"#,
        )
        .unwrap();
        let tasks = load_suite(&path).unwrap();
        assert_eq!(tasks.len(), 1);
        fs::write(&path, "# only a comment\n").unwrap();
        assert!(load_suite(&path).is_err());
    }

    #[test]
    fn raw_prompt_is_pinned_verbatim() {
        let task = BenchTask {
            id: "t".into(),
            prompt: "write add".into(),
            checks: vec![Proof::FileExists { path: "x".into() }],
        };
        assert_eq!(
            raw_prompt(&task),
            "write add\n\nProduce the complete solution as fenced code blocks, one per file, \
each opened with an info string of exactly `file:<relative path>`. No prose between blocks."
        );
    }

    #[test]
    fn held_out_checks_never_enter_the_raw_prompt() {
        // The raw prompt is built from the task text alone; suite checks
        // (the held-out answers) have no path into it.
        let task = BenchTask {
            id: "t".into(),
            prompt: "write add".into(),
            checks: vec![Proof::FileContains {
                path: "solution.py".into(),
                needle: "SECRET_EXPECTED_MARKER_9147".into(),
            }],
        };
        assert!(!raw_prompt(&task).contains("SECRET_EXPECTED_MARKER_9147"));
    }

    #[test]
    fn raw_file_extraction_is_path_safe() {
        let text = "Sure!\n```file:src/main.rs\nfn main() {}\n```\n```file:../escape\nx\n```\n```file:/abs\ny\n```";
        let files = extract_raw_files(text);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, "src/main.rs");
    }

    #[test]
fn stage_hidden_files_overwrites_model_planted_checks_and_refuses_symlinks() {
    let base = std::env::temp_dir().join(format!("rex-stage-test-{}", std::process::id()));
    let stage = base.join("stage").join("suite/task-1").join("tests");
    std::fs::create_dir_all(&stage).unwrap();
    std::fs::write(stage.join("test_solution.py"), b"suite-authored").unwrap();
    let ws = base.join("ws");
    std::fs::create_dir_all(ws.join("tests")).unwrap();
    // model planted its own "tests" during the run
    std::fs::write(ws.join("tests").join("test_solution.py"), b"model-authored").unwrap();
    let staged = stage_hidden_files(&base.join("stage"), "suite/task-1", &ws).unwrap();
    assert_eq!(staged, vec!["tests/test_solution.py".to_string()]);
    let content = std::fs::read(ws.join("tests").join("test_solution.py")).unwrap();
    assert_eq!(content, b"suite-authored");
    // missing stage dir is a no-op
    let none = stage_hidden_files(&base.join("stage"), "suite/absent", &ws).unwrap();
    assert!(none.is_empty());
    #[cfg(unix)]
    {
        let stage2 = base.join("stage2").join("suite/task-2");
        std::fs::create_dir_all(&stage2).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", stage2.join("evil")).unwrap();
        let err = stage_hidden_files(&base.join("stage2"), "suite/task-2", &ws);
        assert!(err.is_err());
    }
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn staged_hidden_tests_score_python_solution_end_to_end() {
    // Full chain: model-written solution + staged hidden pytest file +
    // trusted-scoring verifier. Skipped when pytest is unavailable.
    if std::process::Command::new("pytest")
        .arg("--version")
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        eprintln!("pytest unavailable; skipping end-to-end scoring test");
        return;
    }
    let base = std::env::temp_dir().join(format!("rex-stage-score-{}", std::process::id()));
    let stage = base.join("stage").join("suite/add").join("tests");
    std::fs::create_dir_all(&stage).unwrap();
    std::fs::write(
        stage.join("test_solution.py"),
        b"import importlib.util\ndef test_add():\n    spec = importlib.util.spec_from_file_location(\"solution\", \"solution.py\")\n    m = importlib.util.module_from_spec(spec)\n    spec.loader.exec_module(m)\n    assert m.add(2, 3) == 5\n",
    )
    .unwrap();
    let ws = base.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("solution.py"), b"def add(a, b):\n    return a + b\n").unwrap();
    let staged = stage_hidden_files(&base.join("stage"), "suite/add", &ws).unwrap();
    assert_eq!(staged.len(), 1);
    let task = BenchTask {
        id: "suite/add".into(),
        prompt: "write add".into(),
        checks: vec![
            crate::contract::Proof::FileExists { path: "solution.py".into() },
            crate::contract::Proof::CommandSucceeds {
                argv: vec!["pytest".into(), "-q".into(), "tests/test_solution.py".into()],
                cwd: None,
                timeout_ms: Some(60_000),
            },
        ],
    };
    let report = score_workspace_with_scoring(&task, &ws, &base.join("ev"), Some(&["pytest"]));
    assert!(report.executable_all_proven, "{:?}", report.outcomes);
    // and a broken solution fails the same staged check
    std::fs::write(ws.join("solution.py"), b"def add(a, b):\n    return 0\n").unwrap();
    let staged = stage_hidden_files(&base.join("stage"), "suite/add", &ws).unwrap();
    assert_eq!(staged.len(), 1);
    let report = score_workspace_with_scoring(&task, &ws, &base.join("ev2"), Some(&["pytest"]));
    assert!(!report.executable_all_proven);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
    fn blind_scoring_ignores_prose() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        let task = BenchTask {
            id: "t".into(),
            prompt: "p".into(),
            checks: vec![Proof::FileContains {
                path: "result.txt".into(),
                needle: "42".into(),
            }],
        };
        // The model "says" it wrote the answer. It did not. Score must fail.
        let report = score_workspace(&task, &ws, &dir.path().join("ev"));
        assert!(!report.executable_all_proven);
        fs::write(ws.join("result.txt"), "the answer is 42").unwrap();
        let report = score_workspace(&task, &ws, &dir.path().join("ev2"));
        assert!(report.executable_all_proven);
    }
}
