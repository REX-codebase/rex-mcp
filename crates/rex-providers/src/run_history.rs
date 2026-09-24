//! Recall of earlier finished runs in the same workspace (`past_runs`).
//!
//! Each run leaves `state/brief.json` (task, start time, session name),
//! `state/checkpoint.json` (workspace) and, once it ends,
//! `state/terminal.json` (outcome, completion summary, files changed). This
//! module scans those records under the runs root and returns the runs of
//! one workspace, newest first, optionally filtered by a query. It reads
//! only REX's own run records: never file contents, never other
//! workspaces.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Most runs one `past_runs` call returns.
pub(crate) const MAX_RECALL: usize = 5;
/// Characters kept of a task or summary in a recall reply.
const TEXT_CHARS: usize = 600;
/// Files listed per run.
const MAX_FILES: usize = 10;
/// Run directories scanned at most (newest by name are not guaranteed, so
/// this only bounds the work on a huge runs root).
const MAX_SCAN: usize = 2_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunRecord {
    pub run_id: String,
    pub task: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub started_at_ms: u128,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
}

#[derive(Deserialize)]
struct BriefView {
    task: String,
    created_at_ms: u128,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
struct CheckpointView {
    #[serde(default)]
    workspace: Option<PathBuf>,
}

#[derive(Deserialize)]
struct TerminalView {
    #[serde(default)]
    outcome: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    files: Vec<String>,
}

fn read<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn clip(s: &str) -> String {
    let mut out: String = s.chars().take(TEXT_CHARS).collect();
    if out.len() < s.len() {
        out.push_str(" …");
    }
    out
}

/// Finished runs whose workspace is `workspace`, newest first. `query`
/// keeps runs whose task, summary or files contain every word of it
/// (case-insensitive); `exclude` skips one run id (the caller's own).
pub fn past_runs(
    runs_root: &Path,
    workspace: &Path,
    query: Option<&str>,
    exclude: Option<&str>,
    limit: usize,
) -> Vec<RunRecord> {
    let Ok(want) = workspace.canonicalize() else {
        return Vec::new();
    };
    let words: Vec<String> = query
        .unwrap_or("")
        .split_whitespace()
        .map(str::to_lowercase)
        .collect();
    let Ok(entries) = std::fs::read_dir(runs_root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten().take(MAX_SCAN) {
        let run_dir = entry.path();
        let run_id = entry.file_name().to_string_lossy().to_string();
        if exclude == Some(run_id.as_str()) || !run_dir.is_dir() {
            continue;
        }
        let state = run_dir.join("state");
        let Some(terminal) = read::<TerminalView>(&state.join("terminal.json")) else {
            continue; // still running, or not a run
        };
        let Some(brief) = read::<BriefView>(&state.join("brief.json")) else {
            continue;
        };
        let ws = read::<CheckpointView>(&state.join("checkpoint.json"))
            .and_then(|c| c.workspace)
            .unwrap_or_else(|| run_dir.join("workspace"));
        if ws.canonicalize().ok().as_deref() != Some(want.as_path()) {
            continue;
        }
        if !words.is_empty() {
            let hay = format!(
                "{}\n{}\n{}",
                brief.task,
                terminal.summary.as_deref().unwrap_or(""),
                terminal.files.join("\n")
            )
            .to_lowercase();
            if !words.iter().all(|w| hay.contains(w.as_str())) {
                continue;
            }
        }
        out.push(RunRecord {
            run_id,
            task: clip(&brief.task),
            name: brief.name,
            outcome: terminal.outcome.unwrap_or_else(|| "finished".into()),
            summary: terminal.summary.as_deref().map(clip),
            started_at_ms: brief.created_at_ms,
            files: terminal.files.into_iter().take(MAX_FILES).collect(),
        });
    }
    out.sort_by(|a, b| {
        b.started_at_ms
            .cmp(&a.started_at_ms)
            .then(a.run_id.cmp(&b.run_id))
    });
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    fn run(
        root: &Path,
        id: &str,
        ws: Option<&Path>,
        task: &str,
        at: u128,
        term: Option<serde_json::Value>,
    ) {
        let state = root.join(id).join("state");
        fs::create_dir_all(&state).unwrap();
        fs::write(
            state.join("brief.json"),
            json!({"task": task, "created_at_ms": at, "name": null}).to_string(),
        )
        .unwrap();
        fs::write(
            state.join("checkpoint.json"),
            json!({"workspace": ws}).to_string(),
        )
        .unwrap();
        if let Some(t) = term {
            fs::write(state.join("terminal.json"), t.to_string()).unwrap();
        }
    }

    #[test]
    fn lists_finished_runs_of_one_workspace_newest_first() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let ws = root.join("proj");
        let other = root.join("other");
        fs::create_dir_all(&ws).unwrap();
        fs::create_dir_all(&other).unwrap();
        let done = |summary: &str| {
            Some(json!({"outcome": "completed", "summary": summary, "files": ["src/parse.rs"]}))
        };
        run(
            root,
            "a",
            Some(&ws),
            "Fix the Parser",
            1,
            done("fixed Nested lists in parse"),
        );
        run(
            root,
            "b",
            Some(&ws),
            "add a CLI flag",
            3,
            Some(json!({"outcome": "blocked: step budget exhausted"})),
        );
        run(
            root,
            "c",
            Some(&other),
            "fix the parser elsewhere",
            5,
            done("x"),
        );
        run(root, "d", Some(&ws), "still running", 7, None);
        run(root, "self", Some(&ws), "the current run", 9, done("me"));
        // a legacy terminal.json with only a reason still counts
        run(
            root,
            "e",
            Some(&ws),
            "old run",
            2,
            Some(json!({"reason": "Completed", "elapsed_ms": 5})),
        );

        let all = past_runs(root, &ws, None, Some("self"), 10);
        let ids: Vec<&str> = all.iter().map(|r| r.run_id.as_str()).collect();
        assert_eq!(ids, vec!["b", "e", "a"]);
        assert_eq!(all[0].outcome, "blocked: step budget exhausted");
        assert_eq!(all[1].outcome, "finished");
        assert_eq!(
            all[2].summary.as_deref(),
            Some("fixed Nested lists in parse")
        );
        assert_eq!(all[2].files, vec!["src/parse.rs"]);

        // every query word must match, case-insensitively, in task, summary or files
        let hits = past_runs(root, &ws, Some("PARSER nested"), Some("self"), 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].run_id, "a");
        assert_eq!(
            past_runs(root, &ws, Some("parse.rs"), Some("self"), 10).len(),
            1
        );
        assert!(past_runs(root, &ws, Some("parser flag"), Some("self"), 10).is_empty());
        // limit, and a workspace that does not exist
        assert_eq!(past_runs(root, &ws, None, Some("self"), 2).len(), 2);
        assert!(past_runs(root, &root.join("missing"), None, None, 10).is_empty());
        // without exclude, the current run is listed too
        assert_eq!(past_runs(root, &ws, None, None, 10)[0].run_id, "self");
    }

    #[test]
    fn long_text_is_clipped_and_files_capped() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let ws = root.join("proj");
        fs::create_dir_all(&ws).unwrap();
        let files: Vec<String> = (0..20).map(|i| format!("f{i}.rs")).collect();
        run(
            root,
            "a",
            Some(&ws),
            &"t".repeat(2_000),
            1,
            Some(json!({"outcome": "completed", "summary": "s".repeat(2_000), "files": files})),
        );
        // a run with no stored workspace uses its own workspace dir
        let own = root.join("b").join("workspace");
        run(
            root,
            "b",
            None,
            "own ws",
            2,
            Some(json!({"outcome": "completed"})),
        );
        fs::create_dir_all(&own).unwrap();
        let r = &past_runs(root, &ws, None, None, 10)[0];
        assert_eq!(r.task.chars().count(), TEXT_CHARS + 2);
        assert!(r.summary.as_deref().unwrap().ends_with(" …"));
        assert_eq!(r.files.len(), MAX_FILES);
        let own_runs = past_runs(root, &own, None, None, 10);
        assert_eq!(own_runs.len(), 1);
        assert_eq!(own_runs[0].run_id, "b");
    }
}
