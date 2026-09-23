//! Diff-hunk epistemic provenance (leapfrog bet 4).
//!
//! Every executed tool call already carries an audit receipt with the
//! unified diff it produced. This module folds those diffs into a
//! per-file, per-hunk attribution map: for each file the run touched,
//! which event (in order) wrote which hunks, with which tool.
//!
//! The receipt's `provenance` field answers "which step wrote these
//! lines?" It is mechanical bookkeeping, not a semantic claim: it says
//! which tool call emitted the bytes, not that the bytes are correct.

use rex_providers::autonomous::AgentEvent;
use serde::Serialize;
use std::collections::BTreeMap;

/// One tool call's contribution to one file.
#[derive(Debug, Clone, Serialize)]
pub struct HunkProvenance {
    pub event_seq: usize,
    pub tool: String,
    pub hunks: Vec<String>,
}

/// Parse `@@ -a,b +c,d @@` headers out of a unified diff.
fn hunk_headers(diff: &str) -> Vec<String> {
    diff.lines()
        .filter_map(|l| {
            let t = l.trim();
            if !t.starts_with("@@") {
                return None;
            }
            // Keep "@@ -a,b +c,d @@", drop any trailing section heading.
            let end = t[2..].find("@@").map(|i| i + 4)?;
            Some(t[..end].to_string())
        })
        .collect()
}

fn relativize(root: &str, target: &str) -> String {
    let root = root.trim_end_matches('/');
    match target.strip_prefix(root) {
        Some(rest) => rest.trim_start_matches('/').to_string(),
        None => target.to_string(),
    }
}

/// Build the file -> contributions map from a run's event log.
/// `event_seq` is the index of the event in the log, so ordering is
/// the order things actually happened.
pub fn build(events: &[AgentEvent]) -> BTreeMap<String, Vec<HunkProvenance>> {
    let mut out: BTreeMap<String, Vec<HunkProvenance>> = BTreeMap::new();
    for (seq, event) in events.iter().enumerate() {
        let AgentEvent::ToolFinished { result } = event else {
            continue;
        };
        if !result.ok {
            continue;
        }
        let diff = match result.receipt.diff.as_deref() {
            Some(d) => d,
            None => continue,
        };
        let hunks = hunk_headers(diff);
        if hunks.is_empty() {
            continue;
        }
        let file = match result.receipt.target.as_deref() {
            Some(t) => relativize(&result.receipt.workspace_root, t),
            None => continue,
        };
        out.entry(file).or_default().push(HunkProvenance {
            event_seq: seq,
            tool: result.tool.clone(),
            hunks,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rex_tools::{AuditReceipt, CallState, ToolResult};

    fn finished(tool: &str, target: Option<&str>, diff: Option<&str>) -> AgentEvent {
        AgentEvent::ToolFinished {
            result: ToolResult {
                call_id: "c1".to_string(),
                ok: true,
                tool: tool.to_string(),
                state: CallState::Executed,
                output: None,
                error: None,
                receipt: AuditReceipt {
                    started_at_ms: 0,
                    duration_ms: 1,
                    workspace_root: "/tmp/ws".to_string(),
                    target: target.map(str::to_string),
                    command: None,
                    exit_code: None,
                    bytes_read: 0,
                    bytes_written: 10,
                    output_truncated: false,
                    diff: diff.map(str::to_string),
                    redactions: 0,
                    sandbox: None,
                },
            },
        }
    }

    const DIFF1: &str = "--- a.txt\n+++ a.txt\n@@ -1,2 +1,3 @@\n a\n+b\n c\n";
    const DIFF2: &str = "--- a.txt\n+++ a.txt\n@@ -5,2 +6,2 @@\n-x\n+y\n";

    #[test]
    fn attributes_hunks_to_events_in_order() {
        let events = vec![
            finished("write_file", Some("/tmp/ws/a.txt"), Some(DIFF1)),
            finished("edit_file", Some("/tmp/ws/a.txt"), Some(DIFF2)),
        ];
        let p = build(&events);
        let entries = &p["a.txt"];
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].event_seq, 0);
        assert_eq!(entries[0].tool, "write_file");
        assert_eq!(entries[0].hunks, vec!["@@ -1,2 +1,3 @@"]);
        assert_eq!(entries[1].event_seq, 1);
        assert_eq!(entries[1].hunks, vec!["@@ -5,2 +6,2 @@"]);
    }

    #[test]
    fn skips_events_without_changes() {
        let events = vec![
            finished("read_file", Some("/tmp/ws/a.txt"), None),
            AgentEvent::ToolFinished {
                result: {
                    let mut r = match finished("write_file", Some("/tmp/ws/b.txt"), Some(DIFF1)) {
                        AgentEvent::ToolFinished { result } => result,
                        _ => unreachable!(),
                    };
                    r.ok = false;
                    r
                },
            },
        ];
        assert!(build(&events).is_empty());
    }

    #[test]
    fn hunk_header_parsing_ignores_section_headings() {
        let d = "@@ -1,2 +1,3 @@ fn main()\n context\n";
        assert_eq!(hunk_headers(d), vec!["@@ -1,2 +1,3 @@"]);
    }
}
