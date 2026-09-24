//! Working memory: bounded excerpts of older tool results.
//!
//! REX sends each turn a fresh state message plus only the previous turn's
//! exact tool exchange, so context never overflows. The cost was that a
//! file read three turns ago was gone except for a one-line digest entry,
//! and the model re-read it. opencode (`session/compaction.ts`, PRUNE_PROTECT)
//! and Hermes (`agent/context_compressor.py`, protected tail) instead keep a
//! transcript and clear old tool output once a budget is hit. This module
//! gives REX the useful half of that behaviour without a transcript: the
//! newest tool results stay visible as clipped excerpts under a fixed
//! character budget, oldest dropped first, and reads of a file the run later
//! changed are dropped so the model never works from stale content.

use serde::{Deserialize, Serialize};

pub const OBS_EXCERPT_CHARS: usize = 2_000;
pub const OBS_BUDGET_CHARS: usize = 12_000;
pub const OBS_MAX_ENTRIES: usize = 24;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Observation {
    pub turn: usize,
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    pub ok: bool,
    pub excerpt: String,
}

impl Observation {
    pub fn new(turn: usize, tool: &str, target: Option<String>, ok: bool, text: &str) -> Self {
        let (excerpt, _) = rex_tools::clip_middle(text, OBS_EXCERPT_CHARS);
        Self {
            turn,
            tool: tool.to_string(),
            target,
            ok,
            excerpt,
        }
    }
    fn cost(&self) -> usize {
        self.excerpt.chars().count()
    }
}

/// Add an observation, then enforce the entry and character budgets by
/// dropping the oldest entries.
pub fn record(list: &mut Vec<Observation>, obs: Observation) {
    if obs.excerpt.trim().is_empty() {
        return;
    }
    list.push(obs);
    while list.len() > OBS_MAX_ENTRIES {
        list.remove(0);
    }
    while list.len() > 1 && list.iter().map(Observation::cost).sum::<usize>() > OBS_BUDGET_CHARS {
        list.remove(0);
    }
}

/// A successful write changed `path` (or, for `None`, an unknown set of
/// files, e.g. a multi-file patch): drop reads that may now be stale.
pub fn supersede(list: &mut Vec<Observation>, path: Option<&str>) {
    list.retain(|o| {
        if o.tool != "read_file" {
            return true;
        }
        match (path, o.target.as_deref()) {
            (None, _) => false,
            (Some(p), Some(t)) => !same_path(p, t),
            (Some(_), None) => true,
        }
    });
}

fn same_path(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.trim_start_matches("./").trim_end_matches('/').to_string();
    norm(a) == norm(b)
        || a.ends_with(&format!("/{}", norm(b)))
        || b.ends_with(&format!("/{}", norm(a)))
}

/// Observations older than `current_turn` (the newest turn's results are
/// already in the exact exchange the request carries).
pub fn earlier(list: &[Observation], current_turn: usize) -> Vec<&Observation> {
    list.iter().filter(|o| o.turn < current_turn).collect()
}

pub const MEMORY_NOTE: &str = "Clipped excerpts of older tool results, newest last. Oldest are dropped first; reads of files you later changed are removed. Re-read a file when you need its exact current content.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_drops_oldest_first() {
        let mut l = Vec::new();
        for t in 0..20 {
            record(
                &mut l,
                Observation::new(
                    t,
                    "read_file",
                    Some(format!("f{t}")),
                    true,
                    &"x".repeat(OBS_EXCERPT_CHARS),
                ),
            );
        }
        let total: usize = l.iter().map(Observation::cost).sum();
        assert!(total <= OBS_BUDGET_CHARS);
        assert_eq!(l.last().unwrap().turn, 19);
        assert!(l.first().unwrap().turn > 0);
    }

    #[test]
    fn entry_cap_and_empty_skip() {
        let mut l = Vec::new();
        record(&mut l, Observation::new(1, "glob_files", None, true, "   "));
        assert!(l.is_empty());
        for t in 0..40 {
            record(&mut l, Observation::new(t, "glob_files", None, true, "a"));
        }
        assert_eq!(l.len(), OBS_MAX_ENTRIES);
        assert_eq!(l[0].turn, 40 - OBS_MAX_ENTRIES);
    }

    #[test]
    fn writes_supersede_stale_reads_only() {
        let mut l = Vec::new();
        record(
            &mut l,
            Observation::new(1, "read_file", Some("src/a.rs".into()), true, "old a"),
        );
        record(
            &mut l,
            Observation::new(1, "read_file", Some("src/b.rs".into()), true, "b"),
        );
        record(
            &mut l,
            Observation::new(2, "run_command", Some("cargo test".into()), false, "fail"),
        );
        supersede(&mut l, Some("./src/a.rs"));
        assert_eq!(
            l.iter()
                .map(|o| o.target.clone().unwrap())
                .collect::<Vec<_>>(),
            ["src/b.rs", "cargo test"]
        );
        supersede(&mut l, None);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].tool, "run_command");
    }

    #[test]
    fn earlier_excludes_current_turn_and_clips_on_char_boundary() {
        let mut l = Vec::new();
        record(
            &mut l,
            Observation::new(
                1,
                "read_file",
                None,
                true,
                &"é".repeat(OBS_EXCERPT_CHARS * 2),
            ),
        );
        record(&mut l, Observation::new(2, "read_file", None, true, "now"));
        let e = earlier(&l, 2);
        assert_eq!(e.len(), 1);
        assert!(e[0].excerpt.chars().count() <= OBS_EXCERPT_CHARS + 200);
    }
}
