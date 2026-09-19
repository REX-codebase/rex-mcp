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
use crate::verify::{verify_contract, ObligationOutcome, VerificationReport};
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    pub task_id: String,
    pub mode: BenchMode,
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
    verify_contract(&contract, workspace, &mut evidence)
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

pub fn raw_prompt(task: &BenchTask) -> String {
    format!(
        "{}\n\nProduce the complete solution as fenced code blocks, one per file, \
each opened with an info string of exactly `file:<relative path>`. No prose between blocks.",
        task.prompt
    )
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
    fn raw_file_extraction_is_path_safe() {
        let text = "Sure!\n```file:src/main.rs\nfn main() {}\n```\n```file:../escape\nx\n```\n```file:/abs\ny\n```";
        let files = extract_raw_files(text);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, "src/main.rs");
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
