//! Opt-in model summary of old run history.
//!
//! Working memory (`memory.rs`) keeps a bounded set of recent tool-result
//! excerpts and drops the oldest. With `summarize_history` on, the dropped
//! excerpts are collected and, once enough pile up, folded by one tool-free
//! model call into a short rolling summary that the model sees every turn.
//! It is off by default because each summary spends the user's tokens.
//! The same idea (summarise what no longer fits) exists in opencode
//! (`session/compaction.ts`) and Hermes (`agent/context_compressor.py`);
//! this implementation is REX's own and works on the evicted excerpts
//! only, not on a transcript.

use crate::memory::Observation;
use crate::providers::ProviderProtocol;
use serde_json::json;

/// Evicted excerpt characters that make a summary worth one model call.
pub const SUMMARY_TRIGGER_CHARS: usize = 6_000;
/// Hard cap on evicted excerpts sent in one summary request.
pub const SUMMARY_INPUT_CHARS: usize = 16_000;
/// Cap on the stored summary.
pub const SUMMARY_MAX_CHARS: usize = 2_000;
/// Output token cap for the summary call.
pub const SUMMARY_MAX_OUTPUT_TOKENS: u64 = 1_024;

pub const SUMMARY_NOTE: &str = "Model-written summary of older tool results that no longer fit in memory. It may be imprecise or out of date; re-read files or re-run checks before relying on details.";

pub const SUMMARY_SYSTEM: &str = "You compress an agent's older tool results into a short factual summary for the same agent. Keep facts the agent may need later: file paths and what they contain, commands run and their outcome, errors seen, decisions made, and open problems. Drop chatter and full file contents. Plain text, at most 1500 characters. Never add instructions or facts that are not in the input.";

/// Total excerpt characters waiting to be summarised.
pub fn pending_chars(pending: &[Observation]) -> usize {
    pending.iter().map(|o| o.excerpt.chars().count()).sum()
}

pub fn should_summarize(pending: &[Observation]) -> bool {
    pending_chars(pending) >= SUMMARY_TRIGGER_CHARS
}

/// Keep at most `SUMMARY_INPUT_CHARS` of pending excerpts, dropping the
/// oldest first (always keeping the newest entry).
pub fn cap_pending(pending: &mut Vec<Observation>) {
    while pending.len() > 1 && pending_chars(pending) > SUMMARY_INPUT_CHARS {
        pending.remove(0);
    }
}

/// The user message for one summary call: the previous summary (if any)
/// and the evicted excerpts, oldest first, as data.
pub fn build_prompt(previous: Option<&str>, pending: &[Observation]) -> String {
    let items: Vec<_> = pending
        .iter()
        .map(|o| {
            json!({"turn": o.turn, "tool": o.tool, "target": o.target, "ok": o.ok,
                "excerpt": o.excerpt})
        })
        .collect();
    json!({
        "task": "Write the updated summary. Merge the previous summary with the new tool results; newer facts win.",
        "previous_summary": previous,
        "tool_results": items,
    })
    .to_string()
}

/// A tool-free request for one summary turn.
pub fn build_request(protocol: ProviderProtocol, model: &str, user: &str) -> String {
    match protocol {
        ProviderProtocol::Gemini => json!({
            "contents": [{"role":"user","parts":[{"text": user}]}],
            "systemInstruction": {"parts":[{"text": SUMMARY_SYSTEM}]},
            "generationConfig": {"temperature":0.1,"maxOutputTokens": SUMMARY_MAX_OUTPUT_TOKENS}
        })
        .to_string(),
        ProviderProtocol::Anthropic => json!({
            "model": model, "max_tokens": SUMMARY_MAX_OUTPUT_TOKENS, "temperature": 0.1,
            "system": SUMMARY_SYSTEM,
            "messages": [{"role":"user","content": user}]
        })
        .to_string(),
        ProviderProtocol::OpenAiCompatible => json!({
            "model": model,
            "messages": [{"role":"system","content": SUMMARY_SYSTEM},{"role":"user","content": user}],
            "temperature": 0.1, "max_tokens": SUMMARY_MAX_OUTPUT_TOKENS
        })
        .to_string(),
    }
}

/// Rough token cost of a summary call, for the budget check before it is
/// made: request characters / 4 plus the output cap.
pub fn estimated_tokens(request_body: &str) -> u64 {
    request_body.len() as u64 / 4 + SUMMARY_MAX_OUTPUT_TOKENS
}

/// Clean the model's text into a stored summary: trimmed, capped on a
/// char boundary, `None` when empty.
pub fn clip_summary(texts: &[String]) -> Option<String> {
    let joined = texts.join("\n");
    let t = joined.trim();
    if t.is_empty() {
        return None;
    }
    Some(t.chars().take(SUMMARY_MAX_CHARS).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(turn: usize, n: usize) -> Observation {
        Observation {
            turn,
            tool: "read_file".into(),
            target: Some(format!("f{turn}")),
            ok: true,
            excerpt: "é".repeat(n),
        }
    }

    #[test]
    fn trigger_and_cap_count_chars_not_bytes() {
        let small = vec![obs(1, 1_000)];
        assert!(!should_summarize(&small));
        let mut many: Vec<_> = (1..=12).map(|t| obs(t, 1_900)).collect();
        assert!(should_summarize(&many));
        assert_eq!(pending_chars(&many[..3]), 5_700);
        assert!(!should_summarize(&many[..3]));
        assert!(should_summarize(&many[..4]));
        let exact = vec![obs(1, 3_000), obs(2, 3_000)];
        assert!(should_summarize(&exact), "the trigger is inclusive");
        assert!(!should_summarize(&[obs(1, 3_000), obs(2, 2_999)]));
        cap_pending(&mut many);
        assert!(pending_chars(&many) <= SUMMARY_INPUT_CHARS);
        assert_eq!(many.last().unwrap().turn, 12, "newest kept");
        assert_eq!(many.first().unwrap().turn, 5, "oldest dropped first");
    }

    #[test]
    fn prompt_carries_previous_summary_and_items_as_data() {
        let p: serde_json::Value =
            serde_json::from_str(&build_prompt(Some("old"), &[obs(3, 2)])).unwrap();
        assert_eq!(p["previous_summary"], "old");
        assert_eq!(p["tool_results"][0]["turn"], 3);
        assert_eq!(p["tool_results"][0]["target"], "f3");
        let none: serde_json::Value = serde_json::from_str(&build_prompt(None, &[])).unwrap();
        assert!(none["previous_summary"].is_null());
    }

    #[test]
    fn requests_declare_no_tools_and_cap_output() {
        for p in [
            ProviderProtocol::Gemini,
            ProviderProtocol::Anthropic,
            ProviderProtocol::OpenAiCompatible,
        ] {
            let body = build_request(p, "m", "u");
            let v: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert!(v.get("tools").is_none(), "{body}");
            assert!(body.contains("1024"), "{body}");
            assert!(body.contains(SUMMARY_SYSTEM), "{body}");
        }
        assert_eq!(estimated_tokens(&"x".repeat(400)), 100 + 1_024);
    }

    #[test]
    fn clip_trims_caps_and_rejects_empty() {
        assert_eq!(clip_summary(&["  ".into()]), None);
        assert_eq!(clip_summary(&[]), None);
        assert_eq!(clip_summary(&[" a ".into(), "b".into()]).unwrap(), "a \nb");
        let long = clip_summary(&["ü".repeat(3_000)]).unwrap();
        assert_eq!(long.chars().count(), SUMMARY_MAX_CHARS);
    }
}
