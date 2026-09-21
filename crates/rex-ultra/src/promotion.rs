//! Evidence-gated promotion and verified rollback (architecture section J).
//!
//! Sequence: require a completed kernel with a recorded qualified candidate,
//! revalidate the winning sealed bundle, snapshot the destination, stage the
//! bundle in a scratch workspace, rerun REX-checkable gates on the stage,
//! write a prepare receipt, apply only while the destination still equals
//! the captured hash, validate the resulting tree, then commit. Any failure
//! after staging restores the snapshot and verifies the restore; an
//! unverifiable rollback becomes CorruptState, never best-effort state.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::contract::{AcceptanceContract, Proof};
use crate::external_kernel::{ExternalHostAdapter, KernelState};
use rex_protocol::schema::canonical_hash;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromotionState {
    Prepared,
    Committed,
    RolledBack,
    CorruptState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromotionReceipt {
    pub task_id: String,
    pub candidate_id: String,
    pub bundle_hash: String,
    pub destination_hash_before: String,
    pub staging_hash: String,
    /// Obligation ids whose REX-checkable proofs passed on the stage.
    pub gates_rerun: Vec<String>,
    /// Obligation ids whose proofs need a host or custody grant; recorded,
    /// never silently treated as re-proven.
    pub gates_not_rerun: Vec<String>,
    pub state: PromotionState,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromotionError {
    KernelNotCompleted,
    NoQualifiedCandidate,
    InvalidBundle(String),
    DestinationChanged,
    GateFailure(Vec<String>),
    Io(String),
    Encoding(String),
}

/// One full-file change in a sealed candidate bundle. Patches are full
/// contents, not diffs: application is deterministic and idempotent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BundleFile {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SealedCandidateBundle {
    pub files: Vec<BundleFile>,
}

const MAX_BUNDLE_FILES: usize = 64;
const MAX_FILE_BYTES: usize = 256 * 1024;
const MAX_TREE_ENTRIES: usize = 100_000;
const MAX_SNAPSHOT_BYTES: u64 = 64 * 1024 * 1024;

fn validate_bundle_path(path: &str) -> Result<(), PromotionError> {
    let invalid = |reason: &str| PromotionError::InvalidBundle(format!("path {path:?}: {reason}"));
    if path.is_empty() {
        return Err(invalid("empty"));
    }
    if path.starts_with('/') || path.starts_with('\\') {
        return Err(invalid("absolute"));
    }
    if path.contains('\\') {
        return Err(invalid("backslash separator"));
    }
    if path
        .split('/')
        .any(|part| part == ".." || part == "." || part.is_empty())
    {
        return Err(invalid("traversal or empty segment"));
    }
    Ok(())
}

/// Parse and validate the winning candidate's sealed bundle. Anything that
/// cannot be applied deterministically and confined is rejected here.
pub fn parse_bundle(content: &str) -> Result<SealedCandidateBundle, PromotionError> {
    let bundle: SealedCandidateBundle = serde_json::from_str(content)
        .map_err(|e| PromotionError::InvalidBundle(format!("not a sealed bundle: {e}")))?;
    if bundle.files.is_empty() {
        return Err(PromotionError::InvalidBundle("bundle has no files".into()));
    }
    if bundle.files.len() > MAX_BUNDLE_FILES {
        return Err(PromotionError::InvalidBundle(format!(
            "{} files exceeds {MAX_BUNDLE_FILES}",
            bundle.files.len()
        )));
    }
    let mut seen = std::collections::BTreeSet::new();
    for file in &bundle.files {
        validate_bundle_path(&file.path)?;
        if !seen.insert(file.path.clone()) {
            return Err(PromotionError::InvalidBundle(format!(
                "duplicate path {:?}",
                file.path
            )));
        }
        if file.content.len() > MAX_FILE_BYTES {
            return Err(PromotionError::InvalidBundle(format!(
                "path {:?} exceeds {MAX_FILE_BYTES} bytes",
                file.path
            )));
        }
    }
    Ok(bundle)
}

/// Deterministic recursive tree hash over sorted (path, content-hash) pairs.
/// Errors honestly when the tree exceeds the entry bound.
pub fn tree_hash(root: &Path) -> Result<String, PromotionError> {
    let mut entries: BTreeMap<String, String> = BTreeMap::new();
    let mut total_bytes: u64 = 0;
    collect_tree(root, root, &mut entries, &mut total_bytes)?;
    canonical_hash(&entries).map_err(|e| PromotionError::Encoding(e.to_string()))
}

fn collect_tree(
    root: &Path,
    dir: &Path,
    entries: &mut BTreeMap<String, String>,
    total_bytes: &mut u64,
) -> Result<(), PromotionError> {
    if entries.len() >= MAX_TREE_ENTRIES {
        return Err(PromotionError::Io(format!(
            "tree exceeds {MAX_TREE_ENTRIES} entries"
        )));
    }
    let read = fs::read_dir(dir).map_err(|e| PromotionError::Io(e.to_string()))?;
    for entry in read.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            collect_tree(root, &path, entries, total_bytes)?;
        } else if kind.is_file() {
            let bytes = fs::read(&path).map_err(|e| PromotionError::Io(e.to_string()))?;
            *total_bytes += bytes.len() as u64;
            if *total_bytes > MAX_SNAPSHOT_BYTES {
                return Err(PromotionError::Io(format!(
                    "tree exceeds {MAX_SNAPSHOT_BYTES} bytes"
                )));
            }
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            entries.insert(
                relative,
                canonical_hash(&bytes).map_err(|e| PromotionError::Encoding(e.to_string()))?,
            );
        }
    }
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), PromotionError> {
    fs::create_dir_all(destination).map_err(|e| PromotionError::Io(e.to_string()))?;
    let read = fs::read_dir(source).map_err(|e| PromotionError::Io(e.to_string()))?;
    for entry in read.flatten() {
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            copy_tree(&from, &to)?;
        } else if kind.is_file() {
            fs::copy(&from, &to).map_err(|e| PromotionError::Io(e.to_string()))?;
        }
    }
    Ok(())
}

fn clear_tree(dir: &Path) -> Result<(), PromotionError> {
    if !dir.exists() {
        return Ok(());
    }
    let read = fs::read_dir(dir).map_err(|e| PromotionError::Io(e.to_string()))?;
    for entry in read.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() && !kind.is_symlink() {
            fs::remove_dir_all(&path).map_err(|e| PromotionError::Io(e.to_string()))?;
        } else {
            fs::remove_file(&path).map_err(|e| PromotionError::Io(e.to_string()))?;
        }
    }
    Ok(())
}

fn apply_bundle(bundle: &SealedCandidateBundle, root: &Path) -> Result<(), PromotionError> {
    for file in &bundle.files {
        let target = root.join(&file.path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| PromotionError::Io(e.to_string()))?;
        }
        fs::write(&target, &file.content).map_err(|e| PromotionError::Io(e.to_string()))?;
    }
    Ok(())
}

/// REX-checkable gates re-run on the stage: file proofs are deterministic
/// here; command and behavior proofs need a host or custody grant and are
/// recorded as not re-run.
fn rerun_gates(
    contract: &AcceptanceContract,
    stage: &Path,
) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut rerun = Vec::new();
    let mut not_rerun = Vec::new();
    let mut failed = Vec::new();
    for obligation in &contract.obligations {
        match &obligation.proof {
            Proof::FileExists { path } => {
                let ok = fs::metadata(stage.join(path))
                    .map(|m| m.is_file() && m.len() > 0)
                    .unwrap_or(false);
                if ok {
                    rerun.push(obligation.id.clone())
                } else {
                    failed.push(obligation.id.clone())
                }
            }
            Proof::FileContains { path, needle } => {
                let ok = fs::read_to_string(stage.join(path))
                    .map(|c| c.contains(needle))
                    .unwrap_or(false);
                if ok {
                    rerun.push(obligation.id.clone())
                } else {
                    failed.push(obligation.id.clone())
                }
            }
            _ => not_rerun.push(obligation.id.clone()),
        }
    }
    (rerun, not_rerun, failed)
}

pub struct PromotionStore {
    dir: PathBuf,
}

impl PromotionStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, PromotionError> {
        let dir = root.into().join("promotion");
        fs::create_dir_all(&dir).map_err(|e| PromotionError::Io(e.to_string()))?;
        Ok(Self { dir })
    }

    pub fn receipt(&self, task_id: &str) -> Result<Option<PromotionReceipt>, PromotionError> {
        let path = self.receipt_path(task_id)?;
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(&path).map_err(|e| PromotionError::Io(e.to_string()))?;
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| PromotionError::Encoding(e.to_string()))
    }

    fn receipt_path(&self, task_id: &str) -> Result<PathBuf, PromotionError> {
        if task_id.is_empty()
            || !task_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err(PromotionError::InvalidBundle(
                "invalid task id for promotion store".into(),
            ));
        }
        Ok(self.dir.join(format!("receipt-{task_id}.json")))
    }

    fn persist_receipt(&self, receipt: &PromotionReceipt) -> Result<(), PromotionError> {
        let path = self.receipt_path(&receipt.task_id)?;
        let temporary = path.with_extension("json.tmp");
        let bytes =
            serde_json::to_vec(receipt).map_err(|e| PromotionError::Encoding(e.to_string()))?;
        fs::write(&temporary, bytes).map_err(|e| PromotionError::Io(e.to_string()))?;
        fs::rename(&temporary, &path).map_err(|e| PromotionError::Io(e.to_string()))
    }

    /// Full promotion sequence for one task. Durable receipts mark each
    /// boundary; a failure after staging restores and verifies the snapshot,
    /// and an unverifiable restore is CorruptState, never a guess.
    pub fn promote(
        &self,
        task_id: &str,
        adapter: &ExternalHostAdapter,
        contract: &AcceptanceContract,
        destination: &Path,
    ) -> Result<PromotionReceipt, PromotionError> {
        if adapter.kernel().state != KernelState::Completed {
            return Err(PromotionError::KernelNotCompleted);
        }
        let candidate_id = adapter
            .qualified_candidate()
            .ok_or(PromotionError::NoQualifiedCandidate)?
            .to_string();
        let response = adapter
            .responses()
            .find(|r| r.candidate_id == candidate_id)
            .ok_or(PromotionError::NoQualifiedCandidate)?;
        let bundle = parse_bundle(&response.content)?;
        let bundle_hash = canonical_hash(&response.content)
            .map_err(|e| PromotionError::Encoding(e.to_string()))?;

        // Recover a dangling prepare from a crashed promotion before
        // snapshotting again: a dirty destination is never a new base.
        // Unverifiable recovery is CorruptState and refuses to promote.
        if let Some(prior) = self.receipt(task_id)? {
            if prior.state == PromotionState::Prepared {
                let snapshot = self.dir.join(format!("snapshot-{task_id}"));
                clear_tree(destination)
                    .and_then(|()| copy_tree(&snapshot, destination))
                    .ok();
                let mut recovered = prior;
                match tree_hash(destination) {
                    Ok(restored) if restored == recovered.destination_hash_before => {
                        recovered.state = PromotionState::RolledBack;
                        recovered.detail = "recovered a crashed promotion; restore verified".into();
                    }
                    _ => {
                        recovered.state = PromotionState::CorruptState;
                        recovered.detail = "crashed promotion recovery is unverifiable".into();
                    }
                }
                let corrupt = recovered.state == PromotionState::CorruptState;
                self.persist_receipt(&recovered)?;
                if corrupt {
                    return Ok(recovered);
                }
            }
        }

        // Snapshot the destination before any mutation.
        let destination_hash_before = tree_hash(destination)?;
        let snapshot = self.dir.join(format!("snapshot-{task_id}"));
        if snapshot.exists() {
            fs::remove_dir_all(&snapshot).map_err(|e| PromotionError::Io(e.to_string()))?;
        }
        copy_tree(destination, &snapshot)?;

        // Stage the bundle on a private copy and rerun REX-checkable gates.
        let stage = self.dir.join(format!("stage-{task_id}"));
        if stage.exists() {
            fs::remove_dir_all(&stage).map_err(|e| PromotionError::Io(e.to_string()))?;
        }
        copy_tree(destination, &stage)?;

        let finish = |receipt: PromotionReceipt| -> Result<PromotionReceipt, PromotionError> {
            self.persist_receipt(&receipt)?;
            Ok(receipt)
        };
        let mut receipt = PromotionReceipt {
            task_id: task_id.to_string(),
            candidate_id: candidate_id.clone(),
            bundle_hash,
            destination_hash_before: destination_hash_before.clone(),
            staging_hash: String::new(),
            gates_rerun: Vec::new(),
            gates_not_rerun: Vec::new(),
            state: PromotionState::Prepared,
            detail: "staged".into(),
        };

        let applied = apply_bundle(&bundle, &stage);
        let outcome: Result<(), PromotionError> = (|| {
            applied?;
            let (rerun, not_rerun, failed) = rerun_gates(contract, &stage);
            receipt.gates_rerun = rerun;
            receipt.gates_not_rerun = not_rerun;
            if !failed.is_empty() {
                return Err(PromotionError::GateFailure(failed));
            }
            receipt.staging_hash = tree_hash(&stage)?;
            // A prepare receipt exists before any destination mutation.
            self.persist_receipt(&receipt)?;
            // Compare-and-swap: the destination must not have moved.
            if tree_hash(destination)? != destination_hash_before {
                return Err(PromotionError::DestinationChanged);
            }
            apply_bundle(&bundle, destination)?;
            if tree_hash(destination)? != receipt.staging_hash {
                return Err(PromotionError::Io(
                    "destination hash mismatch after apply".into(),
                ));
            }
            receipt.state = PromotionState::Committed;
            receipt.detail = "promoted and destination hash verified".into();
            Ok(())
        })();

        match outcome {
            Ok(()) => finish(receipt),
            Err(error) => {
                // Roll back: restore the snapshot and verify the restore.
                clear_tree(destination)
                    .and_then(|()| copy_tree(&snapshot, destination))
                    .ok();
                match tree_hash(destination) {
                    Ok(restored) if restored == destination_hash_before => {
                        receipt.state = PromotionState::RolledBack;
                        receipt.detail = format!("rolled back and verified after: {error:?}");
                        finish(receipt)
                    }
                    _ => {
                        receipt.state = PromotionState::CorruptState;
                        receipt.detail = format!("rollback unverifiable after: {error:?}");
                        finish(receipt)
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{AcceptanceContract, Obligation, Proof};
    use crate::external_kernel::{
        AdversaryEvidence, CandidateResponse, EvidenceKind, VerifierEvidence,
    };
    use tempfile::tempdir;

    fn contract() -> AcceptanceContract {
        AcceptanceContract {
            task: "produce the result".into(),
            work_kind: crate::contract::WorkKind::General,
            obligations: vec![Obligation {
                id: "o1".into(),
                statement: "result exists and passes".into(),
                proof: Proof::FileContains {
                    path: "result.txt".into(),
                    needle: "PASS".into(),
                },
            }],
            forbidden_regressions: vec![],
        }
    }

    fn bundle(files: &[(&str, &str)]) -> String {
        serde_json::json!({
            "files": files.iter().map(|(path, content)| serde_json::json!({"path":path,"content":content})).collect::<Vec<_>>()
        })
        .to_string()
    }

    fn completed_adapter(contract: &AcceptanceContract, content: &str) -> ExternalHostAdapter {
        let mut adapter = ExternalHostAdapter::new(contract.clone(), 1).unwrap();
        adapter.attach_host(1).unwrap();
        let request = adapter.requests().unwrap().remove(0);
        adapter
            .record_response(CandidateResponse {
                candidate_id: request.candidate_id.clone(),
                response_hash: canonical_hash(&content).unwrap(),
                content: content.to_string(),
            })
            .unwrap();
        adapter.advance().unwrap();
        let requests = adapter.evidence_requests().unwrap();
        let adversary = requests
            .iter()
            .find(|r| r.kind == EvidenceKind::Adversary)
            .unwrap()
            .clone();
        adapter
            .record_adversary(AdversaryEvidence {
                request_id: adversary.request_id,
                candidate_id: request.candidate_id.clone(),
                response_hash: canonical_hash(&"{\"defects\":[]}").unwrap(),
                content: "{\"defects\":[]}".into(),
            })
            .unwrap();
        let verifier = requests
            .iter()
            .find(|r| r.kind == EvidenceKind::Verifier)
            .unwrap()
            .clone();
        let verdict = "{\"outcomes\":[{\"obligation_id\":\"o1\",\"status\":\"proven\"}]}";
        adapter
            .record_verifier(VerifierEvidence {
                request_id: verifier.request_id,
                candidate_id: request.candidate_id.clone(),
                response_hash: canonical_hash(&verdict).unwrap(),
                content: verdict.into(),
            })
            .unwrap();
        assert!(matches!(
            adapter.finalize().unwrap(),
            KernelState::Completed
        ));
        adapter
    }

    #[test]
    fn promotion_commits_and_verifies_the_destination_hash() {
        let directory = tempdir().unwrap();
        let destination = directory.path().join("dest");
        fs::create_dir_all(&destination).unwrap();
        fs::write(destination.join("keep.txt"), "original").unwrap();
        let before = tree_hash(&destination).unwrap();
        let store = PromotionStore::open(directory.path().join("state")).unwrap();
        let adapter = completed_adapter(&contract(), &bundle(&[("result.txt", "PASS\n")]));
        let receipt = store
            .promote("task-1", &adapter, &contract(), &destination)
            .unwrap();
        assert_eq!(receipt.state, PromotionState::Committed);
        assert_eq!(receipt.destination_hash_before, before);
        assert_eq!(receipt.gates_rerun, vec!["o1".to_string()]);
        assert_eq!(
            fs::read_to_string(destination.join("result.txt")).unwrap(),
            "PASS\n"
        );
        assert_eq!(
            fs::read_to_string(destination.join("keep.txt")).unwrap(),
            "original"
        );
        assert_eq!(tree_hash(&destination).unwrap(), receipt.staging_hash);
        assert_eq!(store.receipt("task-1").unwrap(), Some(receipt));
    }

    #[test]
    fn promotion_requires_a_completed_kernel() {
        let directory = tempdir().unwrap();
        let destination = directory.path().join("dest");
        fs::create_dir_all(&destination).unwrap();
        let store = PromotionStore::open(directory.path().join("state")).unwrap();
        let adapter = ExternalHostAdapter::new(contract(), 1).unwrap();
        assert_eq!(
            store.promote("task-2", &adapter, &contract(), &destination),
            Err(PromotionError::KernelNotCompleted)
        );
        assert!(store.receipt("task-2").unwrap().is_none());
    }

    #[test]
    fn bundle_path_escape_and_duplicates_are_rejected() {
        assert!(parse_bundle(&bundle(&[("../evil.txt", "x")])).is_err());
        assert!(parse_bundle(&bundle(&[("/abs.txt", "x")])).is_err());
        assert!(parse_bundle(&bundle(&[("a\\b.txt", "x")])).is_err());
        assert!(parse_bundle(&bundle(&[("same.txt", "x"), ("same.txt", "y")])).is_err());
        assert!(parse_bundle("not json").is_err());
        assert!(parse_bundle(&serde_json::json!({"files":[]}).to_string()).is_err());
    }

    #[test]
    fn gate_failure_rolls_back_and_verifies_the_restore() {
        let directory = tempdir().unwrap();
        let destination = directory.path().join("dest");
        fs::create_dir_all(&destination).unwrap();
        fs::write(destination.join("keep.txt"), "original").unwrap();
        let before = tree_hash(&destination).unwrap();
        let store = PromotionStore::open(directory.path().join("state")).unwrap();
        // The bundle builds the wrong content; the staged gate must catch it.
        let adapter = completed_adapter(&contract(), &bundle(&[("result.txt", "FAIL\n")]));
        let receipt = store
            .promote("task-3", &adapter, &contract(), &destination)
            .unwrap();
        assert_eq!(receipt.state, PromotionState::RolledBack);
        assert_eq!(tree_hash(&destination).unwrap(), before);
        assert!(!destination.join("result.txt").exists());
        assert_eq!(
            fs::read_to_string(destination.join("keep.txt")).unwrap(),
            "original"
        );
    }

    #[test]
    fn apply_failure_rolls_back_to_a_verified_snapshot() {
        let directory = tempdir().unwrap();
        let destination = directory.path().join("dest");
        fs::create_dir_all(&destination).unwrap();
        // A directory where the bundle wants a file makes staging fail.
        fs::create_dir_all(destination.join("result.txt")).unwrap();
        let before = tree_hash(&destination).unwrap();
        let store = PromotionStore::open(directory.path().join("state")).unwrap();
        let adapter = completed_adapter(&contract(), &bundle(&[("result.txt", "PASS\n")]));
        let receipt = store
            .promote("task-4", &adapter, &contract(), &destination)
            .unwrap();
        assert_eq!(receipt.state, PromotionState::RolledBack);
        assert_eq!(tree_hash(&destination).unwrap(), before);
        assert!(destination.join("result.txt").is_dir());
    }
    fn simulate_crashed_promotion(
        store: &PromotionStore,
        task_id: &str,
        destination: &Path,
    ) -> String {
        // A prepare receipt plus snapshot, then uncommitted destination
        // dirt: exactly what a crash between prepare and commit leaves.
        let before = tree_hash(destination).unwrap();
        let snapshot = store.dir.join(format!("snapshot-{task_id}"));
        if snapshot.exists() {
            fs::remove_dir_all(&snapshot).unwrap();
        }
        copy_tree(destination, &snapshot).unwrap();
        store
            .persist_receipt(&PromotionReceipt {
                task_id: task_id.into(),
                candidate_id: "crashed".into(),
                bundle_hash: "b".repeat(64),
                destination_hash_before: before.clone(),
                staging_hash: String::new(),
                gates_rerun: vec![],
                gates_not_rerun: vec![],
                state: PromotionState::Prepared,
                detail: "staged".into(),
            })
            .unwrap();
        fs::write(destination.join("junk.txt"), "half-applied").unwrap();
        fs::write(destination.join("keep.txt"), "dirtied").unwrap();
        before
    }

    #[test]
    fn crashed_promotion_recovers_before_promoting_again() {
        let directory = tempdir().unwrap();
        let destination = directory.path().join("dest");
        fs::create_dir_all(&destination).unwrap();
        fs::write(destination.join("keep.txt"), "original").unwrap();
        let store = PromotionStore::open(directory.path().join("state")).unwrap();
        simulate_crashed_promotion(&store, "task-5", &destination);
        let adapter = completed_adapter(&contract(), &bundle(&[("result.txt", "PASS\n")]));
        let receipt = store
            .promote("task-5", &adapter, &contract(), &destination)
            .unwrap();
        assert_eq!(receipt.state, PromotionState::Committed);
        // The dirty state was rolled back first, then the bundle applied.
        assert_eq!(
            fs::read_to_string(destination.join("keep.txt")).unwrap(),
            "original"
        );
        assert!(!destination.join("junk.txt").exists());
        assert_eq!(
            fs::read_to_string(destination.join("result.txt")).unwrap(),
            "PASS\n"
        );
    }

    #[test]
    fn crashed_promotion_with_unverifiable_restore_is_corrupt_state() {
        let directory = tempdir().unwrap();
        let destination = directory.path().join("dest");
        fs::create_dir_all(&destination).unwrap();
        fs::write(destination.join("keep.txt"), "original").unwrap();
        let store = PromotionStore::open(directory.path().join("state")).unwrap();
        simulate_crashed_promotion(&store, "task-6", &destination);
        fs::remove_dir_all(store.dir.join("snapshot-task-6")).unwrap();
        let adapter = completed_adapter(&contract(), &bundle(&[("result.txt", "PASS\n")]));
        let receipt = store
            .promote("task-6", &adapter, &contract(), &destination)
            .unwrap();
        assert_eq!(receipt.state, PromotionState::CorruptState);
        // Fail closed: nothing was promoted onto an unverifiable base.
        assert!(!destination.join("result.txt").exists());
        assert_eq!(
            store.receipt("task-6").unwrap().unwrap().state,
            PromotionState::CorruptState
        );
    }
}
