//! Small custody-authoritative primitives for protocol 1.1 boundaries.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransactionState {
    Prepared,
    Committed,
    Aborted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransactionRecord {
    #[serde(default)]
    pub branch_id: BranchId,
    pub grant_id: String,
    pub idempotency_key: String,
    pub request_hash: String,
    pub nonce: u64,
    #[serde(default)]
    pub parent_revision: u64,
    #[serde(default)]
    pub revision: u64,
    pub state: TransactionState,
    pub outcome: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BranchId(String);

impl BranchId {
    pub fn new(value: impl Into<String>) -> Result<Self, JournalError> {
        let value = value.into();
        if value.trim().is_empty() || value.contains('\0') {
            return Err(JournalError::InvalidBranch);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str { &self.0 }
}

impl Default for BranchId {
    fn default() -> Self { Self("main".into()) }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BranchSpec {
    pub id: BranchId,
    pub parent: Option<BranchId>,
    pub base_revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MergeResult {
    pub source: BranchId,
    pub target: BranchId,
    pub imported: usize,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JournalError {
    Io(String),
    Encoding(String),
    Conflict,
    NonceReplay,
    NotPrepared,
    InvalidBranch,
    UnknownBranch,
    StaleBase { expected: u64, actual: u64 },
    UncommittedSource,
    MergeConflict,
    NonceConflict,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct JournalSnapshot {
    records: BTreeMap<String, TransactionRecord>,
    latest_nonce: BTreeMap<String, u64>,
    #[serde(default)]
    branches: BTreeMap<BranchId, BranchSpec>,
    #[serde(default)]
    branch_heads: BTreeMap<BranchId, u64>,
}

pub struct TransactionJournal {
    path: PathBuf,
    snapshot: JournalSnapshot,
}

impl TransactionJournal {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, JournalError> {
        let path = path.into();
        let mut snapshot = if path.exists() {
            serde_json::from_slice(&fs::read(&path).map_err(io_error)?)
                .map_err(|error| JournalError::Encoding(error.to_string()))?
        } else {
            JournalSnapshot::default()
        };
        let main = BranchId::default();
        snapshot.branches.entry(main.clone()).or_insert(BranchSpec {
            id: main.clone(), parent: None, base_revision: 0,
        });
        snapshot.branch_heads.entry(main).or_insert(0);
        Ok(Self { path, snapshot })
    }

    pub fn get(&self, idempotency_key: &str) -> Option<&TransactionRecord> {
        self.get_on_branch(&BranchId::default(), idempotency_key)
    }

    pub fn get_on_branch(&self, branch_id: &BranchId, idempotency_key: &str) -> Option<&TransactionRecord> {
        self.snapshot.records.get(&record_key(branch_id, idempotency_key))
    }

    pub fn create_branch(
        &mut self,
        id: BranchId,
        parent: BranchId,
        base_revision: u64,
    ) -> Result<BranchSpec, JournalError> {
        if self.snapshot.branches.contains_key(&id) || !self.snapshot.branches.contains_key(&parent) {
            return Err(JournalError::InvalidBranch);
        }
        let actual = self.branch_head(&parent)?;
        if base_revision > actual {
            return Err(JournalError::StaleBase { expected: base_revision, actual });
        }
        let spec = BranchSpec { id: id.clone(), parent: Some(parent), base_revision };
        self.snapshot.branches.insert(id.clone(), spec.clone());
        self.snapshot.branch_heads.insert(id, base_revision);
        self.persist()?;
        Ok(spec)
    }

    pub fn branch_head(&self, branch_id: &BranchId) -> Result<u64, JournalError> {
        self.snapshot.branch_heads.get(branch_id).copied().ok_or(JournalError::UnknownBranch)
    }

    pub fn prepare(
        &mut self,
        grant_id: &str,
        idempotency_key: &str,
        payload: &[u8],
        nonce: u64,
    ) -> Result<TransactionRecord, JournalError> {
        self.prepare_on_branch(&BranchId::default(), grant_id, idempotency_key, payload, nonce)
    }

    pub fn prepare_on_branch(
        &mut self,
        branch_id: &BranchId,
        grant_id: &str,
        idempotency_key: &str,
        payload: &[u8],
        nonce: u64,
    ) -> Result<TransactionRecord, JournalError> {
        self.branch_head(branch_id)?;
        let request_hash = hash(payload);
        let key = record_key(branch_id, idempotency_key);
        if let Some(record) = self.snapshot.records.get(&key) {
            if record.grant_id != grant_id || record.request_hash != request_hash {
                return Err(JournalError::Conflict);
            }
            return Ok(record.clone());
        }
        let nonce_key = nonce_key(branch_id, grant_id);
        if nonce <= self.snapshot.latest_nonce.get(&nonce_key).copied().unwrap_or(0) {
            return Err(JournalError::NonceReplay);
        }
        let parent_revision = self.branch_head(branch_id)?;
        let revision = parent_revision + 1;
        let record = TransactionRecord {
            branch_id: branch_id.clone(), grant_id: grant_id.into(), idempotency_key: idempotency_key.into(),
            request_hash, nonce, parent_revision, revision, state: TransactionState::Prepared, outcome: None,
        };
        self.snapshot.latest_nonce.insert(nonce_key, nonce);
        self.snapshot.branch_heads.insert(branch_id.clone(), revision);
        self.snapshot.records.insert(key, record.clone());
        self.persist()?;
        Ok(record)
    }

    pub fn commit(&mut self, idempotency_key: &str, outcome: &str) -> Result<TransactionRecord, JournalError> {
        self.commit_on_branch(&BranchId::default(), idempotency_key, outcome)
    }

    pub fn commit_on_branch(&mut self, branch_id: &BranchId, idempotency_key: &str, outcome: &str) -> Result<TransactionRecord, JournalError> {
        let key = record_key(branch_id, idempotency_key);
        let record = self.snapshot.records.get_mut(&key).ok_or(JournalError::NotPrepared)?;
        if record.state == TransactionState::Committed {
            return Ok(record.clone());
        }
        if record.state != TransactionState::Prepared {
            return Err(JournalError::NotPrepared);
        }
        record.state = TransactionState::Committed;
        record.outcome = Some(outcome.into());
        let result = record.clone();
        self.persist()?;
        Ok(result)
    }

    pub fn abort(&mut self, idempotency_key: &str, reason: &str) -> Result<TransactionRecord, JournalError> {
        self.abort_on_branch(&BranchId::default(), idempotency_key, reason)
    }

    pub fn abort_on_branch(&mut self, branch_id: &BranchId, idempotency_key: &str, reason: &str) -> Result<TransactionRecord, JournalError> {
        let key = record_key(branch_id, idempotency_key);
        let record = self.snapshot.records.get_mut(&key).ok_or(JournalError::NotPrepared)?;
        if record.state == TransactionState::Aborted {
            return Ok(record.clone());
        }
        if record.state != TransactionState::Prepared {
            return Err(JournalError::NotPrepared);
        }
        record.state = TransactionState::Aborted;
        record.outcome = Some(reason.into());
        let result = record.clone();
        self.persist()?;
        Ok(result)
    }

    pub fn merge(&mut self, target: &BranchId, source: &BranchId) -> Result<MergeResult, JournalError> {
        let source_spec = self.snapshot.branches.get(source).ok_or(JournalError::UnknownBranch)?.clone();
        let target_head = self.branch_head(target)?;
        if source_spec.parent.as_ref() != Some(target) {
            return Err(JournalError::MergeConflict);
        }
        if target_head != source_spec.base_revision {
            return Err(JournalError::StaleBase { expected: source_spec.base_revision, actual: target_head });
        }
        let source_records: Vec<TransactionRecord> = self.snapshot.records.values()
            .filter(|record| &record.branch_id == source)
            .cloned()
            .collect();
        if source_records.iter().any(|record| record.state != TransactionState::Committed) {
            return Err(JournalError::UncommittedSource);
        }
        let mut planned_head = target_head;
        let mut planned_nonces: BTreeMap<String, u64> = self.snapshot.latest_nonce.clone();
        let mut imports = Vec::new();
        for source_record in source_records {
            let target_key = record_key(target, &source_record.idempotency_key);
            if let Some(target_record) = self.snapshot.records.get(&target_key) {
                if target_record.request_hash != source_record.request_hash {
                    return Err(JournalError::MergeConflict);
                }
                continue;
            }
            let nonce_key = nonce_key(target, &source_record.grant_id);
            if planned_nonces.get(&nonce_key).copied().unwrap_or(0) >= source_record.nonce {
                return Err(JournalError::NonceConflict);
            }
            let revision = planned_head + 1;
            let mut imported_record = source_record;
            imported_record.branch_id = target.clone();
            imported_record.parent_revision = revision - 1;
            imported_record.revision = revision;
            planned_nonces.insert(nonce_key, imported_record.nonce);
            planned_head = revision;
            imports.push((target_key, imported_record));
        }
        for (target_key, imported_record) in imports {
            self.snapshot.latest_nonce.insert(nonce_key(target, &imported_record.grant_id), imported_record.nonce);
            self.snapshot.records.insert(target_key, imported_record);
        }
        let imported = self.snapshot.records.values().filter(|record| &record.branch_id == target && record.revision > target_head).count();
        self.snapshot.branch_heads.insert(target.clone(), planned_head);
        let revision = planned_head;
        self.persist()?;
        Ok(MergeResult { source: source.clone(), target: target.clone(), imported, revision })
    }

    fn persist(&self) -> Result<(), JournalError> {
        let temporary = self.path.with_extension("journal.tmp");
        let bytes = serde_json::to_vec(&self.snapshot)
            .map_err(|error| JournalError::Encoding(error.to_string()))?;
        fs::write(&temporary, bytes).map_err(io_error)?;
        fs::rename(&temporary, &self.path).map_err(io_error)
    }
}

fn record_key(branch_id: &BranchId, idempotency_key: &str) -> String {
    format!("{}\0{}", branch_id.as_str(), idempotency_key)
}

fn nonce_key(branch_id: &BranchId, grant_id: &str) -> String {
    format!("{}\0{}", branch_id.as_str(), grant_id)
}

fn io_error(error: std::io::Error) -> JournalError {
    JournalError::Io(error.to_string())
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
    use tempfile::tempdir;

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

    #[test]
    fn journal_replays_commits_and_rejects_conflicts() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("transactions.json");
        let mut journal = TransactionJournal::open(&path).unwrap();
        journal.prepare("grant", "request", b"payload", 1).unwrap();
        let committed = journal.commit("request", "accepted").unwrap();
        assert_eq!(journal.commit("request", "ignored").unwrap(), committed);
        assert_eq!(journal.prepare("grant", "request", b"changed", 1), Err(JournalError::Conflict));
    }

    #[test]
    fn journal_recovers_after_prepare_boundary_without_double_commit() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("transactions.json");
        {
            let mut journal = TransactionJournal::open(&path).unwrap();
            let prepared = journal.prepare("grant", "prepare", b"payload", 1).unwrap();
            assert_eq!(prepared.state, TransactionState::Prepared);
        }
        let mut recovered = TransactionJournal::open(&path).unwrap();
        assert_eq!(recovered.get("prepare").unwrap().state, TransactionState::Prepared);
        let committed = recovered.commit("prepare", "once").unwrap();
        let restarted = TransactionJournal::open(&path).unwrap();
        assert_eq!(restarted.get("prepare").unwrap(), &committed);
    }

    #[test]
    fn journal_recovers_at_commit_and_abort_boundaries() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("transactions.json");
        let mut journal = TransactionJournal::open(&path).unwrap();
        journal.prepare("grant", "commit", b"a", 1).unwrap();
        journal.commit("commit", "done").unwrap();
        journal.prepare("grant", "abort", b"b", 2).unwrap();
        journal.abort("abort", "cancelled").unwrap();

        let mut recovered = TransactionJournal::open(&path).unwrap();
        assert_eq!(recovered.get("commit").unwrap().state, TransactionState::Committed);
        assert_eq!(recovered.get("abort").unwrap().state, TransactionState::Aborted);
        assert_eq!(recovered.commit("commit", "different").unwrap().outcome.as_deref(), Some("done"));
        assert_eq!(recovered.commit("abort", "wrong"), Err(JournalError::NotPrepared));
        assert_eq!(recovered.prepare("grant", "new", b"c", 2), Err(JournalError::NonceReplay));
    }

    fn branch(name: &str) -> BranchId {
        BranchId::new(name).unwrap()
    }

    #[test]
    fn concurrent_branches_isolate_same_keys_and_nonces() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("transactions.json");
        let mut journal = TransactionJournal::open(&path).unwrap();
        let left = branch("left");
        let right = branch("right");
        journal.create_branch(left.clone(), BranchId::default(), 0).unwrap();
        journal.create_branch(right.clone(), BranchId::default(), 0).unwrap();
        journal.prepare_on_branch(&left, "grant", "same-key", b"left", 1).unwrap();
        journal.prepare_on_branch(&right, "grant", "same-key", b"right", 1).unwrap();
        journal.commit_on_branch(&left, "same-key", "left-done").unwrap();
        journal.commit_on_branch(&right, "same-key", "right-done").unwrap();
        assert_eq!(journal.get_on_branch(&left, "same-key").unwrap().outcome.as_deref(), Some("left-done"));
        assert_eq!(journal.get_on_branch(&right, "same-key").unwrap().outcome.as_deref(), Some("right-done"));
        journal.merge(&BranchId::default(), &left).unwrap();
        assert_eq!(journal.merge(&BranchId::default(), &right), Err(JournalError::StaleBase { expected: 0, actual: 1 }));
    }

    #[test]
    fn stale_base_is_rejected_deterministically() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("transactions.json");
        let mut journal = TransactionJournal::open(&path).unwrap();
        let feature = branch("feature");
        journal.create_branch(feature.clone(), BranchId::default(), 0).unwrap();
        journal.prepare("grant", "main-op", b"main", 1).unwrap();
        journal.commit("main-op", "done").unwrap();
        journal.prepare_on_branch(&feature, "grant", "feature-op", b"feature", 1).unwrap();
        journal.commit_on_branch(&feature, "feature-op", "done").unwrap();
        assert_eq!(journal.merge(&BranchId::default(), &feature), Err(JournalError::StaleBase { expected: 0, actual: 1 }));
    }

    #[test]
    fn conflicting_merge_rejects_different_payloads() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("transactions.json");
        let mut journal = TransactionJournal::open(&path).unwrap();
        journal.prepare("grant", "shared", b"target", 1).unwrap();
        journal.commit("shared", "target-done").unwrap();
        let feature = branch("conflict");
        let base = journal.branch_head(&BranchId::default()).unwrap();
        journal.create_branch(feature.clone(), BranchId::default(), base).unwrap();
        journal.prepare_on_branch(&feature, "grant", "shared", b"source", 1).unwrap();
        journal.commit_on_branch(&feature, "shared", "source-done").unwrap();
        assert_eq!(journal.merge(&BranchId::default(), &feature), Err(JournalError::MergeConflict));
    }

    #[test]
    fn restart_preserves_branch_isolation() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("transactions.json");
        let left = branch("restart-left");
        let right = branch("restart-right");
        {
            let mut journal = TransactionJournal::open(&path).unwrap();
            journal.create_branch(left.clone(), BranchId::default(), 0).unwrap();
            journal.create_branch(right.clone(), BranchId::default(), 0).unwrap();
            journal.prepare_on_branch(&left, "grant", "same", b"left", 1).unwrap();
            journal.prepare_on_branch(&right, "grant", "same", b"right", 1).unwrap();
        }
        let recovered = TransactionJournal::open(&path).unwrap();
        assert_eq!(recovered.get_on_branch(&left, "same").unwrap().request_hash, hash(b"left"));
        assert_eq!(recovered.get_on_branch(&right, "same").unwrap().request_hash, hash(b"right"));
        assert_eq!(recovered.get_on_branch(&left, "same").unwrap().nonce, recovered.get_on_branch(&right, "same").unwrap().nonce);
    }
}