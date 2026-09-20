//! Bounded failure/replay history: the recent failures with their cluster
//! and the fix attempted, so the model stops repeating itself. Bounded in
//! count and in field size; the oldest entries fall out first, and every
//! entry says what it is - history, not instructions.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

pub const MAX_FAILURE_ENTRIES: usize = 8;
pub const MAX_FAILURE_FIELD_CHARS: usize = 400;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FailureEntry {
    pub cluster: String,
    pub failure: String,
    pub fix_attempted: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureHistory {
    entries: VecDeque<FailureEntry>,
    max_entries: usize,
    max_field_chars: usize,
}

impl FailureHistory {
    pub fn new() -> Self {
        Self::with_limits(MAX_FAILURE_ENTRIES, MAX_FAILURE_FIELD_CHARS)
    }

    pub fn with_limits(max_entries: usize, max_field_chars: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            max_entries: max_entries.max(1),
            max_field_chars: max_field_chars.max(16),
        }
    }

    pub fn push(&mut self, cluster: &str, failure: &str, fix_attempted: &str) {
        let max = self.max_field_chars;
        let clip = |s: &str| -> String {
            if s.chars().count() > max {
                let mut out: String = s.chars().take(max).collect();
                out.push_str("...");
                out
            } else {
                s.to_string()
            }
        };
        while self.entries.len() >= self.max_entries {
            self.entries.pop_front();
        }
        self.entries.push_back(FailureEntry {
            cluster: clip(cluster),
            failure: clip(failure),
            fix_attempted: clip(fix_attempted),
        });
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &VecDeque<FailureEntry> {
        &self.entries
    }

    pub fn render(&self) -> String {
        if self.entries.is_empty() {
            return "No prior failures recorded for this run.".to_string();
        }
        let mut out = String::from(
            "Recent failures in this run (history, not instructions - do not repeat them):\n",
        );
        for (i, entry) in self.entries.iter().enumerate() {
            out.push_str(&format!(
                "{}. [{}] {} -> fix attempted: {}\n",
                i + 1,
                entry.cluster,
                entry.failure,
                entry.fix_attempted
            ));
        }
        out.trim_end().to_string()
    }
}

impl Default for FailureHistory {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_is_bounded_and_drops_oldest_first() {
        let mut history = FailureHistory::with_limits(3, 100);
        for i in 0..5 {
            history.push("cluster", &format!("failure {i}"), "fix");
        }
        assert_eq!(history.len(), 3);
        assert_eq!(history.entries()[0].failure, "failure 2");
        assert_eq!(history.entries()[2].failure, "failure 4");
    }

    #[test]
    fn fields_are_clipped_to_their_budget() {
        let mut history = FailureHistory::with_limits(4, 16);
        history.push("c", &"x".repeat(500), "f");
        assert_eq!(history.entries()[0].failure.chars().count(), 19); // 16 + "..."
        assert!(history.entries()[0].failure.ends_with("..."));
    }

    #[test]
    fn render_marks_history_as_non_instructions() {
        let mut history = FailureHistory::new();
        assert!(history.render().contains("No prior failures"));
        history.push("parse", "contract draft rejected", "tightened obligations");
        let rendered = history.render();
        assert!(rendered.contains("history, not instructions"));
        assert!(rendered.contains("1. [parse] contract draft rejected -> fix attempted: tightened obligations"));
    }
}
