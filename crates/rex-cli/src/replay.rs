//! Proof replay as the primary review primitive (leapfrog bet 5).
//!
//! `rex replay RECEIPT.json` re-derives every mechanical claim in a
//! receipt from the evidence it carries, check by check:
//!
//! 1. `signature` — the Ed25519 certificate verifies (bytes unmodified).
//! 2. `bid.terms` — `bid_met`/`bid_gaps` recomputed from status + summary
//!    and compared with the recorded values.
//! 3. `bid.ceiling` — the worst-case $ ceiling recomputed from the price
//!    table and compared with the recorded ceiling.
//! 4. `policy` — the recorded contract re-evaluated against the run.
//! 5. `provenance` — hunk attributions are well-formed.
//! 6. `ledger` — the run_id is present in the local run ledger.
//!
//! Checks that don't apply (no bid, no policy, no provenance on the
//! receipt) are reported as *skipped*, never passed — a replay never
//! claims more than the receipt supports. Exit 0: every applicable check
//! passed. Exit 3: at least one failed.

use crate::cert::verify_receipt;
use crate::exec::state_dir;
use crate::ledger;
use serde_json::Value;

const SCHEMA: &str = "rex.replay.report/1";

#[derive(Debug, Clone, PartialEq, Eq)]
enum Verdict {
    Pass,
    Fail,
    Skip,
}

impl Verdict {
    fn as_str(&self) -> &'static str {
        match self {
            Verdict::Pass => "pass",
            Verdict::Fail => "fail",
            Verdict::Skip => "skip",
        }
    }
}

struct Check {
    name: &'static str,
    verdict: Verdict,
    detail: String,
}

impl Check {
    fn json(&self) -> Value {
        serde_json::json!({
            "name": self.name,
            "verdict": self.verdict.as_str(),
            "detail": self.detail,
        })
    }
}

fn str_of(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_string)
}

fn check_signature(r: &Value) -> Check {
    match verify_receipt(r.clone(), None) {
        Ok(report) => Check {
            name: "signature",
            verdict: Verdict::Pass,
            detail: format!("signed by {}", report.public_key),
        },
        Err(e) => Check {
            name: "signature",
            verdict: Verdict::Fail,
            detail: format!("{e}"),
        },
    }
}

/// Recompute the bid's proof terms from status + result summary.
fn check_bid_terms(r: &Value) -> Check {
    let bid = match r.get("bid") {
        Some(b) if b.is_object() => b,
        _ => {
            return Check {
                name: "bid.terms",
                verdict: Verdict::Skip,
                detail: "no bid on this receipt".to_string(),
            }
        }
    };
    let status = str_of(r, "status").unwrap_or_default();
    let summary = r.get("result").and_then(Value::as_str);
    let mut gaps = Vec::new();
    if status != "completed" {
        gaps.push(format!("status is '{status}', bid required 'completed'"));
    }
    if summary.is_none_or(|s| s.trim().is_empty()) {
        gaps.push("no result summary, bid required one".to_string());
    }
    let _ = bid;
    let recorded_met = r.get("bid_met").and_then(Value::as_bool);
    let mut recorded_gaps: Vec<String> = r
        .get("bid_gaps")
        .and_then(Value::as_array)
        .map(|xs| {
            xs.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    recorded_gaps.sort();
    gaps.sort();
    let met = gaps.is_empty();
    if recorded_met == Some(met) && recorded_gaps == gaps {
        Check {
            name: "bid.terms",
            verdict: Verdict::Pass,
            detail: if met {
                "bid_met=true, recomputed terms agree".to_string()
            } else {
                format!("bid_met=false, gaps agree: {gaps:?}")
            },
        }
    } else {
        Check {
            name: "bid.terms",
            verdict: Verdict::Fail,
            detail: format!(
                "recorded bid_met={recorded_met:?} gaps={recorded_gaps:?}; recomputed met={met} gaps={gaps:?}"
            ),
        }
    }
}

/// Recompute the worst-case $ ceiling from the price table.
fn check_bid_ceiling(r: &Value) -> Check {
    let bid = match r.get("bid") {
        Some(b) if b.is_object() => b,
        _ => {
            return Check {
                name: "bid.ceiling",
                verdict: Verdict::Skip,
                detail: "no bid on this receipt".to_string(),
            }
        }
    };
    let provider = str_of(r, "provider").unwrap_or_default();
    let model = bid.get("model").and_then(Value::as_str);
    let max_tokens = bid.get("max_tokens").and_then(Value::as_u64);
    let recorded = r.get("cost_usd_ceiling").and_then(Value::as_f64);
    let (model, max_tokens) = match (model, max_tokens) {
        (Some(m), Some(t)) => (m, t),
        _ => {
            return Check {
                name: "bid.ceiling",
                verdict: Verdict::Skip,
                detail: "bid has no model/max_tokens to recompute from".to_string(),
            }
        }
    };
    match crate::bid::worst_case_cost(max_tokens, &provider, model) {
        Some(recomputed) => {
            let close = recorded.is_some_and(|c| (c - recomputed).abs() < 1e-9);
            if close {
                Check {
                    name: "bid.ceiling",
                    verdict: Verdict::Pass,
                    detail: format!("ceiling ${recomputed} recomputed from price table"),
                }
            } else {
                Check {
                    name: "bid.ceiling",
                    verdict: Verdict::Fail,
                    detail: format!("recorded ceiling ${recorded:?}; recomputed ${recomputed}"),
                }
            }
        }
        None => {
            if recorded.is_none() {
                Check {
                    name: "bid.ceiling",
                    verdict: Verdict::Pass,
                    detail: "no known price; receipt honestly records no ceiling".to_string(),
                }
            } else {
                Check {
                    name: "bid.ceiling",
                    verdict: Verdict::Fail,
                    detail: "receipt claims a ceiling but the model has no known price".to_string(),
                }
            }
        }
    }
}

fn check_policy(r: &Value) -> Check {
    let pol = match r.get("policy") {
        Some(p) if p.is_object() => p,
        _ => {
            return Check {
                name: "policy",
                verdict: Verdict::Skip,
                detail: "no policy on this receipt".to_string(),
            }
        }
    };
    let policy = match crate::policy::parse(pol) {
        Ok(p) => p,
        Err(e) => {
            return Check {
                name: "policy",
                verdict: Verdict::Fail,
                detail: format!("recorded policy is malformed: {e}"),
            }
        }
    };
    let provider = str_of(r, "provider").unwrap_or_default();
    let bid_accepted = r.get("bid").is_some_and(Value::is_object);
    let ceiling = r.get("cost_usd_ceiling").and_then(Value::as_f64);
    match policy.check(&provider, bid_accepted, ceiling) {
        Ok(()) => Check {
            name: "policy",
            verdict: Verdict::Pass,
            detail: "recorded contract re-evaluated: satisfied".to_string(),
        },
        Err(e) => Check {
            name: "policy",
            verdict: Verdict::Fail,
            detail: format!("recorded contract re-evaluated: {e}"),
        },
    }
}

fn check_provenance(r: &Value) -> Check {
    let prov = match r.get("provenance") {
        Some(p) if p.is_object() => p,
        _ => {
            return Check {
                name: "provenance",
                verdict: Verdict::Skip,
                detail: "no provenance on this receipt".to_string(),
            }
        }
    };
    let obj = prov.as_object().unwrap();
    if obj.is_empty() {
        return Check {
            name: "provenance",
            verdict: Verdict::Pass,
            detail: "no file writes recorded".to_string(),
        };
    }
    let mut files = 0;
    let mut hunks = 0;
    for (file, entries) in obj {
        let entries = match entries.as_array() {
            Some(e) => e,
            None => {
                return Check {
                    name: "provenance",
                    verdict: Verdict::Fail,
                    detail: format!("provenance[{file}] is not an array"),
                }
            }
        };
        for e in entries {
            let ok_seq = e.get("event_seq").and_then(Value::as_u64).is_some();
            let ok_tool = e.get("tool").and_then(Value::as_str).is_some();
            let hs = e.get("hunks").and_then(Value::as_array);
            let ok_hunks = hs.is_some_and(|xs| {
                !xs.is_empty()
                    && xs.iter().all(|h| {
                        h.as_str()
                            .is_some_and(|s| s.starts_with("@@") && s[2..].contains("@@"))
                    })
            });
            if !(ok_seq && ok_tool && ok_hunks) {
                return Check {
                    name: "provenance",
                    verdict: Verdict::Fail,
                    detail: format!("provenance[{file}] has a malformed entry"),
                };
            }
            hunks += hs.unwrap().len();
        }
        files += 1;
    }
    Check {
        name: "provenance",
        verdict: Verdict::Pass,
        detail: format!("{files} file(s), {hunks} hunk(s) attributed"),
    }
}

fn check_ledger(r: &Value) -> Check {
    let run_id = match str_of(r, "run_id") {
        Some(id) => id,
        None => {
            return Check {
                name: "ledger",
                verdict: Verdict::Fail,
                detail: "receipt has no run_id".to_string(),
            }
        }
    };
    let path = ledger::ledger_path(&state_dir());
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => {
            return Check {
                name: "ledger",
                verdict: Verdict::Skip,
                detail: "no local ledger; receipt may come from another machine".to_string(),
            }
        }
    };
    let found = raw.lines().any(|line| {
        serde_json::from_str::<Value>(line)
            .ok()
            .and_then(|v| str_of(&v, "run_id"))
            .is_some_and(|id| id == run_id)
    });
    if found {
        Check {
            name: "ledger",
            verdict: Verdict::Pass,
            detail: format!("run {run_id} is in the local ledger"),
        }
    } else {
        Check {
            name: "ledger",
            verdict: Verdict::Fail,
            detail: format!("run {run_id} not found in the local ledger"),
        }
    }
}

pub fn run_replay(path: &str, json: bool) -> Result<i32, crate::exec::ExecError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| crate::exec::ExecError::usage(format!("cannot read {path}: {e}")))?;
    let receipt: Value = serde_json::from_str(&raw)
        .map_err(|e| crate::exec::ExecError::usage(format!("{path} is not valid JSON: {e}")))?;
    if !receipt.is_object() {
        return Err(crate::exec::ExecError::usage(
            "receipt is not a JSON object",
        ));
    }

    let checks = vec![
        check_signature(&receipt),
        check_bid_terms(&receipt),
        check_bid_ceiling(&receipt),
        check_policy(&receipt),
        check_provenance(&receipt),
        check_ledger(&receipt),
    ];
    let (pass, fail, skip) = checks
        .iter()
        .fold((0, 0, 0), |(p, f, s), c| match c.verdict {
            Verdict::Pass => (p + 1, f, s),
            Verdict::Fail => (p, f + 1, s),
            Verdict::Skip => (p, f, s + 1),
        });

    let report = serde_json::json!({
        "schema": SCHEMA,
        "run_id": str_of(&receipt, "run_id"),
        "checks": checks.iter().map(Check::json).collect::<Vec<_>>(),
        "passed": pass,
        "failed": fail,
        "skipped": skip,
    });

    if json {
        println!("{}", serde_json::to_string(&report).unwrap());
    } else {
        for c in &checks {
            let mark = match c.verdict {
                Verdict::Pass => "✓",
                Verdict::Fail => "✗",
                Verdict::Skip => "·",
            };
            println!("{mark} {:<12} {}", c.name, c.detail);
        }
        println!("replay: {pass} passed, {fail} failed, {skip} skipped");
    }

    Ok(if fail > 0 { 3 } else { 0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt_with_bid() -> Value {
        serde_json::json!({
            "schema": "rex.exec.receipt/1",
            "run_id": "r1",
            "status": "completed",
            "provider": "anthropic",
            "result": "did it",
            "bid": {"model": "claude-sonnet-4-6", "max_tokens": 250000u64},
            "bid_met": true,
            "bid_gaps": [],
            "cost_usd_ceiling": 3.75,
            "policy": {"schema": "rex.policy/1", "require_bid": true},
            "provenance": {"a.txt": [{"event_seq": 0u64, "tool": "write_file", "hunks": ["@@ -1,2 +1,3 @@"]}]},
        })
    }

    #[test]
    fn bid_terms_agree() {
        let c = check_bid_terms(&receipt_with_bid());
        assert_eq!(c.verdict, Verdict::Pass);
    }

    #[test]
    fn bid_terms_catch_lie() {
        let mut r = receipt_with_bid();
        r["bid_met"] = serde_json::json!(false);
        let c = check_bid_terms(&r);
        assert_eq!(c.verdict, Verdict::Fail);
    }

    #[test]
    fn bid_terms_skip_without_bid() {
        let c = check_bid_terms(&serde_json::json!({"status": "completed"}));
        assert_eq!(c.verdict, Verdict::Skip);
    }

    #[test]
    fn bid_ceiling_recomputes() {
        let c = check_bid_ceiling(&receipt_with_bid());
        assert_eq!(c.verdict, Verdict::Pass, "{}", c.detail);
    }

    #[test]
    fn bid_ceiling_catches_tamper() {
        let mut r = receipt_with_bid();
        r["cost_usd_ceiling"] = serde_json::json!(0.01);
        let c = check_bid_ceiling(&r);
        assert_eq!(c.verdict, Verdict::Fail);
    }

    #[test]
    fn policy_reevaluates() {
        let c = check_policy(&receipt_with_bid());
        assert_eq!(c.verdict, Verdict::Pass, "{}", c.detail);
    }

    #[test]
    fn policy_catches_violation() {
        let mut r = receipt_with_bid();
        r["provider"] = serde_json::json!("openai");
        r["policy"] = serde_json::json!({
            "schema": "rex.policy/1",
            "allowed_providers": ["anthropic"],
        });
        let c = check_policy(&r);
        assert_eq!(c.verdict, Verdict::Fail);
    }

    #[test]
    fn provenance_validates_shape() {
        let c = check_provenance(&receipt_with_bid());
        assert_eq!(c.verdict, Verdict::Pass);
        let mut r = receipt_with_bid();
        r["provenance"] = serde_json::json!({"a.txt": [{"event_seq": 0}]});
        assert_eq!(check_provenance(&r).verdict, Verdict::Fail);
    }
}
