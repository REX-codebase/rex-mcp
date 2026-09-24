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

/// One `.gitignore` rule. As in git, the last rule that matches a path
/// decides: a `!` rule re-includes what an earlier rule ignored. A file
/// inside an ignored folder stays ignored because the walk never enters
/// that folder (git cannot re-include it either).
#[derive(Debug, Clone)]
struct Rule {
    glob: Regex,
    dir_only: bool,
    anchored: bool,
    /// `!pattern`: matching paths are not ignored.
    negate: bool,
    /// Folder of the `.gitignore` the rule came from, relative to the
    /// workspace root with a trailing `/` ("" for the root file). The rule
    /// only applies below it, matched against the rest of the path.
    base: String,
}

/// Ignore rules from the workspace root `.gitignore` and, while walking,
/// the `.gitignore` files of the folders below it (`Ignore::enter`), the
/// way git and ripgrep read them. opencode's glob and grep and Hermes's
/// search both run ripgrep, so nested files count there too.
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
        Self::parse_in(text, "")
    }

    /// Rules of a `.gitignore` that sits in folder `base` (relative to the
    /// workspace root, "" for the root).
    pub fn parse_in(text: &str, base: &str) -> Self {
        let base = if base.is_empty() {
            String::new()
        } else {
            format!("{}/", base.trim_end_matches('/'))
        };
        let mut rules = Vec::new();
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            // `\!` and `\#` start a pattern with a literal `!` or `#`
            let (negate, line) = match line.strip_prefix('!') {
                Some(rest) => (true, rest),
                None => (false, line),
            };
            let line = line
                .strip_prefix("\\!")
                .map(|r| format!("!{r}"))
                .or_else(|| line.strip_prefix("\\#").map(|r| format!("#{r}")))
                .unwrap_or_else(|| line.to_string());
            let line = line.as_str();
            let dir_only = line.ends_with('/');
            let pat = line.trim_end_matches('/');
            let anchored = pat.starts_with('/') || pat.trim_start_matches('/').contains('/');
            let pat = pat.trim_start_matches('/');
            if pat.is_empty() {
                continue;
            }
            let Some(glob) = glob_to_regex(pat) else {
                continue;
            };
            rules.push(Rule {
                glob,
                dir_only,
                anchored,
                negate,
                base: base.clone(),
            });
        }
        Self { rules }
    }

    /// These rules plus those of `dir/.gitignore`, when `dir` (below
    /// `root`) has one; `None` when it has none, so callers keep sharing
    /// the current rules.
    pub fn enter(&self, root: &Path, dir: &Path) -> Option<Self> {
        let text = fs::read_to_string(dir.join(".gitignore")).ok()?;
        let rel = rel_path(root, dir);
        if rel.is_empty() {
            return None;
        }
        let mut next = self.clone();
        next.rules.extend(Self::parse_in(&text, &rel).rules);
        Some(next)
    }

    /// Whether `rel` (relative to the workspace root) is ignored.
    pub fn is_ignored(&self, rel: &str, is_dir: bool) -> bool {
        let hit = |r: &Rule| {
            if r.dir_only && !is_dir {
                return false;
            }
            let Some(rest) = rel.strip_prefix(r.base.as_str()) else {
                return false;
            };
            let name = rest.rsplit('/').next().unwrap_or(rest);
            if r.anchored {
                r.glob.is_match(rest)
            } else {
                r.glob.is_match(name) || r.glob.is_match(rest)
            }
        };
        // deeper files' rules come later, so they win over the root's
        self.rules
            .iter()
            .rev()
            .find(|r| hit(r))
            .is_some_and(|r| !r.negate)
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
            match ignore.enter(root, &path) {
                Some(inner) => walk_inner(root, &path, &inner, cap, depth + 1, out, capped),
                None => walk_inner(root, &path, ignore, cap, depth + 1, out, capped),
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    struct Tmp(PathBuf);
    impl Tmp {
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn tree(files: &[(&str, &str)]) -> Tmp {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let t = Tmp(std::env::temp_dir().join(format!(
            "rex-walk-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        )));
        fs::create_dir_all(t.path()).unwrap();
        for (p, body) in files {
            let f = t.path().join(p);
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(f, body).unwrap();
        }
        t
    }

    fn listed(root: &Path) -> Vec<String> {
        let (files, _) = walk_files(root, root, &Ignore::load(root), 1000);
        files.iter().map(|f| rel_path(root, f)).collect()
    }

    #[test]
    fn nested_gitignore_applies_below_its_folder_only() {
        let t = tree(&[
            (".gitignore", "*.log\n"),
            ("web/.gitignore", "/out/\ncache.json\nsrc/gen.ts\n"),
            ("web/out/bundle.js", "x"),
            ("web/cache.json", "x"),
            ("web/src/cache.json", "x"),
            ("web/src/gen.ts", "x"),
            ("web/src/app.ts", "x"),
            ("web/deep/out/keep.js", "x"),
            ("web/a.log", "x"),
            ("api/cache.json", "x"),
            ("api/out/keep.txt", "x"),
            ("api/src/gen.ts", "x"),
        ]);
        assert_eq!(
            listed(t.path()),
            [
                ".gitignore",
                "api/cache.json",
                "api/out/keep.txt",
                "api/src/gen.ts",
                "web/.gitignore",
                "web/deep/out/keep.js",
                "web/src/app.ts",
            ]
        );
    }

    #[test]
    fn rules_stack_through_several_levels() {
        let t = tree(&[
            ("a/.gitignore", "*.tmp\n"),
            ("a/b/.gitignore", "secret/\n"),
            ("a/b/x.tmp", "x"),
            ("a/b/secret/k.txt", "x"),
            ("a/b/c/secret/k.txt", "x"),
            ("a/secret/k.txt", "x"),
            ("a/b/ok.rs", "x"),
            ("z.tmp", "x"),
        ]);
        assert_eq!(
            listed(t.path()),
            [
                "a/.gitignore",
                "a/b/.gitignore",
                "a/b/ok.rs",
                "a/secret/k.txt",
                "z.tmp",
            ]
        );
    }

    #[test]
    fn negation_re_includes_and_the_last_match_wins() {
        let t = tree(&[
            (
                ".gitignore",
                "*.log\n!keep.log\nout/\n!out/\ndocs/*.md\n!docs/README.md\n\\!bang.txt\n\\#hash.txt\n",
            ),
            ("a.log", "x"),
            ("sub/keep.log", "x"),
            ("sub/b.log", "x"),
            ("out/app.js", "x"),
            ("docs/guide.md", "x"),
            ("docs/README.md", "x"),
            ("!bang.txt", "x"),
            ("#hash.txt", "x"),
            ("plain.txt", "x"),
            ("web/.gitignore", "!*.log\nkeep.log\n"),
            ("web/c.log", "x"),
            ("web/keep.log", "x"),
        ]);
        assert_eq!(
            listed(t.path()),
            [
                ".gitignore",
                "docs/README.md",
                "out/app.js",
                "plain.txt",
                "sub/keep.log",
                "web/.gitignore",
                "web/c.log",
            ]
        );
        // a file inside an ignored folder cannot be re-included
        let t = tree(&[(".gitignore", "gen/\n!gen/keep.rs\n"), ("gen/keep.rs", "x")]);
        assert_eq!(listed(t.path()), [".gitignore"]);
        // a lone negation ignores nothing
        let ig = Ignore::parse("!x.txt\n");
        assert!(!ig.is_ignored("x.txt", false));
        assert!(!ig.is_ignored("y.txt", false));
    }

    #[test]
    fn rule_base_is_matched_on_whole_folder_names() {
        let ig = Ignore::parse_in("x.txt\n", "web");
        assert!(ig.is_ignored("web/x.txt", false));
        assert!(ig.is_ignored("web/sub/x.txt", false));
        assert!(!ig.is_ignored("webapp/x.txt", false));
        assert!(!ig.is_ignored("x.txt", false));
        // the folder itself is not matched by its own rules
        let dir = Ignore::parse_in("*\n", "web/");
        assert!(!dir.is_ignored("web", true));
        assert!(dir.is_ignored("web/a", false));
        // a folder with no .gitignore shares the current rules
        let t = tree(&[("plain/a.txt", "x")]);
        assert!(Ignore::default()
            .enter(t.path(), &t.path().join("plain"))
            .is_none());
        assert!(Ignore::default().enter(t.path(), t.path()).is_none());
        // the root .gitignore is loaded once, not again on entering the root
        fs::write(t.path().join(".gitignore"), "*.txt\n").unwrap();
        assert!(Ignore::default().enter(t.path(), t.path()).is_none());
    }
}
