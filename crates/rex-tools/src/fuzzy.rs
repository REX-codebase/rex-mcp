//! Tolerant, still-safe text matching for `edit_file`.
//!
//! Cheap models often reproduce the text they want to replace with small
//! drift: wrong indentation, trailing spaces, collapsed whitespace, smart
//! quotes, literal `\n` escapes, or an extra blank line at the edges. An
//! exact-only matcher turns each of those into a failed turn and burned
//! quota. This module tries an ordered chain of strategies, most precise
//! first, and only accepts a looser strategy when it identifies a single
//! region (or every region when the caller explicitly asked for
//! `replace_all`). Every strategy returns byte spans into the ORIGINAL
//! content, so the bytes outside the matched region are never touched.
//!
//! Written for REX from first principles; the idea of a precision-ordered
//! replacer chain is common to open agent harnesses (opencode `edit.ts`,
//! Hermes `fuzzy_match.py`, both MIT) but no code is shared with them.

/// A half-open byte range `[start, end)` in the original content.
pub type Span = (usize, usize);

/// Outcome of planning an edit without touching the filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditPlan {
    pub new_content: String,
    pub replaced: usize,
    pub strategy: &'static str,
}

/// Why an edit could not be planned. `hint` is model-facing guidance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    NotFound {
        hint: String,
    },
    Ambiguous {
        strategy: &'static str,
        count: usize,
        lines: Vec<usize>,
    },
}

type Strategy = fn(&str, &str) -> Vec<Span>;

/// Precision-ordered chain. Order matters: the first strategy that finds
/// anything decides, so a looser strategy can never override a precise one.
const STRATEGIES: &[(&str, Strategy)] = &[
    ("exact", exact),
    ("line_trimmed", line_trimmed),
    ("whitespace_normalized", whitespace_normalized),
    ("indentation_flexible", indentation_flexible),
    ("unicode_normalized", unicode_normalized),
    ("escape_normalized", escape_normalized),
    ("trimmed_boundary", trimmed_boundary),
    ("block_anchor", block_anchor),
];

/// Plan replacing `find` with `replacement` in `content`.
pub fn plan_edit(
    content: &str,
    find: &str,
    replacement: &str,
    replace_all: bool,
) -> Result<EditPlan, PlanError> {
    for (name, strategy) in STRATEGIES {
        let mut spans = strategy(content, find);
        spans.sort_unstable();
        spans.dedup();
        if spans.is_empty() {
            continue;
        }
        if overlapping(&spans) {
            // A strategy that yields overlapping regions cannot be applied
            // deterministically; treat it as ambiguous.
            return Err(ambiguous(content, name, &spans));
        }
        if spans.len() > 1 && (!replace_all || *name == "block_anchor") {
            return Err(ambiguous(content, name, &spans));
        }
        let mut out = String::with_capacity(content.len() + replacement.len());
        let mut cursor = 0;
        for &(s, e) in &spans {
            out.push_str(&content[cursor..s]);
            let matched = &content[s..e];
            if *name == "exact" {
                out.push_str(replacement);
            } else {
                out.push_str(&reindent(find, matched, replacement));
            }
            cursor = e;
        }
        out.push_str(&content[cursor..]);
        return Ok(EditPlan {
            new_content: out,
            replaced: spans.len(),
            strategy: name,
        });
    }
    Err(PlanError::NotFound {
        hint: closest_hint(content, find),
    })
}

fn overlapping(spans: &[Span]) -> bool {
    spans.windows(2).any(|w| w[1].0 < w[0].1)
}

fn ambiguous(content: &str, strategy: &'static str, spans: &[Span]) -> PlanError {
    let lines = spans
        .iter()
        .take(10)
        .map(|&(s, _)| line_number(content, s))
        .collect();
    PlanError::Ambiguous {
        strategy,
        count: spans.len(),
        lines,
    }
}

/// 1-based line number of byte offset `at`.
pub fn line_number(content: &str, at: usize) -> usize {
    content.as_bytes()[..at.min(content.len())]
        .iter()
        .filter(|b| **b == b'\n')
        .count()
        + 1
}

// ---------------------------------------------------------------- lines

/// A content line: byte range without its terminator (`\n`, and a `\r`
/// before it is excluded from `text`).
struct Line<'a> {
    start: usize,
    end: usize,
    text: &'a str,
}

fn lines_of(content: &str) -> Vec<Line<'_>> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, b) in content.bytes().enumerate() {
        if b == b'\n' {
            let mut end = i;
            if end > start && content.as_bytes()[end - 1] == b'\r' {
                end -= 1;
            }
            out.push(Line {
                start,
                end,
                text: &content[start..end],
            });
            start = i + 1;
        }
    }
    if start < content.len() {
        out.push(Line {
            start,
            end: content.len(),
            text: &content[start..],
        });
    }
    out
}

/// Split `find` into lines, dropping one trailing empty line produced by a
/// terminating newline. Returns the lines and whether `find` ended in `\n`.
fn find_lines(find: &str) -> (Vec<&str>, bool) {
    let ends_nl = find.ends_with('\n');
    let body = find.strip_suffix('\n').unwrap_or(find);
    let lines = body
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    (lines, ends_nl)
}

/// Generic line-window matcher: every window of `find`'s line count whose
/// lines all satisfy `eq(content_line, find_line)` becomes a span covering
/// whole lines. If `find` ended in a newline, the span also covers the
/// newline after the window so the replacement's own newline lines up.
fn window_match(content: &str, find: &str, eq: impl Fn(&str, &str) -> bool) -> Vec<Span> {
    let (flines, ends_nl) = find_lines(find);
    if flines.is_empty() || flines.iter().all(|l| l.trim().is_empty()) {
        return Vec::new();
    }
    let clines = lines_of(content);
    let n = flines.len();
    if n > clines.len() {
        return Vec::new();
    }
    let mut spans = Vec::new();
    for i in 0..=clines.len() - n {
        if (0..n).all(|k| eq(clines[i + k].text, flines[k])) {
            let start = clines[i].start;
            let mut end = clines[i + n - 1].end;
            if ends_nl {
                end = extend_over_newline(content, end);
            }
            spans.push((start, end));
        }
    }
    spans
}

fn extend_over_newline(content: &str, end: usize) -> usize {
    let bytes = content.as_bytes();
    let mut e = end;
    if e < bytes.len() && bytes[e] == b'\r' {
        e += 1;
    }
    if e < bytes.len() && bytes[e] == b'\n' {
        e += 1;
    }
    e
}

// ----------------------------------------------------------- strategies

fn exact(content: &str, find: &str) -> Vec<Span> {
    if find.is_empty() {
        return Vec::new();
    }
    content
        .match_indices(find)
        .map(|(s, m)| (s, s + m.len()))
        .collect()
}

fn line_trimmed(content: &str, find: &str) -> Vec<Span> {
    window_match(content, find, |c, f| c.trim() == f.trim())
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn whitespace_normalized(content: &str, find: &str) -> Vec<Span> {
    window_match(content, find, |c, f| collapse_ws(c) == collapse_ws(f))
}

/// Leading indentation in bytes, counting only ASCII spaces and tabs so
/// slicing by this width is always on a char boundary.
fn indent_width(s: &str) -> usize {
    s.bytes().take_while(|b| *b == b' ' || *b == b'\t').count()
}

fn min_indent(lines: &[&str]) -> usize {
    lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| indent_width(l))
        .min()
        .unwrap_or(0)
}

fn dedent<'a>(lines: &[&'a str]) -> Vec<&'a str> {
    let m = min_indent(lines);
    lines
        .iter()
        .map(|l| {
            if l.trim().is_empty() {
                ""
            } else {
                l[m.min(l.len())..].trim_end()
            }
        })
        .collect()
}

fn indentation_flexible(content: &str, find: &str) -> Vec<Span> {
    let (flines, ends_nl) = find_lines(find);
    if flines.iter().all(|l| l.trim().is_empty()) {
        return Vec::new();
    }
    let target = dedent(&flines);
    let clines = lines_of(content);
    let n = flines.len();
    if n > clines.len() {
        return Vec::new();
    }
    let mut spans = Vec::new();
    for i in 0..=clines.len() - n {
        let window: Vec<&str> = clines[i..i + n].iter().map(|l| l.text).collect();
        if dedent(&window) == target {
            let start = clines[i].start;
            let mut end = clines[i + n - 1].end;
            if ends_nl {
                end = extend_over_newline(content, end);
            }
            spans.push((start, end));
        }
    }
    spans
}

fn unicode_fold(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{2032}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{2033}' => '"',
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2212}' => '-',
            '\u{00A0}' | '\u{2007}' | '\u{202F}' | '\u{3000}' => ' ',
            '\u{2026}' => '.',
            other => other,
        })
        .filter(|c| *c != '\u{200B}' && *c != '\u{FEFF}')
        .collect()
}

fn unicode_normalized(content: &str, find: &str) -> Vec<Span> {
    let folded = unicode_fold(find);
    window_match(content, &folded, |c, f| unicode_fold(c).trim() == f.trim())
}

fn unescape(s: &str) -> Option<String> {
    if !s.contains('\\') {
        return None;
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    let mut changed = false;
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek().copied() {
                Some('n') => {
                    out.push('\n');
                    chars.next();
                    changed = true;
                }
                Some('t') => {
                    out.push('\t');
                    chars.next();
                    changed = true;
                }
                Some('r') => {
                    out.push('\r');
                    chars.next();
                    changed = true;
                }
                Some('"') => {
                    out.push('"');
                    chars.next();
                    changed = true;
                }
                Some('\'') => {
                    out.push('\'');
                    chars.next();
                    changed = true;
                }
                Some('\\') => {
                    out.push('\\');
                    chars.next();
                    changed = true;
                }
                _ => out.push(c),
            }
        } else {
            out.push(c);
        }
    }
    changed.then_some(out)
}

fn escape_normalized(content: &str, find: &str) -> Vec<Span> {
    match unescape(find) {
        Some(u) => {
            let direct = exact(content, &u);
            if direct.is_empty() {
                line_trimmed(content, &u)
            } else {
                direct
            }
        }
        None => Vec::new(),
    }
}

fn trimmed_boundary(content: &str, find: &str) -> Vec<Span> {
    let t = find.trim();
    if t.is_empty() || t == find {
        return Vec::new();
    }
    let direct = exact(content, t);
    if direct.is_empty() {
        line_trimmed(content, t)
    } else {
        direct
    }
}

/// Last resort for blocks of 3+ lines: first and last lines match after
/// trimming, the block has the same line count, and the interior lines are
/// at least 75% similar on average. Only a single candidate is accepted,
/// even under `replace_all`, because similarity is not identity.
fn block_anchor(content: &str, find: &str) -> Vec<Span> {
    let (flines, ends_nl) = find_lines(find);
    let n = flines.len();
    if n < 3 {
        return Vec::new();
    }
    let first = flines[0].trim();
    let last = flines[n - 1].trim();
    if first.is_empty() || last.is_empty() {
        return Vec::new();
    }
    let clines = lines_of(content);
    if n > clines.len() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    for i in 0..=clines.len() - n {
        if clines[i].text.trim() != first || clines[i + n - 1].text.trim() != last {
            continue;
        }
        let mut total = 0.0;
        for k in 1..n - 1 {
            total += similarity(clines[i + k].text.trim(), flines[k].trim());
        }
        let avg = total / (n - 2) as f64;
        if avg >= 0.75 {
            let start = clines[i].start;
            let mut end = clines[i + n - 1].end;
            if ends_nl {
                end = extend_over_newline(content, end);
            }
            hits.push((start, end));
        }
    }
    // Several fuzzy candidates are all returned so the caller refuses them
    // as ambiguous rather than guessing.
    hits
}

/// Normalized Levenshtein similarity in [0, 1] over chars, capped at 240
/// chars per side to bound cost.
pub fn similarity(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().take(240).collect();
    let b: Vec<char> = b.chars().take(240).collect();
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    let dist = prev[b.len()];
    1.0 - dist as f64 / a.len().max(b.len()) as f64
}

/// When a looser strategy matched, carry the file's real indentation into
/// the replacement. Pairing `find` line k with matched line k gives a map
/// from the indent the model wrote to the indent the file really has
/// (e.g. 0 -> 4 spaces, 4 -> 8 spaces). Each replacement line is re-based
/// through that map; a deeper, unmapped indent keeps its extra whitespace
/// on top of the nearest shallower mapping. If the map is inconsistent
/// (one model indent maps to two file indents) or is the identity, the
/// replacement is used verbatim.
fn reindent(find: &str, matched: &str, replacement: &str) -> String {
    let (flines, _) = find_lines(find);
    let (mlines, _) = find_lines(matched);
    if flines.len() != mlines.len() {
        return replacement.to_string();
    }
    let mut map: Vec<(usize, String)> = Vec::new();
    for (f, m) in flines.iter().zip(&mlines) {
        if f.trim().is_empty() || m.trim().is_empty() {
            continue;
        }
        let fw = indent_width(f);
        let mp = m[..indent_width(m)].to_string();
        match map.iter().find(|(k, _)| *k == fw) {
            Some((_, existing)) if *existing != mp => {
                return realign(&flines, &mlines, replacement);
            }
            Some(_) => {}
            None => map.push((fw, mp)),
        }
    }
    let identity = flines
        .iter()
        .zip(&mlines)
        .all(|(f, m)| f.trim().is_empty() || f[..indent_width(f)] == m[..indent_width(m)]);
    if map.is_empty() || identity {
        return replacement.to_string();
    }
    map.sort_by_key(|(k, _)| *k);
    let ends_nl = replacement.ends_with('\n');
    let body = replacement.strip_suffix('\n').unwrap_or(replacement);
    let mut out = body
        .split('\n')
        .map(|l| {
            if l.trim().is_empty() {
                return l.to_string();
            }
            let w = indent_width(l);
            match map.iter().rev().find(|(k, _)| *k <= w) {
                Some((k, prefix)) => format!("{prefix}{}", &l[*k..]),
                None => l.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    if ends_nl {
        out.push('\n');
    }
    out
}

/// Fallback for a flattened or scrambled `find` (one model indent maps to
/// several file indents): align replacement lines to `find` lines by their
/// trimmed text. An aligned line takes its matched file line's indent; a
/// changed line takes the indent of the line it replaces; an inserted line
/// takes the indent of the line before it, plus whatever extra indentation
/// the model gave it relative to that anchor.
fn realign(flines: &[&str], mlines: &[&str], replacement: &str) -> String {
    let n = flines.len();
    let ends_nl = replacement.ends_with('\n');
    let body = replacement.strip_suffix('\n').unwrap_or(replacement);
    let rlines: Vec<&str> = body.split('\n').collect();
    let mut next = 0usize;
    let mut last: Option<usize> = None;
    let mut out = Vec::new();
    for (k, l) in rlines.iter().enumerate() {
        let l = *l;
        if l.trim().is_empty() {
            out.push(l.to_string());
            continue;
        }
        let hit = (next..n).find(|&j| flines[j].trim() == l.trim());
        let (idx, aligned) = match hit {
            Some(j) => {
                next = j + 1;
                (j, true)
            }
            None => {
                // A changed line most likely replaces the next unconsumed
                // find line, unless that line still appears later in the
                // replacement (then this is an insertion).
                let replaces = next < n
                    && !rlines[k + 1..]
                        .iter()
                        .any(|r| r.trim() == flines[next].trim());
                if replaces {
                    next += 1;
                    (next - 1, false)
                } else {
                    (last.unwrap_or(k.min(n - 1)), false)
                }
            }
        };
        last = Some(idx);
        let m = mlines[idx];
        let prefix = &m[..indent_width(m)];
        let w = indent_width(l);
        let extra = if aligned {
            0
        } else {
            w.saturating_sub(indent_width(flines[idx]))
        };
        out.push(format!("{prefix}{}{}", &l[w - extra..w], &l[w..]));
    }
    let mut joined = out.join("\n");
    if ends_nl {
        joined.push('\n');
    }
    joined
}

/// Model-facing hint when nothing matched: the most similar window of the
/// same line count, with its 1-based line number and similarity, so the
/// next attempt can copy the real text instead of guessing again.
fn closest_hint(content: &str, find: &str) -> String {
    let (flines, _) = find_lines(find);
    let n = flines.len().max(1);
    let clines = lines_of(content);
    if clines.is_empty() {
        return "the file is empty".to_string();
    }
    let n = n.min(clines.len());
    let needle = flines
        .iter()
        .map(|l| l.trim())
        .collect::<Vec<_>>()
        .join("\n");
    let mut best: Option<(f64, usize)> = None;
    // Bound the scan so a huge file cannot stall the tool.
    let limit = clines.len().saturating_sub(n) + 1;
    for i in 0..limit.min(20_000) {
        let hay = clines[i..i + n]
            .iter()
            .map(|l| l.text.trim())
            .collect::<Vec<_>>()
            .join("\n");
        let s = similarity(&hay, &needle);
        if best.is_none_or(|(b, _)| s > b) {
            best = Some((s, i));
        }
    }
    match best {
        Some((s, i)) if s >= 0.5 => {
            let snippet = clines[i..i + n]
                .iter()
                .map(|l| l.text)
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "closest region starts at line {} ({:.0}% similar); re-read the file and copy it exactly:\n{}",
                i + 1,
                s * 100.0,
                truncate_chars(&snippet, 1200)
            )
        }
        _ => "no similar region found; re-read the file before editing".to_string(),
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max).collect();
        t.push('…');
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(c: &str, f: &str, r: &str) -> Result<EditPlan, PlanError> {
        plan_edit(c, f, r, false)
    }

    #[test]
    fn exact_wins_and_is_verbatim() {
        let p = apply("a = 1\nb = 2\n", "b = 2", "b = 3").unwrap();
        assert_eq!(p.strategy, "exact");
        assert_eq!(p.new_content, "a = 1\nb = 3\n");
    }

    #[test]
    fn wrong_indentation_is_rebased_onto_file_indent() {
        let c = "fn main() {\n    if x {\n        run();\n    }\n}\n";
        // Model dropped the indentation entirely.
        let p = apply(
            c,
            "if x {\n    run();\n}",
            "if x {\n    run();\n    log();\n}",
        )
        .unwrap();
        assert_eq!(p.strategy, "line_trimmed");
        assert_eq!(
            p.new_content,
            "fn main() {\n    if x {\n        run();\n        log();\n    }\n}\n"
        );
    }

    #[test]
    fn trailing_whitespace_drift_matches() {
        let c = "let a = 1;   \nlet b = 2;\n";
        let p = apply(c, "let a = 1;\nlet b = 2;", "let a = 9;\nlet b = 2;").unwrap();
        assert_eq!(p.new_content, "let a = 9;\nlet b = 2;\n");
    }

    #[test]
    fn collapsed_internal_whitespace_matches() {
        let c = "call(a,    b,\tc);\n";
        let p = apply(c, "call(a, b, c);", "call(a, b);").unwrap();
        assert_eq!(p.strategy, "whitespace_normalized");
        assert_eq!(p.new_content, "call(a, b);\n");
    }

    #[test]
    fn smart_quotes_fold() {
        let c = "say(\"hi\");\n";
        let p = apply(c, "say(\u{201C}hi\u{201D});", "say(\"yo\");").unwrap();
        assert_eq!(p.strategy, "unicode_normalized");
        assert_eq!(p.new_content, "say(\"yo\");\n");
    }

    #[test]
    fn literal_escapes_unescape() {
        let c = "a\nb\n";
        let p = apply(c, "a\\nb", "x\ny").unwrap();
        assert_eq!(p.strategy, "escape_normalized");
        assert_eq!(p.new_content, "x\ny\n");
    }

    #[test]
    fn padded_find_trims_to_boundary() {
        let c = "alpha beta gamma";
        let p = apply(c, "  beta  ", "BETA").unwrap();
        assert_eq!(p.strategy, "trimmed_boundary");
        assert_eq!(p.new_content, "alpha BETA gamma");
    }

    #[test]
    fn block_anchor_tolerates_interior_drift_once() {
        let c = "fn f() {\n    let total = compute(a, b);\n    total\n}\n";
        let find = "fn f() {\n    let total = compute(a,b) ;\n    total\n}";
        let p = apply(c, find, "fn f() {\n    0\n}").unwrap();
        assert_eq!(p.strategy, "block_anchor");
        assert_eq!(p.new_content, "fn f() {\n    0\n}\n");
    }

    #[test]
    fn block_anchor_never_guesses_between_two() {
        let c = "{\n  value_1\n}\n{\n  value_2\n}\n";
        let e = plan_edit(c, "{\n  value_3\n}", "{}", true).unwrap_err();
        assert!(matches!(
            e,
            PlanError::Ambiguous {
                strategy: "block_anchor",
                count: 2,
                ..
            }
        ));
    }

    #[test]
    fn fuzzy_duplicate_is_ambiguous_not_first_hit() {
        let c = "  x();\n  y();\n    x();\n";
        let e = apply(c, "x();\n", "z();\n").unwrap_err();
        match e {
            PlanError::Ambiguous { count, lines, .. } => {
                assert_eq!(count, 2);
                assert_eq!(lines, vec![1, 3]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn precise_strategy_is_never_overridden() {
        // Exact matches once; the looser line_trimmed would match twice.
        let c = "foo();\n  foo();\n";
        let p = apply(c, "  foo();", "  bar();").unwrap();
        assert_eq!(p.strategy, "exact");
        assert_eq!(p.new_content, "foo();\n  bar();\n");
    }

    #[test]
    fn not_found_points_at_closest_region() {
        let c = "one\ntwo\nfn compute_total(a: u32) -> u32 {\nthree\n";
        let e = apply(c, "fn compute_totals(a: u64) -> u32 {", "x").unwrap_err();
        match e {
            PlanError::NotFound { hint } => {
                assert!(hint.contains("line 3"), "{hint}");
                assert!(hint.contains("compute_total(a: u32)"), "{hint}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn dedented_block_is_rebased_line_by_line() {
        let c = "impl A {\n    fn f() {\n        let a = 1;\n    }\n}\n";
        let find = "fn f() {\nlet a = 1;\n}\n";
        let p = apply(c, find, "fn f() {\nlet a = 2;\n    nested();\n}\n").unwrap();
        assert_eq!(p.strategy, "line_trimmed");
        assert_eq!(
            p.new_content,
            "impl A {\n    fn f() {\n        let a = 2;\n            nested();\n    }\n}\n"
        );
    }

    #[test]
    fn inconsistent_indent_map_falls_back_to_verbatim() {
        let c = "a\n    b\n";
        let p = apply(c, "a\nb", "x\ny").unwrap();
        // find indent 0 maps to both "" and "    "; unaligned lines keep
        // the file's structure positionally.
        assert_eq!(p.new_content, "x\n    y\n");
    }

    #[test]
    fn crlf_files_keep_their_bytes_outside_the_edit() {
        let c = "a\r\n  b\r\nc\r\n";
        let p = apply(c, "b\nc", "B\nc").unwrap();
        // The replaced lines take the file's indentation; the surrounding
        // CRLF bytes survive untouched.
        assert_eq!(p.new_content, "a\r\n  B\nc\r\n");
    }

    #[test]
    fn unicode_indent_never_panics() {
        let c = "\u{00A0}\u{00A0}x = 1\n";
        let _ = apply(c, "x = 1\ny", "z");
        let _ = apply(c, "  x = 1", "z");
    }

    #[test]
    fn blank_find_never_matches() {
        assert!(apply("a\n\n", "\n", "x").is_err());
        assert!(matches!(
            apply("a\nb\n", "  \n", "x"),
            Err(PlanError::NotFound { .. })
        ));
    }
}
