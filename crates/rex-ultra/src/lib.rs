//! REX Ultra mode: contract-driven, adversarially verified agent runs.
//!
//! Simple runs a capable worker against completion gates. Ultra wraps the
//! same worker model in a verification institution: an acceptance contract
//! compiled before work, a deterministic verifier that re-executes every
//! executable proof fresh, an adversary world that tries to prove the result
//! wrong, a clean-room judge that never sees the builder's narrative, an
//! epistemic ledger separating observation from inference from guess, and a
//! proof-carrying completion bundle on disk. See docs/ultra-mode.md.

pub mod adversary;
pub mod bench;
pub mod contract;
pub mod evidence;
pub mod judge;
pub mod ledger;
pub mod orchestrator;
pub mod verify;

pub fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}
