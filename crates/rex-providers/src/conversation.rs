//! Provider wire adapters for agent conversations.
//!
//! Provider JSON stops here. The rest of REX sees a small normalized turn and
//! the `rex-tools` request schema. Unknown content is rejected rather than
//! guessed into a local capability.
use crate::providers::ProviderProtocol;
use rex_tools::ToolRequest;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NormalizedToolCall {
    pub provider_call_id: String,
    pub request: ToolRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NormalizedTurn {
    pub text: Vec<String>,
    pub tool_calls: Vec<NormalizedToolCall>,
    pub finished: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolOutcome {
    pub provider_call_id: String,
    pub tool_name: String,
    pub ok: bool,
    pub content: String,
}

pub fn decode_turn(protocol: ProviderProtocol, body: &str) -> Result<NormalizedTurn, String> {
    let value: Value =
        serde_json::from_str(body).map_err(|e| format!("invalid provider JSON: {e}"))?;
    match protocol {
        ProviderProtocol::Gemini => decode_gemini(&value),
        ProviderProtocol::Anthropic => decode_anthropic(&value),
        ProviderProtocol::OpenAiCompatible => decode_openai(&value),
    }
}

pub fn encode_tool_outcomes(protocol: ProviderProtocol, outcomes: &[ToolOutcome]) -> Value {
    match protocol {
        ProviderProtocol::OpenAiCompatible => Value::Array(
            outcomes
                .iter()
                .map(|o| {
                    json!({
                        "role": "tool", "tool_call_id": o.provider_call_id,
                        "content": o.content
                    })
                })
                .collect(),
        ),
        ProviderProtocol::Anthropic => {
            json!({"role":"user","content": outcomes.iter().map(|o| json!({
            "type":"tool_result", "tool_use_id":o.provider_call_id,
            "is_error":!o.ok, "content":o.content
        })).collect::<Vec<_>>() })
        }
        ProviderProtocol::Gemini => json!({"role":"user","parts": outcomes.iter().map(|o| json!({
            "functionResponse":{"name":o.tool_name,"response":{"ok":o.ok,"content":o.content}}
        })).collect::<Vec<_>>() }),
    }
}

fn tool(value: &Value, id: String, name: &str) -> Result<NormalizedToolCall, String> {
    let mut object = value.clone();
    if !object.is_object() {
        return Err(format!("tool {name} arguments must be an object"));
    }
    object
        .as_object_mut()
        .unwrap()
        .insert("tool".into(), Value::String(name.into()));
    let request =
        serde_json::from_value(object).map_err(|e| format!("invalid {name} request: {e}"))?;
    Ok(NormalizedToolCall {
        provider_call_id: id,
        request,
    })
}

fn decode_openai(v: &Value) -> Result<NormalizedTurn, String> {
    let choice = v
        .get("choices")
        .and_then(|x| x.as_array())
        .and_then(|x| x.first())
        .ok_or("missing choices[0]")?;
    let msg = choice.get("message").ok_or("missing message")?;
    let text = msg
        .get("content")
        .and_then(Value::as_str)
        .map(|s| vec![s.into()])
        .unwrap_or_default();
    let mut calls = vec![];
    for c in msg
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let id = c
            .get("id")
            .and_then(Value::as_str)
            .ok_or("tool call missing id")?
            .to_string();
        let f = c.get("function").ok_or("tool call missing function")?;
        let name = f
            .get("name")
            .and_then(Value::as_str)
            .ok_or("tool call missing name")?;
        let raw = f
            .get("arguments")
            .and_then(Value::as_str)
            .ok_or("tool call missing arguments")?;
        // Blank arguments mean "no arguments", as Hermes reads them
        // (`agent/turn_tool_validation.py`); the agent-mode decoder does
        // the same (Round 46).
        let args = if raw.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(raw).map_err(|e| format!("invalid tool arguments: {e}"))?
        };
        calls.push(tool(&args, id, name)?);
    }
    Ok(NormalizedTurn {
        text,
        tool_calls: calls,
        finished: choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .is_some_and(|r| r != "tool_calls"),
    })
}

fn decode_anthropic(v: &Value) -> Result<NormalizedTurn, String> {
    let mut text = vec![];
    let mut calls = vec![];
    for b in v
        .get("content")
        .and_then(Value::as_array)
        .ok_or("missing content")?
    {
        match b.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(s) = b.get("text").and_then(Value::as_str) {
                    text.push(s.into())
                }
            }
            Some("tool_use") => calls.push(tool(
                b.get("input").ok_or("tool_use missing input")?,
                b.get("id")
                    .and_then(Value::as_str)
                    .ok_or("tool_use missing id")?
                    .into(),
                b.get("name")
                    .and_then(Value::as_str)
                    .ok_or("tool_use missing name")?,
            )?),
            _ => {}
        }
    }
    Ok(NormalizedTurn {
        text,
        tool_calls: calls,
        finished: v
            .get("stop_reason")
            .and_then(Value::as_str)
            .is_some_and(|r| r != "tool_use"),
    })
}

fn decode_gemini(v: &Value) -> Result<NormalizedTurn, String> {
    let candidate = v
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|x| x.first())
        .ok_or("missing candidates[0]")?;
    let parts = candidate
        .pointer("/content/parts")
        .and_then(Value::as_array)
        .ok_or("missing content.parts")?;
    let mut text = vec![];
    let mut calls = vec![];
    for (i, p) in parts.iter().enumerate() {
        if let Some(s) = p.get("text").and_then(Value::as_str) {
            text.push(s.into())
        }
        if let Some(f) = p.get("functionCall") {
            let name = f
                .get("name")
                .and_then(Value::as_str)
                .ok_or("functionCall missing name")?;
            calls.push(tool(
                f.get("args").unwrap_or(&json!({})),
                format!("gemini-{i}-{name}"),
                name,
            )?);
        }
    }
    Ok(NormalizedTurn {
        text,
        tool_calls: calls,
        finished: candidate
            .get("finishReason")
            .and_then(Value::as_str)
            .is_some_and(|r| r == "STOP"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn openai_tool_call() {
        let b = r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"c1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"src/lib.rs\"}"}}]},"finish_reason":"tool_calls"}]}"#;
        let t = decode_turn(ProviderProtocol::OpenAiCompatible, b).unwrap();
        assert_eq!(t.tool_calls.len(), 1);
        assert!(!t.finished)
    }
    #[test]
    fn anthropic_tool_call() {
        let b = r#"{"content":[{"type":"text","text":"checking"},{"type":"tool_use","id":"t1","name":"run_command","input":{"argv":["cargo","test"]}}],"stop_reason":"tool_use"}"#;
        let t = decode_turn(ProviderProtocol::Anthropic, b).unwrap();
        assert_eq!(t.text, vec!["checking"]);
        assert_eq!(t.tool_calls.len(), 1)
    }
    #[test]
    fn gemini_tool_call() {
        let b = r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"search_files","args":{"query":"TODO"}}}]},"finishReason":"STOP"}]}"#;
        let t = decode_turn(ProviderProtocol::Gemini, b).unwrap();
        assert_eq!(t.tool_calls.len(), 1)
    }
    #[test]
    fn openai_blank_arguments_read_as_empty_object() {
        let turn = |args: &str| {
            let b = json!({"choices":[{"message":{"tool_calls":[{"id":"c1","function":{"name":"read_file","arguments": args}}]},"finish_reason":"tool_calls"}]}).to_string();
            decode_turn(ProviderProtocol::OpenAiCompatible, &b)
        };
        // read_file needs a path, so {} reaches request validation, not JSON parsing
        for blank in ["", "  \n"] {
            let e = turn(blank).unwrap_err();
            assert!(e.starts_with("invalid read_file request"), "{e}");
        }
        let e = turn("{\"path\":").unwrap_err();
        assert!(e.starts_with("invalid tool arguments"), "{e}");
        assert_eq!(turn("{\"path\":\"a\"}").unwrap().tool_calls.len(), 1);
    }
    #[test]
    fn model_cannot_smuggle_approval() {
        let b = r#"{"choices":[{"message":{"tool_calls":[{"id":"c1","function":{"name":"create_file","arguments":"{\"path\":\"x\",\"content\":\"a\",\"overwrite\":false,\"approved\":true}"}}]},"finish_reason":"tool_calls"}]}"#;
        assert!(decode_turn(ProviderProtocol::OpenAiCompatible, b).is_err())
    }
}

#[cfg(test)]
mod adversarial_wire_tests {
    use super::*;

    #[test]
    fn provider_result_envelopes_preserve_native_identifiers() {
        let outcome = ToolOutcome {
            provider_call_id: "call-42".into(),
            tool_name: "read_file".into(),
            ok: true,
            content: "ok".into(),
        };
        let openai = encode_tool_outcomes(
            ProviderProtocol::OpenAiCompatible,
            std::slice::from_ref(&outcome),
        );
        assert_eq!(openai[0]["tool_call_id"], "call-42");
        let anthropic =
            encode_tool_outcomes(ProviderProtocol::Anthropic, std::slice::from_ref(&outcome));
        assert_eq!(anthropic["content"][0]["tool_use_id"], "call-42");
        let gemini = encode_tool_outcomes(ProviderProtocol::Gemini, &[outcome]);
        assert_eq!(gemini["parts"][0]["functionResponse"]["name"], "read_file");
    }

    #[test]
    fn gemini_stop_is_finished_and_max_tokens_is_not() {
        let stop =
            r#"{"candidates":[{"content":{"parts":[{"text":"done"}]},"finishReason":"STOP"}]}"#;
        let max = r#"{"candidates":[{"content":{"parts":[{"text":"partial"}]},"finishReason":"MAX_TOKENS"}]}"#;
        assert!(
            decode_turn(ProviderProtocol::Gemini, stop)
                .unwrap()
                .finished
        );
        assert!(!decode_turn(ProviderProtocol::Gemini, max).unwrap().finished);
    }

    #[test]
    fn unknown_control_fields_are_rejected_for_every_wire_format() {
        let openai = r#"{"choices":[{"message":{"tool_calls":[{"id":"c","function":{"name":"read_file","arguments":"{\"path\":\"x\",\"approved\":true}"}}]},"finish_reason":"tool_calls"}]}"#;
        let anthropic = r#"{"content":[{"type":"tool_use","id":"c","name":"read_file","input":{"path":"x","approved":true}}],"stop_reason":"tool_use"}"#;
        let gemini = r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"read_file","args":{"path":"x","approved":true}}}]},"finishReason":"STOP"}]}"#;
        assert!(decode_turn(ProviderProtocol::OpenAiCompatible, openai).is_err());
        assert!(decode_turn(ProviderProtocol::Anthropic, anthropic).is_err());
        assert!(decode_turn(ProviderProtocol::Gemini, gemini).is_err());
    }
}
