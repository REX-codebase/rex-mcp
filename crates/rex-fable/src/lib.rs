//! rex-fable: the native Fable gate.
//!
//! Fable Mode separates confidence from permission. A session moves through
//! four gates:
//!
//! ```text
//! THINK -> PROVE -> ATTACK -> WRITE
//! ```
//!
//! - **THINK**: inspect the repository, log epistemic items (PROVEN /
//!   HYPOTHESIS / UNKNOWN). No edits.
//! - **PROVE**: record falsifiable invariants, back claims with evidence.
//!   The gate to ATTACK opens only via `unlock_execution`, which requires
//!   the epistemic prerequisites (2 PROVEN items + 1 invariant) and the
//!   mechanical authority timer to have elapsed.
//! - **ATTACK**: challenge the changed behavior with adversarial review.
//! - **WRITE**: rerun checks, evaluate the rubric, write the completion
//!   record.
//!
//! This crate enforces the lifecycle. It does not replace the host's
//! permissions, sandbox, tests, or human approval rules.

pub mod error;
pub mod ledger;
pub mod session;

pub use error::FableError;
pub use ledger::{EpistemicItem, EpistemicLedger, EpistemicStatus, Invariant};
pub use session::{FablePhase, FableSession};
