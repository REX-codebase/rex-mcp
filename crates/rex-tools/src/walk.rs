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

/// A .gitignore pattern with git's `**` rules applied: `**` counts as
/// "any depth" only as a whole path segment (`**/x`, `x/**`, `a/**/b`);
/// anywhere else it is a plain `*` that stays inside one name
/// (gitignore(5): "Other consecutive asterisks are considered regular
/// asterisks"). So `a**b` never crosses a `/`.
fn git_stars(pat: &str) -> String {
    let chars: Vec<char> = pat.chars().collect();
    let mut out = String::with_capacity(pat.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '*' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && chars[i] == '*' {
            i += 1;
        }
        let whole_segment =
            (start == 0 || chars[start - 1] == '/') && (i == chars.len() || chars[i] == '/');
        out.push_str(if i - start >= 2 && whole_segment {
            "**"
        } else {
            "*"
        });
    }
    out
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

/// Where git looks for the user's config and global excludes.
#[derive(Debug, Clone, Default)]
pub struct GitEnv {
    pub home: Option<PathBuf>,
    /// `$XDG_CONFIG_HOME`
    pub xdg_config: Option<PathBuf>,
    /// `$GIT_CONFIG_GLOBAL`, which replaces both global config files
    pub config_global: Option<PathBuf>,
}

impl GitEnv {
    pub fn from_process() -> Self {
        let var = |k: &str| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        Self {
            home: var("HOME"),
            xdg_config: var("XDG_CONFIG_HOME"),
            config_global: var("GIT_CONFIG_GLOBAL"),
        }
    }

    fn xdg_git(&self) -> Option<PathBuf> {
        self.xdg_config
            .clone()
            .or_else(|| self.home.as_ref().map(|h| h.join(".config")))
            .map(|d| d.join("git"))
    }
}

/// The value of `core.excludesFile` in one git config file, if set there.
/// A small reader: `[core]` section, `excludesfile = value` in any case,
/// optional quotes, `#`/`;` comments; the last setting wins.
fn config_excludes(text: &str) -> Option<String> {
    let mut in_core = false;
    let mut found = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            let name = line.trim_start_matches('[').split([']', ' ', '"']).next();
            in_core = name.is_some_and(|n| n.eq_ignore_ascii_case("core"));
            continue;
        }
        if !in_core || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("excludesfile") {
            let value = value.trim();
            let value = if let Some(q) = value.strip_prefix('"') {
                q.split('"').next().unwrap_or("")
            } else {
                value.split([' ', '\t', '#', ';']).next().unwrap_or("")
            };
            found = Some(value.to_string());
        }
    }
    found.filter(|v| !v.is_empty())
}

/// The global excludes file git would use for the repository at `root`:
/// `core.excludesFile` from the repository config, else the global config
/// (`$GIT_CONFIG_GLOBAL`, or `~/.gitconfig` over the XDG `git/config`),
/// else the XDG default `git/ignore`. A leading `~/` is the home folder;
/// a relative path is taken from the repository root.
pub fn global_excludes_file(root: &Path, env: &GitEnv) -> Option<PathBuf> {
    let read = |p: PathBuf| fs::read_to_string(p).ok();
    let mut configs = vec![read(root.join(".git").join("config"))];
    match &env.config_global {
        Some(p) => configs.push(read(p.clone())),
        None => {
            configs.push(env.home.as_ref().and_then(|h| read(h.join(".gitconfig"))));
            configs.push(env.xdg_git().and_then(|d| read(d.join("config"))));
        }
    }
    let set = configs
        .into_iter()
        .flatten()
        .find_map(|t| config_excludes(&t));
    match set {
        Some(v) => {
            if let Some(rest) = v.strip_prefix("~/") {
                env.home.as_ref().map(|h| h.join(rest))
            } else {
                let p = PathBuf::from(&v);
                Some(if p.is_absolute() { p } else { root.join(p) })
            }
        }
        None => env.xdg_git().map(|d| d.join("ignore")),
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
    /// Rules of the repository's `.git/info/exclude` (local ignores that
    /// are not committed) followed by the root `.gitignore`. Later rules
    /// win, so `.gitignore` files override `info/exclude`, as in git.
    /// A `.git` that is a file (a worktree or submodule link) is not
    /// followed.
    ///
    /// Inside a git repository the user's global excludes file
    /// (`core.excludesFile`, by default `~/.config/git/ignore`) comes
    /// first, with the lowest priority, as in git and ripgrep. opencode's
    /// snapshot test checks that a global `excludesFile` is honoured
    /// (`test/snapshot/snapshot.test.ts`).
    pub fn load(root: &Path) -> Self {
        Self::load_with(root, &GitEnv::from_process())
    }

    /// `load` with the home and config locations given, for tests.
    pub fn load_with(root: &Path, env: &GitEnv) -> Self {
        let mut rules = Vec::new();
        if root.join(".git").exists() {
            if let Some(path) = global_excludes_file(root, env) {
                let text = fs::read_to_string(path).unwrap_or_default();
                rules.extend(Self::parse(&text).rules);
            }
        }
        let exclude =
            fs::read_to_string(root.join(".git").join("info").join("exclude")).unwrap_or_default();
        let text = fs::read_to_string(root.join(".gitignore")).unwrap_or_default();
        rules.extend(Self::parse(&exclude).rules);
        rules.extend(Self::parse(&text).rules);
        Self { rules }
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
            let Some(glob) = glob_to_regex(&git_stars(pat)) else {
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
            // a rule without `/` matches the name at any depth
            r.glob.is_match(if r.anchored { rest } else { name })
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
    fn double_star_inside_a_name_stays_in_one_name() {
        let t = tree(&[
            (".gitignore", "a**b\nlogs/**\nx/c**d\n"),
            ("axyb", "x"),
            ("x/cqd", "x"),
            ("x/c/q/d", "x"),
            ("sub/aqqb", "x"),
            ("a/x/b", "x"),
            ("logs/deep/x.txt", "x"),
            ("sub/logs/y.txt", "x"),
        ]);
        assert_eq!(
            listed(t.path()),
            [".gitignore", "a/x/b", "sub/logs/y.txt", "x/c/q/d"]
        );
        for (pat, want) in [
            ("a**b", "a*b"),
            ("**/x", "**/x"),
            ("x/**", "x/**"),
            ("a/**/b", "a/**/b"),
            ("***", "**"),
            ("a/***b", "a/*b"),
            ("b**/c", "b*/c"),
            ("*", "*"),
            ("**", "**"),
            ("x", "x"),
        ] {
            assert_eq!(git_stars(pat), want, "{pat}");
        }
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
    fn global_excludes_file_is_found_like_git() {
        let t = tree(&[("repo/.git/HEAD", "x")]);
        let root = t.path().join("repo");
        let home = t.path().join("home");
        let env = |xdg: Option<&str>, global: Option<&str>| GitEnv {
            home: Some(home.clone()),
            xdg_config: xdg.map(|x| t.path().join(x)),
            config_global: global.map(|g| t.path().join(g)),
        };
        let write = |p: &str, body: &str| {
            let f = t.path().join(p);
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(f, body).unwrap();
        };
        // nothing set: the XDG default, from HOME or XDG_CONFIG_HOME
        assert_eq!(
            global_excludes_file(&root, &env(None, None)),
            Some(home.join(".config/git/ignore"))
        );
        assert_eq!(
            global_excludes_file(&root, &env(Some("xdg"), None)),
            Some(t.path().join("xdg/git/ignore"))
        );
        assert_eq!(global_excludes_file(&root, &GitEnv::default()), None);
        // the XDG config file sets it
        write(
            "home/.config/git/config",
            "[core]\n\texcludesFile = /x/xdg.ignore\n",
        );
        assert_eq!(
            global_excludes_file(&root, &env(None, None)),
            Some(PathBuf::from("/x/xdg.ignore"))
        );
        // ~/.gitconfig wins over it; ~/ is the home folder; other sections
        // and comments do not count; the last setting wins
        write(
            "home/.gitconfig",
            "[core]\n; excludesfile = /nope\n\tEXCLUDESFILE = /first\n\texcludesfile = ~/g.ignore # mine\n[user]\n\texcludesfile = /wrong\n",
        );
        assert_eq!(
            global_excludes_file(&root, &env(None, None)),
            Some(home.join("g.ignore"))
        );
        // GIT_CONFIG_GLOBAL replaces both global files
        write(
            "alt.gitconfig",
            "[core]\n\texcludesfile = \"/a b/alt.ignore\"\n",
        );
        assert_eq!(
            global_excludes_file(&root, &env(None, Some("alt.gitconfig"))),
            Some(PathBuf::from("/a b/alt.ignore"))
        );
        // the repository config wins over all; relative is from the root
        write(
            "repo/.git/config",
            "[core]\n\tbare = false\n\texcludesfile = local.ignore\n",
        );
        assert_eq!(
            global_excludes_file(&root, &env(None, Some("alt.gitconfig"))),
            Some(root.join("local.ignore"))
        );
    }

    #[test]
    fn global_excludes_have_the_lowest_priority_and_need_a_repo() {
        let t = tree(&[
            ("home/.config/git/ignore", "global.tmp\n*.log\nnotes.md\n"),
            ("repo/.git/info/exclude", "!keep.log\n"),
            ("repo/.gitignore", "!notes.md\n"),
            ("repo/global.tmp", "x"),
            ("repo/a.log", "x"),
            ("repo/keep.log", "x"),
            ("repo/notes.md", "x"),
            ("plain/global.tmp", "x"),
        ]);
        let env = GitEnv {
            home: Some(t.path().join("home")),
            ..GitEnv::default()
        };
        let list = |root: &Path| {
            let (files, _) = walk_files(root, root, &Ignore::load_with(root, &env), 1000);
            files.iter().map(|f| rel_path(root, f)).collect::<Vec<_>>()
        };
        assert_eq!(
            list(&t.path().join("repo")),
            [".gitignore", "keep.log", "notes.md"]
        );
        // not a git repository: the global file does not apply
        assert_eq!(list(&t.path().join("plain")), ["global.tmp"]);
    }

    #[test]
    fn git_info_exclude_counts_below_gitignore() {
        let t = tree(&[
            (".git/info/exclude", "# local\nscratch/\n*.bak\nnotes.md\n"),
            (".gitignore", "!notes.md\n"),
            ("scratch/a.rs", "x"),
            ("sub/old.bak", "x"),
            ("notes.md", "x"),
            ("main.rs", "x"),
        ]);
        assert_eq!(listed(t.path()), [".gitignore", "main.rs", "notes.md"]);
        // no .git folder: only .gitignore
        let t = tree(&[(".gitignore", "*.bak\n"), ("a.bak", "x"), ("b.rs", "x")]);
        assert_eq!(listed(t.path()), [".gitignore", "b.rs"]);
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
