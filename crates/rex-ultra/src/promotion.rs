//! Evidence-gated promotion with symlink-safe application and an atomic,
//! crash-safe tree swap (architecture section J; audit findings 4 and 12).
//!
//! The destination is never written file-by-file. The complete next tree is
//! built in a private sibling directory: bundle files are written and bundle
//! deletes are removed through descriptor-relative `openat` operations with
//! `O_NOFOLLOW` on every component, so no symlink anywhere in the tree can
//! redirect a write outside it (audit finding 4). REX-checkable gates rerun
//! on that stage, a prepare receipt is persisted, the destination hash is
//! re-verified (compare-and-swap guard), and the swap itself is one atomic
//! `renameat2(RENAME_EXCHANGE)` on Linux - readers never observe a partial
//! tree (audit finding 12). The exchange preserves the old tree at the
//! scratch path, so rollback is a swap-back, not a copy; there is no
//! snapshot whose loss could make a restore unverifiable mid-sequence.
//!
//! Crash safety: the durable receipt plus the stage/backup directories make
//! every crash point recoverable. A crash before the swap leaves the
//! destination untouched (RolledBack). A crash after the swap but before the
//! commit record is healed to Committed on the next promote call. An
//! unknown destination state is restored from the preserved old tree and
//! verified; an unverifiable restore is CorruptState, never a guess.
//!
//! Honest limits: symlinks in the destination are not part of the tree
//! contract (tree hashes skip them) and a committed promotion replaces the
//! tree with the staged one, which contains no symlinks. On non-Linux
//! systems the swap falls back to two renames with a small crash window;
//! the receipt records which method was used and recovery covers the window.

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
    /// How the tree swap was performed ("rename_exchange" or
    /// "rename_fallback"); empty when no swap was attempted.
    #[serde(default)]
    pub swap: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromotionError {
    KernelNotCompleted,
    NoQualifiedCandidate,
    InvalidBundle(String),
    DestinationChanged,
    GateFailure(Vec<String>),
    /// A symlink stood where the bundle needed to write or delete;
    /// promotion refuses to follow it rather than escape the tree.
    SymlinkRefused(String),
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
    /// Paths removed from the tree by this promotion. Deletions are
    /// representable because the staged tree replaces the destination
    /// wholesale (audit finding 12).
    #[serde(default)]
    pub deletes: Vec<String>,
}

const MAX_BUNDLE_FILES: usize = 64;
const MAX_FILE_BYTES: usize = 256 * 1024;
const MAX_TREE_ENTRIES: usize = 100_000;
const MAX_SNAPSHOT_BYTES: u64 = 64 * 1024 * 1024;

pub fn validate_bundle_path(path: &str) -> Result<(), PromotionError> {
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
    if bundle.files.is_empty() && bundle.deletes.is_empty() {
        return Err(PromotionError::InvalidBundle("bundle changes nothing".into()));
    }
    if bundle.files.len() + bundle.deletes.len() > MAX_BUNDLE_FILES {
        return Err(PromotionError::InvalidBundle(format!(
            "{} entries exceeds {MAX_BUNDLE_FILES}",
            bundle.files.len() + bundle.deletes.len()
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
    for delete in &bundle.deletes {
        validate_bundle_path(delete)?;
        if !seen.insert(delete.clone()) {
            return Err(PromotionError::InvalidBundle(format!(
                "path {delete:?} is both written and deleted"
            )));
        }
    }
    Ok(bundle)
}

/// Deterministic recursive tree hash over sorted (path, content-hash) pairs.
/// Symlinks are not part of the tree contract and are skipped, never
/// followed. Errors honestly when the tree exceeds the entry bound.
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

/// Descriptor-relative filesystem operations. Every path component is
/// opened with O_NOFOLLOW relative to an owned directory descriptor, so a
/// symlink anywhere in a bundle path refuses the operation instead of
/// redirecting it outside the tree. This is the mechanism behind the
/// finding-4 fix; the stage-only apply above it is the structure.
#[cfg(unix)]
mod dirfd {
    use super::PromotionError;
    use std::ffi::CString;
    use std::io::{self, Write};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
    use std::path::PathBuf;

    /// Owned directory descriptor.
    pub struct DirFd(RawFd);

    impl Drop for DirFd {
        fn drop(&mut self) {
            unsafe {
                libc::close(self.0);
            }
        }
    }

    fn map_err(op: &str, name: &str, e: io::Error) -> PromotionError {
        match e.raw_os_error() {
            Some(libc::ELOOP) => PromotionError::SymlinkRefused(format!(
                "{op}: component {name:?} is a symlink; promotion never follows symlinks"
            )),
            _ => PromotionError::Io(format!("{op} {name:?}: {e}")),
        }
    }

    fn c_name(name: &str) -> Result<CString, PromotionError> {
        CString::new(name)
            .map_err(|_| PromotionError::InvalidBundle(format!("NUL in path component {name:?}")))
    }

    pub fn open_root(path: &std::path::Path) -> Result<DirFd, PromotionError> {
        let c = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| PromotionError::Io("NUL in root path".into()))?;
        let fd = unsafe {
            libc::open(
                c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(PromotionError::Io(format!(
                "open root {}: {}",
                path.display(),
                io::Error::last_os_error()
            )));
        }
        Ok(DirFd(fd))
    }

    fn dup_fd(dir: &DirFd) -> Result<DirFd, PromotionError> {
        let fd = unsafe { libc::fcntl(dir.0, libc::F_DUPFD_CLOEXEC, 0) };
        if fd < 0 {
            return Err(PromotionError::Io(format!(
                "dup dirfd: {}",
                io::Error::last_os_error()
            )));
        }
        Ok(DirFd(fd))
    }

    fn open_dir(parent: &DirFd, name: &str) -> io::Result<DirFd> {
        let c = c_name(name).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL"))?;
        let fd = unsafe {
            libc::openat(
                parent.0,
                c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(DirFd(fd))
    }

    /// Linux reports ENOTDIR (not ELOOP) for O_NOFOLLOW|O_DIRECTORY on a
    /// symlink; look at the component to tell a symlink refusal apart from
    /// an ordinary not-a-directory conflict.
    fn classify_dir_err(parent: &DirFd, name: &str, e: io::Error) -> PromotionError {
        if e.raw_os_error() == Some(libc::ENOTDIR) {
            if let Ok(c) = c_name(name) {
                let mut st: libc::stat = unsafe { std::mem::zeroed() };
                if unsafe { libc::fstatat(parent.0, c.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) }
                    == 0
                    && (st.st_mode & libc::S_IFMT) == libc::S_IFLNK
                {
                    return PromotionError::SymlinkRefused(format!(
                        "open dir: component {name:?} is a symlink; promotion never follows symlinks"
                    ));
                }
            }
        }
        map_err("open dir", name, e)
    }

    fn ensure_dir(parent: &DirFd, name: &str) -> Result<DirFd, PromotionError> {
        match open_dir(parent, name) {
            Ok(d) => Ok(d),
            Err(e) if e.raw_os_error() == Some(libc::ENOENT) => {
                let c = c_name(name)?;
                if unsafe { libc::mkdirat(parent.0, c.as_ptr(), 0o755) } != 0 {
                    let e = io::Error::last_os_error();
                    if e.raw_os_error() != Some(libc::EEXIST) {
                        return Err(map_err("mkdir", name, e));
                    }
                }
                open_dir(parent, name).map_err(|e| map_err("open dir", name, e))
            }
            Err(e) => Err(classify_dir_err(parent, name, e)),
        }
    }

    /// Write one bundle file, creating intermediate directories. A symlink
    /// in any component refuses the write instead of being followed.
    pub fn write_file(root: &DirFd, rel: &str, bytes: &[u8]) -> Result<(), PromotionError> {
        let parts: Vec<&str> = rel.split('/').collect();
        let mut dir = dup_fd(root)?;
        for part in &parts[..parts.len() - 1] {
            dir = ensure_dir(&dir, part)?;
        }
        let name = parts[parts.len() - 1];
        let c = c_name(name)?;
        let fd = unsafe {
            libc::openat(
                dir.0,
                c.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o644,
            )
        };
        if fd < 0 {
            return Err(map_err("write file", rel, io::Error::last_os_error()));
        }
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(file.as_raw_fd(), &mut st) } != 0
            || (st.st_mode & libc::S_IFMT) != libc::S_IFREG
        {
            return Err(PromotionError::Io(format!(
                "write file {rel:?}: target is not a regular file"
            )));
        }
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|e| PromotionError::Io(format!("write file {rel:?}: {e}")))
    }

    /// Remove one delete target (file, symlink, or directory tree), never
    /// following a symlink. Returns false when the path is already absent.
    pub fn remove_path(root: &DirFd, rel: &str) -> Result<bool, PromotionError> {
        let parts: Vec<&str> = rel.split('/').collect();
        let mut dir = dup_fd(root)?;
        for part in &parts[..parts.len() - 1] {
            match open_dir(&dir, part) {
                Ok(d) => dir = d,
                Err(e) if e.raw_os_error() == Some(libc::ENOENT) => return Ok(false),
                Err(e) => return Err(classify_dir_err(&dir, part, e)),
            }
        }
        let name = parts[parts.len() - 1];
        let c = c_name(name)?;
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstatat(dir.0, c.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) } != 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::ENOENT) {
                return Ok(false);
            }
            return Err(map_err("stat", rel, e));
        }
        if (st.st_mode & libc::S_IFMT) == libc::S_IFDIR {
            remove_dir_tree(&dir, name)?;
        } else if unsafe { libc::unlinkat(dir.0, c.as_ptr(), 0) } != 0 {
            return Err(map_err("unlink", rel, io::Error::last_os_error()));
        }
        Ok(true)
    }

    fn remove_dir_tree(parent: &DirFd, name: &str) -> Result<(), PromotionError> {
        let dir = open_dir(parent, name).map_err(|e| map_err("open dir", name, e))?;
        // Entries are read through the descriptor itself, so the walk stays
        // inside this directory even if names are moved mid-delete.
        #[cfg(target_os = "linux")]
        let fd_path = PathBuf::from(format!("/proc/self/fd/{}", dir.0));
        #[cfg(not(target_os = "linux"))]
        let fd_path = PathBuf::from(format!("/dev/fd/{}", dir.0));
        let read = std::fs::read_dir(&fd_path)
            .map_err(|e| PromotionError::Io(format!("read dir {name:?}: {e}")))?;
        for entry in read.flatten() {
            let entry_name = entry
                .file_name()
                .into_string()
                .map_err(|_| PromotionError::Io("non-UTF8 name in tree".into()))?;
            let kind = entry
                .file_type()
                .map_err(|e| PromotionError::Io(e.to_string()))?;
            if kind.is_dir() {
                remove_dir_tree(&dir, &entry_name)?;
            } else {
                let c = c_name(&entry_name)?;
                if unsafe { libc::unlinkat(dir.0, c.as_ptr(), 0) } != 0 {
                    return Err(map_err("unlink", &entry_name, io::Error::last_os_error()));
                }
            }
        }
        drop(dir);
        let c = c_name(name)?;
        if unsafe { libc::unlinkat(parent.0, c.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
            return Err(map_err("rmdir", name, io::Error::last_os_error()));
        }
        Ok(())
    }
}

#[cfg(not(unix))]
mod dirfd {
    use super::PromotionError;

    /// The confined apply is unix-only; elsewhere promotion refuses to
    /// stage rather than apply unconfined.
    pub struct DirFd;

    pub fn open_root(_path: &std::path::Path) -> Result<DirFd, PromotionError> {
        Err(PromotionError::Io(
            "symlink-safe promotion requires unix descriptor-relative operations".into(),
        ))
    }

    pub fn write_file(_root: &DirFd, _rel: &str, _bytes: &[u8]) -> Result<(), PromotionError> {
        unreachable!()
    }

    pub fn remove_path(_root: &DirFd, _rel: &str) -> Result<bool, PromotionError> {
        unreachable!()
    }
}

fn fsync_dir(path: &Path) -> Result<(), PromotionError> {
    fs::File::open(path)
        .and_then(|f| f.sync_all())
        .map_err(|e| PromotionError::Io(format!("fsync {}: {e}", path.display())))
}

fn remove_tree_quiet(path: &Path) {
    if path.exists() {
        let _ = fs::remove_dir_all(path);
    }
}

#[cfg(all(unix, target_os = "linux"))]
fn rename_exchange(a: &Path, b: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let c = |p: &Path| {
        std::ffi::CString::new(p.as_os_str().as_bytes())
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL in path"))
    };
    let a_c = c(a)?;
    let b_c = c(b)?;
    if unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            a_c.as_ptr(),
            libc::AT_FDCWD,
            b_c.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Move the staged tree onto the destination in one atomic exchange where
/// the OS supports it, preserving the old tree for rollback. Returns the
/// method used; the receipt records it honestly.
fn swap_trees(stage: &Path, destination: &Path, backup: &Path) -> Result<String, PromotionError> {
    #[cfg(all(unix, target_os = "linux"))]
    {
        match rename_exchange(stage, destination) {
            Ok(()) => return Ok("rename_exchange".into()),
            Err(e) if e.raw_os_error() == Some(libc::ENOSYS) => {}
            Err(e) => return Err(PromotionError::Io(format!("rename exchange: {e}"))),
        }
    }
    // Fallback: two renames with a small crash window. The durable prepare
    // receipt plus the backup directory make that window recoverable.
    fs::rename(destination, backup)
        .map_err(|e| PromotionError::Io(format!("backup rename: {e}")))?;
    fs::rename(stage, destination)
        .map_err(|e| PromotionError::Io(format!("stage rename: {e}")))?;
    Ok("rename_fallback".into())
}

/// Undo a landed swap: put the preserved old tree back at the destination.
/// After a Linux exchange the old tree sits at the stage path; after the
/// fallback it sits at the backup path.
fn restore_trees(stage: &Path, destination: &Path, backup: &Path) -> Result<(), PromotionError> {
    #[cfg(all(unix, target_os = "linux"))]
    {
        if stage.exists() {
            rename_exchange(stage, destination)
                .map_err(|e| PromotionError::Io(format!("restore exchange: {e}")))?;
            return Ok(());
        }
    }
    if backup.exists() {
        if destination.exists() {
            // Move the bad new tree aside first; restore must not lose it
            // before the old tree is back.
            fs::rename(destination, stage)
                .map_err(|e| PromotionError::Io(format!("restore aside rename: {e}")))?;
        }
        fs::rename(backup, destination)
            .map_err(|e| PromotionError::Io(format!("restore rename: {e}")))?;
        return Ok(());
    }
    Err(PromotionError::Io(
        "no preserved tree to restore from".into(),
    ))
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

fn validate_task_id(task_id: &str) -> Result<(), PromotionError> {
    if task_id.is_empty()
        || !task_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err(PromotionError::InvalidBundle(
            "invalid task id for promotion store".into(),
        ));
    }
    Ok(())
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
        validate_task_id(task_id)?;
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

    /// Heal a dangling Prepared receipt from a crashed promotion. Every
    /// crash point maps to one recovery: the swap never landed (destination
    /// untouched -> RolledBack), the swap landed but the commit record was
    /// lost (-> Committed), or the destination is unknown (-> restore the
    /// preserved old tree and verify, else CorruptState).
    fn recover_prepared(
        &self,
        mut prior: PromotionReceipt,
        destination: &Path,
        stage: &Path,
        backup: &Path,
    ) -> Result<PromotionReceipt, PromotionError> {
        // The fallback window can leave the destination missing with the
        // old tree at the backup path; put it back before judging.
        if !destination.exists() && backup.exists() {
            fs::rename(backup, destination)
                .map_err(|e| PromotionError::Io(format!("recovery rename: {e}")))?;
        }
        let dest_hash = if destination.is_dir() {
            Some(tree_hash(destination)?)
        } else {
            None
        };
        if !prior.staging_hash.is_empty() && dest_hash.as_ref() == Some(&prior.staging_hash) {
            // The swap landed before the crash; only the commit record was
            // lost. The old tree at the scratch path is now disposable.
            remove_tree_quiet(stage);
            remove_tree_quiet(backup);
            prior.state = PromotionState::Committed;
            prior.detail =
                "recovered a crashed promotion: the swap had landed; commit record repaired".into();
            return Ok(prior);
        }
        if dest_hash.as_ref() == Some(&prior.destination_hash_before) {
            // The swap never landed; nothing to undo.
            remove_tree_quiet(stage);
            remove_tree_quiet(backup);
            prior.state = PromotionState::RolledBack;
            prior.detail = "recovered a crashed promotion: destination was never swapped".into();
            return Ok(prior);
        }
        if !destination.exists() && stage.exists() {
            // Post-exchange crash with the destination gone is not
            // reachable through either swap path; treat the scratch tree
            // as the only candidate old tree and move it back.
            let _ = fs::rename(stage, destination);
        }
        let restored = if destination.exists() {
            restore_trees(stage, destination, backup)
        } else {
            Err(PromotionError::Io("no recoverable tree".into()))
        };
        remove_tree_quiet(stage);
        remove_tree_quiet(backup);
        match (restored, tree_hash(destination)) {
            (Ok(()), Ok(restored_hash)) if restored_hash == prior.destination_hash_before => {
                prior.state = PromotionState::RolledBack;
                prior.detail = "recovered a crashed promotion; restore verified".into();
            }
            _ => {
                prior.state = PromotionState::CorruptState;
                prior.detail = "crashed promotion recovery is unverifiable".into();
            }
        }
        Ok(prior)
    }

    /// Full promotion sequence for one task. Durable receipts mark each
    /// boundary; the destination changes only through one atomic swap, and
    /// any failure restores and verifies the old tree. An unverifiable
    /// restore is CorruptState, never best-effort state.
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
        let bundle_hash =
            canonical_hash(&response.content).map_err(|e| PromotionError::Encoding(e.to_string()))?;
        validate_task_id(task_id)?;
        if !destination.is_dir() {
            return Err(PromotionError::Io(
                "promotion destination is not a directory".into(),
            ));
        }
        let parent = destination
            .parent()
            .ok_or_else(|| PromotionError::Io("promotion destination has no parent".into()))?;
        let stage = parent.join(format!(".rex-stage-{task_id}"));
        let backup = parent.join(format!(".rex-backup-{task_id}"));

        // Recover a dangling prepare from a crashed promotion before
        // staging again: a dirty or unknown destination is never a new base.
        if let Some(prior) = self.receipt(task_id)? {
            if prior.state == PromotionState::Prepared {
                let recovered = self.recover_prepared(prior, destination, &stage, &backup)?;
                self.persist_receipt(&recovered)?;
                match recovered.state {
                    PromotionState::CorruptState | PromotionState::Committed => {
                        return Ok(recovered)
                    }
                    _ => {}
                }
            }
        }

        let destination_hash_before = tree_hash(destination)?;
        let mut receipt = PromotionReceipt {
            task_id: task_id.to_string(),
            candidate_id,
            bundle_hash,
            destination_hash_before: destination_hash_before.clone(),
            staging_hash: String::new(),
            gates_rerun: Vec::new(),
            gates_not_rerun: Vec::new(),
            state: PromotionState::Prepared,
            detail: "staged".into(),
            swap: String::new(),
        };

        // Build the complete next tree in a private sibling directory.
        remove_tree_quiet(&stage);
        remove_tree_quiet(&backup);
        copy_tree(destination, &stage)?;

        let outcome: Result<(), PromotionError> = (|| {
            // Confined application: every component of every bundle path is
            // opened descriptor-relative with O_NOFOLLOW.
            let root = dirfd::open_root(&stage)?;
            for file in &bundle.files {
                dirfd::write_file(&root, &file.path, file.content.as_bytes())?;
            }
            for delete in &bundle.deletes {
                dirfd::remove_path(&root, delete)?;
            }
            drop(root);
            fsync_dir(&stage)?;
            let (rerun, not_rerun, failed) = rerun_gates(contract, &stage);
            receipt.gates_rerun = rerun;
            receipt.gates_not_rerun = not_rerun;
            if !failed.is_empty() {
                return Err(PromotionError::GateFailure(failed));
            }
            receipt.staging_hash = tree_hash(&stage)?;
            // A prepare receipt is durable before any swap attempt.
            self.persist_receipt(&receipt)?;
            fsync_dir(&self.dir)?;
            // Compare-and-swap guard: the destination must not have moved.
            if tree_hash(destination)? != destination_hash_before {
                return Err(PromotionError::DestinationChanged);
            }
            // The single mutation of the destination: one atomic exchange.
            receipt.swap = swap_trees(&stage, destination, &backup)?;
            fsync_dir(parent)?;
            if tree_hash(destination)? != receipt.staging_hash {
                return Err(PromotionError::Io(
                    "destination hash mismatch after swap".into(),
                ));
            }
            receipt.state = PromotionState::Committed;
            receipt.detail = format!(
                "promoted via {} swap; destination hash verified",
                receipt.swap
            );
            Ok(())
        })();

        match outcome {
            Ok(()) => {
                // The old tree survives only as scratch; drop it.
                remove_tree_quiet(&stage);
                remove_tree_quiet(&backup);
                self.persist_receipt(&receipt)?;
                Ok(receipt)
            }
            Err(error) => {
                // Roll back. If the swap landed, swap the preserved old
                // tree back; otherwise the destination was never touched.
                let swap_landed = !receipt.staging_hash.is_empty()
                    && matches!(tree_hash(destination), Ok(now) if now == receipt.staging_hash);
                let mut restore_error: Option<PromotionError> = None;
                if swap_landed {
                    if let Err(e) = restore_trees(&stage, destination, &backup) {
                        restore_error = Some(e);
                    }
                }
                remove_tree_quiet(&stage);
                remove_tree_quiet(&backup);
                match (restore_error, tree_hash(destination)) {
                    (None, Ok(restored)) if restored == destination_hash_before => {
                        receipt.state = PromotionState::RolledBack;
                        receipt.detail = format!("rolled back and verified after: {error:?}");
                    }
                    _ => {
                        receipt.state = PromotionState::CorruptState;
                        receipt.detail = format!("rollback unverifiable after: {error:?}");
                    }
                }
                self.persist_receipt(&receipt)?;
                Ok(receipt)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{AcceptanceContract, Obligation, Proof};
    use crate::external_kernel::CandidateResponse;
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

    fn bundle_with_deletes(files: &[(&str, &str)], deletes: &[&str]) -> String {
        serde_json::json!({
            "files": files.iter().map(|(path, content)| serde_json::json!({"path":path,"content":content})).collect::<Vec<_>>(),
            "deletes": deletes,
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
        adapter
            .record_daemon_adversary(
                &request.candidate_id,
                crate::daemon_adversary::DaemonAdversaryReport {
                    defects: Vec::new(),
                    scanned_files: 1,
                    scanned_bytes: 1,
                    tree_hash: "tree".into(),
                    truncated: false,
                },
                canonical_hash(&"daemon-scan").unwrap(),
            )
            .unwrap();
        adapter
            .record_daemon_verifier(
                &request.candidate_id,
                std::collections::BTreeMap::from([("o1".to_string(), true)]),
                canonical_hash(&"daemon-run").unwrap(),
            )
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
        assert!(!receipt.swap.is_empty(), "receipt records the swap method");
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
        // No scratch trees survive a committed promotion.
        assert!(!directory.path().join(".rex-stage-task-1").exists());
        assert!(!directory.path().join(".rex-backup-task-1").exists());
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
        // Deletes are validated exactly like writes.
        assert!(parse_bundle(&bundle_with_deletes(&[], &["../evil.txt"])).is_err());
        assert!(parse_bundle(&bundle_with_deletes(&[], &["/abs.txt"])).is_err());
        assert!(
            parse_bundle(&bundle_with_deletes(&[("a.txt", "x")], &["a.txt"])).is_err(),
            "a path cannot be both written and deleted"
        );
        assert!(parse_bundle(&bundle_with_deletes(&[], &["a.txt", "a.txt"])).is_err());
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
    fn stage_apply_failure_leaves_the_destination_untouched() {
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

    #[cfg(unix)]
    #[test]
    fn symlink_parent_escape_is_impossible() {
        // Audit finding 4's exact exploit: destination holds a symlink to an
        // outside directory and the winning bundle writes through it.
        use std::os::unix::fs::symlink;
        let directory = tempdir().unwrap();
        let destination = directory.path().join("dest");
        let outside = directory.path().join("outside");
        fs::create_dir_all(&destination).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(destination.join("keep.txt"), "original").unwrap();
        symlink(&outside, destination.join("linked")).unwrap();
        let store = PromotionStore::open(directory.path().join("state")).unwrap();
        let adapter = completed_adapter(
            &contract(),
            &bundle(&[("result.txt", "PASS\n"), ("linked/pwned.txt", "escape")]),
        );
        let receipt = store
            .promote("task-5", &adapter, &contract(), &destination)
            .unwrap();
        assert_eq!(receipt.state, PromotionState::Committed);
        // The outside directory was never written.
        assert!(!outside.join("pwned.txt").exists());
        // The symlink is gone from the promoted tree; the path is now a
        // real directory inside the workspace holding the bundle's file.
        let meta = fs::symlink_metadata(destination.join("linked")).unwrap();
        assert!(meta.is_dir() && !meta.file_type().is_symlink());
        assert_eq!(
            fs::read_to_string(destination.join("linked/pwned.txt")).unwrap(),
            "escape"
        );
        assert_eq!(tree_hash(&destination).unwrap(), receipt.staging_hash);
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_relative_writes_never_follow_symlinks() {
        use std::os::unix::fs::symlink;
        let directory = tempdir().unwrap();
        let root_path = directory.path().join("root");
        let outside = directory.path().join("outside");
        fs::create_dir_all(&root_path).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("victim.txt"), "precious").unwrap();
        symlink(outside.join("victim.txt"), root_path.join("file-link")).unwrap();
        symlink(&outside, root_path.join("dir-link")).unwrap();
        let root = dirfd::open_root(&root_path).unwrap();
        // Final component symlink: refused, target untouched.
        match dirfd::write_file(&root, "file-link", b"pwned") {
            Err(PromotionError::SymlinkRefused(_)) => {}
            other => panic!("expected SymlinkRefused, got {other:?}"),
        }
        // Intermediate component symlink: refused, target untouched.
        match dirfd::write_file(&root, "dir-link/pwned.txt", b"pwned") {
            Err(PromotionError::SymlinkRefused(_)) => {}
            other => panic!("expected SymlinkRefused, got {other:?}"),
        }
        assert_eq!(
            fs::read_to_string(outside.join("victim.txt")).unwrap(),
            "precious"
        );
        assert!(!outside.join("pwned.txt").exists());
    }

    #[test]
    fn deletes_are_applied_and_committed() {
        let directory = tempdir().unwrap();
        let destination = directory.path().join("dest");
        fs::create_dir_all(&destination).unwrap();
        fs::write(destination.join("keep.txt"), "original").unwrap();
        fs::write(destination.join("old.txt"), "stale").unwrap();
        fs::create_dir_all(destination.join("obsolete/nested")).unwrap();
        fs::write(destination.join("obsolete/nested/junk.txt"), "junk").unwrap();
        let store = PromotionStore::open(directory.path().join("state")).unwrap();
        let adapter = completed_adapter(
            &contract(),
            &bundle_with_deletes(
                &[("result.txt", "PASS\n")],
                &["old.txt", "obsolete", "already-absent.txt"],
            ),
        );
        let receipt = store
            .promote("task-6", &adapter, &contract(), &destination)
            .unwrap();
        assert_eq!(receipt.state, PromotionState::Committed);
        assert!(!destination.join("old.txt").exists());
        assert!(!destination.join("obsolete").exists());
        assert_eq!(
            fs::read_to_string(destination.join("keep.txt")).unwrap(),
            "original"
        );
        assert_eq!(tree_hash(&destination).unwrap(), receipt.staging_hash);
    }

    #[test]
    fn crashed_promotion_before_swap_recovers_and_promotes() {
        // Prepare receipt on disk, destination untouched: exactly what a
        // crash between the durable prepare and the swap leaves.
        let directory = tempdir().unwrap();
        let destination = directory.path().join("dest");
        fs::create_dir_all(&destination).unwrap();
        fs::write(destination.join("keep.txt"), "original").unwrap();
        let before = tree_hash(&destination).unwrap();
        let store = PromotionStore::open(directory.path().join("state")).unwrap();
        store
            .persist_receipt(&PromotionReceipt {
                task_id: "task-7".into(),
                candidate_id: "crashed".into(),
                bundle_hash: "b".repeat(64),
                destination_hash_before: before,
                staging_hash: "f".repeat(64),
                gates_rerun: vec![],
                gates_not_rerun: vec![],
                state: PromotionState::Prepared,
                detail: "staged".into(),
                swap: String::new(),
            })
            .unwrap();
        let adapter = completed_adapter(&contract(), &bundle(&[("result.txt", "PASS\n")]));
        let receipt = store
            .promote("task-7", &adapter, &contract(), &destination)
            .unwrap();
        assert_eq!(receipt.state, PromotionState::Committed);
        assert_eq!(
            fs::read_to_string(destination.join("result.txt")).unwrap(),
            "PASS\n"
        );
        assert_eq!(
            fs::read_to_string(destination.join("keep.txt")).unwrap(),
            "original"
        );
    }

    #[test]
    fn crashed_promotion_after_swap_recovers_to_committed() {
        // The swap landed but the commit record was lost: destination holds
        // the new tree, the scratch path holds the old one, and the durable
        // receipt still says Prepared.
        let directory = tempdir().unwrap();
        let destination = directory.path().join("dest");
        fs::create_dir_all(&destination).unwrap();
        fs::write(destination.join("keep.txt"), "original").unwrap();
        let before = tree_hash(&destination).unwrap();
        let store = PromotionStore::open(directory.path().join("state")).unwrap();
        let stage = directory.path().join(".rex-stage-task-8");
        copy_tree(&destination, &stage).unwrap();
        fs::write(stage.join("result.txt"), "PASS\n").unwrap();
        let staging_hash = tree_hash(&stage).unwrap();
        store
            .persist_receipt(&PromotionReceipt {
                task_id: "task-8".into(),
                candidate_id: "crashed".into(),
                bundle_hash: "b".repeat(64),
                destination_hash_before: before,
                staging_hash: staging_hash.clone(),
                gates_rerun: vec!["o1".into()],
                gates_not_rerun: vec![],
                state: PromotionState::Prepared,
                detail: "staged".into(),
                swap: String::new(),
            })
            .unwrap();
        // Simulate the landed swap: new tree at destination, old at stage.
        let holding = directory.path().join("holding");
        fs::rename(&destination, &holding).unwrap();
        fs::rename(&stage, &destination).unwrap();
        fs::rename(&holding, &stage).unwrap();
        assert_eq!(tree_hash(&destination).unwrap(), staging_hash);
        let adapter = completed_adapter(&contract(), &bundle(&[("result.txt", "PASS\n")]));
        let receipt = store
            .promote("task-8", &adapter, &contract(), &destination)
            .unwrap();
        assert_eq!(receipt.state, PromotionState::Committed);
        assert!(receipt.detail.contains("swap had landed"));
        assert_eq!(
            fs::read_to_string(destination.join("result.txt")).unwrap(),
            "PASS\n"
        );
        // The preserved old tree was cleaned up.
        assert!(!stage.exists());
    }

    #[test]
    fn crashed_promotion_with_unverifiable_restore_is_corrupt_state() {
        // A prepared promotion whose destination no longer matches either
        // known hash, with no preserved old tree anywhere: fail closed.
        let directory = tempdir().unwrap();
        let destination = directory.path().join("dest");
        fs::create_dir_all(&destination).unwrap();
        fs::write(destination.join("keep.txt"), "original").unwrap();
        let before = tree_hash(&destination).unwrap();
        let store = PromotionStore::open(directory.path().join("state")).unwrap();
        store
            .persist_receipt(&PromotionReceipt {
                task_id: "task-9".into(),
                candidate_id: "crashed".into(),
                bundle_hash: "b".repeat(64),
                destination_hash_before: before,
                staging_hash: "f".repeat(64),
                gates_rerun: vec![],
                gates_not_rerun: vec![],
                state: PromotionState::Prepared,
                detail: "staged".into(),
                swap: String::new(),
            })
            .unwrap();
        fs::write(destination.join("junk.txt"), "foreign dirt").unwrap();
        fs::write(destination.join("keep.txt"), "dirtied").unwrap();
        let adapter = completed_adapter(&contract(), &bundle(&[("result.txt", "PASS\n")]));
        let receipt = store
            .promote("task-9", &adapter, &contract(), &destination)
            .unwrap();
        assert_eq!(receipt.state, PromotionState::CorruptState);
        // Fail closed: nothing was promoted onto an unverifiable base.
        assert!(!destination.join("result.txt").exists());
        assert_eq!(
            store.receipt("task-9").unwrap().unwrap().state,
            PromotionState::CorruptState
        );
    }
}
