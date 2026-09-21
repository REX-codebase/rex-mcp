//! Small custody-authoritative primitives for protocol 1.1 boundaries.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResumeToken {
    pub grant_id: String,
    pub epoch: u64,
    pub nonce: u64,
    pub authenticator: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LeaseGrant {
    pub token: ResumeToken,
    pub expires_at_ms: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IdempotencyRecord {
    pub request_hash: String,
    pub result: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AuthoritySnapshot {
    leases: BTreeMap<String, LeaseGrant>,
    revoked: BTreeSet<String>,
    idempotency: BTreeMap<String, IdempotencyRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorityError {
    InvalidToken,
    UnknownGrant,
    Revoked,
    Expired,
    NonceReplay,
    IdempotencyConflict,
}

pub struct LeaseAuthority {
    secret: String,
    snapshot: AuthoritySnapshot,
}

impl LeaseAuthority {
    pub fn new(secret: impl Into<String>) -> Self {
        Self { secret: secret.into(), snapshot: AuthoritySnapshot::default() }
    }

    pub fn from_snapshot(secret: impl Into<String>, snapshot: AuthoritySnapshot) -> Self {
        Self { secret: secret.into(), snapshot }
    }

    pub fn snapshot(&self) -> AuthoritySnapshot { self.snapshot.clone() }

    pub fn acquire(&mut self, grant_id: &str, now_ms: u128, ttl_ms: u128) -> Result<LeaseGrant, AuthorityError> {
        if self.snapshot.revoked.contains(grant_id) { return Err(AuthorityError::Revoked); }
        let grant = LeaseGrant {
            token: self.token(grant_id, 1, 1),
            expires_at_ms: now_ms.saturating_add(ttl_ms),
        };
        self.snapshot.leases.insert(grant_id.into(), grant.clone());
        Ok(grant)
    }

    pub fn renew(&mut self, token: &ResumeToken, now_ms: u128, ttl_ms: u128) -> Result<LeaseGrant, AuthorityError> {
        let current = self.authenticate(token, now_ms)?;
        if token.nonce < current.token.nonce { return Err(AuthorityError::NonceReplay); }
        let grant = LeaseGrant {
            token: self.token(&token.grant_id, current.token.epoch + 1, token.nonce + 1),
            expires_at_ms: now_ms.saturating_add(ttl_ms),
        };
        self.snapshot.leases.insert(token.grant_id.clone(), grant.clone());
        Ok(grant)
    }

    pub fn revoke(&mut self, token: &ResumeToken, now_ms: u128) -> Result<(), AuthorityError> {
        self.authenticate(token, now_ms)?;
        self.snapshot.revoked.insert(token.grant_id.clone());
        self.snapshot.leases.remove(&token.grant_id);
        Ok(())
    }

    pub fn authenticate(&self, token: &ResumeToken, now_ms: u128) -> Result<&LeaseGrant, AuthorityError> {
        if self.snapshot.revoked.contains(&token.grant_id) { return Err(AuthorityError::Revoked); }
        let grant = self.snapshot.leases.get(&token.grant_id).ok_or(AuthorityError::UnknownGrant)?;
        if grant.expires_at_ms <= now_ms { return Err(AuthorityError::Expired); }
        if token != &grant.token || token.authenticator != self.authenticator(&token.grant_id, token.epoch, token.nonce) {
            return Err(AuthorityError::InvalidToken);
        }
        Ok(grant)
    }

    /// Returns the existing result for an exact replay and rejects a reused
    /// key with different input before any side effect can run.
    pub fn exactly_once(&mut self, key: &str, payload: &[u8], result: &str) -> Result<String, AuthorityError> {
        let request_hash = hash(payload);
        if let Some(record) = self.snapshot.idempotency.get(key) {
            if record.request_hash != request_hash { return Err(AuthorityError::IdempotencyConflict); }
            return Ok(record.result.clone());
        }
        let record = IdempotencyRecord { request_hash, result: result.into() };
        self.snapshot.idempotency.insert(key.into(), record.clone());
        Ok(record.result)
    }

    fn token(&self, grant_id: &str, epoch: u64, nonce: u64) -> ResumeToken {
        ResumeToken { grant_id: grant_id.into(), epoch, nonce, authenticator: self.authenticator(grant_id, epoch, nonce) }
    }

    fn authenticator(&self, grant_id: &str, epoch: u64, nonce: u64) -> String {
        hash(format!("{}\0{}\0{}\0{}", self.secret, grant_id, epoch, nonce).as_bytes())
    }
}

fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forged_and_replayed_tokens_fail() {
        let mut authority = LeaseAuthority::new("secret");
        let grant = authority.acquire("task", 10, 100).unwrap();
        let mut forged = grant.token.clone();
        forged.authenticator.replace_range(..2, "00");
        assert_eq!(authority.authenticate(&forged, 11), Err(AuthorityError::InvalidToken));
        let renewed = authority.renew(&grant.token, 11, 100).unwrap();
        assert_eq!(authority.renew(&grant.token, 12, 100), Err(AuthorityError::InvalidToken));
        assert!(renewed.token.nonce > grant.token.nonce);
    }

    #[test]
    fn restart_preserves_revocation_and_idempotency() {
        let mut authority = LeaseAuthority::new("secret");
        let grant = authority.acquire("task", 10, 100).unwrap();
        assert_eq!(authority.exactly_once("k", b"input", "done").unwrap(), "done");
        let snapshot = authority.snapshot();
        let mut recovered = LeaseAuthority::from_snapshot("secret", snapshot);
        assert_eq!(recovered.exactly_once("k", b"input", "other").unwrap(), "done");
        assert_eq!(recovered.exactly_once("k", b"changed", "other"), Err(AuthorityError::IdempotencyConflict));
        recovered.revoke(&grant.token, 11).unwrap();
        assert_eq!(recovered.authenticate(&grant.token, 12), Err(AuthorityError::Revoked));
    }
}