//! Write journal: undo for agent file changes.
//!
//! Hermes snapshots the working tree into a shadow git store before
//! mutating tool calls (`tools/checkpoint_manager.py`); opencode keeps a
//! git snapshot per step with restore/revert (`src/snapshot/index.ts`).
//! REX journals each successful write itself, with no git dependency: the
//! prior bytes of every touched file (or "absent") plus a fingerprint of
//! what the agent wrote. Undo is newest-first and refuses to touch a file
//! whose current bytes differ from what the agent left, so undoing never
//! clobbers edits made after the agent (by the user or anyone else).
//! The journal lives on disk, so a finished or crashed run can still be
//! undone.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JournalFile {
    /// Path relative to the workspace root.
    pub path: String,
    /// Blob holding the bytes before the write; `None` = file did not exist.
    pub before_blob: Option<String>,
    /// Fingerprint of the bytes after the write; `None` = file was deleted.
    pub after_fp: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JournalEntry {
    pub seq: u64,
    pub call_id: String,
    pub tool: String,
    pub at_ms: u128,
    pub files: Vec<JournalFile>,
    #[serde(default)]
    pub undone: bool,
}

pub fn fingerprint(bytes: &[u8]) -> u64 {
    // FNV-1a: stable across processes and Rust versions, unlike the std
    // hasher, because journals outlive the process that wrote them.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Before-state captured ahead of a write.
pub struct Pending {
    pub files: Vec<(String, Option<Vec<u8>>)>,
}

impl Pending {
    pub fn capture(root: &Path, rels: &[String]) -> Self {
        Self {
            files: rels
                .iter()
                .map(|r| (r.clone(), fs::read(root.join(r)).ok()))
                .collect(),
        }
    }
}

pub struct Journal {
    dir: PathBuf,
}

impl Journal {
    pub fn open(dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(dir.join("blobs"))?;
        Ok(Self { dir })
    }

    fn index(&self) -> PathBuf {
        self.dir.join("journal.jsonl")
    }

    pub fn entries(&self) -> Vec<JournalEntry> {
        fs::read_to_string(self.index())
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    fn rewrite(&self, entries: &[JournalEntry]) -> std::io::Result<()> {
        let tmp = self.dir.join("journal.jsonl.tmp");
        let mut f = fs::File::create(&tmp)?;
        for e in entries {
            writeln!(f, "{}", serde_json::to_string(e).unwrap_or_default())?;
        }
        f.sync_all()?;
        fs::rename(tmp, self.index())
    }

    /// Record a completed write: `pending` is the state before, and the
    /// current disk state is the state after.
    pub fn record(
        &self,
        root: &Path,
        call_id: &str,
        tool: &str,
        pending: Pending,
    ) -> std::io::Result<()> {
        let seq = self.entries().last().map_or(1, |e| e.seq + 1);
        let mut files = Vec::new();
        for (n, (rel, before)) in pending.files.into_iter().enumerate() {
            let before_blob = match before {
                Some(bytes) => {
                    let name = format!("{seq}-{n}.bin");
                    fs::write(self.dir.join("blobs").join(&name), bytes)?;
                    Some(name)
                }
                None => None,
            };
            let after_fp = fs::read(root.join(&rel)).ok().map(|b| fingerprint(&b));
            files.push(JournalFile {
                path: rel,
                before_blob,
                after_fp,
            });
        }
        let entry = JournalEntry {
            seq,
            call_id: call_id.to_string(),
            tool: tool.to_string(),
            at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
            files,
            undone: false,
        };
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.index())?;
        writeln!(f, "{}", serde_json::to_string(&entry).unwrap_or_default())?;
        f.sync_all()
    }

    /// Undo the newest write that is not yet undone. Refuses, changing
    /// nothing, when any of its files no longer holds what the agent wrote.
    pub fn undo_last(&self, root: &Path) -> Result<JournalEntry, String> {
        let mut entries = self.entries();
        let Some(idx) = entries.iter().rposition(|e| !e.undone) else {
            return Err("nothing to undo".into());
        };
        let entry = entries[idx].clone();
        for f in &entry.files {
            if f.path.contains("..") || Path::new(&f.path).is_absolute() {
                return Err(format!("journal path {} is not workspace-relative", f.path));
            }
            let now = fs::read(root.join(&f.path)).ok().map(|b| fingerprint(&b));
            if now != f.after_fp {
                return Err(format!(
                    "{} changed after the agent wrote it; undo refused so those changes are kept",
                    f.path
                ));
            }
        }
        let mut blobs = Vec::new();
        for f in &entry.files {
            let bytes = match &f.before_blob {
                Some(name) => Some(
                    fs::read(self.dir.join("blobs").join(name))
                        .map_err(|e| format!("journal blob for {} is missing: {e}", f.path))?,
                ),
                None => None,
            };
            blobs.push(bytes);
        }
        for (f, bytes) in entry.files.iter().zip(blobs).rev() {
            let target = root.join(&f.path);
            match bytes {
                Some(b) => {
                    if let Some(p) = target.parent() {
                        fs::create_dir_all(p).map_err(|e| e.to_string())?;
                    }
                    let tmp = target.with_extension("rex-undo.tmp");
                    fs::write(&tmp, &b).map_err(|e| e.to_string())?;
                    fs::rename(&tmp, &target).map_err(|e| e.to_string())?;
                }
                None => {
                    if target.exists() {
                        fs::remove_file(&target).map_err(|e| e.to_string())?;
                    }
                }
            }
        }
        entries[idx].undone = true;
        self.rewrite(&entries).map_err(|e| e.to_string())?;
        Ok(entries[idx].clone())
    }

    /// Undo every write after `seq` (newest first), leaving the workspace
    /// as it was right after write `seq`; `seq` 0 undoes the whole run.
    /// All or nothing: every step is checked against the file state the
    /// earlier undos would leave before any file is touched, so a file
    /// changed after the agent wrote it refuses the whole rewind.
    pub fn undo_to(&self, root: &Path, seq: u64) -> Result<Vec<JournalEntry>, String> {
        let entries = self.entries();
        let todo: Vec<&JournalEntry> = entries
            .iter()
            .rev()
            .filter(|e| !e.undone && e.seq > seq)
            .collect();
        if todo.is_empty() {
            return Err(format!("nothing to undo after write {seq}"));
        }
        // simulated fingerprints: path -> Some(fp) / None (absent)
        let mut state: std::collections::HashMap<String, Option<u64>> =
            std::collections::HashMap::new();
        for e in &todo {
            for f in &e.files {
                if f.path.contains("..") || Path::new(&f.path).is_absolute() {
                    return Err(format!("journal path {} is not workspace-relative", f.path));
                }
                let now = match state.get(&f.path) {
                    Some(fp) => *fp,
                    None => fs::read(root.join(&f.path)).ok().map(|b| fingerprint(&b)),
                };
                if now != f.after_fp {
                    return Err(format!(
                        "{} changed after the agent wrote it (write {}); nothing was undone so those changes are kept",
                        f.path, e.seq
                    ));
                }
            }
            for f in &e.files {
                let before = match &f.before_blob {
                    Some(name) => Some(fingerprint(
                        &fs::read(self.dir.join("blobs").join(name)).map_err(|err| {
                            format!("journal blob for {} is missing: {err}", f.path)
                        })?,
                    )),
                    None => None,
                };
                state.insert(f.path.clone(), before);
            }
        }
        let mut done = Vec::new();
        for _ in 0..todo.len() {
            done.push(self.undo_last(root)?);
        }
        Ok(done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "rex-journal-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn three_writes(root: &Path, j: &Journal) {
        fs::write(root.join("a.txt"), "one").unwrap();
        let p = Pending::capture(root, &["a.txt".into(), "new.txt".into()]);
        fs::write(root.join("a.txt"), "two").unwrap();
        fs::write(root.join("new.txt"), "created").unwrap();
        j.record(root, "c1", "apply_patch", p).unwrap();
        let p = Pending::capture(root, &["b.txt".into()]);
        fs::write(root.join("b.txt"), "b").unwrap();
        j.record(root, "c2", "create_file", p).unwrap();
        let p = Pending::capture(root, &["a.txt".into()]);
        fs::write(root.join("a.txt"), "three").unwrap();
        j.record(root, "c3", "edit_file", p).unwrap();
    }

    #[test]
    fn undo_to_rewinds_to_a_step_or_the_whole_run() {
        let root = tmp();
        let j = Journal::open(root.join(".j")).unwrap();
        three_writes(&root, &j);
        let done = j.undo_to(&root, 1).unwrap();
        assert_eq!(done.iter().map(|e| e.seq).collect::<Vec<_>>(), [3, 2]);
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "two");
        assert!(!root.join("b.txt").exists());
        assert!(root.join("new.txt").exists());
        assert!(j.undo_to(&root, 1).unwrap_err().contains("nothing to undo"));
        assert_eq!(j.undo_to(&root, 0).unwrap().len(), 1);
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "one");
        assert!(!root.join("new.txt").exists());
        // one call through a file two writes touched (a.txt: 3 then 1)
        let root = tmp();
        let j = Journal::open(root.join(".j")).unwrap();
        three_writes(&root, &j);
        assert_eq!(j.undo_to(&root, 0).unwrap().len(), 3);
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "one");
        assert!(!root.join("b.txt").exists() && !root.join("new.txt").exists());
    }

    #[test]
    fn undo_to_is_all_or_nothing_when_a_later_step_is_blocked() {
        let root = tmp();
        let j = Journal::open(root.join(".j")).unwrap();
        three_writes(&root, &j);
        // someone edits b.txt, which only write 2 touched
        fs::write(root.join("b.txt"), "user").unwrap();
        let e = j.undo_to(&root, 0).unwrap_err();
        assert!(e.contains("b.txt") && e.contains("write 2"), "{e}");
        // write 3 (a.txt) was undoable on its own but nothing was touched
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "three");
        assert_eq!(fs::read_to_string(root.join("b.txt")).unwrap(), "user");
        assert!(j.entries().iter().all(|e| !e.undone));
        // a rewind that stops before write 2 still works
        assert_eq!(j.undo_to(&root, 2).unwrap().len(), 1);
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "two");
    }

    #[test]
    fn undo_restores_newest_first_and_removes_created_files() {
        let root = tmp();
        let j = Journal::open(root.join(".j")).unwrap();
        fs::write(root.join("a.txt"), "one").unwrap();
        let p = Pending::capture(&root, &["a.txt".into(), "new.txt".into()]);
        fs::write(root.join("a.txt"), "two").unwrap();
        fs::write(root.join("new.txt"), "created").unwrap();
        j.record(&root, "c1", "apply_patch", p).unwrap();
        let p = Pending::capture(&root, &["a.txt".into()]);
        fs::write(root.join("a.txt"), "three").unwrap();
        j.record(&root, "c2", "edit_file", p).unwrap();

        assert_eq!(j.undo_last(&root).unwrap().call_id, "c2");
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "two");
        assert_eq!(j.undo_last(&root).unwrap().call_id, "c1");
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "one");
        assert!(!root.join("new.txt").exists());
        assert!(j.undo_last(&root).unwrap_err().contains("nothing to undo"));
    }

    #[test]
    fn undo_refuses_when_someone_changed_the_file_since() {
        let root = tmp();
        let j = Journal::open(root.join(".j")).unwrap();
        let p = Pending::capture(&root, &["b.txt".into()]);
        fs::write(root.join("b.txt"), "agent").unwrap();
        j.record(&root, "c1", "create_file", p).unwrap();
        fs::write(root.join("b.txt"), "user edit").unwrap();
        let e = j.undo_last(&root).unwrap_err();
        assert!(e.contains("changed after the agent wrote it"), "{e}");
        assert_eq!(fs::read_to_string(root.join("b.txt")).unwrap(), "user edit");
        assert!(!j.entries()[0].undone);
    }

    #[test]
    fn deleted_files_come_back_and_journal_survives_reopen() {
        let root = tmp();
        fs::write(root.join("gone.txt"), "keep me").unwrap();
        {
            let j = Journal::open(root.join(".j")).unwrap();
            let p = Pending::capture(&root, &["gone.txt".into()]);
            fs::remove_file(root.join("gone.txt")).unwrap();
            j.record(&root, "c1", "apply_patch", p).unwrap();
        }
        let j = Journal::open(root.join(".j")).unwrap();
        j.undo_last(&root).unwrap();
        assert_eq!(
            fs::read_to_string(root.join("gone.txt")).unwrap(),
            "keep me"
        );
        assert!(j.entries()[0].undone);
    }
}
