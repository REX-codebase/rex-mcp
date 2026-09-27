//! Content-addressed immutable artifact store: the first-party evidence
//! anchor. Bytes land under `evidence/artifacts/<sha256>` exactly once
//! (write-once, read-only, temp file + atomic rename + fsync), so a host
//! cannot rewrite an artifact after its digest has been cited. Every put
//! appends a binding record (task, kind, candidate, round, digest, byte
//! count) to a per-task manifest; presenting a digest that is already
//! bound to a different candidate or round is rejected as reused evidence.
//! Reads re-hash and fail on tamper or removal.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::evidence::sha256_hex;

/// Artifact bodies are evidence, not builds: cap them.
pub const MAX_ARTIFACT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactBinding {
    pub task_id: String,
    pub kind: String,
    pub sha256: String,
    pub bytes: u64,
    /// Candidate this artifact is evidence for, when applicable. The
    /// digest-plus-candidate pair is what later gates may cite.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_id: Option<String>,
    /// Iteration round this artifact was produced in, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round: Option<u64>,
    pub recorded_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactError {
    TooLarge {
        bytes: u64,
        max: u64,
    },
    /// Digest already bound to a different candidate or round: stale or
    /// reused evidence, never silently re-anchored.
    ReusedDigest {
        sha256: String,
    },
    /// Stored bytes no longer match their content address.
    Tampered {
        sha256: String,
    },
    Missing {
        sha256: String,
    },
    Io(String),
}

impl std::fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArtifactError::TooLarge { bytes, max } => {
                write!(f, "artifact too large: {bytes} bytes (max {max})")
            }
            ArtifactError::ReusedDigest { sha256 } => write!(
                f,
                "digest {sha256} is already bound to a different candidate or round"
            ),
            ArtifactError::Tampered { sha256 } => {
                write!(f, "stored artifact {sha256} failed its hash check")
            }
            ArtifactError::Missing { sha256 } => write!(f, "no artifact stored at {sha256}"),
            ArtifactError::Io(m) => write!(f, "artifact store io: {m}"),
        }
    }
}

impl std::error::Error for ArtifactError {}

pub struct ArtifactStore {
    artifacts: PathBuf,
    bindings: PathBuf,
}

impl ArtifactStore {
    /// Open (creating) the store rooted at `<root>/evidence`.
    pub fn open(root: &Path) -> Result<Self, ArtifactError> {
        let artifacts = root.join("evidence").join("artifacts");
        let bindings = root.join("evidence").join("bindings");
        fs::create_dir_all(&artifacts).map_err(ioe)?;
        fs::create_dir_all(&bindings).map_err(ioe)?;
        Ok(Self {
            artifacts,
            bindings,
        })
    }

    /// Store bytes under their sha256 and bind them to (task, candidate,
    /// round). Idempotent for an identical binding; `fresh` is false then.
    pub fn put(
        &self,
        task_id: &str,
        kind: &str,
        bytes: &[u8],
        candidate_id: Option<String>,
        round: Option<u64>,
    ) -> Result<(ArtifactBinding, bool), ArtifactError> {
        if bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(ArtifactError::TooLarge {
                bytes: bytes.len() as u64,
                max: MAX_ARTIFACT_BYTES as u64,
            });
        }
        let digest = sha256_hex(bytes);
        let path = self.artifacts.join(&digest);
        if path.exists() {
            // Content addressing only holds if existing bytes still hash
            // to their name; check before re-anchoring anything to them.
            self.verify_stored(&digest)?;
        } else {
            write_immutable(&path, bytes)?;
        }
        let existing = self.bindings(task_id);
        for b in &existing {
            if b.sha256 == digest {
                if b.candidate_id != candidate_id || b.round != round {
                    return Err(ArtifactError::ReusedDigest { sha256: digest });
                }
                if b.kind == kind {
                    return Ok((b.clone(), false));
                }
                // One unchanged frame can legitimately prove both start and
                // reverse for a reversible control. Preserve each evidence
                // kind binding while still rejecting cross-round reuse.
                break;
            }
        }
        let binding = ArtifactBinding {
            task_id: task_id.to_string(),
            kind: kind.to_string(),
            sha256: digest,
            bytes: bytes.len() as u64,
            candidate_id,
            round,
            recorded_ms: crate::now_ms(),
        };
        let file = self.bindings.join(format!("{task_id}.jsonl"));
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file)
            .map_err(ioe)?;
        writeln!(f, "{}", serde_json::to_string(&binding).unwrap_or_default()).map_err(ioe)?;
        f.sync_data().map_err(ioe)?;
        Ok((binding, true))
    }

    /// Read an artifact, re-hashing it: tampered or truncated storage is
    /// an error, never silent data.
    pub fn get(&self, sha256: &str) -> Result<Vec<u8>, ArtifactError> {
        let path = self.artifacts.join(sha256);
        let bytes = fs::read(&path).map_err(|_| ArtifactError::Missing {
            sha256: sha256.to_string(),
        })?;
        if sha256_hex(&bytes) != sha256 {
            return Err(ArtifactError::Tampered {
                sha256: sha256.to_string(),
            });
        }
        Ok(bytes)
    }

    /// Verify a stored artifact still matches its content address.
    pub fn verify_stored(&self, sha256: &str) -> Result<(), ArtifactError> {
        self.get(sha256).map(|_| ())
    }

    /// All bindings recorded for a task, oldest first.
    pub fn bindings(&self, task_id: &str) -> Vec<ArtifactBinding> {
        let file = self.bindings.join(format!("{task_id}.jsonl"));
        let Ok(text) = fs::read_to_string(file) else {
            return Vec::new();
        };
        text.lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }
}

/// Write bytes to their final content-addressed path exactly once:
/// temp file in the same directory, fsync, atomic rename, read-only.
fn write_immutable(path: &Path, bytes: &[u8]) -> Result<(), ArtifactError> {
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    {
        let mut f = fs::File::create(&tmp).map_err(ioe)?;
        f.write_all(bytes).map_err(ioe)?;
        f.sync_all().map_err(ioe)?;
    }
    // A racing put of identical bytes may have landed first; that is the
    // same content, so an existing destination is fine.
    if !path.exists() {
        fs::rename(&tmp, path).map_err(ioe)?;
    } else {
        let _ = fs::remove_file(&tmp);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o444)).map_err(ioe)?;
    }
    Ok(())
}

fn ioe(e: std::io::Error) -> ArtifactError {
    ArtifactError::Io(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, ArtifactStore) {
        let d = tempfile::tempdir().unwrap();
        let s = ArtifactStore::open(d.path()).unwrap();
        (d, s)
    }

    #[test]
    fn put_get_roundtrip_and_content_addressed() {
        let (_d, s) = store();
        let (b, fresh) = s
            .put(
                "task-1",
                "screenshot",
                b"png-bytes",
                Some("cand-1".into()),
                Some(1),
            )
            .unwrap();
        assert!(fresh);
        assert_eq!(b.sha256, sha256_hex(b"png-bytes"));
        assert_eq!(b.bytes, 9);
        assert_eq!(s.get(&b.sha256).unwrap(), b"png-bytes");
        let path = _d.path().join("evidence/artifacts").join(&b.sha256);
        assert!(path.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o444
            );
        }
    }

    #[test]
    fn identical_binding_is_idempotent() {
        let (_d, s) = store();
        let (b1, f1) = s
            .put("task-1", "shot", b"x", Some("c".into()), Some(1))
            .unwrap();
        let (b2, f2) = s
            .put("task-1", "shot", b"x", Some("c".into()), Some(1))
            .unwrap();
        assert!(f1 && !f2);
        assert_eq!(b1, b2);
        assert_eq!(s.bindings("task-1").len(), 1);
    }

    #[test]
    fn same_frame_can_bind_two_kinds_in_one_round() {
        let (_d, s) = store();
        let (first, _) = s
            .put("task-1", "render.state.start", b"same-frame", None, None)
            .unwrap();
        let (reverse, fresh) = s
            .put("task-1", "render.state.reverse", b"same-frame", None, None)
            .unwrap();
        assert!(fresh);
        assert_eq!(first.sha256, reverse.sha256);
        assert_eq!(s.bindings("task-1").len(), 2);
    }

    #[test]
    fn digest_reused_for_other_candidate_or_round_is_rejected() {
        let (_d, s) = store();
        s.put("task-1", "shot", b"x", Some("cand-1".into()), Some(1))
            .unwrap();
        // Same bytes, different candidate: reused evidence.
        assert!(matches!(
            s.put("task-1", "shot", b"x", Some("cand-2".into()), Some(1)),
            Err(ArtifactError::ReusedDigest { .. })
        ));
        // Same bytes, later round: stale evidence.
        assert!(matches!(
            s.put("task-1", "shot", b"x", Some("cand-1".into()), Some(2)),
            Err(ArtifactError::ReusedDigest { .. })
        ));
        assert_eq!(s.bindings("task-1").len(), 1);
    }

    #[test]
    fn same_digest_is_reusable_across_distinct_tasks() {
        let (_d, s) = store();
        s.put("task-1", "shot", b"x", Some("c".into()), Some(1))
            .unwrap();
        let (_b, fresh) = s
            .put("task-2", "shot", b"x", Some("c".into()), Some(1))
            .unwrap();
        assert!(fresh);
    }

    #[test]
    fn tampered_storage_fails_closed() {
        let (d, s) = store();
        let (b, _) = s.put("task-1", "shot", b"original", None, None).unwrap();
        let path = d.path().join("evidence/artifacts").join(&b.sha256);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        }
        fs::write(&path, b"modified").unwrap();
        assert!(matches!(
            s.get(&b.sha256),
            Err(ArtifactError::Tampered { .. })
        ));
        // Re-putting the same binding must not re-anchor on bad bytes.
        assert!(matches!(
            s.put("task-1", "shot", b"original", None, None),
            Err(ArtifactError::Tampered { .. })
        ));
    }

    #[test]
    fn missing_artifact_is_an_error_not_empty_bytes() {
        let (_d, s) = store();
        assert!(matches!(
            s.get(&"f".repeat(64)),
            Err(ArtifactError::Missing { .. })
        ));
    }

    #[test]
    fn oversized_artifact_is_rejected() {
        let (_d, s) = store();
        let big = vec![0u8; MAX_ARTIFACT_BYTES + 1];
        assert!(matches!(
            s.put("task-1", "dump", &big, None, None),
            Err(ArtifactError::TooLarge { .. })
        ));
    }
}
