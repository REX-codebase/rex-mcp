//! Redacted, machine-readable provider HTTP failures.
//!
//! Provider bodies can include prompt fragments or other request context.  We
//! never copy the free-form message into durable run state.  For Google 429s
//! we retain only the fields needed to diagnose quota and schedule a retry.

use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Serialize)]
struct HttpFailure {
    http_status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider_code: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider_status: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    quota_violations: Vec<QuotaViolation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_delay: Option<String>,
    redacted: bool,
}

#[derive(Debug, Serialize)]
struct QuotaViolation {
    #[serde(skip_serializing_if = "Option::is_none")]
    metric: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    scope: BTreeMap<String, String>,
}

/// Produce a stable JSON detail without preserving the provider's free-form
/// message or any unknown fields.
pub(crate) fn structured_http_failure(status: u16, body: &str) -> String {
    let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let error = value.get("error").unwrap_or(&value);
    let mut out = HttpFailure {
        http_status: status,
        provider_code: error.get("code").and_then(Value::as_u64),
        provider_status: error
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_owned),
        quota_violations: Vec::new(),
        retry_delay: None,
        redacted: true,
    };

    if let Some(details) = error.get("details").and_then(Value::as_array) {
        for detail in details {
            let kind = detail.get("@type").and_then(Value::as_str).unwrap_or("");
            if kind.ends_with("google.rpc.QuotaFailure") {
                for violation in detail
                    .get("violations")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let scope = violation
                        .get("quotaDimensions")
                        .and_then(Value::as_object)
                        .map(|m| {
                            m.iter()
                                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                                .collect()
                        })
                        .unwrap_or_default();
                    out.quota_violations.push(QuotaViolation {
                        metric: violation
                            .get("quotaMetric")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        limit: violation
                            .get("quotaId")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        scope,
                    });
                }
            } else if kind.ends_with("google.rpc.RetryInfo") {
                out.retry_delay = detail
                    .get("retryDelay")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
        }
    }

    serde_json::to_string(&out).expect("HTTP failure record is serializable")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_429_is_structured_and_free_form_text_is_redacted() {
        let body = r#"{
          "error": {
            "code": 429,
            "status": "RESOURCE_EXHAUSTED",
            "message": "prompt fragment and key must not survive",
            "details": [
              {"@type":"type.googleapis.com/google.rpc.QuotaFailure","violations":[{
                "quotaMetric":"generativelanguage.googleapis.com/generate_content_free_tier_requests",
                "quotaId":"GenerateRequestsPerDayPerProjectPerModel-FreeTier",
                "quotaDimensions":{"location":"global","model":"gemini-3.5-flash-lite"}
              }]},
              {"@type":"type.googleapis.com/google.rpc.RetryInfo","retryDelay":"37s"}
            ]
          }
        }"#;
        let detail = structured_http_failure(429, body);
        let v: Value = serde_json::from_str(&detail).unwrap();
        assert_eq!(v["http_status"], 429);
        assert_eq!(v["provider_status"], "RESOURCE_EXHAUSTED");
        assert_eq!(
            v["quota_violations"][0]["limit"],
            "GenerateRequestsPerDayPerProjectPerModel-FreeTier"
        );
        assert_eq!(
            v["quota_violations"][0]["scope"]["model"],
            "gemini-3.5-flash-lite"
        );
        assert_eq!(v["retry_delay"], "37s");
        assert_eq!(v["redacted"], true);
        assert!(!detail.contains("prompt fragment"));
        assert!(!detail.contains("must not survive"));
    }

    #[test]
    fn malformed_body_stays_structured_and_redacted() {
        let detail = structured_http_failure(503, "not json and possibly sensitive");
        assert_eq!(detail, r#"{"http_status":503,"redacted":true}"#);
    }
}
