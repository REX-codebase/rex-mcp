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
        Lang::Yaml => check_yaml(content),
        other => {
            let jsx = path.ends_with(".jsx") || path.ends_with(".tsx");
            check_delimiters(content, other, jsx)
        }
    })
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
    let mut block_indent: Option<usize> = None;
    // open flow brackets: (char, line, col)
    let mut flow: Vec<(char, usize, usize)> = Vec::new();
    for (idx, raw) in content.lines().enumerate() {
        let line_no = idx + 1;
        let indent = raw.len() - raw.trim_start_matches([' ', '\t']).len();
        let body = raw.trim_start_matches([' ', '\t']);
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
            if !templated && !raw[..indent].contains('\t') {
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
            match c {
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
                '[' | '{' => flow.push((c, line_no, indent + i + 1)),
                ']' | '}' => {
                    let want = if c == ']' { '[' } else { '{' };
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
                            break;
                        }
                        None => {}
                    }
                    if flow.is_empty() {
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

/// Normalised mapping key, or `None` for keys REX does not compare
/// (merge keys, explicit `?` keys, aliases, empty keys).
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
        let f = check("a.yaml", "x: 1\na: [1, 2\n").unwrap();
        assert_eq!((f[0].line, f[0].col), (2, 4), "{f:?}");
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
        clean("a.yaml", "- name: a\n- name: b\nx:\n- name: c\n- name: d\n");
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
