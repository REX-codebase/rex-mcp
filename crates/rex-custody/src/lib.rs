//! rex-custody: task-scoped custody for REX's agent-operator mode.
//!
//! When an operator says "I'll do this task", REX takes custody of the task
//! as an institution: it mints a grant that fixes the operator's identity
//! and role, the worker (a REX-managed model loop, or the external agent
//! itself entering through a declared protocol boundary), the exact
//! capability set and workspace, budgets, lease terms and the completion
//! contract. Custody ends only on verified completion, explicit failure,
//! budget or lease expiry, human stop, or operator cancel - and is seized
//! into quarantine on violations.
//!
//! Structural guarantees:
//! - one custody per task, tombstoned at release (no duplicate execution)
//! - epoch-fenced leases with rotated secrets (no stale lease resurrection)
//! - unforgeable capability tokens checked on every tool call (no confused
//!   deputy, no privilege widening)
//! - no delegation capability exists (no recursive agent loops)
//! - completion is decided by evidence gates, never by the operator's claim
//!   (no false completion)
//! - every transition lands in a hash-chained audit log
//!
//! See `docs/agent-operator-mode.md` for the full architecture.

pub mod audit;
pub mod budget;
pub mod capability;
pub mod contract;
pub mod identity;
pub mod lease;
pub mod offer;
pub mod registry;
pub mod state;
pub mod tools;

pub use budget::{Consumption, CustodyBudgets};
pub use capability::{CapabilityDenial, CapabilitySet, CapabilityToken, ToolClass};
pub use contract::{CompletionClaim, CompletionContract, EvidenceGate, GateEvaluator, GateOutcome};
pub use identity::{AgentIdentity, AgentProtocol, OperatorIdentity, WorkerMode};
pub use lease::{Lease, LeaseTerms};
pub use offer::{CustodyAcceptance, CustodyOffer};
pub use registry::{CustodyError, CustodyRegistry, Tombstone};
pub use state::{BudgetKind, CustodyGrant, CustodyPhase, ReleaseReason, Violation};
pub use tools::{CustodiedToolRuntime, CustodyToolError};
