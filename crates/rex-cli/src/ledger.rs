//! The run ledger: every signed receipt `rex` produces is appended here.
//!
//! `$REX_STATE_DIR/runs.jsonl`, one JSON receipt per line. It is the local,
//! offline-auditable history of everything the machine ran — `rex runs`
//! lists it, `rex show <id>` inspects one entry, and `rex verify` still
//! checks each receipt's own certificate, so the ledger is tamper-evident
//! entry by entry.

use serde_json::Value;
use std::path::{Path, PathBuf};

pub fn ledger_path(state_dir: &Path) -> PathBuf {
    state_dir.join("runs.jsonl")
}

/// Best-effort append. A ledger failure must never fail the run it records.
pub fn append(state_dir: &Path, receipt: &Value) {
    let path = ledger_path(state_dir);
    if let Err(e) = (|| -> std::io::Result<()> {
        use std::io::Write;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        writeln!(f, "{}", serde_json::to_string(receipt).unwrap())?;
        Ok(())
    })() {
        eprintln!("rex: warning: could not append to run ledger: {e}");
    }
}

/// Read all parseable entries, oldest first. Corrupt lines are skipped with
/// a warning on stderr — the ledger degrades, it never lies.
pub fn read_all(state_dir: &Path) -> Vec<Value> {
    let path = ledger_path(state_dir);
    let raw = match std::fs::read_to_string(&path) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for (n, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(v) => out.push(v),
            Err(e) => eprintln!("rex: warning: skipping corrupt ledger line {}: {e}", n + 1),
        }
    }
    out
}

/// Exact match first, then a unique prefix. Ambiguous or missing → None.
pub fn find(state_dir: &Path, id: &str) -> Option<Value> {
    let entries = read_all(state_dir);
    if let Some(v) = entries
        .iter()
        .find(|v| v.get("run_id").and_then(Value::as_str) == Some(id))
    {
        return Some(v.clone());
    }
    let mut hits = entries.iter().filter(|v| {
        v.get("run_id")
            .and_then(Value::as_str)
            .is_some_and(|rid| rid.starts_with(id))
    });
    let first = hits.next()?.clone();
    if hits.next().is_some() {
        return None; // ambiguous
    }
    Some(first)
}

pub fn schema_of(v: &Value) -> &str {
    v.get("schema").and_then(Value::as_str).unwrap_or("?")
}

pub fn kind_of(v: &Value) -> &str {
    match schema_of(v) {
        "rex.tournament.receipt/1" => "tournament",
        _ => "exec",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_state() -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "rex-ledger-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn append_read_find_roundtrip() {
        let s = temp_state();
        assert!(read_all(&s).is_empty());
        append(
            &s,
            &serde_json::json!({"run_id": "abc123", "status": "completed"}),
        );
        append(
            &s,
            &serde_json::json!({"run_id": "abd456", "status": "failed"}),
        );
        // A corrupt line is skipped, not fatal.
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(ledger_path(&s))
                .unwrap();
            writeln!(f, "{{not json}}").unwrap();
        }
        let all = read_all(&s);
        assert_eq!(all.len(), 2);
        assert!(find(&s, "abc123").is_some());
        assert!(find(&s, "abc").is_some()); // unique prefix
        assert!(find(&s, "ab").is_none()); // ambiguous
        assert!(find(&s, "zzz").is_none());
        let _ = std::fs::remove_dir_all(&s);
    }
}
