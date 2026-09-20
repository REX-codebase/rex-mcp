//! Leases, heartbeats and epoch fencing.
//!
//! A lease is the *time-bounded* part of custody. Every resume or transfer
//! bumps the epoch and mints fresh secrets; anything presented from an older
//! epoch is proof of a stale process and seizes custody
//! (`Violation::StaleLeaseUse`). Heartbeats must arrive with the exact next
//! monotonic sequence number: replayed heartbeats are rejected without
//! extending the lease.

use serde::{Deserialize, Serialize};

/// Terms fixed in the offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseTerms {
    /// How long one lease lives without a heartbeat.
    pub lease_ms: u64,
    /// Expected heartbeat cadence. Informational for the operator; the
    /// hard boundary is `lease_ms`.
    pub heartbeat_interval_ms: u64,
    /// After suspension or lease lapse, how long a valid resume is
    /// accepted before custody releases as expired.
    pub resume_grace_ms: u64,
}

impl Default for LeaseTerms {
    fn default() -> Self {
        Self {
            lease_ms: 5 * 60 * 1000,
            heartbeat_interval_ms: 30 * 1000,
            resume_grace_ms: 15 * 60 * 1000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub lease_id: String,
    pub epoch: u64,
    pub issued_ms: u128,
    pub expires_ms: u128,
    pub heartbeat_interval_ms: u64,
    /// Next heartbeat sequence the registry will accept.
    pub next_seq: u64,
}

impl Lease {
    pub fn is_live(&self, now_ms: u128) -> bool {
        now_ms < self.expires_ms
    }
}
