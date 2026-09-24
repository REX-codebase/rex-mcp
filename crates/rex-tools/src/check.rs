//! In-process syntax checks run after a successful write.
//!
//! opencode returns LSP diagnostics with an edit (`tool/edit.ts`), and
//! Hermes runs per-extension linters with a pre/post delta
//! (`tools/file_operations_lint.py`). REX does it without spawning any
//! process, so there is no approval, sandbox or toolchain dependency: JSON
//! is parsed, and code files get a lexer-aware delimiter check (brackets,
//! strings, comments) per language family. Only problems the write
//! introduced are reported as new; problems that were already there are
//! labelled as pre-existing. The check is advisory and never blocks the
//! write.

use std::path::Path;

const MAX_CHECK_BYTES: usize = 512 * 1024;
const MAX_FINDINGS: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub line: usize,
    pub col: usize,
    pub message: String,
}

impl Finding {
    fn render(&self) -> String {
        format!("{}:{}: {}", self.line, self.col, self.message)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lang {
    Rust,
    Js,
    Go,
    CFamily,
    Python,
    Css,
    Json,
    Toml,
    Yaml,
}

fn lang_for(path: &str) -> Option<Lang> {
    let ext = Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "rs" => Lang::Rust,
        "js" | "mjs" | "cjs" | "jsx" | "ts" | "tsx" | "mts" | "cts" => Lang::Js,
        "go" => Lang::Go,
        "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "java" | "cs" | "kt" | "kts"
        | "swift" | "scala" | "dart" => Lang::CFamily,
        "py" | "pyi" => Lang::Python,
        "css" | "scss" | "less" => Lang::Css,
        "json" => Lang::Json,
        "toml" => Lang::Toml,
        "yaml" | "yml" => Lang::Yaml,
        _ => return None,
    })
}

/// Check `content` as the language implied by `path`. `None` means the
/// file type is not checked (or it is too large to check cheaply).
pub fn check(path: &str, content: &str) -> Option<Vec<Finding>> {
    let lang = lang_for(path)?;
    if content.len() > MAX_CHECK_BYTES {
        return None;
    }
    Some(match lang {
        Lang::Json => check_json(content),
        Lang::Toml => check_toml(content),
        Lang::Yaml => {
            let found = check_yaml(content);
            if found.is_empty() && !yaml_templated(content) {
                check_yaml_parse(content)
            } else {
                found
            }
        }
        Lang::Rust => {
            let found = check_delimiters(content, Lang::Rust, false);
            if found.is_empty() {
                check_rust_parse(content)
            } else {
                found
            }
        }
        Lang::Python => {
            let found = check_delimiters(content, Lang::Python, false);
            if found.is_empty() {
                guarded_parse(content, python_parse_findings)
            } else {
                found
            }
        }
        Lang::Js => {
            // The parser decides: a clean parse clears lexical false alarms
            // (JSX prose like "1) item"); on a failed parse the delimiter
            // wording is kept when it found something, since it names the
            // unmatched bracket.
            let jsx = path.ends_with(".jsx") || path.ends_with(".tsx");
            let found = check_delimiters(content, Lang::Js, jsx);
            let path = path.to_string();
            let parsed = guarded_parse_with(content, move |c| {
                js_parse_findings(&path, c).unwrap_or_default()
            });
            match parsed {
                Some(p) if p.is_empty() => Vec::new(),
                Some(p) if found.is_empty() => p,
                _ => found,
            }
        }
        other => {
            let jsx = path.ends_with(".jsx") || path.ends_with(".tsx");
            check_delimiters(content, other, jsx)
        }
    })
}

/// Full Rust parse (syn), run only when the delimiter check is clean so a
/// broken bracket is still reported in its own words. Reports the first
/// syntax error with its position. Hermes shells out to `rustfmt --check`
/// for `.rs` (`tools/file_operations_lint.py:22`), which also fails on
/// formatting; this reports syntax only and needs no toolchain.
fn check_rust_parse(content: &str) -> Vec<Finding> {
    // syn recurses per nesting level; a deeply nested (often generated) file
    // can overflow a small stack, and a stack overflow aborts the process.
    // Skip very deep inputs (size is capped by check) and parse on a thread
    // with a big stack.
    guarded_parse(content, rust_parse_findings)
}

/// Run a parser on its own thread with a big stack, skipping inputs nested
/// deeper than `PARSE_MAX_DEPTH` (parsers and AST drops recurse per level,
/// and a stack overflow aborts the whole process).
fn guarded_parse(content: &str, parse: fn(&str) -> Vec<Finding>) -> Vec<Finding> {
    guarded_parse_with(content, parse).unwrap_or_default()
}

/// Like `guarded_parse`, but `None` when the parse was skipped or failed to
/// run, so callers can tell "clean" from "not checked".
fn guarded_parse_with<F>(content: &str, parse: F) -> Option<Vec<Finding>>
where
    F: FnOnce(&str) -> Vec<Finding> + Send + 'static,
{
    if nesting_depth(content) > PARSE_MAX_DEPTH {
        return None;
    }
    let owned = content.to_string();
    let worker = std::thread::Builder::new()
        .stack_size(PARSE_STACK)
        .spawn(move || parse(&owned));
    match worker {
        Ok(handle) => handle.join().ok(),
        Err(_) => None,
    }
}

/// Full Python parse (ruff_python_parser, MIT), run only when the
/// delimiter check is clean. Hermes runs `python -m py_compile` on each
/// written `.py` file (`tools/file_operations_lint.py:17-22`), which needs a
/// Python install; this needs none. The grammar is current (3.12 f-strings,
/// 3.13 type parameter defaults, 3.14 t-strings). Errors that depend on the
/// target Python version are not reported, since the version is unknown.
fn python_parse_findings(content: &str) -> Vec<Finding> {
    let err = match ruff_python_parser::parse_module(content) {
        Ok(_) => return Vec::new(),
        Err(e) => e,
    };
    let (line, col) = line_col(content, u32::from(err.location.start()) as usize);
    vec![Finding {
        line,
        col,
        message: format!("Python syntax error: {}", err.error),
    }]
}

/// Full JavaScript/TypeScript parse (oxc_parser, MIT), JSX/TSX included,
/// with the dialect taken from the file name. Hermes runs `node --check` on
/// `.js` and single-file `tsc --noEmit` on `.ts`, skipping the latter when
/// a language server claims the file (`tools/file_operations_lint.py:17-29`).
/// Syntax only: no types, no resolution. `None` when the name maps to no
/// dialect.
fn js_parse_findings(path: &str, content: &str) -> Option<Vec<Finding>> {
    let source_type = oxc_span::SourceType::from_path(path).ok()?;
    let allocator = oxc_allocator::Allocator::default();
    // CommonJS modules run inside a function wrapper, so a top-level
    // `return` is valid there; snippets use it too.
    let options = oxc_parser::ParseOptions {
        allow_return_outside_function: true,
        ..oxc_parser::ParseOptions::default()
    };
    let ret = oxc_parser::Parser::new(&allocator, content, source_type)
        .with_options(options)
        .parse();
    let Some(d) = ret.diagnostics.first() else {
        return Some(Vec::new());
    };
    let offset = d.labels.first().map_or(0, |l| l.offset() as usize);
    let (line, col) = line_col(content, offset);
    Some(vec![Finding {
        line,
        col,
        message: format!("JS/TS syntax error: {}", d.message),
    }])
}

const PARSE_MAX_DEPTH: usize = 128;
const PARSE_STACK: usize = 64 * 1024 * 1024;

/// Upper bound on bracket nesting (ignores strings and comments, so it can
/// over-count; that only makes the skip more cautious).
fn nesting_depth(content: &str) -> usize {
    let (mut depth, mut max) = (0usize, 0usize);
    for b in content.bytes() {
        match b {
            b'(' | b'[' | b'{' => {
                depth += 1;
                max = max.max(depth);
            }
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    max
}

fn rust_parse_findings(content: &str) -> Vec<Finding> {
    match syn::parse_file(content) {
        Ok(_) => Vec::new(),
        Err(e) => {
            let start = e.span().start();
            vec![Finding {
                line: start.line.max(1),
                col: start.column + 1,
                message: format!("Rust syntax error: {e}"),
            }]
        }
    }
}

fn check_json(content: &str) -> Vec<Finding> {
    if content.trim().is_empty() {
        return Vec::new();
    }
    match serde_json::from_str::<serde_json::Value>(content) {
        Ok(_) => Vec::new(),
        // tsconfig.json and editor settings are JSON-with-comments
        Err(_) if serde_json::from_str::<serde_json::Value>(&strip_jsonc(content)).is_ok() => {
            Vec::new()
        }
        Err(e) => vec![Finding {
            line: e.line(),
            col: e.column(),
            message: format!("invalid JSON: {e}"),
        }],
    }
}

/// 1-based line and column of a byte offset.
fn line_col(src: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(src.len());
    let before = &src[..src.floor_char_boundary(offset)];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    (line, col)
}

/// Full TOML parse. The message carries no position, so the same problem
/// on a shifted line still counts as pre-existing in `delta_note`.
fn check_toml(content: &str) -> Vec<Finding> {
    match toml::from_str::<toml::Table>(content) {
        Ok(_) => Vec::new(),
        Err(e) => {
            let (line, col) = e.span().map_or((1, 1), |r| line_col(content, r.start));
            vec![Finding {
                line,
                col,
                message: format!("invalid TOML: {}", e.message().trim()),
            }]
        }
    }
}

/// Helm/Jinja templates are not YAML until rendered.
fn yaml_templated(content: &str) -> bool {
    // `{{` not preceded by `$` (GitHub Actions `${{ }}` is plain YAML text)
    content.contains("{%")
        || content
            .match_indices("{{")
            .any(|(i, _)| !content[..i].ends_with('$'))
}

/// Full YAML 1.2 parse (saphyr-parser, MIT/Apache-2.0), run only when the
/// line checks are clean and the file is not a template. Catches what the
/// line checks cannot: bad indentation inside mappings, aliases to anchors
/// never defined, stray block entries, unclosed quotes. Hermes parses YAML
/// events in-process with PyYAML (`tools/file_operations_lint.py:70-83`,
/// `yaml.parse`, not a load); this is the
/// same idea without a Python runtime. Only the first error is reported.
fn check_yaml_parse(content: &str) -> Vec<Finding> {
    for event in saphyr_parser::Parser::new_from_str(content) {
        if let Err(e) = event {
            // saphyr follows the spec's indentation rules for flow and
            // quoted content more strictly than PyYAML and most loaders
            // (real files: Hermes locales, base64's CircleCI config), so
            // those errors are not reported.
            if e.info().starts_with("invalid indentation") {
                return Vec::new();
            }
            let m = e.marker();
            return vec![Finding {
                line: m.line().max(1),
                col: m.col() + 1,
                message: format!("YAML parse error: {}", e.info()),
            }];
        }
    }
    Vec::new()
}

/// Conservative YAML check, tuned for no false alarms rather than full
/// validation: tabs used for indentation (YAML forbids them) and flow
/// collections (`key: [a, b` / `{`) that are never closed or close with the
/// wrong bracket. Block scalars (`|`, `>`) are skipped, and brackets inside
/// plain scalars such as `run: echo ${{ x }}` are ignored because only a
/// value that *starts* a flow collection is tracked.
fn check_yaml(content: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    // Duplicate keys in block mappings. Templated files (Helm/Jinja) can
    // repeat a key under different branches, so they are not checked.
    let templated = content.lines().any(|l| {
        let t = l.trim_start();
        t.starts_with("{{") || t.starts_with("{%")
    });
    // open block mappings: (key column, keys seen with their line)
    let mut scopes: Vec<(usize, Vec<(String, usize)>)> = Vec::new();
    // last `key: plain value` line: (key column, line, key)
    let mut last_scalar: Option<(usize, usize, String)> = None;
    // block sequences and mapping keys at one indent: the previous block
    // line (indent, key column, key text, key has an inline value) and the
    // open `- ` runs (column, how the run started, first line)
    let mut prev_line: Option<(usize, Option<usize>, String, bool)> = None;
    let mut runs: Vec<(usize, SeqStart, usize)> = Vec::new();
    // a quoted value still open from an earlier line (multi-line string)
    let mut open_quote: Option<char> = None;
    let mut block_indent: Option<usize> = None;
    // open flow brackets: (char, line, col)
    let mut flow: Vec<(char, usize, usize)> = Vec::new();
    // per open flow bracket: keys seen (with line) and whether the next
    // entry of a `{` mapping starts here (after `{` or `,`)
    let mut flow_keys: Vec<(Vec<(String, usize)>, bool)> = Vec::new();
    for (idx, raw) in content.lines().enumerate() {
        let line_no = idx + 1;
        let indent = raw.len() - raw.trim_start_matches([' ', '\t']).len();
        let body = raw.trim_start_matches([' ', '\t']);
        if let Some(q) = open_quote {
            if quote_closes(raw, q) {
                open_quote = None;
            }
            continue;
        }
        if let Some(bi) = block_indent {
            if body.is_empty() || indent > bi {
                continue;
            }
            block_indent = None;
        }
        if body.is_empty() || body.starts_with('#') {
            continue;
        }
        if flow.is_empty() && (body.starts_with("---") || body.starts_with("...")) {
            scopes.clear();
            runs.clear();
            prev_line = None;
        }
        if flow.is_empty() && raw[..indent].contains('\t') {
            out.push(Finding {
                line: line_no,
                col: 1,
                message: "tab used for indentation (YAML allows spaces only)".into(),
            });
        }
        let chars: Vec<char> = body.chars().collect();
        let mut i = 0;
        if flow.is_empty() {
            // find where a value starts: after `- ` markers and `key: `
            let mut start = 0;
            loop {
                if chars.get(start) == Some(&'-') && chars.get(start + 1).is_none_or(|c| *c == ' ')
                {
                    start += 2;
                    while chars.get(start) == Some(&' ') {
                        start += 1;
                    }
                    continue;
                }
                break;
            }
            let key_at = find_mapping_colon(&chars[start.min(chars.len())..]);
            // A key that already has a plain value cannot own nested keys:
            // `name: web\n  image: x` is a YAML error. Only simple keys on
            // the deeper line count, so wrapped plain text is not flagged.
            let prev = last_scalar.take();
            if let (Some((pcol, pline, pkey)), Some(c)) = (prev, key_at) {
                let key: String = chars[start..start + c].iter().collect();
                let simple = !key.is_empty()
                    && key
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'));
                if indent > pcol && simple && !raw[..indent].contains('\t') {
                    out.push(Finding {
                        line: line_no,
                        col: indent + 1,
                        message: format!(
                            "'{key}' is indented under '{pkey}', which already has a value on line {pline}"
                        ),
                    });
                }
            }
            if let Some(c) = key_at {
                let value: String = chars[start + c + 1..].iter().collect();
                let value = value.split(" #").next().unwrap_or("").trim();
                if let Some(q) = value.chars().next().filter(|c| *c == '"' || *c == '\'') {
                    if !quote_closes(&value[1..], q) {
                        open_quote = Some(q);
                    }
                }
                let plain = !value.is_empty()
                    && !value.starts_with(['|', '>', '[', '{', '&', '!', '*', '\'', '"', '#']);
                if plain {
                    let key: String = chars[start..start + c].iter().collect();
                    last_scalar = Some((indent + start, line_no, key.trim().to_string()));
                }
            }
            let doc_marker = body.starts_with("---") || body.starts_with("...");
            if !templated && !doc_marker && !raw[..indent].contains('\t') {
                seq_map_mix(
                    &chars,
                    indent,
                    start,
                    key_at,
                    line_no,
                    &mut prev_line,
                    &mut runs,
                    &mut out,
                );
                let col = indent + start;
                let new_item = start > 0;
                if let Some(c) = key_at {
                    let key = yaml_key(&chars[start..start + c]);
                    scopes.retain(|(k, _)| if new_item { *k < col } else { *k <= col });
                    match scopes.last_mut() {
                        Some((k, keys)) if *k == col => {
                            if let Some(key) = key {
                                if let Some((_, first)) = keys.iter().find(|(n, _)| *n == key) {
                                    out.push(Finding {
                                        line: line_no,
                                        col: col + 1,
                                        message: format!(
                                            "duplicate key '{key}' (first on line {first})"
                                        ),
                                    });
                                } else {
                                    keys.push((key, line_no));
                                }
                            }
                        }
                        _ => {
                            scopes.push((col, key.map(|k| vec![(k, line_no)]).unwrap_or_default()))
                        }
                    }
                } else if new_item {
                    // a bare `- value` item ends mappings nested deeper
                    scopes.retain(|(k, _)| *k <= indent);
                }
            }
            let value_start = match key_at {
                Some(c) => {
                    let mut v = start + c + 1;
                    while chars.get(v) == Some(&' ') {
                        v += 1;
                    }
                    v
                }
                None => start,
            };
            match chars.get(value_start) {
                Some('[') | Some('{') => i = value_start,
                Some('|') | Some('>') => {
                    let rest: String = chars[value_start + 1..].iter().collect();
                    let rest = rest.split('#').next().unwrap_or("").trim().to_string();
                    if rest
                        .chars()
                        .all(|c| c.is_ascii_digit() || c == '-' || c == '+')
                    {
                        block_indent = Some(indent);
                    }
                    continue;
                }
                _ => continue,
            }
        }
        // inside (or entering) a flow collection: track brackets, skipping
        // quoted strings and comments
        while i < chars.len() {
            let c = chars[i];
            if c != ' ' && c != '#' && flow.last().is_some_and(|f| f.0 == '{') {
                if let Some((keys, expect)) = flow_keys.last_mut() {
                    if *expect {
                        *expect = false;
                        if let Some(key) = flow_key(&chars[i..]).filter(|_| !templated) {
                            if let Some((_, first)) = keys.iter().find(|(k, _)| *k == key) {
                                out.push(Finding {
                                    line: line_no,
                                    col: indent + i + 1,
                                    message: format!(
                                        "duplicate key '{key}' in flow mapping (first on line {first})"
                                    ),
                                });
                            } else {
                                keys.push((key, line_no));
                            }
                        }
                    }
                }
            }
            match c {
                ',' => {
                    if let Some((_, expect)) = flow_keys.last_mut() {
                        *expect = true;
                    }
                }
                '"' | '\'' => {
                    let q = c;
                    i += 1;
                    while i < chars.len() {
                        if q == '"' && chars[i] == '\\' {
                            i += 2;
                            continue;
                        }
                        if chars[i] == q {
                            if q == '\'' && chars.get(i + 1) == Some(&'\'') {
                                i += 2;
                                continue;
                            }
                            break;
                        }
                        i += 1;
                    }
                }
                '#' if i == 0 || chars[i - 1] == ' ' => break,
                '[' | '{' => {
                    flow.push((c, line_no, indent + i + 1));
                    // the flag is only read while a `{` is innermost
                    flow_keys.push((Vec::new(), true));
                }
                ']' | '}' => {
                    let want = if c == ']' { '[' } else { '{' };
                    flow_keys.pop();
                    match flow.pop() {
                        Some((open, _, _)) if open == want => {}
                        Some((open, l, _)) => {
                            out.push(Finding {
                                line: line_no,
                                col: indent + i + 1,
                                message: format!(
                                    "flow collection opened with '{open}' on line {l} closed with '{c}'"
                                ),
                            });
                            flow.clear();
                            flow_keys.clear();
                            break;
                        }
                        None => {}
                    }
                    if flow.is_empty() {
                        // only a comment, or `:` of a complex key like
                        // `[a, b]: v`, may follow a closed flow collection
                        let rest: String = chars[i + 1..].iter().collect();
                        let rest = rest.trim();
                        let ok = rest.is_empty()
                            || rest.starts_with('#')
                            || rest == ":"
                            || rest.starts_with(": ");
                        if !ok {
                            out.push(Finding {
                                line: line_no,
                                col: indent + i + 2,
                                message: format!(
                                    "text after the flow collection closed with '{c}': {:?}",
                                    rest.chars().take(40).collect::<String>()
                                ),
                            });
                        }
                        break;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        if out.len() >= MAX_FINDINGS {
            break;
        }
    }
    if let Some((open, line, col)) = flow.first().copied() {
        out.push(Finding {
            line,
            col,
            message: format!("flow collection '{open}' is never closed"),
        });
    }
    cap(out)
}

/// The key of a flow-mapping entry starting at `chars` (`key: value`,
/// `"key": value`), or `None` for a value-only entry, a nested
/// collection or anything unclear.
fn flow_key(chars: &[char]) -> Option<String> {
    let (key, rest) = match chars.first()? {
        '"' | '\'' => {
            let q = chars[0];
            let mut j = 1;
            while j < chars.len() {
                if q == '"' && chars[j] == '\\' {
                    j += 2;
                    continue;
                }
                if chars[j] == q {
                    if q == '\'' && chars.get(j + 1) == Some(&'\'') {
                        j += 2;
                        continue;
                    }
                    break;
                }
                j += 1;
            }
            if j >= chars.len() {
                return None;
            }
            (chars[1..j].iter().collect::<String>(), &chars[j + 1..])
        }
        '{' | '[' | ']' | '}' | ',' | '?' | '&' | '*' | '!' | '|' | '>' => return None,
        _ => {
            let end = (0..chars.len()).find(|&j| {
                matches!(chars[j], ',' | '{' | '}' | '[' | ']' | '#')
                    || (chars[j] == ':'
                        && chars
                            .get(j + 1)
                            .is_none_or(|n| matches!(n, ' ' | ',' | '}' | ']')))
            })?;
            if chars[end] != ':' {
                return None;
            }
            let key: String = chars[..end].iter().collect();
            (key.trim().to_string(), &chars[end..])
        }
    };
    let rest: String = rest.iter().collect();
    let rest = rest.trim_start();
    (rest.starts_with(':') && !key.is_empty()).then_some(key)
}

/// Whether `text` holds the closing `q` of a quoted scalar (`\"` escapes
/// in double quotes, `''` in single quotes).
fn quote_closes(text: &str, q: char) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if q == '"' && chars[i] == '\\' {
            i += 2;
            continue;
        }
        if chars[i] == q {
            if q == '\'' && chars.get(i + 1) == Some(&'\'') {
                i += 2;
                continue;
            }
            return true;
        }
        i += 1;
    }
    false
}

/// Normalised mapping key, or `None` for keys REX does not compare
/// (merge keys, explicit `?` keys, aliases, empty keys).
/// How a block sequence (`- ` items at one column) started, which decides
/// whether a mapping key may follow at that column.
#[derive(Clone, Copy, PartialEq)]
enum SeqStart {
    /// right under `key:` at the same column: later keys there are siblings
    UnderKey,
    /// indented under its parent, or first in the document: a key at the
    /// same column would mix a sequence and a mapping
    Alone,
    /// anything else; never flagged
    Unknown,
}

/// Flag a `- ` item at the column of a key that already has a value, and a
/// key at the column of a sequence that does not belong to a key there.
#[allow(clippy::too_many_arguments)]
fn seq_map_mix(
    chars: &[char],
    indent: usize,
    start: usize,
    key_at: Option<usize>,
    line_no: usize,
    prev_line: &mut Option<(usize, Option<usize>, String, bool)>,
    runs: &mut Vec<(usize, SeqStart, usize)>,
    out: &mut Vec<Finding>,
) {
    let key_info = key_at.map(|c| {
        let key: String = chars[start..start + c].iter().collect();
        let value: String = chars[start + c + 1..].iter().collect();
        let value = value.split(" #").next().unwrap_or("").trim().to_string();
        (key.trim().to_string(), !value.is_empty())
    });
    if start > 0 {
        runs.retain(|(c, _, _)| *c <= indent);
        if !runs.iter().any(|(c, _, _)| *c == indent) {
            let kind = match prev_line {
                Some((_, Some(kc), key, true)) if *kc == indent => {
                    out.push(Finding {
                        line: line_no,
                        col: indent + 1,
                        message: format!(
                            "list item at the same indent as '{key}', which already has a value"
                        ),
                    });
                    SeqStart::Unknown
                }
                Some((_, Some(kc), _, false)) if *kc == indent => SeqStart::UnderKey,
                Some((pi, _, _, _)) if *pi < indent => SeqStart::Alone,
                None => SeqStart::Alone,
                _ => SeqStart::Unknown,
            };
            runs.push((indent, kind, line_no));
        }
    } else if let Some((key, _)) = &key_info {
        runs.retain(|(c, _, _)| *c <= indent);
        if let Some((_, SeqStart::Alone, first)) = runs.iter().find(|(c, _, _)| *c == indent) {
            out.push(Finding {
                line: line_no,
                col: indent + 1,
                message: format!(
                    "key '{key}' at the same indent as the list that starts on line {first}"
                ),
            });
        }
        runs.retain(|(c, _, _)| *c < indent);
    } else {
        runs.retain(|(c, _, _)| *c <= indent);
    }
    *prev_line = Some(match key_info {
        Some((key, has_value)) => (indent, Some(indent + start), key, has_value),
        None => (indent, None, String::new(), false),
    });
}

fn yaml_key(chars: &[char]) -> Option<String> {
    let raw: String = chars.iter().collect();
    let k = raw.trim();
    if k.is_empty() || k == "<<" || k.starts_with('?') || k.starts_with('*') || k.starts_with('&') {
        return None;
    }
    let unq = if k.len() >= 2
        && ((k.starts_with('"') && k.ends_with('"')) || (k.starts_with('\'') && k.ends_with('\'')))
    {
        &k[1..k.len() - 1]
    } else {
        k
    };
    Some(unq.to_string())
}

/// Index of the `:` that ends a mapping key (followed by space or end of
/// line), outside quotes. `None` when the line is not `key: value`.
fn find_mapping_colon(chars: &[char]) -> Option<usize> {
    let mut i = 0;
    if matches!(chars.first(), Some('"') | Some('\'')) {
        let q = chars[0];
        i = 1;
        while i < chars.len() && chars[i] != q {
            i += 1;
        }
        i += 1;
    }
    while i < chars.len() {
        match chars[i] {
            ':' if chars.get(i + 1).is_none_or(|c| *c == ' ') => return Some(i),
            '#' if i > 0 && chars[i - 1] == ' ' => return None,
            '[' | '{' if i == 0 => return None,
            _ => {}
        }
        i += 1;
    }
    None
}

/// Drop `//` and `/* */` comments outside strings, then trailing commas.
fn strip_jsonc(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' {
            out.push(c);
            i += 1;
            while i < chars.len() {
                out.push(chars[i]);
                if chars[i] == '\\' && i + 1 < chars.len() {
                    out.push(chars[i + 1]);
                    i += 2;
                    continue;
                }
                i += 1;
                if chars[i - 1] == '"' {
                    break;
                }
            }
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                if chars[i] == '\n' {
                    out.push('\n');
                }
                i += 1;
            }
            i += 2;
            continue;
        }
        if c == ',' {
            let next = chars[i + 1..].iter().find(|c| !c.is_whitespace());
            if matches!(next, Some('}') | Some(']')) {
                i += 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

struct Scanner<'a> {
    chars: Vec<char>,
    i: usize,
    line: usize,
    col: usize,
    _src: &'a str,
}

impl<'a> Scanner<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            chars: src.chars().collect(),
            i: 0,
            line: 1,
            col: 1,
            _src: src,
        }
    }
    fn peek(&self, k: usize) -> Option<char> {
        self.chars.get(self.i + k).copied()
    }
    fn bump(&mut self) -> Option<char> {
        let c = self.chars.get(self.i).copied()?;
        self.i += 1;
        if c == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(c)
    }
    fn starts_with(&self, s: &str) -> bool {
        s.chars().enumerate().all(|(k, c)| self.peek(k) == Some(c))
    }
}

fn closer_for(open: char) -> char {
    match open {
        '$' => '}',
        '(' => ')',
        '[' => ']',
        _ => '}',
    }
}

/// Skip a quoted string whose opening quote was already consumed. Returns
/// false when the input ends first (or, for single-line strings, a newline
/// comes first).
fn skip_string(s: &mut Scanner, quote: char, multiline: bool, escapes: bool) -> bool {
    while let Some(c) = s.bump() {
        if escapes && c == '\\' {
            s.bump();
            continue;
        }
        if c == quote {
            return true;
        }
        if c == '\n' && !multiline {
            return false;
        }
    }
    false
}

fn check_delimiters(src: &str, lang: Lang, jsx: bool) -> Vec<Finding> {
    let mut s = Scanner::new(src);
    let mut stack: Vec<(char, usize, usize)> = Vec::new();
    let mut out: Vec<Finding> = Vec::new();
    // last significant char, for JS regex-literal detection
    let mut last_sig: Option<char> = None;
    let mut last_word = String::new();
    let mut word_break = false;
    let hash_comments = lang == Lang::Python;
    let slash_comments = lang != Lang::Python;
    // plain CSS has no line comments ("//" appears inside url(...))
    let line_slash_comments = slash_comments && lang != Lang::Css;
    while let Some(c) = s.peek(0) {
        let (line, col) = (s.line, s.col);
        let saved = (s.i, s.line, s.col);
        // comments
        if hash_comments && c == '#' {
            while let Some(d) = s.peek(0) {
                if d == '\n' {
                    break;
                }
                s.bump();
            }
            continue;
        }
        if line_slash_comments && s.starts_with("//") {
            while let Some(d) = s.peek(0) {
                if d == '\n' {
                    break;
                }
                s.bump();
            }
            continue;
        }
        if slash_comments && s.starts_with("/*") {
            s.bump();
            s.bump();
            let mut depth = 1usize;
            let mut closed = false;
            while s.peek(0).is_some() {
                if lang == Lang::Rust && s.starts_with("/*") {
                    s.bump();
                    s.bump();
                    depth += 1;
                } else if s.starts_with("*/") {
                    s.bump();
                    s.bump();
                    depth -= 1;
                    if depth == 0 {
                        closed = true;
                        break;
                    }
                } else {
                    s.bump();
                }
            }
            if !closed {
                out.push(Finding {
                    line,
                    col,
                    message: "unterminated block comment".into(),
                });
                break;
            }
            continue;
        }
        // strings
        let string_end = match (lang, c) {
            (Lang::Python, '"' | '\'') => {
                let triple: String = std::iter::repeat_n(c, 3).collect();
                if s.starts_with(&triple) {
                    for _ in 0..3 {
                        s.bump();
                    }
                    let mut ok = false;
                    while s.peek(0).is_some() {
                        if s.peek(0) == Some('\\') {
                            s.bump();
                            s.bump();
                            continue;
                        }
                        if s.starts_with(&triple) {
                            for _ in 0..3 {
                                s.bump();
                            }
                            ok = true;
                            break;
                        }
                        s.bump();
                    }
                    Some(ok)
                } else {
                    s.bump();
                    Some(skip_string(&mut s, c, false, true))
                }
            }
            (Lang::Rust, 'r')
                if matches!(s.peek(1), Some('"') | Some('#'))
                    && (!last_word_char(last_sig) || last_word == "b") =>
            {
                // raw string r"..." / r#"..."#
                let mut k = 1;
                while s.peek(k) == Some('#') {
                    k += 1;
                }
                if s.peek(k) == Some('"') {
                    let hashes = k - 1;
                    for _ in 0..=k {
                        s.bump();
                    }
                    let end: String = std::iter::once('"')
                        .chain(std::iter::repeat_n('#', hashes))
                        .collect();
                    let mut ok = false;
                    while s.peek(0).is_some() {
                        if s.starts_with(&end) {
                            for _ in 0..end.chars().count() {
                                s.bump();
                            }
                            ok = true;
                            break;
                        }
                        s.bump();
                    }
                    Some(ok)
                } else {
                    None
                }
            }
            (Lang::Rust, '\'') => {
                // char literal vs lifetime/label
                if s.peek(1) == Some('\\') {
                    s.bump();
                    Some(skip_string(&mut s, '\'', false, true))
                } else if s.peek(2) == Some('\'') {
                    s.bump();
                    s.bump();
                    s.bump();
                    Some(true)
                } else {
                    s.bump();
                    last_sig = Some('\'');
                    continue;
                }
            }
            (_, '"') => {
                s.bump();
                Some(skip_string(&mut s, '"', lang == Lang::Rust, true))
            }
            (Lang::Js | Lang::CFamily | Lang::Css, '\'') => {
                s.bump();
                Some(skip_string(&mut s, '\'', false, true))
            }
            (Lang::Go, '\'') => {
                s.bump();
                Some(skip_string(&mut s, '\'', false, true))
            }
            (Lang::Js, '`') => {
                s.bump();
                match scan_template_text(&mut s) {
                    Tpl::End => Some(true),
                    Tpl::Subst => {
                        stack.push(('$', line, col));
                        last_sig = Some('{');
                        last_word.clear();
                        continue;
                    }
                    Tpl::Unterminated => Some(false),
                }
            }
            (Lang::Go, '`') => {
                s.bump();
                Some(skip_string(&mut s, '`', true, false))
            }
            (Lang::Js, '/') if last_sig != Some('<') && regex_allowed(last_sig, &last_word) => {
                s.bump();
                Some(skip_regex(&mut s))
            }
            _ => None,
        };
        if let Some(ok) = string_end {
            if !ok && lang == Lang::Js && c == '/' {
                // not a regex after all (division, TS `x! / y`, JSX text)
                (s.i, s.line, s.col) = saved;
                s.bump();
                last_sig = Some('/');
                last_word.clear();
                continue;
            }
            if !ok && jsx && matches!(c, '\'' | '"') {
                // JSX text such as `don't` is not a string: step past the
                // quote and keep scanning.
                (s.i, s.line, s.col) = saved;
                s.bump();
                continue;
            }
            if !ok {
                out.push(Finding {
                    line,
                    col,
                    message: "unterminated string or literal".into(),
                });
                // a runaway string poisons everything after it
                return cap(out);
            }
            last_sig = Some('"');
            last_word.clear();
            continue;
        }
        s.bump();
        match c {
            '(' | '[' | '{' => stack.push((c, line, col)),
            ')' | ']' | '}' => match stack.pop() {
                Some(('$', ol, oc)) if c == '}' => match scan_template_text(&mut s) {
                    Tpl::End => {
                        last_sig = Some('"');
                        last_word.clear();
                        continue;
                    }
                    Tpl::Subst => {
                        stack.push(('$', ol, oc));
                        last_sig = Some('{');
                        last_word.clear();
                        continue;
                    }
                    Tpl::Unterminated => {
                        out.push(Finding {
                            line: ol,
                            col: oc,
                            message: "unterminated template literal".into(),
                        });
                        return cap(out);
                    }
                },
                Some((open, _, _)) if closer_for(open) == c => {}
                Some((open, ol, oc)) => {
                    out.push(Finding {
                        line,
                        col,
                        message: format!(
                            "found '{c}' but '{open}' from {ol}:{oc} is still open (expected '{}')",
                            closer_for(open)
                        ),
                    });
                    return cap(out);
                }
                None => {
                    out.push(Finding {
                        line,
                        col,
                        message: format!("unexpected '{c}' with no matching opener"),
                    });
                    return cap(out);
                }
            },
            _ => {}
        }
        if c.is_whitespace() {
            word_break = true;
        } else {
            if c.is_alphanumeric() || c == '_' || c == '$' {
                if word_break || !last_word_char(last_sig) {
                    last_word.clear();
                }
                last_word.push(c);
            } else {
                last_word.clear();
            }
            word_break = false;
            last_sig = Some(c);
        }
    }
    for (open, line, col) in stack.into_iter().rev().take(MAX_FINDINGS) {
        out.push(Finding {
            line,
            col,
            message: if open == '$' {
                "template literal substitution '${' is never closed".to_string()
            } else {
                format!("'{open}' is never closed")
            },
        });
    }
    cap(out)
}

fn last_word_char(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

fn regex_allowed(last: Option<char>, last_word: &str) -> bool {
    match last {
        None => true,
        Some(c) if c.is_alphanumeric() || c == '_' || c == '$' => matches!(
            last_word,
            "return"
                | "typeof"
                | "case"
                | "do"
                | "else"
                | "in"
                | "of"
                | "new"
                | "delete"
                | "void"
                | "throw"
                | "yield"
                | "await"
        ),
        Some(')' | ']' | '}' | '"') => false,
        Some(_) => true,
    }
}

fn skip_regex(s: &mut Scanner) -> bool {
    let mut in_class = false;
    while let Some(c) = s.bump() {
        match c {
            '\\' => {
                s.bump();
            }
            '[' => in_class = true,
            ']' => in_class = false,
            '/' if !in_class => return true,
            '\n' => return false,
            _ => {}
        }
    }
    false
}

enum Tpl {
    End,
    Subst,
    Unterminated,
}

/// Scan JS template text up to the closing backtick or the next `${`
/// (the substitution is then lexed by the main loop as ordinary code).
fn scan_template_text(s: &mut Scanner) -> Tpl {
    while let Some(c) = s.bump() {
        match c {
            '\\' => {
                s.bump();
            }
            '`' => return Tpl::End,
            '$' if s.peek(0) == Some('{') => {
                s.bump();
                return Tpl::Subst;
            }
            _ => {}
        }
    }
    Tpl::Unterminated
}

fn cap(mut v: Vec<Finding>) -> Vec<Finding> {
    v.truncate(MAX_FINDINGS);
    v
}

/// Compare the file before and after a write. Returns a short note for the
/// model, or `None` when the result is clean or the type is unchecked.
pub fn delta_note(path: &str, before: Option<&str>, after: &str) -> Option<String> {
    let post = check(path, after)?;
    if post.is_empty() {
        return None;
    }
    let pre = before.and_then(|b| check(path, b)).unwrap_or_default();
    let pre_msgs: Vec<&str> = pre.iter().map(|f| f.message.as_str()).collect();
    let lines: Vec<String> = post.iter().map(Finding::render).collect();
    if !pre.is_empty() && post.iter().all(|f| pre_msgs.contains(&f.message.as_str())) {
        return Some(format!(
            "syntax check: problems that were already in the file before this write remain: {}",
            lines.join("; ")
        ));
    }
    if path.ends_with(".jsx") || path.ends_with(".tsx") {
        // JSX prose such as "1) item" can unbalance a lexical check.
        return Some(format!(
            "syntax check (JSX; prose text can cause false alarms): after this write: {}. Check it before moving on.",
            lines.join("; ")
        ));
    }
    Some(format!(
        "syntax check: this write left the file with: {}. Fix it before moving on.",
        lines.join("; ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(path: &str, src: &str) {
        let f = check(path, src).expect("checked type");
        assert!(f.is_empty(), "{path}: unexpected {f:?}");
    }
    fn dirty(path: &str, src: &str, needle: &str) {
        let f = check(path, src).expect("checked type");
        assert!(
            f.iter().any(|f| f.message.contains(needle)),
            "{path}: wanted {needle}, got {f:?}"
        );
    }

    #[test]
    fn rust_syntax_errors_past_balanced_brackets_are_caught() {
        clean(
            "a.rs",
            "fn main() { let x = vec![1, 2]; println!(\"{x:?}\"); }\n",
        );
        clean("a.rs", "use std::io;\nimpl<T: Clone> Foo<T> where T: Send { fn f(&self) -> io::Result<()> { Ok(()) } }\n");
        let f = check("a.rs", "fn main() {\n    let x = 1 +;\n}\n").unwrap();
        assert_eq!(f.len(), 1, "{f:?}");
        assert!(f[0].message.starts_with("Rust syntax error"), "{f:?}");
        assert_eq!((f[0].line, f[0].col), (2, 16), "{f:?}");
        dirty("a.rs", "fn f() { let }\n", "Rust syntax error");
        dirty("a.rs", "struct S { a: u8 b: u8 }\n", "Rust syntax error");
        // an unbalanced file keeps the delimiter message, not a parse error
        let f = check("a.rs", "fn f() {\n").unwrap();
        assert!(
            f.iter().all(|f| !f.message.contains("Rust syntax")),
            "{f:?}"
        );
    }

    #[test]
    fn rust_parse_skips_deep_input_without_crashing() {
        let deep = format!("fn f() {{ {}{} }}\n", "(".repeat(5000), ")".repeat(5000));
        assert!(nesting_depth(&deep) > PARSE_MAX_DEPTH);
        clean("a.rs", &deep);
        let edge = format!("const A: u8 = {}1{};\n", "(".repeat(120), ")".repeat(120));
        assert_eq!(nesting_depth(&edge), 120);
        clean("a.rs", &edge);
        let bad_edge = format!("const A: u8 = {}1 +{};\n", "(".repeat(120), ")".repeat(120));
        dirty("a.rs", &bad_edge, "Rust syntax error");
        assert_eq!(nesting_depth("a)(b"), 1);
        assert_eq!(nesting_depth("[{}]"), 2);
        assert_eq!(nesting_depth("(())()"), 2);
        assert_eq!(nesting_depth("{}{}{}"), 1);
        // too deep: the parse is skipped, so even a real error is not reported
        let deep_bad = format!(
            "fn f() {{ let x = 1 +; {}{} }}\n",
            "(".repeat(300),
            ")".repeat(300)
        );
        clean("a.rs", &deep_bad);
        // exactly at the limit still parses
        let limit = PARSE_MAX_DEPTH - 1; // the fn body brace adds one
        let at_limit = format!(
            "fn f() {{ let x = {}1 +{}; }}\n",
            "(".repeat(limit),
            ")".repeat(limit)
        );
        assert_eq!(nesting_depth(&at_limit), PARSE_MAX_DEPTH);
        dirty("a.rs", &at_limit, "Rust syntax error");
        let under = format!("fn f() {{ let x = 1 +; }}\n{}", "// pad\n".repeat(1000));
        dirty("a.rs", &under, "Rust syntax error");
    }

    #[test]
    fn yaml_full_parse_catches_what_line_checks_miss() {
        let f = check("a.yaml", "base: &b {x: 1}\nuse: *missing\n").unwrap();
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!((f[0].line, f[0].col), (2, 6), "{f:?}");
        assert!(f[0].message.starts_with("YAML parse error"), "{f:?}");
        dirty("a.yaml", "a: \"never closed\n  b: 1\n", "YAML parse error");
        dirty(
            "a.yaml",
            "g: long text that\n  wraps onto the next line: here\n",
            "mapping values",
        );
        dirty("a.yaml", "a:\n  b: 1\n c: 2\n", "YAML parse error");
        clean("a.yaml", "base: &b {x: 1}\nuse: *b\n---\nsecond: doc\n");
        // indentation strictness beyond PyYAML is not reported
        clean("a.yaml", "r:\n  e: \"one\nline two\"\n");
        clean("a.yml", "i:\n  - [\n      'a'\n    ]\n");
        // templates are skipped
        clean("t.yaml", "a: {{ .Values.x }}\nuse: *nope\n");
        clean("t.yaml", "{% if x %}\nuse: *nope\n{% endif %}\n");
        clean("t.yaml", "a: 1\n{{- include \"x\" . }}\nuse: *nope\n");
        assert!(yaml_templated("x: {{- y }}"));
        assert!(!yaml_templated("x: \"${{ github.ref }}\""));
        assert!(yaml_templated("{{ x }}"));
        assert!(yaml_templated("a: ${{ b }} {{ c }}"));
    }

    #[test]
    fn python_syntax_errors_past_balanced_brackets_are_caught() {
        clean("a.py", "import os\n\ndef f(x, /, y=1, *, z):\n    match x:\n        case [a, *b]:\n            return a\n    return f\"{x!r:>{y}}\"\n");
        clean("a.pyi", "class A:\n    def f(self) -> int: ...\n");
        let f = check("a.py", "def f():\n    x = 1 +\n    return x\n").unwrap();
        assert_eq!(f.len(), 1, "{f:?}");
        assert!(f[0].message.starts_with("Python syntax error"), "{f:?}");
        assert_eq!((f[0].line, f[0].col), (2, 12), "{f:?}");
        let f = check("a.py", "# café\nx = é +\n").unwrap();
        assert_eq!((f[0].line, f[0].col), (2, 8), "{f:?}");
        dirty("a.py", "if x\n    pass\n", "Python syntax error");
        dirty("a.py", "def f(:\n    pass\n)\n", "Python syntax error");
        // current syntax parses: 3.12 f-strings, 3.13 defaults, 3.14 t-strings
        clean("a.py", "x = f\"{d[\"k\"]}\"\n");
        clean("a.py", "class A[T = int]: pass\ntype X[T] = list[T]\n");
        clean("a.py", "x = t\"hi {name}\"\n");
        // Python 2 print is an error
        dirty("a.py", "print 'x'\n", "Python syntax error");
        // unbalanced files keep the delimiter message
        let f = check("a.py", "x = (1,\n").unwrap();
        assert!(
            f.iter().all(|f| !f.message.contains("Python syntax")),
            "{f:?}"
        );
        // deep nesting is skipped instead of risking the stack
        let deep = format!("x = {}1 +{}\n", "(".repeat(500), ")".repeat(500));
        clean("a.py", &deep);
    }

    #[test]
    fn js_ts_full_parse_catches_errors_and_clears_jsx_prose() {
        let f = check("a.ts", "const a = 1;\nlet x = 1 +;\n").unwrap();
        assert_eq!(f.len(), 1, "{f:?}");
        assert!(f[0].message.starts_with("JS/TS syntax error"), "{f:?}");
        assert_eq!((f[0].line, f[0].col), (2, 12), "{f:?}");
        dirty("a.js", "const o = { a: 1 b: 2 };\n", "JS/TS syntax error");
        dirty(
            "a.ts",
            "function f(x: number) { return x + }\n",
            "JS/TS syntax error",
        );
        // TS syntax in a .js file is an error; in .ts it is fine
        dirty("a.js", "let x: number = 1;\n", "JS/TS syntax error");
        clean("a.ts", "let x: number = 1;\nexport type T<U> = U[];\n");
        clean("a.d.ts", "declare function f(x: string): void;\n");
        clean("a.mjs", "export const a = await import('./b.js');\n");
        // CommonJS top-level return
        clean("a.cjs", "if (!x) return;\nmodule.exports = 1;\n");
        // JSX prose the lexical check would flag parses clean
        clean(
            "a.tsx",
            "export const A = () => <ol><li>1) item</li></ol>;\n",
        );
        // unbalanced: the delimiter wording (naming the bracket) is kept
        let f = check("a.js", "function f() {\n").unwrap();
        assert!(!f.is_empty(), "{f:?}");
        assert!(f.iter().all(|f| !f.message.contains("JS/TS")), "{f:?}");
        // too deep to parse: the lexical result stands
        let deep = format!("let x = {}1{};\n", "(".repeat(300), ")".repeat(300));
        clean("a.js", &deep);
        let deep_bad = format!("let x = {}1{};\n", "(".repeat(300), ")".repeat(299));
        assert!(!check("a.js", &deep_bad).unwrap().is_empty());
        assert!(js_parse_findings("a.unknownext", "x").is_none());
    }

    #[test]
    fn toml_is_parsed_and_errors_carry_no_position_text() {
        clean("Cargo.toml", "[package]\nname = \"x\"\n[dependencies]\nserde = { version = \"1\", features = [\"derive\"] }\n");
        dirty("a.toml", "[package]\nname = \"x\n", "invalid TOML");
        let f = check("a.toml", "a = 1\na = 2\n").unwrap();
        assert_eq!(f[0].line, 2, "{f:?}");
        assert!(!f[0].message.contains("line"), "{f:?}");
    }

    #[test]
    fn yaml_flags_tabs_and_unclosed_flow_but_not_real_world_shapes() {
        clean(
            "ci.yml",
            "on: [push, pull_request]\njobs:\n  build:\n    runs-on: ${{ matrix.os }}\n    steps:\n      - run: echo \"[not flow\"\n      - run: |\n          if [ -f x ]; then\n            \techo {\n          fi\n      - with: { a: 1, b: [2, 3] }\n      - name: see [docs  # plain scalar\n      - key: 'it''s [ok'\n      - list:\n          [a,\n           b]\n",
        );
        dirty("a.yaml", "a:\n\tb: 1\n", "tab used for indentation");
        dirty("a.yaml", "a: [1, 2\nb: 3\n", "never closed");
        dirty("a.yaml", "a: {x: [1, 2}\n", "closed with");
        dirty("a.yaml", "a: [1, 2] x\n", "text after the flow collection");
        dirty(
            "a.yaml",
            "a: {x: 1, y: 2, x: 3}\n",
            "duplicate key 'x' in flow mapping",
        );
        dirty(
            "a.yaml",
            "a: {\"k\": 1,\n    k: 2}\n",
            "duplicate key 'k' in flow mapping (first on line 1)",
        );
        dirty("a.yaml", "- {a: {b: 1, b: 2}}\n", "duplicate key 'b'");
        dirty("a.yaml", "a: {x: [1, {y: 2}], x: 2}\n", "duplicate key 'x'");
        clean(
            "flow.yaml",
            "a: {x: 1, y: {x: 2}}\nb: [{x: 1}, {x: 2}]\nc: {x: 'a, x: b', y: \"x: 1\"}\nd: {x, x}\ne: {u: http://h/x, v: 1}\nf: {a:b: 1, a:c: 2}\nk: &k x\ng: {*k : 1, *k : 2}\n",
        );
        dirty("a.yaml", "a: {b: 1}}\n", "text after the flow collection");
        dirty(
            "a.yml",
            "a:\n  - [x,\n     y] z\n",
            "text after the flow collection",
        );
        clean(
            "k.yaml",
            "a: [1, 2]  # note\n[x, y]: pair key\n{k: v}: map key\nb: {c: 1}\n[p, q]:\n  - 1\n",
        );
        let f = check("a.yaml", "x: 1\na: [1, 2\n").unwrap();
        assert_eq!((f[0].line, f[0].col), (2, 4), "{f:?}");
    }

    #[test]
    fn yaml_key_under_a_plain_value_is_flagged() {
        let f = check("a.yaml", "services:\n  web: nginx\n    image: x\n").unwrap();
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!((f[0].line, f[0].col), (3, 5));
        assert!(
            f[0].message.contains("'image' is indented under 'web'"),
            "{f:?}"
        );
        dirty("a.yml", "- name: a\n    run: b\n", "indented under 'name'");
        // valid shapes (and wrapped text with a non-simple "key", left
        // unflagged on purpose): nested mapping, block scalar, anchors,
        // sequence items, comments in between
        // a multi-line string closes (past an escaped quote) and checking
        // resumes on the lines after it
        let f = check(
            "loc.yaml",
            "r:\n  e: \"say \\\"hi\n  e: still text\\\" end\"\n  e: 2\n",
        )
        .unwrap();
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f[0].line, 4, "{f:?}");
        assert!(f[0].message.contains("duplicate key 'e'"), "{f:?}");
        // multi-line quoted strings (seen in Hermes locale files)
        clean(
            "loc.yaml",
            "r:\n  e: \"Could not parse: {x}.\nUse quotes, for example: `a`.\"\n  f: \"ok\"\n  g: 'it''s\n  e: dup'\n  h: 1\n",
        );
        clean(
            "ok.yaml",
            "a:\n  b: 1\nc: |\n  d: 2\ne: &x\n  f: 3\nh: v\ni:\n  - j: 1\n    k: 2\nl: v # note\n# m:\nn: 1\n",
        );
    }

    #[test]
    fn yaml_lists_and_keys_mixed_at_one_indent_are_flagged() {
        dirty(
            "a.yaml",
            "a: 1\n- x\n",
            "list item at the same indent as 'a'",
        );
        dirty(
            "a.yaml",
            "- x\n- y\nb: 1\n",
            "key 'b' at the same indent as the list that starts on line 1",
        );
        dirty(
            "a.yaml",
            "list:\n  - a\n  key: 1\n",
            "key 'key' at the same indent",
        );
        dirty("a.yaml", "- a: 1\n  - x\n", "same indent as 'a'");
        dirty("a.yaml", "a: [1,\n  2]\n- x\n", "same indent as 'a'");
        dirty("a.yaml", "a: 1\n---\n- x\nb: 2\n", "key 'b'");
        // valid shapes: a list right under its key, siblings after it,
        // lists inside list items, nested mappings in items, documents
        clean("a.yaml", "a:\n- x\n- y\nb: 1\nc:\n  - z\nd: 2\n");
        clean(
            "a.yml",
            "- items:\n  - a\n  other: 1\n- name: x\n  tags:\n  - t\n  more: 2\n",
        );
        clean(
            "a.yml",
            "steps:\n  - name: s\n    with:\n      k: v\n  - run: b\nnext: 1\n",
        );
        clean("a.yaml", "a: 1\n---\n- x\n");
        clean("a.yaml", "a: # note\n- x\n");
        clean("a.yaml", "a: #note\n- x\n");
        clean("a.yml", "- k:\n    - x\n- y:\n    z: 1\n");
        dirty("a.yaml", "a:\n- x\nb: 1\n- y\n", "same indent as 'b'");
        clean("a.yaml", "text: |\n  - a\n  b: 1\nc: 2\n");
        clean("a.yaml", "- a\n# c\n- b\n");
    }

    #[test]
    fn yaml_duplicate_keys_are_found_per_mapping() {
        let f = check("a.yaml", "name: a\nimage: x\nname: b\n").unwrap();
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f[0].line, 3);
        assert!(f[0]
            .message
            .contains("duplicate key 'name' (first on line 1)"));
        dirty("a.yaml", "a:\n  b: 1\n  \"b\": 2\n", "duplicate key 'b'");
        dirty(
            "a.yml",
            "steps:\n  - name: x\n    run: a\n    run: b\n",
            "duplicate key 'run'",
        );
        // same key in sibling mappings, list items, documents: fine
        clean(
            "ci.yml",
            "jobs:\n  a:\n    name: x\n    steps:\n      - name: s1\n        run: a\n      - name: s2\n        run: b\n  b:\n    name: y\n---\njobs: 1\n",
        );
        clean(
            "a.yaml",
            "w:\n- name: a\n- name: b\nx:\n- name: c\n- name: d\n",
        );
        clean(
            "a.yaml",
            "base: &b\n  x: 1\nc:\n  <<: *b\n  <<: *b\n  x: 2\n",
        );
        clean("a.yaml", "text: |\n  name: a\n  name: b\nname2: c\n");
        // Helm/Jinja templates repeat keys under branches
        clean(
            "values.yaml",
            "{{- if .Values.a }}\nimage: a\n{{- else }}\nimage: b\n{{- end }}\n",
        );
    }

    #[test]
    fn rust_lifetimes_chars_raw_strings_and_nested_comments_are_clean() {
        clean(
            "a.rs",
            "fn f<'a>(x: &'a str) -> char { let _ = r#\"}{\"#; let c = '{'; let e = '\\''; /* { /* } */ */ 'l: loop { break 'l; } c }",
        );
        dirty("a.rs", "fn f() { let v = vec![1, 2; }", "still open");
        dirty("a.rs", "fn f() {\n  if x {\n}\n", "never closed");
        dirty("a.rs", "fn f() { let s = \"abc; }", "unterminated");
    }

    #[test]
    fn js_ts_templates_regex_and_jsx_are_clean() {
        clean(
            "a.tsx",
            "const re = /\\(/g; const t = `a ${ {b: 1}.b } ${`in ${x}`} }`; const x = a / b / c; export const C = () => <div>{items.map(i => (<p key={i}>{i}</p>))}</div>; // ) comment",
        );
        clean("a.js", "if (x) return /[)]/.test(s); else { y = '}'; }");
        clean(
            "a.ts",
            "const q = `'${v.replace(/'/g, \"\\\\'\")}'`; const r = a[i]! / b;",
        );
        clean(
            "a.ts",
            "function f() { return false\n}\nfunction g() { return /^[ ]\\[[^\\]]+\\]:/m.test(t) }",
        );
        dirty("a.ts", "const t = `a ${ (x }`;", "still open");
        clean("a.jsx", "const A = () => <p>don't {x} stop</p>;");
        dirty("a.ts", "function f() { return (1 + 2; }", "still open");
        dirty("a.js", "const s = 'oops;\nfoo()", "unterminated");
    }

    #[test]
    fn python_go_css_and_json() {
        clean(
            "a.py",
            "def f():\n    s = \"\"\"({[\"\"\"  # ) comment\n    return {'a': [1, (2)]}\n",
        );
        dirty("a.py", "x = [1, 2\n", "never closed");
        clean("a.go", "func f() { s := `raw } ` ; c := '}'; _ = s }");
        clean("a.css", "a { content: \"}\"; } /* { */");
        dirty("a.css", "a { color: red;", "never closed");
        clean("a.json", "{\"a\": [1]}");
        dirty("a.json", "{\"a\": [1,}", "invalid JSON");
        clean(
            "tsconfig.json",
            "{ // c\n \"a\": [1,], /* x */ \"u\": \"http://x\" }",
        );
        assert!(check("notes.md", "(((").is_none());
    }

    #[test]
    fn delta_labels_new_versus_pre_existing() {
        assert_eq!(delta_note("a.rs", Some("fn a() {}"), "fn a() {}"), None);
        let new = delta_note("a.rs", Some("fn a() {}"), "fn a() {").unwrap();
        assert!(new.contains("this write left"), "{new}");
        let old = delta_note("a.rs", Some("fn a() {"), "fn a() {\n// note").unwrap();
        assert!(old.contains("already in the file"), "{old}");
        let created = delta_note("b.json", None, "{").unwrap();
        assert!(created.contains("invalid JSON"));
    }

    /// False-positive sweep over a real tree: `REX_CHECK_SWEEP=<dir>`.
    #[test]
    #[ignore]
    fn sweep_tree_for_false_positives() {
        let root = std::env::var("REX_CHECK_SWEEP").expect("set REX_CHECK_SWEEP");
        let mut bad = Vec::new();
        let mut n = 0;
        let mut stack = vec![std::path::PathBuf::from(root)];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                let name = p.file_name().unwrap().to_string_lossy().to_string();
                if p.is_dir() {
                    if !matches!(name.as_str(), "target" | "node_modules" | ".git" | "dist") {
                        stack.push(p);
                    }
                    continue;
                }
                let Ok(src) = std::fs::read_to_string(&p) else {
                    continue;
                };
                if let Some(f) = check(&p.to_string_lossy(), &src) {
                    n += 1;
                    if !f.is_empty() {
                        bad.push(format!("{}: {:?}", p.display(), f[0]));
                    }
                }
            }
        }
        eprintln!("checked {n} files, {} flagged", bad.len());
        for b in bad.iter().take(40) {
            eprintln!("{b}");
        }
        assert!(bad.is_empty());
    }
}
