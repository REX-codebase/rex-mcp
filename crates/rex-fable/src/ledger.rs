//! The epistemic ledger: every material claim recorded with its status.
//!
//! - **PROVEN**: supported by a file, command output, receipt, or source.
//!   Evidence is mandatory — a PROVEN claim without evidence is rejected.
//! - **HYPOTHESIS**: plausible but not verified. Must not carry evidence;
//!   evidence promotes it to PROVEN via a new item.
//! - **UNKNOWN**: a material gap that still needs a probe.
//!
//! The unlock rule is mechanical: at least 2 PROVEN items and at least 1
//! invariant. Relabeling an assumption as PROVEN without evidence fails
//! closed at the type level.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::FableError;

/// Confidence status of a single ledger item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EpistemicStatus {
    Proven,
    Hypothesis,
    Unknown,
}

impl std::fmt::Display for EpistemicStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            EpistemicStatus::Proven => "PROVEN",
            EpistemicStatus::Hypothesis => "HYPOTHESIS",
            EpistemicStatus::Unknown => "UNKNOWN",
        };
        f.write_str(s)
    }
}

/// One recorded claim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpistemicItem {
    pub id: String,
    pub status: EpistemicStatus,
    pub claim: String,
    /// Required for PROVEN, forbidden for HYPOTHESIS/UNKNOWN.
    pub evidence: Option<String>,
    pub logged_at_ms: u64,
}

/// A falsifiable invariant the change must preserve, e.g.
/// "Existing public CLI behavior remains compatible."
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Invariant {
    pub id: String,
    pub statement: String,
    /// How the invariant could be falsified (command, check, or artifact).
    pub falsifiable_check: String,
    pub recorded_at_ms: u64,
}

/// The ledger attached to a Fable session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EpistemicLedger {
    items: Vec<EpistemicItem>,
    invariants: Vec<Invariant>,
    #[serde(default)]
    next_item_seq: u32,
    #[serde(default)]
    next_invariant_seq: u32,
}

/// Unlock prerequisites from the Fable spec.
pub const REQUIRED_PROVEN: usize = 2;
pub const REQUIRED_INVARIANTS: usize = 1;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl EpistemicLedger {
    pub fn new() -> Self {
        EpistemicLedger::default()
    }

    /// Log a claim. PROVEN requires evidence; HYPOTHESIS and UNKNOWN reject
    /// it, so confidence can never be smuggled in through relabeling.
    pub fn log_item(
        &mut self,
        status: EpistemicStatus,
        claim: impl Into<String>,
        evidence: Option<String>,
    ) -> Result<String, FableError> {
        let claim = claim.into();
        if claim.trim().is_empty() {
            return Err(FableError::EmptyClaim);
        }
        let evidence = evidence.map(|e| e.trim().to_string()).filter(|e| !e.is_empty());
        match status {
            EpistemicStatus::Proven => {
                if evidence.is_none() {
                    return Err(FableError::EvidenceRequired {
                        claim: claim.clone(),
                    });
                }
            }
            EpistemicStatus::Hypothesis | EpistemicStatus::Unknown => {
                if evidence.is_some() {
                    return Err(FableError::EvidenceForbidden {
                        status: status.to_string(),
                    });
                }
            }
        }
        self.next_item_seq += 1;
        let id = format!("E-{:02}", self.next_item_seq);
        self.items.push(EpistemicItem {
            id: id.clone(),
            status,
            claim,
            evidence,
            logged_at_ms: now_ms(),
        });
        Ok(id)
    }

    /// Record a falsifiable invariant. The check describes how it could be
    /// falsified — an invariant without a check is a wish, not a gate.
    pub fn record_invariant(
        &mut self,
        statement: impl Into<String>,
        falsifiable_check: impl Into<String>,
    ) -> Result<String, FableError> {
        let statement = statement.into();
        let falsifiable_check = falsifiable_check.into();
        if statement.trim().is_empty() {
            return Err(FableError::EmptyClaim);
        }
        if falsifiable_check.trim().is_empty() {
            return Err(FableError::InvariantNeedsCheck);
        }
        self.next_invariant_seq += 1;
        let id = format!("INV-{:02}", self.next_invariant_seq);
        self.invariants.push(Invariant {
            id: id.clone(),
            statement,
            falsifiable_check,
            recorded_at_ms: now_ms(),
        });
        Ok(id)
    }

    pub fn items(&self) -> &[EpistemicItem] {
        &self.items
    }

    pub fn invariants(&self) -> &[Invariant] {
        &self.invariants
    }

    pub fn proven_count(&self) -> usize {
        self.items
            .iter()
            .filter(|i| i.status == EpistemicStatus::Proven)
            .count()
    }

    /// The mechanical unlock rule: 2 PROVEN items + 1 invariant.
    pub fn prerequisites_met(&self) -> bool {
        self.proven_count() >= REQUIRED_PROVEN && self.invariants.len() >= REQUIRED_INVARIANTS
    }

    /// Human-readable list of unmet prerequisites, for honest denial messages.
    pub fn unmet_prerequisites(&self) -> Vec<String> {
        let mut out = Vec::new();
        let proven = self.proven_count();
        if proven < REQUIRED_PROVEN {
            out.push(format!(
                "need {REQUIRED_PROVEN} PROVEN items, have {proven}"
            ));
        }
        if self.invariants.len() < REQUIRED_INVARIANTS {
            out.push(format!(
                "need {REQUIRED_INVARIANTS} invariant, have {}",
                self.invariants.len()
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proven_requires_evidence() {
        let mut l = EpistemicLedger::new();
        let err = l
            .log_item(EpistemicStatus::Proven, "auth is safe", None)
            .unwrap_err();
        assert!(matches!(err, FableError::EvidenceRequired { .. }));
    }

    #[test]
    fn hypothesis_rejects_evidence() {
        let mut l = EpistemicLedger::new();
        let err = l
            .log_item(
                EpistemicStatus::Hypothesis,
                "maybe racy",
                Some("looks racy".to_string()),
            )
            .unwrap_err();
        assert!(matches!(err, FableError::EvidenceForbidden { .. }));
    }

    #[test]
    fn unlock_rule_counts() {
        let mut l = EpistemicLedger::new();
        assert!(!l.prerequisites_met());
        l.log_item(
            EpistemicStatus::Proven,
            "cli exit codes unchanged",
            Some("smoke.sh output".to_string()),
        )
        .unwrap();
        assert!(!l.prerequisites_met());
        l.log_item(
            EpistemicStatus::Proven,
            "no new network calls",
            Some("strace log".to_string()),
        )
        .unwrap();
        assert!(!l.prerequisites_met());
        l.record_invariant("public CLI behavior compatible", "run smoke.sh")
            .unwrap();
        assert!(l.prerequisites_met());
        assert!(l.unmet_prerequisites().is_empty());
    }

    #[test]
    fn invariant_needs_falsifiable_check() {
        let mut l = EpistemicLedger::new();
        assert!(matches!(
            l.record_invariant("be safe", "   ").unwrap_err(),
            FableError::InvariantNeedsCheck
        ));
    }

    #[test]
    fn unmet_prerequisites_are_honest() {
        let l = EpistemicLedger::new();
        let unmet = l.unmet_prerequisites();
        assert_eq!(unmet.len(), 2);
    }
}
