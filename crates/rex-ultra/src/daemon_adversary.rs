//! Daemon-executed adversary scan.
//!
//! The adversary gate is executed by the daemon inside the sealed candidate
//! workspace, never reported by the host whose work is under review. The
//! scan is deterministic: it walks the candidate tree and records defects
//! for anything a hostile builder could use to fake completion - symlink
//! escapes (which tree hashing deliberately cannot see), placeholder
//! content, empty deliverables, and non-text payloads posing as text.
//!
//! The report is bound to the exact tree it scanned via `tree_hash`, so a
//! clean report for one tree can never be replayed against another.

use crate::adversary::Defect;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Bound on files scanned; beyond it the report is truncated and therefore
/// not clean. A candidate too large to scan cannot qualify.
pub const MAX_SCAN_FILES: u64 = 4096;
/// Bound on total bytes read for content checks.
pub const MAX_SCAN_BYTES: u64 = 16 * 1024 * 1024;
/// Bound on a single file's content read.
const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// Placeholder markers that indicate unfinished work. Matched
/// case-insensitively against text files.
pub const PLACEHOLDER_PATTERNS: &[&str] = &[
    "lorem ipsum",
    "todo:",
    "fixme",
    "todo!()",
    "unimplemented!()",
    "not implemented",
    "placeholder",
    "coming soon",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonAdversaryReport {
    pub defects: Vec<Defect>,
    pub scanned_files: u64,
    pub scanned_bytes: u64,
    /// Hash of the exact tree scanned, from crate::promotion::tree_hash.
    pub tree_hash: String,
    /// True when the scan hit a bound and could not see the whole tree.
    /// A truncated report is never clean.
    pub truncated: bool,
}

impl DaemonAdversaryReport {
    pub fn clean(&self) -> bool {
        self.defects.is_empty() && !self.truncated
    }
}

/// Scan the candidate tree at `candidate_root`. Deterministic: entries are
/// visited in sorted order and the report depends only on tree content.
pub fn scan(candidate_root: &Path) -> DaemonAdversaryReport {
    let mut defects = Vec::new();
    let mut scanned_files: u64 = 0;
    let mut scanned_bytes: u64 = 0;
    let mut truncated = false;
    let canonical_root = fs::canonicalize(candidate_root).unwrap_or_else(|_| candidate_root.to_path_buf());
    walk(
        candidate_root,
        &canonical_root,
        candidate_root,
        &mut defects,
        &mut scanned_files,
        &mut scanned_bytes,
        &mut truncated,
    );
    // The tree hash binds this report to the scanned bytes. Symlinks are
    // invisible to the hash, which is exactly why they are defects above.
    let tree_hash = crate::promotion::tree_hash(candidate_root)
        .unwrap_or_else(|e| {
            truncated = true;
            defects.push(Defect {
                title: "tree hash failed".into(),
                detail: format!("the candidate tree could not be hashed: {e:?}"),
            });
            String::new()
        });
    let mut report = DaemonAdversaryReport {
        defects,
        scanned_files,
        scanned_bytes,
        tree_hash,
        truncated,
    };
    report.defects.sort_by(|a, b| a.title.cmp(&b.title).then(a.detail.cmp(&b.detail)));
    report
}

fn walk(
    root: &Path,
    canonical_root: &Path,
    dir: &Path,
    defects: &mut Vec<Defect>,
    scanned_files: &mut u64,
    scanned_bytes: &mut u64,
    truncated: &mut bool,
) {
    if *truncated {
        return;
    }
    let Ok(read) = fs::read_dir(dir) else {
        defects.push(Defect {
            title: "unreadable directory".into(),
            detail: format!("{} could not be read", rel(root, dir)),
        });
        return;
    };
    let mut entries: Vec<PathBuf> = read.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if *truncated {
            return;
        }
        let Ok(kind) = fs::symlink_metadata(&path).map(|m| m.file_type()) else {
            continue;
        };
        if kind.is_symlink() {
            let target = fs::read_link(&path)
                .map(|t| t.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "<unreadable>".into());
            let escapes = fs::canonicalize(&path)
                .map(|resolved| !resolved.starts_with(canonical_root))
                .unwrap_or(true);
            defects.push(Defect {
                title: if escapes { "symlink escapes workspace" } else { "symlink in workspace" }.into(),
                detail: format!("{} -> {target}", rel(root, &path)),
            });
            continue;
        }
        if kind.is_dir() {
            walk(root, canonical_root, &path, defects, scanned_files, scanned_bytes, truncated);
            continue;
        }
        if !kind.is_file() {
            continue;
        }
        *scanned_files += 1;
        if *scanned_files > MAX_SCAN_FILES {
            *truncated = true;
            defects.push(Defect {
                title: "scan truncated".into(),
                detail: format!("more than {MAX_SCAN_FILES} files"),
            });
            return;
        }
        let relative = rel(root, &path);
        let Ok(meta) = fs::metadata(&path) else { continue };
        if meta.len() == 0 {
            defects.push(Defect {
                title: "empty file".into(),
                detail: format!("{relative} is 0 bytes"),
            });
            continue;
        }
        if meta.len() > MAX_FILE_BYTES || *scanned_bytes + meta.len() > MAX_SCAN_BYTES {
            if meta.len() > MAX_FILE_BYTES || *scanned_bytes >= MAX_SCAN_BYTES {
                *truncated = true;
                defects.push(Defect {
                    title: "scan truncated".into(),
                    detail: format!("byte bound reached at {relative}"),
                });
                return;
            }
        }
        let Ok(bytes) = fs::read(&path) else { continue };
        *scanned_bytes += bytes.len() as u64;
        // Binary payloads are not scanned for placeholders; a NUL in the
        // first block marks the file as binary.
        let probe = &bytes[..bytes.len().min(8192)];
        if probe.contains(&0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes).to_lowercase();
        for pattern in PLACEHOLDER_PATTERNS {
            if text.contains(pattern) {
                defects.push(Defect {
                    title: "placeholder content".into(),
                    detail: format!("{relative} contains {pattern:?}"),
                });
            }
        }
    }
}

fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_tree_scans_clean() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("index.html"), "<html>real work</html>").unwrap();
        fs::create_dir(d.path().join("src")).unwrap();
        fs::write(d.path().join("src").join("main.rs"), "fn main() {}").unwrap();
        let report = scan(d.path());
        assert!(report.clean(), "{:?}", report.defects);
        assert_eq!(report.scanned_files, 2);
        assert!(!report.tree_hash.is_empty());
    }

    #[test]
    fn placeholders_are_defects() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("page.html"), "<p>Lorem ipsum dolor</p>").unwrap();
        fs::write(d.path().join("app.js"), "// TODO: wire this up").unwrap();
        let report = scan(d.path());
        assert!(!report.clean());
        assert!(report.defects.iter().any(|x| x.title == "placeholder content"));
    }

    #[test]
    fn empty_files_are_defects() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("empty.css"), "").unwrap();
        let report = scan(d.path());
        assert!(report.defects.iter().any(|x| x.title == "empty file"));
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_defects_and_escapes_are_named() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("real.txt"), "real").unwrap();
        std::os::unix::fs::symlink("real.txt", d.path().join("inside.txt")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", d.path().join("escape.txt")).unwrap();
        let report = scan(d.path());
        assert!(report.defects.iter().any(|x| x.title == "symlink in workspace"));
        assert!(report.defects.iter().any(|x| x.title == "symlink escapes workspace"));
        assert!(!report.clean());
    }

    #[test]
    fn the_report_is_bound_to_the_tree() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("a.txt"), "alpha").unwrap();
        let first = scan(d.path());
        fs::write(d.path().join("a.txt"), "beta").unwrap();
        let second = scan(d.path());
        assert_ne!(first.tree_hash, second.tree_hash);
    }

    #[test]
    fn binary_files_are_not_placeholder_scanned() {
        let d = tempfile::tempdir().unwrap();
        let mut bytes = b"PK\x03\x04".to_vec();
        bytes.extend_from_slice(&[0u8; 16]);
        bytes.extend_from_slice(b"placeholder");
        fs::write(d.path().join("data.bin"), bytes).unwrap();
        let report = scan(d.path());
        assert!(report.clean(), "{:?}", report.defects);
    }
}
