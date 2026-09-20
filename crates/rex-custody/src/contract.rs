//! Completion contracts and evidence gates.
//!
//! "Done" is not the operator's word. The offer carries a completion
//! contract: the gates that must independently pass before custody releases
//! as verified. The registry owns the *decision procedure*; host code
//! supplies the *evidence* through `GateEvaluator` (running tests, diffing
//! the workspace, confirming no pending approvals). Failed claims consume
//! attempts; exhausting them is `Violation::FalseCompletion`.

use crate::state::CustodyGrant;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvidenceGate {
    /// No tool call is still waiting on a trusted approval.
    NoPendingApprovals,
    /// The host's check/test command passes in the workspace.
    ChecksPass,
    /// Every changed file lies inside the granted scope.
    WithinScopeChanges,
    /// A human explicitly confirmed the result.
    HumanConfirmation,
    /// A host-defined named gate (e.g. "preview_renders").
    Custom { name: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionContract {
    pub gates: Vec<EvidenceGate>,
    /// How many rejected claims are tolerated before false completion.
    pub max_claim_attempts: u8,
}

impl Default for CompletionContract {
    fn default() -> Self {
        Self {
            gates: vec![
                EvidenceGate::NoPendingApprovals,
                EvidenceGate::WithinScopeChanges,
            ],
            max_claim_attempts: 2,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionClaim {
    pub summary: String,
}

pub enum GateOutcome {
    Passed,
    Failed(String),
}

/// Host-supplied evidence. Implemented by the runtime integration (which
/// can see tool receipts, diffs and test results), never by the operator.
pub trait GateEvaluator {
    fn evaluate(&self, gate: &EvidenceGate, grant: &CustodyGrant) -> GateOutcome;
}
