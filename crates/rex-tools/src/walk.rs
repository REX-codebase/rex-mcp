//! Workspace walking for search: glob matching, ignore rules, and a
//! deterministic file walk that never descends into dependency or build
//! output (`.git`, `node_modules`, `target`, ...) or `.gitignore`d paths.
//!
//! Without this, a content search in a built Rust or JS repo spends its
//! whole file budget inside `target/` or `node_modules/` and can return
//! nothing from the real source.

use regex::Regex;
use std::fs;
use std::path::{Path, PathBuf};

/// Directories that are never useful to search and often enormous.
pub const ALWAYS_SKIP: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "target",
    ".next",
    ".nuxt",
    ".svelte-kit",
    "__pycache__",
    ".venv",
    "venv",
    ".mypy_cache",
    ".pytest_cache",
    ".tox",
    ".gradle",
    ".turbo",
    ".cache",
    "dist",
    "build",
    "coverage",
];

/// Compile a glob into an anchored regex over `/`-separated relative paths.
/// Supports `*` (within a segment), `?`, `**` (any depth), `[...]` classes
/// and `{a,b}` alternation. A pattern without `/` matches the basename.
pub fn glob_to_regex(glob: &str) -> Option<Regex> {
    let glob = glob.trim();
    if glob.is_empty() || glob.len() > 512 {
        return None;
    }
    let body = glob.trim_start_matches("./").trim_start_matches('/');
    let chars: Vec<char> = body.chars().collect();
    let mut re = String::from("^");
    let mut i = 0;
    let mut depth = 0usize;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '*' => {
                if i + 1 < chars.len() && chars[i + 1] == '*' {
                    // `**/` = zero or more directories; bare `**` = anything.
                    if i + 2 < chars.len() && chars[i + 2] == '/' {
                        re.push_str("(?:.*/)?");
                        i += 3;
                    } else {
                        re.push_str(".*");
                        i += 2;
                    }
                    continue;
                }
                re.push_str("[^/]*");
            }
            '?' => re.push_str("[^/]"),
            '[' => {
                let close = chars[i + 1..].iter().position(|c| *c == ']')?;
                let class: String = chars[i + 1..i + 1 + close].iter().collect();
                let class = class.replacen('!', "^", usize::from(class.starts_with('!')));
                re.push('[');
                re.push_str(&class.replace('\\', "\\\\"));
                re.push(']');
                i += close + 2;
                continue;
            }
            '{' => {
                depth += 1;
                re.push_str("(?:");
            }
            '}' if depth > 0 => {
                depth -= 1;
                re.push(')');
            }
            ',' if depth > 0 => re.push('|'),
            other => re.push_str(&regex::escape(&other.to_string())),
        }
        i += 1;
    }
    if depth != 0 {
        return None;
    }
    re.push('$');
    Regex::new(&re).ok()
}

/// A compiled glob plus whether it applies to the basename only.
#[derive(Debug, Clone)]
pub struct Glob {
    re: Regex,
    basename_only: bool,
}

impl Glob {
    pub fn new(pattern: &str) -> Option<Self> {
        Some(Self {
            re: glob_to_regex(pattern)?,
            basename_only: !pattern.contains('/'),
        })
    }

    /// Match against a `/`-separated path relative to the search base.
    pub fn matches(&self, rel: &str) -> bool {
        if self.basename_only {
            let base = rel.rsplit('/').next().unwrap_or(rel);
            self.re.is_match(base)
        } else {
            self.re.is_match(rel)
        }
    }
}

/// One `.gitignore` rule. Negations (`!`) are not evaluated; when a file
/// has any, only directory rules are kept (see `Ignore::parse`) so a
/// re-included file is never hidden from search.
#[derive(Debug, Clone)]
struct Rule {
    glob: Regex,
    dir_only: bool,
    anchored: bool,
}

/// Ignore rules from the workspace root `.gitignore`.
#[derive(Debug, Clone, Default)]
pub struct Ignore {
    rules: Vec<Rule>,
}

impl Ignore {
    pub fn load(root: &Path) -> Self {
        let text = fs::read_to_string(root.join(".gitignore")).unwrap_or_default();
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Self {
        let lines: Vec<&str> = text.lines().map(str::trim).collect();
        let has_negation = lines.iter().any(|l| l.starts_with('!'));
        let mut rules = Vec::new();
        for line in lines {
            if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
                continue;
            }
            let dir_only = line.ends_with('/');
            let pat = line.trim_end_matches('/');
            let anchored = pat.starts_with('/') || pat.trim_start_matches('/').contains('/');
            let pat = pat.trim_start_matches('/');
            if pat.is_empty() {
                continue;
            }
            // With negations present we cannot know which positive rules
            // are overridden, so only keep directory rules; a hidden file
            // is worse than a slower search.
            if has_negation && !dir_only {
                continue;
            }
            let Some(glob) = glob_to_regex(pat) else {
                continue;
            };
            rules.push(Rule {
                glob,
                dir_only,
                anchored,
            });
        }
        Self { rules }
    }

    /// Whether `rel` (relative to the workspace root) is ignored.
    pub fn is_ignored(&self, rel: &str, is_dir: bool) -> bool {
        let base = rel.rsplit('/').next().unwrap_or(rel);
        self.rules.iter().any(|r| {
            if r.dir_only && !is_dir {
                return false;
            }
            if r.anchored {
                r.glob.is_match(rel)
            } else {
                r.glob.is_match(base) || r.glob.is_match(rel)
            }
        })
    }
}

/// Walk `base` depth-first in sorted order, skipping symlinks, the
/// `ALWAYS_SKIP` directories and root `.gitignore` matches. `root` is the
/// workspace root that ignore rules are relative to. Stops after `cap`
/// files; returns whether the cap was hit.
pub fn walk_files(root: &Path, base: &Path, ignore: &Ignore, cap: usize) -> (Vec<PathBuf>, bool) {
    let mut out = Vec::new();
    let mut capped = false;
    walk_inner(root, base, ignore, cap, 0, &mut out, &mut capped);
    (out, capped)
}

fn walk_inner(
    root: &Path,
    dir: &Path,
    ignore: &Ignore,
    cap: usize,
    depth: usize,
    out: &mut Vec<PathBuf>,
    capped: &mut bool,
) {
    if depth > 32 || *capped {
        return;
    }
    let Ok(read) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = read.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = rel_path(root, &path);
        if ft.is_dir() {
            if ALWAYS_SKIP.contains(&name.as_str()) || ignore.is_ignored(&rel, true) {
                continue;
            }
            walk_inner(root, &path, ignore, cap, depth + 1, out, capped);
            if *capped {
                return;
            }
        } else if ft.is_file() {
            if ignore.is_ignored(&rel, false) {
                continue;
            }
            if out.len() >= cap {
                *capped = true;
                return;
            }
            out.push(path);
        }
    }
}

/// `/`-separated path of `p` relative to `root`.
pub fn rel_path(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}
