//! The custody state machine: phases, terminal release reasons, violations.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CustodyPhase {
    /// Offer minted, waiting for the commitment handshake.
    Offered,
    /// Handshake verified; lease live; work may proceed.
    Active,
    /// Interrupted (crash, network loss, operator pause). No work may
    /// proceed; resume requires the per-epoch resume secret.
    Suspended,
    /// Completion claimed; evidence gates are being evaluated. No further
    /// tool calls are allowed in this phase.
    Verifying,
    /// Terminal: custody released for a stated reason.
    Released,
    /// Terminal: custody seized after a violation. Requires human review;
    /// the task tombstone still blocks re-execution until a human reopens.
    Quarantined,
    /// Terminal: the offer itself expired before acceptance.
    Expired,
}

impl CustodyPhase {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            CustodyPhase::Released | CustodyPhase::Quarantined | CustodyPhase::Expired
        )
    }
}

/// The exhaustive set of ways custody can end. There is no "the agent said
/// so" path: completion requires verified evidence gates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReleaseReason {
    /// Every evidence gate in the completion contract passed.
    VerifiedCompletion { summary: String },
    /// The operator itself declared it cannot complete the task.
    ExplicitFailure { summary: String },
    /// A granted budget ran out.
    BudgetExhausted { which: BudgetKind },
    /// Lease expired without a valid resume inside the grace window.
    LeaseExpired,
    /// A human pressed stop. Always available, never overridable.
    HumanStop,
    /// The operator cancelled its own custody.
    OperatorCancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetKind {
    Steps,
    ToolCalls,
    Tokens,
    WallTime,
}

/// Conduct that seizes custody immediately. Each variant names a specific
/// confused-deputy / replay / honesty failure mode the kernel prevents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Violation {
    /// Tool call outside the granted capability set.
    CapabilityEscape { detail: String },
    /// A token or heartbeat from a superseded lease epoch was presented
    /// (stale lease resurrection, e.g. an old process waking up).
    StaleLeaseUse {
        presented_epoch: u64,
        current_epoch: u64,
    },
    /// A second custody or acceptance was attempted for a task that
    /// already has one.
    DoubleExecution { task_id: String },
    /// A completion claim was made whose evidence gates failed, beyond the
    /// allowed claim attempts.
    FalseCompletion { attempts: u8 },
    /// A custodied operator attempted to create further custody.
    NestedCustody { task_id: String },
    /// Acceptance did not match the offer's nonce and commitment hash.
    CommitmentMismatch,
    /// A capability token whose secret the registry never minted was used.
    TokenForgery { grant_id: String },
}

use crate::budget::{Consumption, CustodyBudgets};
use crate::capability::CapabilitySet;
use crate::contract::CompletionContract;
use crate::identity::{OperatorIdentity, WorkerMode};
use crate::lease::{Lease, LeaseTerms};

/// One grant of custody: the whole deal, durably recorded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustodyGrant {
    pub grant_id: String,
    pub offer_id: String,
    pub task_id: String,
    pub task_summary: String,
    pub operator: OperatorIdentity,
    pub worker: WorkerMode,
    pub capabilities: CapabilitySet,
    pub budgets: CustodyBudgets,
    pub consumed: Consumption,
    pub lease_terms: LeaseTerms,
    pub lease: Lease,
    pub contract: CompletionContract,
    pub phase: CustodyPhase,
    pub claim_attempts: u8,
    /// Per-epoch secret the recovered worker must present to resume.
    /// Rotated on every resume alongside the epoch bump.
    pub resume_secret: String,
    /// Secret half of the live capability token.
    pub token_secret: String,
    pub created_ms: u128,
    pub updated_ms: u128,
    /// When the current suspension began (for grace accounting).
    pub suspended_at_ms: Option<u128>,
    pub release: Option<ReleaseReason>,
    pub violation: Option<Violation>,
}

impl CustodyGrant {
    pub fn is_live(&self, now_ms: u128) -> bool {
        matches!(self.phase, CustodyPhase::Active) && self.lease.is_live(now_ms)
    }
}
