//! Epistemic ledger: what the run knows, and how it knows it.
//!
//! Every fact the orchestrator relies on is recorded with a class. Only
//! Observed facts - each tied to a concrete evidence id - may discharge a
//! contract obligation. Inferred facts chain from observed ones and inform
//! the judge. Guesses are recorded so they are never mistaken for knowledge;
//! they unlock nothing.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum FactClass {
    /// Directly observed by the harness: a command exit code, a file hash, a
    /// snapshot state. Carries the evidence id that proves it.
    Observed { evidence_id: String },
    /// Drawn from observed facts by a model or a rule. Carries the fact ids
    /// it depends on.
    Inferred { from: Vec<String> },
    /// A model's unsupported claim. Recorded for honesty; discharges nothing.
    Guess,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Fact {
    pub id: String,
    pub statement: String,
    #[serde(flatten)]
    pub class: FactClass,
    pub recorded_ms: u128,
}

pub struct EpistemicLedger {
    path: PathBuf,
    facts: Vec<Fact>,
    seq: u64,
}

impl EpistemicLedger {
    pub fn open(dir: &Path) -> std::io::Result<Self> {
        fs::create_dir_all(dir)?;
        let path = dir.join("ledger.jsonl");
        let mut facts = Vec::new();
        let mut seq = 0u64;
        if let Ok(text) = fs::read_to_string(&path) {
            for line in text.lines() {
                if let Ok(fact) = serde_json::from_str::<Fact>(line) {
                    facts.push(fact);
                    seq += 1;
                }
            }
        }
        Ok(Self { path, facts, seq })
    }

    pub fn record(&mut self, statement: &str, class: FactClass) -> String {
        self.seq += 1;
        let fact = Fact {
            id: format!("fact-{:04}", self.seq),
            statement: statement.to_string(),
            class,
            recorded_ms: crate::now_ms(),
        };
        if let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = writeln!(file, "{}", serde_json::to_string(&fact).unwrap_or_default());
        }
        let id = fact.id.clone();
        self.facts.push(fact);
        id
    }

    pub fn facts(&self) -> &[Fact] {
        &self.facts
    }

    /// A fact id discharges an obligation only when it is Observed.
    pub fn is_observed(&self, fact_id: &str) -> bool {
        self.facts
            .iter()
            .any(|f| f.id == fact_id && matches!(f.class, FactClass::Observed { .. }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_observed_discharges() {
        let dir = tempfile::tempdir().unwrap();
        let mut ledger = EpistemicLedger::open(dir.path()).unwrap();
        let observed = ledger.record(
            "tests pass",
            FactClass::Observed {
                evidence_id: "ev-1".into(),
            },
        );
        let inferred = ledger.record(
            "therefore correct",
            FactClass::Inferred {
                from: vec![observed.clone()],
            },
        );
        let guess = ledger.record("probably fine", FactClass::Guess);
        assert!(ledger.is_observed(&observed));
        assert!(!ledger.is_observed(&inferred));
        assert!(!ledger.is_observed(&guess));
    }

    #[test]
    fn ledger_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let id = {
            let mut ledger = EpistemicLedger::open(dir.path()).unwrap();
            ledger.record("durable", FactClass::Guess)
        };
        let ledger = EpistemicLedger::open(dir.path()).unwrap();
        assert_eq!(ledger.facts().len(), 1);
        assert_eq!(ledger.facts()[0].id, id);
    }
}
