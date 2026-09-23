//! Named-session compaction and clear: context reset within a session.
//!
//! A session accumulates runs: every `rex resume` starts a fresh run id with
//! `continued_from` pointing at the previous run. Over a long session that
//! chain gets hard to hold in your head, and the next resume only quotes the
//! single previous task.
//!
//! `rex compact NAME` collapses the whole chain into one mechanical summary
//! stored at `$REX_STATE_DIR/sessions/<name>.json` — run ids, tasks, results,
//! token spend, newest first, plus a short `history` of one-line entries
//! oldest-first. The next `rex resume NAME` injects that history into the
//! continuation task, so the model gets distilled context instead of nothing.
//!
//! `rex clear NAME` deletes the compacted context (idempotent). The run
//! ledger is never rewritten: compaction is an index over immutable history,
//! and clear only drops the index.

use crate::exec::ExecError;
use crate::ledger;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Where a session's compacted context lives. Session names are validated
/// (letters, digits, `-`, `_`), so the name is safe as a file stem.
pub fn session_file(state_dir: &Path, name: &str) -> PathBuf {
    state_dir.join("sessions").join(format!("{name}.json"))
}

fn truncate_chars(s: &str, n: usize) -> String {
    let count = s.chars().count();
    if count <= n {
        return s.to_string();
    }
    let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// The session's run chain, newest first. Follows `continued_from` links
/// from the newest run carrying `name`; a visited set plus a cap keeps a
/// corrupt ledger from looping forever. Runs whose receipts are missing
/// from the ledger end the walk — the chain is best-effort, never invented.
pub fn session_chain(state_dir: &Path, name: &str) -> Vec<Value> {
    let newest = ledger::read_all(state_dir).into_iter().rfind(|v| {
        ledger::kind_of(v) == "exec" && v.get("name").and_then(Value::as_str) == Some(name)
    });
    let mut chain = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut next = newest;
    while let Some(v) = next {
        let id = v.get("run_id").and_then(Value::as_str).unwrap_or("");
        if id.is_empty() || !seen.insert(id.to_string()) || chain.len() >= 200 {
            break;
        }
        let parent = v
            .get("continued_from")
            .and_then(Value::as_str)
            .map(str::to_string);
        chain.push(v);
        next = parent.and_then(|p| ledger::find(state_dir, &p));
    }
    chain
}

fn short_id(v: &Value) -> String {
    v.get("run_id")
        .and_then(Value::as_str)
        .map(|s| s.chars().take(8).collect())
        .unwrap_or_else(|| "?".to_string())
}

/// One compact line per run, oldest first, for injection into a continuation
/// task. Bounded: callers cap how many lines they take.
pub fn history_lines(chain_newest_first: &[Value]) -> Vec<String> {
    chain_newest_first
        .iter()
        .rev()
        .map(|v| {
            let status = v.get("status").and_then(Value::as_str).unwrap_or("?");
            let task = v
                .get("task")
                .and_then(Value::as_str)
                .map(|t| truncate_chars(&t.replace('\n', " "), 120))
                .unwrap_or_else(|| "(no task recorded)".to_string());
            let tokens = v.get("tokens_used").and_then(Value::as_u64).unwrap_or(0);
            format!("{}: {status} — {task} ({tokens} tok)", short_id(v))
        })
        .collect()
}

fn run_summary(v: &Value) -> Value {
    json!({
        "run_id": v.get("run_id").and_then(Value::as_str).unwrap_or(""),
        "task": v.get("task").and_then(Value::as_str).map(|t| truncate_chars(t, 300)).unwrap_or_default(),
        "status": v.get("status").and_then(Value::as_str).unwrap_or("?"),
        "result": v.get("result").and_then(Value::as_str).map(|r| truncate_chars(r, 300)).unwrap_or_default(),
        "tokens_used": v.get("tokens_used").and_then(Value::as_u64).unwrap_or(0),
        "finished_at": v.get("finished_at").and_then(Value::as_str).unwrap_or(""),
        "continued_from": v.get("continued_from").and_then(Value::as_str),
        "workspace": v.get("workspace").and_then(Value::as_str),
    })
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Read a previously compacted session context, if any.
pub fn compacted_history(state_dir: &Path, name: &str) -> Option<Vec<String>> {
    let raw = std::fs::read_to_string(session_file(state_dir, name)).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    let lines: Vec<String> = v
        .get("history")?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    if lines.is_empty() {
        None
    } else {
        Some(lines)
    }
}

fn parse_name_and_json(args: &[String], cmd: &str) -> Result<(String, bool), ExecError> {
    let mut name: Option<String> = None;
    let mut json = false;
    for a in args {
        match a.as_str() {
            "--json" => json = true,
            "--help" | "-h" => {
                return Err(ExecError::usage(format!("usage: rex {cmd} NAME [--json]")));
            }
            other if other.starts_with('-') => {
                return Err(ExecError::usage(format!(
                    "unknown flag '{other}': usage: rex {cmd} NAME [--json]"
                )));
            }
            other => {
                if name.is_some() {
                    return Err(ExecError::usage(format!("usage: rex {cmd} NAME [--json]")));
                }
                name = Some(crate::exec::validate_session_name(other)?);
            }
        }
    }
    let name = name.ok_or_else(|| ExecError::usage(format!("usage: rex {cmd} NAME [--json]")))?;
    Ok((name, json))
}

/// `rex compact NAME [--json]`: collapse the session's run chain into the
/// compacted-context file.
pub fn run_compact(args: &[String]) -> Result<i32, ExecError> {
    let (name, as_json) = parse_name_and_json(args, "compact")?;
    let state = crate::exec::state_dir();
    let chain = session_chain(&state, &name);
    if chain.is_empty() {
        return Err(ExecError::usage(format!(
            "cannot compact '{name}': no runs found for that session name"
        )));
    }
    let history = history_lines(&chain);
    let summary = json!({
        "session": name,
        "compacted_at": now_rfc3339(),
        "runs_compacted": chain.len(),
        "runs": chain.iter().map(run_summary).collect::<Vec<_>>(),
        "history": history,
    });
    let path = session_file(&state, &name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ExecError::internal(format!("cannot create sessions dir: {e}")))?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(&summary).unwrap())
        .map_err(|e| ExecError::internal(format!("cannot write {}: {e}", path.display())))?;
    if as_json {
        println!("{}", serde_json::to_string(&summary).unwrap());
    } else {
        eprintln!(
            "rex: compacted session '{name}': {} run{} → {}",
            chain.len(),
            if chain.len() == 1 { "" } else { "s" },
            path.display()
        );
        for line in &history {
            eprintln!("  {line}");
        }
    }
    Ok(0)
}

/// `rex clear NAME [--json]`: drop the session's compacted context.
/// Idempotent — clearing what was never compacted is not an error.
pub fn run_clear(args: &[String]) -> Result<i32, ExecError> {
    let (name, as_json) = parse_name_and_json(args, "clear")?;
    let state = crate::exec::state_dir();
    let path = session_file(&state, &name);
    let cleared = match std::fs::remove_file(&path) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => {
            return Err(ExecError::internal(format!(
                "cannot clear session '{name}': {e}"
            )));
        }
    };
    if as_json {
        println!(
            "{}",
            serde_json::to_string(&json!({"session": name, "cleared": cleared})).unwrap()
        );
    } else if cleared {
        eprintln!("rex: cleared compacted context for session '{name}'");
    } else {
        eprintln!("rex: session '{name}' had no compacted context (nothing to clear)");
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static SESSION_ENV_LOCK: Mutex<()> = Mutex::new(());

    fn fake_state(tag: &str) -> PathBuf {
        let base =
            std::env::temp_dir().join(format!("rex-session-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    fn receipt(id: &str, name: Option<&str>, continued_from: Option<&str>, task: &str) -> Value {
        let mut m = json!({
            "schema": "rex.exec.receipt/1",
            "run_id": id,
            "task": task,
            "status": "Completed",
            "result": format!("result of {id}"),
            "tokens_used": 1234u64,
            "finished_at": "2026-09-23T00:00:00Z",
        });
        if let Some(n) = name {
            m["name"] = Value::String(n.to_string());
        }
        if let Some(p) = continued_from {
            m["continued_from"] = Value::String(p.to_string());
        }
        m
    }

    fn with_state_dir<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
        let _guard = SESSION_ENV_LOCK.lock().unwrap();
        let prev = std::env::var_os("REX_STATE_DIR");
        std::env::set_var("REX_STATE_DIR", dir);
        let out = f();
        match prev {
            Some(v) => std::env::set_var("REX_STATE_DIR", v),
            None => std::env::remove_var("REX_STATE_DIR"),
        }
        out
    }

    #[test]
    fn compact_collapses_chain_newest_first() {
        let state = fake_state("chain");
        ledger::append(&state, &receipt("run-1", Some("alpha"), None, "first task"));
        ledger::append(
            &state,
            &receipt("run-2", Some("alpha"), Some("run-1"), "second task"),
        );
        ledger::append(
            &state,
            &receipt("run-3", Some("alpha"), Some("run-2"), "third task"),
        );
        ledger::append(&state, &receipt("run-9", Some("other"), None, "unrelated"));

        let chain = session_chain(&state, "alpha");
        assert_eq!(chain.len(), 3);
        assert_eq!(chain[0]["run_id"], json!("run-3"));
        assert_eq!(chain[2]["run_id"], json!("run-1"));

        let history = history_lines(&chain);
        assert_eq!(history.len(), 3);
        assert!(
            history[0].starts_with("run-1"),
            "oldest first, got: {}",
            history[0]
        );
        assert!(history[2].starts_with("run-3"));

        with_state_dir(&state, || {
            let code = run_compact(&["alpha".to_string(), "--json".to_string()]).unwrap();
            assert_eq!(code, 0);
        });
        let raw = std::fs::read_to_string(session_file(&state, "alpha")).unwrap();
        let v: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["session"], json!("alpha"));
        assert_eq!(v["runs_compacted"], json!(3));
        assert_eq!(v["runs"].as_array().unwrap().len(), 3);
        assert_eq!(v["history"].as_array().unwrap().len(), 3);

        // compacted_history round-trips for resume injection.
        let back = compacted_history(&state, "alpha").unwrap();
        assert_eq!(back.len(), 3);
        assert!(compacted_history(&state, "never").is_none());
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn compact_unknown_session_is_a_usage_error() {
        let state = fake_state("unknown");
        with_state_dir(&state, || {
            let e = run_compact(&["ghost".to_string()]).unwrap_err();
            assert_eq!(e.code, 2);
            assert!(e.message.contains("no runs found"), "got: {}", e.message);
        });
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn compact_rejects_bad_names_like_exec_does() {
        let e = parse_name_and_json(&["has space".to_string()], "compact").unwrap_err();
        assert_eq!(e.code, 2);
        let e = parse_name_and_json(&[], "compact").unwrap_err();
        assert_eq!(e.code, 2);
    }

    #[test]
    fn session_chain_terminates_on_cycles() {
        let state = fake_state("cycle");
        ledger::append(&state, &receipt("run-a", Some("loop"), Some("run-b"), "a"));
        ledger::append(&state, &receipt("run-b", Some("loop"), Some("run-a"), "b"));
        let chain = session_chain(&state, "loop");
        assert_eq!(chain.len(), 2, "visited set must stop the cycle");
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn clear_is_idempotent() {
        let state = fake_state("clear");
        ledger::append(&state, &receipt("run-1", Some("beta"), None, "task"));
        with_state_dir(&state, || {
            run_compact(&["beta".to_string()]).unwrap();
            assert!(session_file(&state, "beta").exists());
            let code = run_clear(&["beta".to_string()]).unwrap();
            assert_eq!(code, 0);
            assert!(!session_file(&state, "beta").exists());
            // Second clear: nothing to clear, still success.
            let code = run_clear(&["beta".to_string(), "--json".to_string()]).unwrap();
            assert_eq!(code, 0);
        });
        let _ = std::fs::remove_dir_all(&state);
    }
}
