//! Deterministic state machine for an external host. There is no provider,
//! prompt, or managed-inference fallback in this module.

use rex_protocol::schema::canonical_hash;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KernelState {
    New,
    Leased,
    Running,
    AwaitingEvidence,
    Completed,
    Failed,
    Revoked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KernelEvent {
    Acquire { lease_epoch: u64 },
    Begin { action_id: String },
    SubmitEvidence { digest: String },
    Complete,
    Fail { reason: String },
    Revoke,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelSnapshot {
    pub task_id: String,
    pub state: KernelState,
    pub step: u64,
    pub lease_epoch: u64,
    pub transcript_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KernelError {
    InvalidTransition { state: KernelState, event: String },
    EmptyValue,
    NonMonotonicLease,
}

pub fn transition(mut snapshot: KernelSnapshot, event: KernelEvent) -> Result<KernelSnapshot, KernelError> {
    let next = match (&snapshot.state, &event) {
        (KernelState::New, KernelEvent::Acquire { lease_epoch: 1 }) => KernelState::Leased,
        (KernelState::Leased, KernelEvent::Begin { action_id }) if !action_id.trim().is_empty() => KernelState::Running,
        (KernelState::Running, KernelEvent::SubmitEvidence { digest }) if !digest.trim().is_empty() => KernelState::AwaitingEvidence,
        (KernelState::AwaitingEvidence, KernelEvent::Complete) => KernelState::Completed,
        (KernelState::Leased | KernelState::Running | KernelState::AwaitingEvidence, KernelEvent::Fail { reason }) if !reason.trim().is_empty() => KernelState::Failed,
        (KernelState::New | KernelState::Leased | KernelState::Running | KernelState::AwaitingEvidence, KernelEvent::Revoke) => KernelState::Revoked,
        (KernelState::New, KernelEvent::Acquire { .. }) => return Err(KernelError::NonMonotonicLease),
        _ => return Err(KernelError::InvalidTransition { state: snapshot.state, event: format!("{event:?}") }),
    };
    if let KernelEvent::Acquire { lease_epoch } = event {
        snapshot.lease_epoch = lease_epoch;
    }
    snapshot.step = snapshot.step.saturating_add(1);
    snapshot.state = next;
    snapshot.transcript_hash = canonical_hash(&(&snapshot.step, &event, &snapshot.transcript_hash))
        .map_err(|_| KernelError::EmptyValue)?;
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn initial() -> KernelSnapshot {
        KernelSnapshot { task_id: "task".into(), state: KernelState::New, step: 0, lease_epoch: 0, transcript_hash: "genesis".into() }
    }

    #[test]
    fn external_host_sequence_is_deterministic() {
        let events = [
            KernelEvent::Acquire { lease_epoch: 1 },
            KernelEvent::Begin { action_id: "a".into() },
            KernelEvent::SubmitEvidence { digest: "evidence".into() },
            KernelEvent::Complete,
        ];
        let left = events.iter().cloned().try_fold(initial(), transition).unwrap();
        let right = events.iter().cloned().try_fold(initial(), transition).unwrap();
        assert_eq!(left, right);
        assert_eq!(left.state, KernelState::Completed);
    }

    #[test]
    fn managed_fallback_and_illegal_order_are_impossible() {
        assert!(transition(initial(), KernelEvent::Complete).is_err());
        assert!(transition(initial(), KernelEvent::Acquire { lease_epoch: 2 }).is_err());
    }
}