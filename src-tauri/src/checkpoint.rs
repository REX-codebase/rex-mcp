//! Workspace checkpoints: snapshots for one-click restore.
//!
//! Checkpoints are full copies of the workspace stored under the REX config
//! dir (`checkpoints/{id}/`). The ledger (`checkpoints.json`) records each
//! checkpoint's workspace, label, and timestamp. Restore copies a checkpoint
//! back over the workspace — but first it auto-snapshots the current state,
//! so a restore is never destructive.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub id: String,
    pub workspace: String,
    pub label: String,
    pub created_at_ms: u64,
    /// Number of files in the snapshot.
    pub file_count: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Ledger {
    checkpoints: Vec<Checkpoint>,
}

pub struct CheckpointStore {
    dir: PathBuf,
    ledger_path: PathBuf,
    inner: Mutex<Ledger>,
}

impl CheckpointStore {
    pub fn new(config_dir: &Path) -> Result<Self, String> {
        let dir = config_dir.join("checkpoints");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let ledger_path = dir.join("checkpoints.json");
        let ledger = if ledger_path.exists() {
            let data = std::fs::read_to_string(&ledger_path).map_err(|e| e.to_string())?;
            serde_json::from_str(&data).map_err(|e| e.to_string())?
        } else {
            Ledger::default()
        };
        Ok(CheckpointStore {
            dir,
            ledger_path,
            inner: Mutex::new(ledger),
        })
    }

    fn save(&self) -> Result<(), String> {
        let ledger = self.inner.lock().map_err(|e| e.to_string())?;
        let data = serde_json::to_string_pretty(&*ledger).map_err(|e| e.to_string())?;
        // Atomic write.
        let tmp = self.ledger_path.with_extension("tmp");
        std::fs::write(&tmp, data).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.ledger_path).map_err(|e| e.to_string())?;
        Ok(())
    }

    fn workspace_dir(&self, workspace: &Path) -> Result<PathBuf, String> {
        let ws = workspace
            .canonicalize()
            .map_err(|e| format!("workspace not found: {e}"))?;
        Ok(ws)
    }

    fn copy_workspace(&self, src: &Path, dst: &Path) -> Result<u64, String> {
        let mut count = 0u64;
        std::fs::create_dir_all(dst).map_err(|e| e.to_string())?;
        for entry in walkdir(src)? {
            let rel = entry.strip_prefix(src).map_err(|e| e.to_string())?;
            // Skip the checkpoint dir itself if nested, and .git.
            if rel
                .components()
                .next()
                .map(|c| c.as_os_str() == ".git")
                .unwrap_or(false)
            {
                continue;
            }
            let target = dst.join(rel);
            if entry.is_dir() {
                std::fs::create_dir_all(&target).map_err(|e| e.to_string())?;
            } else if entry.is_file() {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                std::fs::copy(&entry, &target).map_err(|e| e.to_string())?;
                count += 1;
            }
        }
        Ok(count)
    }

    /// Create a checkpoint of the workspace.
    pub fn create(&self, workspace: &Path, label: &str) -> Result<Checkpoint, String> {
        let ws = self.workspace_dir(workspace)?;
        let n = ID_COUNTER.fetch_add(1, Ordering::SeqCst);
        let id = format!(
            "ckpt-{}-{}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_millis(),
            std::process::id(),
            n
        );
        let dst = self.dir.join(&id);
        let file_count = self.copy_workspace(&ws, &dst)?;
        let cp = Checkpoint {
            id: id.clone(),
            workspace: ws.to_string_lossy().to_string(),
            label: label.to_string(),
            created_at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_millis() as u64,
            file_count,
        };
        {
            let mut ledger = self.inner.lock().map_err(|e| e.to_string())?;
            ledger.checkpoints.push(cp.clone());
        }
        self.save()?;
        Ok(cp)
    }

    /// List checkpoints for a workspace, newest first.
    pub fn list(&self, workspace: &Path) -> Result<Vec<Checkpoint>, String> {
        let ws = self.workspace_dir(workspace)?;
        let ws_str = ws.to_string_lossy().to_string();
        let ledger = self.inner.lock().map_err(|e| e.to_string())?;
        let mut out: Vec<Checkpoint> = ledger
            .checkpoints
            .iter()
            .filter(|c| c.workspace == ws_str)
            .cloned()
            .collect();
        out.sort_by(|a, b| b.created_at_ms.cmp(&a.created_at_ms));
        Ok(out)
    }

    /// Restore a checkpoint. Auto-snapshots the current workspace first, so
    /// the restore is reversible.
    pub fn restore(&self, workspace: &Path, id: &str) -> Result<Checkpoint, String> {
        let ws = self.workspace_dir(workspace)?;
        let src = {
            let ledger = self.inner.lock().map_err(|e| e.to_string())?;
            let cp = ledger
                .checkpoints
                .iter()
                .find(|c| c.id == id)
                .ok_or("checkpoint not found")?;
            if cp.workspace != ws.to_string_lossy() {
                return Err("checkpoint belongs to a different workspace".to_string());
            }
            self.dir.join(id)
        };
        if !src.exists() {
            return Err("checkpoint data missing".to_string());
        }
        // Auto-snapshot current state before overwriting.
        let backup = self.create(&ws, "auto-backup before restore")?;
        // Clear the workspace (but not .git) and copy the checkpoint back.
        for entry in walkdir(&ws)? {
            let rel = entry.strip_prefix(&ws).map_err(|e| e.to_string())?;
            if rel.as_os_str().is_empty() {
                continue;
            }
            if rel
                .components()
                .next()
                .map(|c| c.as_os_str() == ".git")
                .unwrap_or(false)
            {
                continue;
            }
            if entry.is_dir() {
                // Remove dirs after files; walkdir gives us files too.
                continue;
            } else {
                let _ = std::fs::remove_file(&entry);
            }
        }
        // Remove now-empty dirs (deepest first).
        let mut dirs: Vec<PathBuf> = walkdir(&ws)?.into_iter().filter(|p| p.is_dir()).collect();
        dirs.sort_by(|a, b| b.components().count().cmp(&a.components().count()));
        for d in dirs {
            if d == ws {
                continue;
            }
            let rel = d.strip_prefix(&ws).map_err(|e| e.to_string())?;
            if rel
                .components()
                .next()
                .map(|c| c.as_os_str() == ".git")
                .unwrap_or(false)
            {
                continue;
            }
            let _ = std::fs::remove_dir(&d);
        }
        let restored_count = self.copy_workspace(&src, &ws)?;
        // Update the backup's file count to reflect what was there.
        {
            let mut ledger = self.inner.lock().map_err(|e| e.to_string())?;
            if let Some(b) = ledger.checkpoints.iter_mut().find(|c| c.id == backup.id) {
                b.file_count = restored_count;
            }
        }
        self.save()?;
        Ok(backup)
    }
}

fn walkdir(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = std::fs::read_dir(&current).map_err(|e| e.to_string())?;
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            out.push(path.clone());
            if path.is_dir() {
                // Don't descend into .git.
                if path.file_name().map(|n| n == ".git").unwrap_or(false) {
                    out.pop();
                    continue;
                }
                stack.push(path);
            }
        }
    }
    Ok(out)
}

fn uuid_short() -> String {
    // Deprecated: IDs now use an atomic counter for uniqueness.
    format!("{:x}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("rex-ckpt-test-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn create_list_restore() {
        let config = temp();
        let ws = temp();
        std::fs::write(ws.join("a.txt"), "v1").unwrap();

        let store = CheckpointStore::new(&config).unwrap();
        let cp = store.create(&ws, "test").unwrap();
        assert_eq!(cp.file_count, 1);

        let list = store.list(&ws).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, cp.id);

        // Modify and restore.
        std::fs::write(ws.join("a.txt"), "v2").unwrap();
        let backup = store.restore(&ws, &cp.id).unwrap();
        assert_eq!(backup.label, "auto-backup before restore");

        let content = std::fs::read_to_string(ws.join("a.txt")).unwrap();
        assert_eq!(content, "v1");

        // The backup has the v2 content.
        let backup_dir = config.join("checkpoints").join(&backup.id);
        let backup_content = std::fs::read_to_string(backup_dir.join("a.txt")).unwrap();
        assert_eq!(backup_content, "v2");
    }
}
