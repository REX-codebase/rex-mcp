//! Single-shot text completion across every documented provider protocol.
//!
//! Ultra roles (contract drafting, adversary probe, clean-room judge) need a
//! plain prompt -> text call with no tools and no loop. This module is the
//! only place that call is built: one code path per documented wire protocol,
//! credentials read inside the backend, retries bounded, and unknown
//! protocols refused instead of guessed.

use crate::conversation::decode_turn;
use crate::http::Transport;
use crate::provider_failure::structured_http_failure;
use crate::providers::{find_spec, ProviderProtocol};
use crate::service::ProviderService;
use crate::secrets::SecretStore;
use serde_json::json;

const RETRIES: u32 = 3;
const MAX_OUTPUT_TOKENS: u64 = 8192;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OneShot {
    pub text: String,
    pub model: String,
    /// Total tokens the provider reported for the call (0 when unreported).
    /// Bench accounting uses this; it is the provider's own number.
    pub tokens: u64,
}

/// Resolve the effective model: the requested one, or the provider catalog's
/// first callable model (Flash-Lite preferred on Gemini, matching the agent
/// loop). Returns the resolved model id so callers can record exactly which
/// model produced the text.
pub fn resolve_model<S: SecretStore, T: Transport>(
    service: &ProviderService<S, T>,
    provider: &str,
    requested: Option<&str>,
) -> Result<String, String> {
    if let Some(m) = requested.map(str::trim).filter(|m| !m.is_empty()) {
        return Ok(m.to_string());
    }
    let catalog = service
        .refresh(provider)
        .map_err(|e| format!("catalog failed: {e}"))?;
    let ids: Vec<&str> = catalog.models.iter().map(|m| m.id.as_str()).collect();
    let picked = if provider == "gemini" {
        crate::live::pick_flash_lite(&ids).or_else(|| ids.first().map(|s| (*s).to_string()))
    } else {
        ids.first().map(|s| (*s).to_string())
    };
    picked
        .filter(|m| !m.is_empty())
        .ok_or_else(|| "provider returned no callable models".to_string())
}

fn usage_tokens(protocol: ProviderProtocol, body: &str) -> u64 {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return 0;
    };
    match protocol {
        ProviderProtocol::Gemini => v["usageMetadata"]["totalTokenCount"].as_u64().unwrap_or(0),
        ProviderProtocol::Anthropic => {
            let u = &v["usage"];
            u["input_tokens"].as_u64().unwrap_or(0) + u["output_tokens"].as_u64().unwrap_or(0)
        }
        ProviderProtocol::OpenAiCompatible => v["usage"]["total_tokens"].as_u64().unwrap_or(0),
    }
}

/// One bounded text completion. No tools are offered; tool calls in the
/// response are ignored and the text is what returns.
pub fn complete_text<S: SecretStore, T: Transport>(
    service: &ProviderService<S, T>,
    provider: &str,
    model: &str,
    system: &str,
    prompt: &str,
) -> Result<OneShot, String> {
    let spec = find_spec(provider).ok_or_else(|| format!("unknown provider {provider}"))?;
    // Every documented protocol has a text adapter; undocumented providers
    // are already refused by `find_spec` above.
    let protocol = spec.protocol;
    let base_url = service
        .base_url(provider)
        .map_err(|e| format!("no base URL: {e}"))?;
    let key = service
        .get_key(provider)
        .map_err(|e| format!("key lookup failed: {e}"))?
        .ok_or_else(|| "no API key stored for this provider".to_string())?;

    let body = match protocol {
        ProviderProtocol::Gemini => json!({
            "contents": [{"role":"user","parts":[{"text": prompt}]}],
            "systemInstruction": {"parts":[{"text": system}]},
            "generationConfig": {"temperature":0.2,"maxOutputTokens":MAX_OUTPUT_TOKENS}
        })
        .to_string(),
        ProviderProtocol::Anthropic => json!({
            "model": model, "max_tokens": MAX_OUTPUT_TOKENS, "temperature": 0.2,
            "system": system,
            "messages": [{"role":"user","content": prompt}]
        })
        .to_string(),
        ProviderProtocol::OpenAiCompatible => json!({
            "model": model, "temperature": 0.2, "max_tokens": MAX_OUTPUT_TOKENS,
            "messages": [
                {"role":"system","content": system},
                {"role":"user","content": prompt}
            ]
        })
        .to_string(),
    };
    let (url, headers) = match protocol {
        ProviderProtocol::Gemini => (
            format!(
                "{}/v1beta/models/{model}:generateContent",
                base_url.trim_end_matches('/')
            ),
            vec![
                ("x-goog-api-key".to_string(), key.clone()),
                ("content-type".to_string(), "application/json".to_string()),
            ],
        ),
        ProviderProtocol::Anthropic => (
            format!("{}/v1/messages", base_url.trim_end_matches('/')),
            vec![
                ("x-api-key".to_string(), key.clone()),
                ("anthropic-version".to_string(), "2023-06-01".to_string()),
                ("content-type".to_string(), "application/json".to_string()),
            ],
        ),
        ProviderProtocol::OpenAiCompatible => (
            format!("{}/chat/completions", base_url.trim_end_matches('/')),
            vec![
                ("authorization".to_string(), format!("Bearer {key}")),
                ("content-type".to_string(), "application/json".to_string()),
            ],
        ),
    };

    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let result = service.transport().post(&url, &headers, &body);
        let retryable = matches!(&result, Ok((429, _)) | Ok((500..=599, _)) | Err(_));
        match result {
            Ok((status, text)) if status / 100 == 2 => {
                let turn = decode_turn(protocol, &text)?;
                let joined = turn.text.join("\n");
                if joined.trim().is_empty() {
                    return Err("provider returned no text".into());
                }
                let tokens = usage_tokens(protocol, &text);
                return Ok(OneShot {
                    text: joined,
                    model: model.to_string(),
                    tokens,
                });
            }
            Ok((status, text)) => {
                if !retryable || attempt >= RETRIES {
                    let detail = structured_http_failure(status, &text);
                    return Err(format!("provider HTTP {status}: {detail}"));
                }
            }
            Err(e) => {
                if attempt >= RETRIES {
                    return Err(format!("provider transport failed: {e}"));
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(400 * attempt as u64));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ProviderError;
    use crate::secrets::MemorySecretStore;
    use std::sync::Mutex;

    struct Script {
        bodies: Mutex<Vec<String>>,
        response: String,
        status: u16,
    }

    impl Transport for Script {
        fn get(
            &self,
            _url: &str,
            _headers: &[(String, String)],
        ) -> Result<(u16, String), ProviderError> {
            Ok((200, r#"{"models":[{"name":"models/gemini-3.5-flash-lite","displayName":"Flash Lite","supportedGenerationMethods":["generateContent"]}]}"#.into()))
        }
        fn post(
            &self,
            _url: &str,
            _headers: &[(String, String)],
            body: &str,
        ) -> Result<(u16, String), ProviderError> {
            self.bodies.lock().unwrap().push(body.to_string());
            Ok((self.status, self.response.clone()))
        }
    }

    fn service_with(t: Script) -> ProviderService<MemorySecretStore, Script> {
        let store = MemorySecretStore::default();
        store.set_key("gemini", "test-key").unwrap();
        ProviderService::new(store, t)
    }

    #[test]
    fn gemini_text_roundtrip() {
        let script = Script {
            bodies: Mutex::new(Vec::new()),
            response: json!({"candidates":[{"content":{"parts":[{"text":"contract draft"}]},"finishReason":"STOP"}]}).to_string(),
            status: 200,
        };
        let svc = service_with(script);
        let out = complete_text(&svc, "gemini", "gemini-3.5-flash-lite", "sys", "prompt")
            .expect("completion");
        assert_eq!(out.text, "contract draft");
        assert_eq!(out.model, "gemini-3.5-flash-lite");
    }

    #[test]
    fn empty_text_is_an_error_not_a_silent_pass() {
        let script = Script {
            bodies: Mutex::new(Vec::new()),
            response: json!({"candidates":[{"content":{"parts":[{"text":"  "}]},"finishReason":"STOP"}]}).to_string(),
            status: 200,
        };
        let svc = service_with(script);
        assert!(complete_text(&svc, "gemini", "m", "s", "p").is_err());
    }

    #[test]
    fn permanent_failure_does_not_retry_forever() {
        let script = Script {
            bodies: Mutex::new(Vec::new()),
            response: r#"{"error":{"message":"bad key"}}"#.into(),
            status: 400,
        };
        let svc = service_with(script);
        let err = complete_text(&svc, "gemini", "m", "s", "p").unwrap_err();
        assert!(err.contains("400"), "{err}");
    }
}
