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
    /// Set when no run matched every query word and this one matches only
    /// some of them.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub partial: bool,
}

/// How well a run matches the query words: (words found, weight). A word
/// found in the task weighs 3, in the summary 2, in the files 1 (the best
/// place counts once per word). Hermes ranks its session search with FTS5
/// relevance plus a recency bias (`tools/session_search_tool.py`); REX
/// ranks by words found, then weight, then newest.
fn score(words: &[String], task: &str, summary: &str, files: &str) -> (usize, usize) {
    let (task, summary, files) = (
        task.to_lowercase(),
        summary.to_lowercase(),
        files.to_lowercase(),
    );
    let mut found = 0;
    let mut weight = 0;
    for w in words {
        let best = if task.contains(w.as_str()) {
            3
        } else if summary.contains(w.as_str()) {
            2
        } else if files.contains(w.as_str()) {
            1
        } else {
            0
        };
        if best > 0 {
            found += 1;
            weight += best;
        }
    }
    (found, weight)
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

/// Finished runs whose workspace is `workspace`, newest first. With a
/// `query`, runs whose task, summary or files contain every word of it
/// (case-insensitive) come back best match first; when no run has every
/// word, runs with some of them come back marked `partial`, most words
/// first. `exclude` skips one run id (the caller's own).
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
    let words: Vec<String> = {
        let mut seen = Vec::new();
        for w in words {
            if !seen.contains(&w) {
                seen.push(w);
            }
        }
        seen
    };
    let mut out: Vec<(RunRecord, (usize, usize))> = Vec::new();
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
        let rank = score(
            &words,
            &brief.task,
            terminal.summary.as_deref().unwrap_or(""),
            &terminal.files.join("\n"),
        );
        if !words.is_empty() && rank.0 == 0 {
            continue;
        }
        out.push((
            RunRecord {
                run_id,
                task: clip(&brief.task),
                name: brief.name,
                outcome: terminal.outcome.unwrap_or_else(|| "finished".into()),
                summary: terminal.summary.as_deref().map(clip),
                started_at_ms: brief.created_at_ms,
                files: terminal.files.into_iter().take(MAX_FILES).collect(),
                partial: false,
            },
            rank,
        ));
    }
    if !words.is_empty() {
        if out.iter().any(|(_, r)| r.0 == words.len()) {
            out.retain(|(_, r)| r.0 == words.len());
        } else {
            for (rec, _) in &mut out {
                rec.partial = true;
            }
        }
    }
    out.sort_by(|(a, ra), (b, rb)| {
        rb.cmp(ra)
            .then(b.started_at_ms.cmp(&a.started_at_ms))
            .then(a.run_id.cmp(&b.run_id))
    });
    out.truncate(limit);
    out.into_iter().map(|(rec, _)| rec).collect()
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
        // no run has both words: runs with one come back marked partial,
        // newest first when they tie
        let some = past_runs(root, &ws, Some("parser flag"), Some("self"), 10);
        let ids: Vec<&str> = some.iter().map(|r| r.run_id.as_str()).collect();
        assert_eq!(ids, ["b", "a"]);
        assert!(some.iter().all(|r| r.partial));
        assert!(!hits[0].partial);
        assert!(past_runs(root, &ws, Some("zebra"), Some("self"), 10).is_empty());
        // limit, and a workspace that does not exist
        assert_eq!(past_runs(root, &ws, None, Some("self"), 2).len(), 2);
        assert!(past_runs(root, &root.join("missing"), None, None, 10).is_empty());
        // without exclude, the current run is listed too
        assert_eq!(past_runs(root, &ws, None, None, 10)[0].run_id, "self");
    }

    #[test]
    fn matches_rank_by_words_then_place_then_age() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let ws = root.join("proj");
        fs::create_dir_all(&ws).unwrap();
        let t = |summary: &str, files: &[&str]| {
            Some(json!({"outcome": "completed", "summary": summary, "files": files}))
        };
        // "login" in the files only, newest
        run(
            root,
            "files",
            Some(&ws),
            "tidy up",
            9,
            t("cleanup", &["src/login.rs"]),
        );
        // in the summary
        run(
            root,
            "summary",
            Some(&ws),
            "fix auth",
            5,
            t("the login form works", &[]),
        );
        // in the task, oldest
        run(root, "task", Some(&ws), "Login page", 1, t("done", &[]));
        // in the task and again in the summary: counts once
        run(root, "both", Some(&ws), "login", 2, t("login login", &[]));
        let ids = |q: &str| -> Vec<String> {
            past_runs(root, &ws, Some(q), None, 10)
                .into_iter()
                .map(|r| r.run_id)
                .collect()
        };
        assert_eq!(ids("login"), ["both", "task", "summary", "files"]);
        // a repeated query word does not count twice
        assert_eq!(ids("login LOGIN"), ["both", "task", "summary", "files"]);
        // runs with every word hide the ones with only some
        assert_eq!(ids("login form"), ["summary"]);
        // a repeated word cannot lift a partial match over one with more words
        assert_eq!(ids("tidy tidy form works zebra")[0], "summary");
        // partial: two words beat one
        assert_eq!(
            ids("login form zebra"),
            ["summary", "both", "task", "files"]
        );
        // limit keeps the best
        assert_eq!(
            past_runs(root, &ws, Some("login"), None, 1)[0].run_id,
            "both"
        );
        // the partial flag is in the JSON only when set
        let full = serde_json::to_string(&past_runs(root, &ws, Some("login"), None, 1)[0]).unwrap();
        assert!(!full.contains("partial"));
        let part =
            serde_json::to_string(&past_runs(root, &ws, Some("login zebra"), None, 1)[0]).unwrap();
        assert!(part.contains(r#""partial":true"#));
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
