//! Evidence store: sha256-addressed artifacts with a manifest. The verifier
//! hashes the workspace, the judge cites these ids, and tampering with a file
//! after verification breaks its hash and fails any later check.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const MAX_HASH_BYTES: u64 = 8 * 1024 * 1024;
const MAX_MANIFEST_ENTRIES: usize = 4_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceEntry {
    pub id: String,
    pub kind: String,
    pub path: Option<String>,
    pub sha256: String,
    pub bytes: u64,
    pub recorded_ms: u128,
}

pub struct EvidenceStore {
    manifest: PathBuf,
    seq: u64,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

impl EvidenceStore {
    pub fn open(dir: &Path) -> std::io::Result<Self> {
        fs::create_dir_all(dir)?;
        let manifest = dir.join("manifest.jsonl");
        let mut seq = 0u64;
        if let Ok(text) = fs::read_to_string(&manifest) {
            seq = text.lines().filter(|l| !l.trim().is_empty()).count() as u64;
        }
        Ok(Self { manifest, seq })
    }

    fn append(&mut self, kind: &str, path: Option<String>, bytes: &[u8]) -> EvidenceEntry {
        self.seq += 1;
        let entry = EvidenceEntry {
            id: format!("ev-{:04}", self.seq),
            kind: kind.to_string(),
            path,
            sha256: sha256_hex(bytes),
            bytes: bytes.len() as u64,
            recorded_ms: crate::now_ms(),
        };
        if let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.manifest)
        {
            let _ = writeln!(
                file,
                "{}",
                serde_json::to_string(&entry).unwrap_or_default()
            );
        }
        entry
    }

    /// Record raw bytes (a model draft, a verdict document).
    pub fn put_bytes(&mut self, kind: &str, bytes: &[u8]) -> EvidenceEntry {
        self.append(kind, None, bytes)
    }

    /// Hash a workspace file into evidence without copying it.
    pub fn put_file(
        &mut self,
        kind: &str,
        workspace: &Path,
        rel: &str,
    ) -> Result<EvidenceEntry, String> {
        let full = workspace.join(rel);
        let meta = fs::metadata(&full).map_err(|e| format!("{rel}: {e}"))?;
        if meta.len() > MAX_HASH_BYTES {
            return Err(format!("{rel}: too large to hash into evidence"));
        }
        let bytes = fs::read(&full).map_err(|e| format!("{rel}: {e}"))?;
        Ok(self.append(kind, Some(rel.to_string()), &bytes))
    }

    /// Hash every regular file under the workspace (bounded), producing the
    /// artifact manifest the judge and verifier cite.
    pub fn snapshot_workspace(&mut self, workspace: &Path) -> Vec<EvidenceEntry> {
        let mut entries = Vec::new();
        let mut stack = vec![workspace.to_path_buf()];
        while let Some(dir) = stack.pop() {
            if entries.len() >= MAX_MANIFEST_ENTRIES {
                break;
            }
            let Ok(read) = fs::read_dir(&dir) else {
                continue;
            };
            for item in read.flatten() {
                let path = item.path();
                let name = item.file_name().to_string_lossy().to_string();
                if name == "target" || name == "node_modules" || name == ".git" {
                    continue;
                }
                let Ok(meta) = item.metadata() else { continue };
                if meta.is_dir() {
                    stack.push(path);
                } else if meta.is_file() && meta.len() <= MAX_HASH_BYTES {
                    if let Ok(rel) = path.strip_prefix(workspace) {
                        if let Ok(bytes) = fs::read(&path) {
                            entries.push(self.append(
                                "artifact",
                                Some(rel.to_string_lossy().to_string()),
                                &bytes,
                            ));
                        }
                    }
                }
            }
        }
        entries
    }

    pub fn manifest_text(&self) -> String {
        fs::read_to_string(&self.manifest).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tampering_changes_the_hash() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        fs::write(ws.join("a.txt"), b"original").unwrap();
        let mut store = EvidenceStore::open(&dir.path().join("ev")).unwrap();
        let entry = store.put_file("artifact", &ws, "a.txt").unwrap();
        fs::write(ws.join("a.txt"), b"tampered").unwrap();
        let after = sha256_hex(&fs::read(ws.join("a.txt")).unwrap());
        assert_ne!(entry.sha256, after);
    }

    #[test]
    fn snapshot_skips_heavy_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(ws.join("target")).unwrap();
        fs::create_dir_all(ws.join("src")).unwrap();
        fs::write(ws.join("target/big.o"), b"x").unwrap();
        fs::write(ws.join("src/main.rs"), b"fn main(){}").unwrap();
        let mut store = EvidenceStore::open(&dir.path().join("ev")).unwrap();
        let entries = store.snapshot_workspace(&ws);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path.as_deref(), Some("src/main.rs"));
    }
}
