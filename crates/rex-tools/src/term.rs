//! Clean command output before the model sees it. Terminal escape
//! sequences (colours, cursor moves, window titles) are noise the model
//! tends to copy into files, bare control characters can hide text, and
//! Unicode tag characters are invisible to people but read by the model
//! ("ASCII smuggling"). Hermes strips the same classes from subprocess
//! output (`tools/ansi_strip.py`); this is REX's own scanner.

/// `text` without ANSI/ECMA-48 escape sequences (CSI, OSC, DCS/SOS/PM/APC
/// strings, nF and single-character escapes, 8-bit C1 controls), without
/// C0 control characters other than tab, newline and carriage return, and
/// without Unicode tag characters except inside an emoji tag sequence
/// (the flags of Scotland, Wales and England). An escape whose string
/// never ends drops only its introducer, so the rest of the output stays.
pub fn clean_terminal_text(text: &str) -> String {
    if !text.chars().any(needs_work) {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\u{1b}' => i = skip_escape(&chars, i),
            '\u{9b}' => i = skip_csi(&chars, i + 1),
            '\u{9d}' => {
                i = find_end(&chars, i + 1, |cs, j| match cs[j] {
                    '\u{7}' | '\u{9c}' => Some(j + 1),
                    _ => None,
                })
                .unwrap_or(i + 1)
            }
            '\u{80}'..='\u{9f}' => i += 1,
            '\t' | '\n' | '\r' => {
                out.push(c);
                i += 1;
            }
            '\u{0}'..='\u{1f}' | '\u{7f}' => i += 1,
            '\u{1F3F4}' => {
                let end = emoji_tag_end(&chars, i + 1);
                out.extend(&chars[i..end]);
                i = end;
            }
            '\u{E0000}'..='\u{E007F}' => i += 1,
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

fn needs_work(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}' | '\u{7f}'..='\u{9f}' | '\u{E0000}'..='\u{E007F}')
}

/// Index after the escape starting at `i` (an ESC).
fn skip_escape(chars: &[char], i: usize) -> usize {
    let Some(&next) = chars.get(i + 1) else {
        return i + 1;
    };
    match next {
        '[' => skip_csi(chars, i + 2),
        ']' => find_end(chars, i + 2, |cs, j| match cs[j] {
            '\u{7}' => Some(j + 1),
            '\u{1b}' if cs.get(j + 1) == Some(&'\\') => Some(j + 2),
            _ => None,
        })
        .unwrap_or(i + 2),
        'P' | 'X' | '^' | '_' => find_end(chars, i + 2, |cs, j| {
            (cs[j] == '\u{1b}' && cs.get(j + 1) == Some(&'\\')).then_some(j + 2)
        })
        .unwrap_or(i + 2),
        '\u{20}'..='\u{2f}' => {
            let mut j = i + 1;
            while j < chars.len() && ('\u{20}'..='\u{2f}').contains(&chars[j]) {
                j += 1;
            }
            match chars.get(j) {
                Some(c) if ('\u{30}'..='\u{7e}').contains(c) => j + 1,
                _ => i + 1,
            }
        }
        '\u{30}'..='\u{7e}' => i + 2,
        _ => i + 1,
    }
}

/// Index after a CSI whose parameters start at `j`. A sequence with no
/// final byte ends where it stops being one.
fn skip_csi(chars: &[char], mut j: usize) -> usize {
    while j < chars.len() && ('\u{30}'..='\u{3f}').contains(&chars[j]) {
        j += 1;
    }
    while j < chars.len() && ('\u{20}'..='\u{2f}').contains(&chars[j]) {
        j += 1;
    }
    match chars.get(j) {
        Some(c) if ('\u{40}'..='\u{7e}').contains(c) => j + 1,
        _ => j,
    }
}

/// First end found by `end` scanning from `j`, if any.
fn find_end(
    chars: &[char],
    j: usize,
    end: impl Fn(&[char], usize) -> Option<usize>,
) -> Option<usize> {
    (j..chars.len()).find_map(|k| end(chars, k))
}

/// For a black flag at `i - 1`: the index after its tag sequence when the
/// tags form a complete emoji tag sequence, else `i` (the flag alone).
fn emoji_tag_end(chars: &[char], i: usize) -> usize {
    let mut j = i;
    while j < chars.len() && ('\u{E0020}'..='\u{E007E}').contains(&chars[j]) {
        j += 1;
    }
    if j > i && chars.get(j) == Some(&'\u{E007F}') {
        j + 1
    } else {
        i
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_sequences_are_removed() {
        let cases = [
            ("\u{1b}[1;31merror\u{1b}[0m: x", "error: x"),
            ("\u{1b}[?25lhidden cursor\u{1b}[?25h", "hidden cursor"),
            ("\u{1b}[2K\u{1b}[1Gline", "line"),
            ("\u{1b}]0;title\u{7}after", "after"),
            (
                "\u{1b}]8;;https://x.org\u{1b}\\link\u{1b}]8;;\u{1b}\\",
                "link",
            ),
            ("\u{1b}Pq#0;2;0;0;0\u{1b}\\img", "img"),
            ("\u{1b}_apc\u{1b}\\a", "a"),
            ("\u{1b}(Bascii", "ascii"),
            ("\u{1b}=keypad\u{1b}>", "keypad"),
            ("\u{9b}32mgreen\u{9b}0m", "green"),
            ("\u{9d}0;t\u{9c}ok", "ok"),
            ("a\u{85}b", "ab"),
        ];
        for (input, want) in cases {
            assert_eq!(clean_terminal_text(input), want, "{input:?}");
        }
    }

    #[test]
    fn controls_and_tags_are_removed_but_text_stays() {
        assert_eq!(clean_terminal_text("a\u{7}b\u{8}c\u{0}d\u{7f}e"), "abcde");
        assert_eq!(clean_terminal_text("tab\tnl\ncr\r\n"), "tab\tnl\ncr\r\n");
        // kept on the slow path too, next to something that is removed
        assert_eq!(clean_terminal_text("t\tn\nc\r\u{1b}[0mx"), "t\tn\nc\rx");
        // tag characters spelling "ignore" vanish
        let smuggled: String = "ok"
            .chars()
            .chain(
                "ignore"
                    .chars()
                    .map(|c| char::from_u32(0xE0000 + c as u32).unwrap()),
            )
            .collect();
        assert_eq!(clean_terminal_text(&smuggled), "ok");
        // a flag's tag sequence is kept; a flag without one is just a flag
        let scotland = "\u{1F3F4}\u{E0067}\u{E0062}\u{E0073}\u{E0063}\u{E0074}\u{E007F}";
        assert_eq!(clean_terminal_text(scotland), scotland);
        assert_eq!(clean_terminal_text("\u{1F3F4}\u{E0067}x"), "\u{1F3F4}x");
        assert_eq!(clean_terminal_text("\u{1F3F4}\u{E007F}"), "\u{1F3F4}");
        // plain text, including other non-ASCII, is untouched
        let plain = "héllo – 世界 ✓";
        assert_eq!(clean_terminal_text(plain), plain);
    }

    #[test]
    fn broken_escapes_drop_only_the_introducer() {
        assert_eq!(clean_terminal_text("\u{1b}]never ends"), "never ends");
        assert_eq!(clean_terminal_text("\u{1b}Pdcs open"), "dcs open");
        assert_eq!(clean_terminal_text("\u{9d}osc open"), "osc open");
        assert_eq!(clean_terminal_text("end\u{1b}"), "end");
        assert_eq!(clean_terminal_text("\u{1b}[12"), "");
        assert_eq!(clean_terminal_text("\u{1b}[12\u{e9}x"), "\u{e9}x");
        assert_eq!(clean_terminal_text("\u{1b}(\u{e9}"), "(\u{e9}");
        assert_eq!(clean_terminal_text("\u{1b}\u{e9}"), "\u{e9}");
    }
}
