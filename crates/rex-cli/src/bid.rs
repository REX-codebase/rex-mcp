//! Binding cost bids (leapfrog bet 7 — the cost half).
//!
//! `rex exec --bid` prints the binding bid for a run *before* anything
//! executes: the budgets the run will not exceed, plus a worst-case dollar
//! ceiling when the model has a known price. The run only starts with
//! `--accept-bid`. The receipt carries the accepted bid and the actuals, so
//! anyone can check actuals <= bid after the fact.
//!
//! The binding is mechanical, not a promise: the agent loop already
//! enforces max tokens / steps / tool calls / wall time, and the dollar
//! ceiling is derived worst-case from the token budget, so it can only
//! over-estimate. Prices are static estimates (USD per 1M tokens) ported
//! from the desktop app's table (`src-tauri/src/cost.rs`) — verify with
//! your provider before treating a bid as a quote.

use serde_json::Value;

pub struct ModelPrice {
    pub model_id: &'static str,
    pub provider: &'static str,
    pub input_per_1m: f64,
    pub output_per_1m: f64,
    pub as_of: &'static str,
}

pub const PRICES: &[ModelPrice] = &[
    ModelPrice {
        model_id: "gpt-5",
        provider: "openai",
        input_per_1m: 1.25,
        output_per_1m: 10.0,
        as_of: "2026-01",
    },
    ModelPrice {
        model_id: "gpt-5-mini",
        provider: "openai",
        input_per_1m: 0.25,
        output_per_1m: 2.0,
        as_of: "2026-01",
    },
    ModelPrice {
        model_id: "claude-opus-4-6",
        provider: "anthropic",
        input_per_1m: 15.0,
        output_per_1m: 75.0,
        as_of: "2026-01",
    },
    ModelPrice {
        model_id: "claude-sonnet-4-6",
        provider: "anthropic",
        input_per_1m: 3.0,
        output_per_1m: 15.0,
        as_of: "2026-01",
    },
    ModelPrice {
        model_id: "gemini-2.5-pro",
        provider: "google",
        input_per_1m: 1.25,
        output_per_1m: 10.0,
        as_of: "2026-01",
    },
    ModelPrice {
        model_id: "gemini-2.5-flash",
        provider: "google",
        input_per_1m: 0.30,
        output_per_1m: 2.50,
        as_of: "2026-01",
    },
];

fn provider_alias(provider: &str) -> &str {
    match provider {
        "gemini" => "google",
        _ => provider,
    }
}

/// Look up a price by model id. The provider must match when the table
/// knows it; `gemini` is aliased to the table's `google`.
pub fn price_for(provider: &str, model: &str) -> Option<&'static ModelPrice> {
    let model = model.trim();
    PRICES.iter().find(|p| {
        p.model_id.eq_ignore_ascii_case(model)
            && (p.provider == provider || p.provider == provider_alias(provider))
    })
}

#[derive(Debug, Clone)]
pub struct Bid {
    pub model: Option<String>,
    pub max_steps: usize,
    pub max_tool_calls: usize,
    pub max_tokens: u64,
    pub max_wall_ms: u64,
    /// Worst-case dollar ceiling: every budgeted token at the output price.
    pub max_cost_usd: Option<f64>,
    pub price_as_of: Option<String>,
    pub cost_note: String,
}

impl Bid {
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "model": self.model,
            "max_steps": self.max_steps,
            "max_tool_calls": self.max_tool_calls,
            "max_tokens": self.max_tokens,
            "max_wall_ms": self.max_wall_ms,
            "max_cost_usd": self.max_cost_usd,
            "price": self.model.as_deref().and_then(|m| {
                // Provider is not stored on the bid; match on model id only.
                PRICES.iter().find(|p| p.model_id.eq_ignore_ascii_case(m)).map(|p| {
                    serde_json::json!({
                        "input_per_1m": p.input_per_1m,
                        "output_per_1m": p.output_per_1m,
                        "as_of": p.as_of,
                    })
                })
            }),
            "price_as_of": self.price_as_of,
            "cost_note": self.cost_note,
            // The proof half of the bid: what evidence the run owes.
            "proof": {
                "requires_completed": true,
                "requires_result_summary": true,
            },
        })
    }

    /// Check the bid's proof terms against a finished run. Returns the gaps;
    /// empty means the bid was met. Budgets need no check — the loop
    /// enforces them, so they hold by construction.
    pub fn proof_gaps(&self, status: &str, result_summary: Option<&str>) -> Vec<String> {
        let mut gaps = Vec::new();
        if status != "completed" {
            gaps.push(format!("status is '{status}', bid required 'completed'"));
        }
        if result_summary.is_none_or(|s| s.trim().is_empty()) {
            gaps.push("no result summary, bid required one".to_string());
        }
        gaps
    }
}

pub fn build_bid(
    provider: &str,
    model: Option<&str>,
    max_steps: usize,
    max_tool_calls: usize,
    max_tokens: u64,
    max_wall_ms: u64,
) -> Bid {
    let (max_cost_usd, price_as_of, cost_note) = match model {
        Some(m) => match price_for(provider, m) {
            Some(p) => {
                let ceiling = (max_tokens as f64 / 1_000_000.0) * p.output_per_1m;
                (
                    Some((ceiling * 10000.0).round() / 10000.0),
                    Some(p.as_of.to_string()),
                    "worst-case: every budgeted token billed at the output price".to_string(),
                )
            }
            None => (
                None,
                None,
                format!("no price for model '{m}': pass a priced model for a dollar ceiling"),
            ),
        },
        None => (
            None,
            None,
            "model resolves at runtime from the provider catalog: pass --model for a dollar ceiling"
                .to_string(),
        ),
    };
    Bid {
        model: model.map(str::to_string),
        max_steps,
        max_tool_calls,
        max_tokens,
        max_wall_ms,
        max_cost_usd,
        price_as_of,
        cost_note,
    }
}

/// Worst-case token budget for a dollar cap: the cap buys tokens at the
/// expensive (output) price, so the spend can never exceed it.
pub fn tokens_for_budget(budget_usd: f64, provider: &str, model: &str) -> Option<u64> {
    let p = price_for(provider, model)?;
    if p.output_per_1m <= 0.0 {
        return None;
    }
    Some((budget_usd / (p.output_per_1m / 1_000_000.0)).floor() as u64)
}

/// Worst-case spend for tokens actually used.
pub fn worst_case_cost(tokens: u64, provider: &str, model: &str) -> Option<f64> {
    let p = price_for(provider, model)?;
    Some(((tokens as f64 / 1_000_000.0) * p.output_per_1m * 10000.0).round() / 10000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ceiling_is_worst_case() {
        // 100k tokens at claude-sonnet-4-6 output price: 0.1M * $15 = $1.50.
        let b = build_bid(
            "anthropic",
            Some("claude-sonnet-4-6"),
            40,
            200,
            100_000,
            1_200_000,
        );
        assert_eq!(b.max_cost_usd, Some(1.5));
        assert_eq!(b.price_as_of.as_deref(), Some("2026-01"));
    }

    #[test]
    fn unknown_model_has_no_ceiling() {
        let b = build_bid("anthropic", Some("mystery-9"), 40, 200, 100_000, 1_200_000);
        assert!(b.max_cost_usd.is_none());
        let b = build_bid("anthropic", None, 40, 200, 100_000, 1_200_000);
        assert!(b.max_cost_usd.is_none());
    }

    #[test]
    fn gemini_aliases_to_google_prices() {
        assert!(price_for("gemini", "gemini-2.5-flash").is_some());
    }

    #[test]
    fn budget_converts_to_tokens() {
        // $3 at sonnet output price ($15/1M) buys 200k tokens worst-case.
        assert_eq!(
            tokens_for_budget(3.0, "anthropic", "claude-sonnet-4-6"),
            Some(200_000)
        );
        assert!(tokens_for_budget(3.0, "anthropic", "mystery-9").is_none());
    }

    #[test]
    fn proof_gaps_detects_unmet_bid() {
        let b = build_bid(
            "anthropic",
            Some("claude-sonnet-4-6"),
            40,
            200,
            100_000,
            1_200_000,
        );
        assert!(b.proof_gaps("completed", Some("did the thing")).is_empty());
        let gaps = b.proof_gaps("failed", None);
        assert_eq!(gaps.len(), 2);
    }
}
