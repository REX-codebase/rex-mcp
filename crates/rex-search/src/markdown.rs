//! HTML to Markdown for `web_fetch` (REX's own converter, no dependency).
//!
//! opencode's webfetch returns markdown by default (`tool/webfetch.ts`).
//! This keeps the parts a model uses to read docs: headings, paragraphs,
//! lists, links (made absolute), code blocks and inline code, tables as
//! `|` rows. Scripts, styles, `<head>`, SVG and templates are dropped. It
//! is a reader, not a round-trip converter: unknown tags keep their text.

use url::Url;

const SKIP: &[&str] = &[
    "script", "style", "noscript", "head", "svg", "template", "iframe",
];
const BLOCK: &[&str] = &[
    "p",
    "div",
    "section",
    "article",
    "header",
    "footer",
    "main",
    "nav",
    "aside",
    "table",
    "blockquote",
    "form",
    "figure",
    "figcaption",
    "dl",
    "dt",
    "dd",
    "details",
    "summary",
];

struct Tag<'a> {
    name: String,
    close: bool,
    attrs: &'a str,
}

fn parse_tag(inner: &str) -> Option<Tag<'_>> {
    let (close, rest) = match inner.strip_prefix('/') {
        Some(r) => (true, r),
        None => (false, inner),
    };
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '/')
        .unwrap_or(rest.len());
    let name = rest[..end].to_ascii_lowercase();
    // custom elements (`<mdbook-sidebar>`) carry a hyphen
    if !name.starts_with(|c: char| c.is_ascii_alphabetic())
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return None;
    }
    Some(Tag {
        name,
        close,
        attrs: &rest[end..],
    })
}

/// Value of attribute `key` (quoted or bare) in a tag's attribute text.
fn attr(attrs: &str, key: &str) -> Option<String> {
    let lower = attrs.to_ascii_lowercase();
    let mut from = 0;
    while let Some(p) = lower[from..].find(key) {
        let at = from + p;
        from = at + key.len();
        let boundary = at == 0 || lower.as_bytes()[at - 1].is_ascii_whitespace();
        let rest = lower[from..].trim_start();
        if !boundary || !rest.starts_with('=') {
            continue;
        }
        let start = attrs.len() - rest.len() + 1;
        let v = attrs[start..].trim_start();
        let val = match v.chars().next() {
            Some(q @ ('"' | '\'')) => v[1..].split(q).next().unwrap_or(""),
            _ => v
                .split(|c: char| c.is_whitespace() || c == '>')
                .next()
                .unwrap_or(""),
        };
        return Some(decode(val));
    }
    None
}

/// Decode the common named entities plus decimal and hex references.
pub(crate) fn decode(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find('&') {
        out.push_str(&rest[..p]);
        rest = &rest[p..];
        let window = rest.floor_char_boundary(rest.len().min(12));
        let end = rest[..window].find(';');
        let rep = end.and_then(|e| {
            let ent = &rest[1..e];
            let ch = match ent {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" | "#39" => Some('\''),
                "nbsp" => Some(' '),
                _ => ent
                    .strip_prefix("#x")
                    .or_else(|| ent.strip_prefix("#X"))
                    .and_then(|h| u32::from_str_radix(h, 16).ok())
                    .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                    .and_then(char::from_u32),
            };
            ch.map(|c| (c, e + 1))
        });
        match rep {
            Some((c, len)) => {
                out.push(c);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

struct Writer {
    out: String,
    pending_space: bool,
}

impl Writer {
    fn text(&mut self, t: &str) {
        if t.starts_with(char::is_whitespace) {
            self.pending_space = true;
        }
        let mut any = false;
        for word in t.split_whitespace() {
            if self.pending_space && !self.out.is_empty() && !self.out.ends_with([' ', '\n', '[']) {
                self.out.push(' ');
            }
            self.out.push_str(word);
            self.pending_space = true;
            any = true;
        }
        if any {
            self.pending_space = t.ends_with(char::is_whitespace);
        }
    }
    fn raw(&mut self, s: &str) {
        self.out.push_str(s);
        self.pending_space = false;
    }
    fn newline(&mut self) {
        let t = self.out.trim_end_matches(' ').len();
        self.out.truncate(t);
        if !self.out.is_empty() && !self.out.ends_with('\n') {
            self.out.push('\n');
        }
        self.pending_space = false;
    }
    fn blank_line(&mut self) {
        self.newline();
        if !self.out.is_empty() && !self.out.ends_with("\n\n") {
            self.out.push('\n');
        }
    }
}

/// Convert an HTML document to Markdown. Relative links resolve against
/// `base`; `javascript:` and fragment-only links keep only their text.
pub fn html_to_markdown(html: &str, base: &Url) -> String {
    let mut w = Writer {
        out: String::with_capacity(html.len().min(64_000)),
        pending_space: false,
    };
    let lower = html.to_ascii_lowercase();
    let mut lists: Vec<(bool, usize)> = Vec::new(); // (ordered, counter)
    let mut links: Vec<Option<String>> = Vec::new();
    let mut in_pre = false;
    let mut i = 0;
    while i < html.len() {
        let Some(off) = html[i..].find('<') else {
            if in_pre {
                w.raw(&decode(&html[i..]));
            } else {
                w.text(&decode(&html[i..]));
            }
            break;
        };
        if off > 0 {
            let chunk = decode(&html[i..i + off]);
            if in_pre {
                w.raw(&chunk);
            } else {
                w.text(&chunk);
            }
        }
        let at = i + off;
        if lower[at..].starts_with("<!--") {
            i = lower[at..].find("-->").map_or(html.len(), |n| at + n + 3);
            continue;
        }
        let Some(close) = html[at..].find('>') else {
            break;
        };
        let inner = &html[at + 1..at + close];
        i = at + close + 1;
        let Some(tag) = parse_tag(inner) else {
            if inner.starts_with('!') || inner.starts_with('?') {
                continue; // doctype, CDATA, processing instruction
            }
            // a bare `<` in text (`1 < 2`): keep it and rescan after it
            w.text("<");
            i = at + 1;
            continue;
        };
        let name = tag.name.as_str();
        if !tag.close && SKIP.contains(&name) {
            let needle = format!("</{name}");
            i = lower[i..].find(&needle).map_or(html.len(), |n| {
                let e = i + n;
                lower[e..].find('>').map_or(html.len(), |m| e + m + 1)
            });
            continue;
        }
        match (name, tag.close) {
            ("h1" | "h2" | "h3" | "h4" | "h5" | "h6", false) => {
                w.blank_line();
                let level = name[1..].parse::<usize>().unwrap_or(1);
                w.raw(&format!("{} ", "#".repeat(level)));
            }
            ("h1" | "h2" | "h3" | "h4" | "h5" | "h6", true) => w.blank_line(),
            ("pre", false) => {
                w.blank_line();
                w.raw("```\n");
                in_pre = true;
            }
            ("pre", true) => {
                if !w.out.ends_with('\n') {
                    w.raw("\n");
                }
                w.raw("```");
                in_pre = false;
                w.blank_line();
            }
            ("code", _) if !in_pre => {
                if tag.close {
                    w.raw("`");
                    w.pending_space = false;
                } else {
                    if w.pending_space && !w.out.ends_with([' ', '\n']) && !w.out.is_empty() {
                        w.raw(" ");
                    }
                    w.raw("`");
                }
            }
            ("ul" | "ol", false) => {
                w.newline();
                lists.push((name == "ol", 0));
            }
            ("ul" | "ol", true) => {
                lists.pop();
                if lists.is_empty() {
                    w.blank_line();
                } else {
                    w.newline();
                }
            }
            ("li", false) => {
                w.newline();
                let depth = lists.len().saturating_sub(1);
                let marker = match lists.last_mut() {
                    Some((true, n)) => {
                        *n += 1;
                        format!("{n}. ")
                    }
                    _ => "- ".to_string(),
                };
                w.raw(&format!("{}{marker}", "  ".repeat(depth)));
            }
            ("li", true) => w.newline(),
            ("br", _) => {
                if in_pre {
                    w.raw("\n");
                } else {
                    w.newline();
                }
            }
            ("hr", _) => {
                w.blank_line();
                w.raw("---");
                w.blank_line();
            }
            ("tr", false) => w.newline(),
            ("tr", true) => {
                if !w.out.ends_with('\n') {
                    w.raw(" |");
                }
                w.newline();
            }
            ("td" | "th", false) => {
                w.raw(if w.out.ends_with('\n') || w.out.is_empty() {
                    "| "
                } else {
                    " | "
                });
            }
            ("a", false) => {
                let href = attr(tag.attrs, "href")
                    .map(|h| h.trim().to_string())
                    .filter(|h| !h.is_empty() && !h.starts_with('#'))
                    .filter(|h| !h.to_ascii_lowercase().starts_with("javascript:"))
                    .and_then(|h| base.join(&h).ok())
                    .filter(|u| matches!(u.scheme(), "http" | "https" | "mailto"))
                    .map(|u| u.to_string());
                if href.is_some() {
                    if w.pending_space && !w.out.ends_with([' ', '\n']) && !w.out.is_empty() {
                        w.raw(" ");
                    }
                    w.raw("[");
                }
                links.push(href);
            }
            ("a", true) => {
                if let Some(Some(href)) = links.pop() {
                    if w.out.ends_with('[') {
                        // empty link text: drop the bracket
                        w.out.pop();
                    } else {
                        w.raw(&format!("]({href})"));
                    }
                }
            }
            ("strong" | "b", _) => w.raw("**"),
            ("em" | "i", _) => w.raw("_"),
            ("blockquote", false) => {
                w.blank_line();
                w.raw("> ");
            }
            (n, _) if BLOCK.contains(&n) => w.blank_line(),
            _ => {}
        }
    }
    // tidy: no trailing spaces, at most one blank line in a row
    let mut out = String::with_capacity(w.out.len());
    let mut blank = 0;
    for line in w.out.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            blank += 1;
            if blank > 1 || out.is_empty() {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn md(html: &str) -> String {
        html_to_markdown(
            html,
            &Url::parse("https://docs.example.org/guide/intro").unwrap(),
        )
    }

    #[test]
    fn headings_paragraphs_lists_and_links() {
        let out = md("<html><head><title>T</title><style>p{}</style></head><body><h1>Guide</h1><p>Read the <a href=\"../api/\">API docs</a> and <a href='#x'>this</a>.</p><ul><li>one</li><li>two<ol><li>a</li><li>b</li></ol></li></ul><script>alert(1)</script></body></html>");
        assert_eq!(
            out,
            "# Guide\n\nRead the [API docs](https://docs.example.org/api/) and this.\n\n- one\n- two\n  1. a\n  2. b"
        );
    }

    #[test]
    fn code_blocks_keep_whitespace_and_inline_code_is_marked() {
        let out = md("<p>Call <code>spawn()</code> first.</p><pre><code>fn main() {\n    let x = 1 &lt; 2;\n}\n</code></pre><p>after</p>");
        assert_eq!(
            out,
            "Call `spawn()` first.\n\n```\nfn main() {\n    let x = 1 < 2;\n}\n```\n\nafter"
        );
    }

    #[test]
    fn entities_utf8_tables_and_unsafe_links() {
        let out = md("<p>caf&eacute; café &#233; &#x1F600; &amp;</p><table><tr><th>k</th><th>v</th></tr><tr><td>a</td><td>1</td></tr></table><p><a href=\"javascript:alert(1)\">x</a> <a href=\"data:text/html,hi\">y</a></p>");
        assert!(out.starts_with("caf&eacute; café é 😀 &"), "{out}");
        assert!(out.contains("| k | v |\n| a | 1 |"), "{out}");
        assert!(out.ends_with("x y"), "{out}");
        assert_eq!(
            md("<p>a <my-widget class=\"x\">b</my-widget> 1 < 2</p>"),
            "a b 1 < 2"
        );
        assert!(
            !out.contains("javascript") && !out.contains("data:"),
            "{out}"
        );
    }

    #[test]
    fn decode_handles_bad_references() {
        assert_eq!(
            decode("a &bogus; &#xZZ; &#99999999; &"),
            "a &bogus; &#xZZ; &#99999999; &"
        );
        assert_eq!(decode("&lt;b&gt;"), "<b>");
    }

    #[test]
    fn hostile_fragments_never_panic() {
        let parts = [
            "<",
            ">",
            "&",
            "&#",
            ";",
            "é",
            "日",
            "<a href=",
            "\"",
            "'",
            "</a>",
            "<pre>",
            "</pre>",
            "<!--",
            "-->",
            "<code>",
            "</",
            "<li>",
            "</ol>",
            "<script>",
            "x",
            " ",
            "\n",
            "<br/>",
            "<td>",
            "&#x1F600;",
            "<my-el>",
            "<!doctype html>",
            "<h9>",
            "<h1>",
        ];
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        for _ in 0..3000 {
            let mut doc = String::new();
            for _ in 0..24 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                doc.push_str(parts[(seed % parts.len() as u64) as usize]);
            }
            let _ = md(&doc);
        }
    }
}
