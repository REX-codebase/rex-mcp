//! Full text of command output the model only saw clipped.
//!
//! A `run_command` result reaches the model as head + tail within a few
//! thousand characters, so the middle of a long test or build log is lost.
//! The loop keeps the full text of the last few clipped outputs here, in
//! memory and for one run only, and the `read_output` tool pages or
//! searches it. Nothing is written into the workspace.

use serde_json::json;
use std::collections::VecDeque;

/// Clipped outputs kept per run; the oldest is dropped first.
pub(crate) const MAX_SAVED_OUTPUTS: usize = 8;
/// Largest single output kept (bytes). Command output is already capped
/// at 512 KiB per stream by rex-tools, so this only guards the sum.
pub(crate) const MAX_SAVED_BYTES: usize = 1_100_000;
/// Lines per page.
pub(crate) const PAGE_LINES: usize = 200;
/// Matching lines returned for a query.
pub(crate) const MAX_MATCHES: usize = 100;
/// Characters kept of one very long line.
const LINE_CHARS: usize = 400;
/// Characters in one page or match list, whatever the line count.
const REPLY_CHARS: usize = 16_000;

#[derive(Debug, Clone, Default)]
pub(crate) struct OutputStore {
    items: VecDeque<(String, String)>,
}

impl OutputStore {
    /// Keep `text` under `call_id`, replacing an older copy and dropping
    /// the oldest entry past the cap. Returns the line count.
    pub(crate) fn save(&mut self, call_id: &str, text: &str) -> usize {
        self.items.retain(|(id, _)| id != call_id);
        let mut end = text.len().min(MAX_SAVED_BYTES);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let kept = text[..end].to_string();
        let lines = kept.lines().count();
        self.items.push_back((call_id.to_string(), kept));
        while self.items.len() > MAX_SAVED_OUTPUTS {
            self.items.pop_front();
        }
        lines
    }

    fn get(&self, call_id: &str) -> Option<&str> {
        self.items
            .iter()
            .find(|(id, _)| id == call_id)
            .map(|(_, t)| t.as_str())
    }

    /// The model-facing reply: (ok, JSON text).
    pub(crate) fn render(
        &self,
        call_id: &str,
        offset: usize,
        query: Option<&str>,
    ) -> (bool, String) {
        let Some(text) = self.get(call_id) else {
            return (
                false,
                format!(
                    "no saved output for call_id {call_id}; only the last {MAX_SAVED_OUTPUTS} clipped run_command outputs of this run are kept"
                ),
            );
        };
        let lines: Vec<&str> = text.lines().collect();
        let total = lines.len();
        let mut body = String::new();
        let mut push = |n: usize, line: &str| -> bool {
            let clipped: String = line.chars().take(LINE_CHARS).collect();
            let more = if clipped.len() < line.len() {
                " …"
            } else {
                ""
            };
            let row = format!("{n}: {clipped}{more}\n");
            if body.len() + row.len() > REPLY_CHARS {
                return false;
            }
            body.push_str(&row);
            true
        };
        match query.map(str::trim).filter(|q| !q.is_empty()) {
            Some(q) => {
                let needle = q.to_lowercase();
                let hits: Vec<usize> = (0..total)
                    .filter(|&i| lines[i].to_lowercase().contains(&needle))
                    .collect();
                let mut shown = 0;
                for &i in hits.iter().take(MAX_MATCHES) {
                    if !push(i + 1, lines[i]) {
                        break;
                    }
                    shown += 1;
                }
                (
                    true,
                    json!({
                        "call_id": call_id, "query": q, "total_lines": total,
                        "matches": hits.len(), "shown": shown, "lines": body,
                    })
                    .to_string(),
                )
            }
            None => {
                let from = offset.max(1);
                if from > total.max(1) {
                    return (
                        false,
                        format!("offset {from} is past the end ({total} lines)"),
                    );
                }
                let mut to = from - 1;
                for (i, line) in lines.iter().enumerate().skip(from - 1).take(PAGE_LINES) {
                    if !push(i + 1, line) {
                        break;
                    }
                    to = i + 1;
                }
                let next = (to < total).then_some(to + 1);
                (
                    true,
                    json!({
                        "call_id": call_id, "total_lines": total, "from": from, "to": to,
                        "next_offset": next, "lines": body,
                    })
                    .to_string(),
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn log(n: usize) -> String {
        (1..=n)
            .map(|i| {
                if i == 250 {
                    "test parse::nested FAILED".to_string()
                } else {
                    format!("line {i} ok")
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn pages_and_searches_the_full_text() {
        let mut s = OutputStore::default();
        assert_eq!(s.save("c1", &log(450)), 450);
        let (ok, page) = s.render("c1", 0, None);
        assert!(ok);
        let v: Value = serde_json::from_str(&page).unwrap();
        assert_eq!(v["from"], 1);
        assert_eq!(v["to"], PAGE_LINES);
        assert_eq!(v["next_offset"], PAGE_LINES + 1);
        assert_eq!(v["total_lines"], 450);
        assert!(v["lines"].as_str().unwrap().starts_with("1: line 1 ok\n"));
        let (_, page) = s.render("c1", 401, None);
        let v: Value = serde_json::from_str(&page).unwrap();
        assert_eq!(v["to"], 450);
        assert!(v["next_offset"].is_null());
        assert!(v["lines"].as_str().unwrap().ends_with("450: line 450 ok\n"));
        let (ok, _) = s.render("c1", 451, None);
        assert!(!ok);
        // search is case-insensitive and reports line numbers
        let (_, hits) = s.render("c1", 0, Some("failed"));
        let v: Value = serde_json::from_str(&hits).unwrap();
        assert_eq!(v["matches"], 1);
        assert_eq!(v["lines"], "250: test parse::nested FAILED\n");
        let (_, hits) = s.render("c1", 0, Some("ok"));
        let v: Value = serde_json::from_str(&hits).unwrap();
        assert_eq!(v["matches"], 449);
        assert_eq!(v["shown"], MAX_MATCHES);
        // blank query pages instead
        let (_, page) = s.render("c1", 0, Some("  "));
        assert!(page.contains("\"from\":1"));
    }

    #[test]
    fn keeps_only_the_newest_outputs_and_bounds_replies() {
        let mut s = OutputStore::default();
        for i in 0..=MAX_SAVED_OUTPUTS {
            s.save(&format!("c{i}"), "x");
        }
        let (ok, msg) = s.render("c0", 0, None);
        assert!(!ok);
        assert!(msg.contains("only the last"));
        assert!(s.render(&format!("c{MAX_SAVED_OUTPUTS}"), 0, None).0);
        // saving the same id again replaces it, it doesn't take a slot
        s.save("c1", "new");
        s.save("c1", "newer");
        assert_eq!(s.items.len(), MAX_SAVED_OUTPUTS);
        assert!(s.render("c1", 0, None).1.contains("1: newer"));
        // one huge line is clipped, and a page never passes the char cap
        let long = "y".repeat(10_000);
        let text = vec![long.as_str(); 100].join("\n");
        s.save("big", &text);
        let (_, page) = s.render("big", 0, None);
        let v: Value = serde_json::from_str(&page).unwrap();
        let lines = v["lines"].as_str().unwrap();
        assert!(lines.len() <= REPLY_CHARS);
        assert!(lines.starts_with(&format!("1: {} …\n", "y".repeat(LINE_CHARS))));
        assert!(v["next_offset"].as_u64().unwrap() > 1);
        // the stored copy is capped on a char boundary
        let wide = "é".repeat(MAX_SAVED_BYTES);
        s.save("wide", &wide);
        let kept = s.get("wide").unwrap();
        assert!(kept.len() <= MAX_SAVED_BYTES && kept.len() >= MAX_SAVED_BYTES - 1);
    }
}
