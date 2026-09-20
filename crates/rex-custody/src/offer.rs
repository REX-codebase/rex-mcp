//! The commitment/acceptance handshake.
//!
//! When an agent says "I'll do this", that statement must bind to one exact
//! offer: the same task, scope, budgets, lease terms and completion
//! contract. Acceptance is `H(offer_hash || nonce || operator statement)`;
//! the registry recomputes it. An acceptance that matches any other offer,
//! an expired offer, or a task that already has custody is refused. The
//! handshake gives REX cryptographic certainty about *what* the operator
//! committed to, and gives the operator certainty REX cannot silently
//! change the deal afterwards (the hash would stop matching).

use crate::budget::CustodyBudgets;
use crate::capability::{hex_sha256, CapabilitySet};
use crate::contract::CompletionContract;
use crate::identity::{OperatorIdentity, WorkerMode};
use crate::lease::LeaseTerms;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustodyOffer {
    pub offer_id: String,
    /// Stable task identity chosen by the host (e.g. a hash of task text +
    /// workspace). One active custody per task id, ever, unless a human
    /// reopens a tombstoned task.
    pub task_id: String,
    pub task_summary: String,
    pub operator: OperatorIdentity,
    pub worker: WorkerMode,
    pub capabilities: CapabilitySet,
    pub budgets: CustodyBudgets,
    pub lease_terms: LeaseTerms,
    pub contract: CompletionContract,
    /// Random 128-bit single-use nonce; acceptance must echo it.
    pub nonce: String,
    pub created_ms: u128,
    pub expires_ms: u128,
}

impl CustodyOffer {
    /// Canonical hash of everything the operator is committing to. The
    /// nonce is included so hashes differ across re-offers of equal terms.
    pub fn offer_hash(&self) -> String {
        let canon = serde_json::to_vec(self).expect("offer serializes");
        hex_sha256(&canon)
    }

    /// The commitment string the operator must hash into its acceptance.
    pub fn commitment_statement(&self) -> String {
        format!(
            "I commit to complete task {} as {} within the granted scope, budgets and completion contract.",
            self.task_id,
            self.operator.label(),
        )
    }

    /// Expected acceptance digest for this offer.
    pub fn expected_acceptance(&self) -> String {
        let mut buf = self.offer_hash().into_bytes();
        buf.extend_from_slice(self.nonce.as_bytes());
        buf.extend_from_slice(self.commitment_statement().as_bytes());
        hex_sha256(&buf)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustodyAcceptance {
    pub offer_id: String,
    pub nonce_echo: String,
    pub commitment: String,
}
