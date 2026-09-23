//! Cost visibility: provider price tables, per-run cost receipts, budgets.
//!
//! Prices are static estimates (USD per 1M tokens) — providers change them,
//! so the UI labels them as estimates. The store records a receipt per run
//! and tracks spend against user-set budgets.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// Static price table (USD per 1M tokens). Estimates — verify with provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPrice {
    pub model_id: String,
    pub provider: String,
    pub input_per_1m: f64,
    pub output_per_1m: f64,
    pub as_of: String,
}

pub fn price_table() -> Vec<ModelPrice> {
    vec![
        ModelPrice {
            model_id: "gpt-5".to_string(),
            provider: "openai".to_string(),
            input_per_1m: 1.25,
            output_per_1m: 10.0,
            as_of: "2026-01".to_string(),
        },
        ModelPrice {
            model_id: "gpt-5-mini".to_string(),
            provider: "openai".to_string(),
            input_per_1m: 0.25,
            output_per_1m: 2.0,
            as_of: "2026-01".to_string(),
        },
        ModelPrice {
            model_id: "claude-opus-4-6".to_string(),
            provider: "anthropic".to_string(),
            input_per_1m: 15.0,
            output_per_1m: 75.0,
            as_of: "2026-01".to_string(),
        },
        ModelPrice {
            model_id: "claude-sonnet-4-6".to_string(),
            provider: "anthropic".to_string(),
            input_per_1m: 3.0,
            output_per_1m: 15.0,
            as_of: "2026-01".to_string(),
        },
        ModelPrice {
            model_id: "gemini-2.5-pro".to_string(),
            provider: "google".to_string(),
            input_per_1m: 1.25,
            output_per_1m: 10.0,
            as_of: "2026-01".to_string(),
        },
        ModelPrice {
            model_id: "gemini-2.5-flash".to_string(),
            provider: "google".to_string(),
            input_per_1m: 0.30,
            output_per_1m: 2.50,
            as_of: "2026-01".to_string(),
        },
    ]
}

pub fn estimate_cost(model_id: &str, prompt_tokens: u64, completion_tokens: u64) -> Option<f64> {
    let price = price_table()
        .into_iter()
        .find(|p| p.model_id == model_id)?;
    Some(
        (prompt_tokens as f64 / 1_000_000.0) * price.input_per_1m
            + (completion_tokens as f64 / 1_000_000.0) * price.output_per_1m,
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostReceipt {
    pub run_id: String,
    pub model_id: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost_usd: Option<f64>,
    pub at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Budget {
    /// Daily limit in USD. None = no limit.
    pub daily_usd: Option<f64>,
    /// Monthly limit in USD. None = no limit.
    pub monthly_usd: Option<f64>,
}

impl Default for Budget {
    fn default() -> Self {
        Budget {
            daily_usd: None,
            monthly_usd: None,
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoreData {
    receipts: Vec<CostReceipt>,
    budget: Budget,
}

pub struct CostStore {
    path: PathBuf,
    inner: Mutex<StoreData>,
}

impl CostStore {
    pub fn new(config_dir: &Path) -> Result<Self, String> {
        let path = config_dir.join("cost.json");
        let data = if path.exists() {
            let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            serde_json::from_str(&raw).map_err(|e| e.to_string())?
        } else {
            StoreData::default()
        };
        Ok(CostStore {
            path,
            inner: Mutex::new(data),
        })
    }

    fn save(&self) -> Result<(), String> {
        let data = self.inner.lock().map_err(|e| e.to_string())?;
        let raw = serde_json::to_string_pretty(&*data).map_err(|e| e.to_string())?;
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, raw).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Record a run's token usage. Returns the receipt.
    pub fn record(
        &self,
        run_id: &str,
        model_id: &str,
        prompt_tokens: u64,
        completion_tokens: u64,
    ) -> Result<CostReceipt, String> {
        let cost_usd = estimate_cost(model_id, prompt_tokens, completion_tokens);
        let receipt = CostReceipt {
            run_id: run_id.to_string(),
            model_id: model_id.to_string(),
            prompt_tokens,
            completion_tokens,
            cost_usd,
            at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_millis() as u64,
        };
        {
            let mut data = self.inner.lock().map_err(|e| e.to_string())?;
            data.receipts.push(receipt.clone());
            // Keep last 1000 receipts.
            if data.receipts.len() > 1000 {
                let excess = data.receipts.len() - 1000;
                data.receipts.drain(..excess);
            }
        }
        self.save()?;
        Ok(receipt)
    }

    pub fn receipts(&self, limit: usize) -> Result<Vec<CostReceipt>, String> {
        let data = self.inner.lock().map_err(|e| e.to_string())?;
        let mut out = data.receipts.clone();
        out.sort_by(|a, b| b.at_ms.cmp(&a.at_ms));
        out.truncate(limit);
        Ok(out)
    }

    pub fn get_budget(&self) -> Result<Budget, String> {
        let data = self.inner.lock().map_err(|e| e.to_string())?;
        Ok(data.budget.clone())
    }

    pub fn set_budget(&self, budget: Budget) -> Result<(), String> {
        {
            let mut data = self.inner.lock().map_err(|e| e.to_string())?;
            data.budget = budget;
        }
        self.save()?;
        Ok(())
    }

    /// Total spend in the last `days` days (USD). Unknown prices are skipped.
    pub fn spend_last_days(&self, days: u64) -> Result<f64, String> {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_millis() as u64;
        let cutoff = now_ms.saturating_sub(days * 24 * 60 * 60 * 1000);
        let data = self.inner.lock().map_err(|e| e.to_string())?;
        Ok(data
            .receipts
            .iter()
            .filter(|r| r.at_ms >= cutoff)
            .filter_map(|r| r.cost_usd)
            .sum())
    }

    /// Check if a prospective cost would exceed budgets. Returns a warning
    /// message if so, None if within budget.
    pub fn check_budget(&self, additional_usd: f64) -> Result<Option<String>, String> {
        let budget = self.get_budget()?;
        if let Some(daily) = budget.daily_usd {
            let spent = self.spend_last_days(1)?;
            if spent + additional_usd > daily {
                return Ok(Some(format!(
                    "Daily budget exceeded: ${:.2} spent + ${:.2} estimated > ${:.2} limit",
                    spent, additional_usd, daily
                )));
            }
        }
        if let Some(monthly) = budget.monthly_usd {
            let spent = self.spend_last_days(30)?;
            if spent + additional_usd > monthly {
                return Ok(Some(format!(
                    "Monthly budget exceeded: ${:.2} spent + ${:.2} estimated > ${:.2} limit",
                    spent, additional_usd, monthly
                )));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rex-cost-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn estimate_and_record() {
        let dir = temp();
        let store = CostStore::new(&dir).unwrap();

        // Known model.
        let cost = estimate_cost("gpt-5-mini", 1_000_000, 1_000_000).unwrap();
        assert!((cost - 2.25).abs() < 0.01);

        // Unknown model.
        assert!(estimate_cost("unknown-model", 100, 100).is_none());

        // Record.
        let r = store.record("run-1", "gpt-5-mini", 1000, 2000).unwrap();
        assert!(r.cost_usd.is_some());

        let receipts = store.receipts(10).unwrap();
        assert_eq!(receipts.len(), 1);
    }

    #[test]
    fn budget_check() {
        let dir = temp();
        let store = CostStore::new(&dir).unwrap();

        store
            .set_budget(Budget {
                daily_usd: Some(1.0),
                monthly_usd: None,
            })
            .unwrap();

        // Record $0.50 spend.
        store.record("run-1", "gpt-5-mini", 1_000_000, 0).unwrap(); // $0.25
        store.record("run-2", "gpt-5-mini", 1_000_000, 0).unwrap(); // $0.25

        // $0.60 more would exceed $1.00 daily.
        let warning = store.check_budget(0.60).unwrap();
        assert!(warning.is_some());

        // $0.40 more is fine.
        let warning = store.check_budget(0.40).unwrap();
        assert!(warning.is_none());
    }
}
