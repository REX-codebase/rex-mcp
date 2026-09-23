//! The tournament stdout contract: with `--json`, the bracket is the single
//! JSON document on stdout. Nothing else may write there — downstream tools
//! (and the GitHub proof-summary action) parse stdout as one receipt.
//!
//! Uses bogus provider ids so no network or API key is needed: contestants
//! fail fast at provider lookup and still get honest seats in the bracket.

use std::process::Command;

fn rex_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_rex"))
}

#[test]
fn tournament_json_stdout_is_one_document() {
    let state = std::env::temp_dir().join(format!("rex-tournament-stdout-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state);

    let out = Command::new(rex_bin())
        .args([
            "tournament",
            "--task",
            "say hi",
            "--providers",
            "bogus-alpha,bogus-beta",
            "--json",
            "--yes",
        ])
        .env("REX_STATE_DIR", &state)
        .output()
        .expect("failed to run rex tournament");

    // No winner completed (both contestants failed to start).
    assert_eq!(out.status.code(), Some(3));

    let stdout = String::from_utf8(out.stdout).expect("stdout is not UTF-8");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "expected exactly one stdout line, got {}: {stdout:?}",
        lines.len()
    );
    let doc: serde_json::Value =
        serde_json::from_str(lines[0]).expect("stdout line is not valid JSON");
    assert_eq!(
        doc.get("schema").and_then(|s| s.as_str()),
        Some("rex.tournament.receipt/1")
    );
    let contestants = doc
        .get("contestants")
        .and_then(|c| c.as_array())
        .expect("bracket has no contestants array");
    assert_eq!(contestants.len(), 2);
    for c in contestants {
        assert_eq!(c.get("status").and_then(|s| s.as_str()), Some("failed"));
    }
    assert!(doc.get("winner").is_some(), "bracket has no winner block");

    let _ = std::fs::remove_dir_all(&state);
}
