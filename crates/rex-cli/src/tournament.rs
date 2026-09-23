//! Cross-provider tournaments (leapfrog bet 3).
//!
//! `rex tournament --task T --providers anthropic,openai` runs the same task
//! once per provider — same budgets, same workspace copy, same approval
//! policy — then promotes one winner by a deterministic, documented rule.
//! Every contestant receipt is individually signed, and the tournament
//! receipt is signed too, so the whole bracket is verifiable offline.
//!
//! Promotion rule (`completed_then_efficiency`), applied in order:
//!  1. `completed` outranks every other terminal status.
//!  2. Fewest tool calls wins (same work, less thrash).
//!  3. Fewest tokens wins.
//!  4. Least elapsed wall time wins.
//!  5. Provider name, alphabetical — the final tiebreak, so the outcome
//!     is fully deterministic.
//!
//! This is deliberately a *mechanical* rule, not a quality judgment: the
//! receipts carry everything a human or a judge needs to overrule it.

use crate::cert::{load_or_generate, sign_receipt};
use crate::exec::{execute, state_dir, ExecError, ExecOptions};
use crate::ledger;
use serde_json::Value;
use std::cmp::Ordering;

const SCHEMA: &str = "rex.tournament.receipt/1";
const RULE: &str = "completed_then_efficiency";

#[derive(Debug, Clone)]
pub struct TournamentOptions {
    pub task: String,
    pub providers: Vec<String>,
    pub model: Option<String>,
    pub workspace: Option<std::path::PathBuf>,
    pub max_steps: Option<usize>,
    pub max_tool_calls: Option<usize>,
    pub max_tokens: Option<u64>,
    pub timeout_secs: Option<u64>,
    pub json: bool,
    pub yes: bool,
}

fn status_of(r: &Value) -> &str {
    r.get("status").and_then(Value::as_str).unwrap_or("unknown")
}

fn u64_of(r: &Value, k: &str) -> u64 {
    r.get(k).and_then(Value::as_u64).unwrap_or(u64::MAX)
}

fn provider_of(r: &Value) -> &str {
    r.get("provider").and_then(Value::as_str).unwrap_or("")
}

/// Deterministic ordering: the *best* contestant sorts first.
fn rank(a: &Value, b: &Value) -> Ordering {
    let completed = |s: &str| if s == "completed" { 0 } else { 1 };
    completed(status_of(a))
        .cmp(&completed(status_of(b)))
        .then_with(|| u64_of(a, "tool_calls").cmp(&u64_of(b, "tool_calls")))
        .then_with(|| u64_of(a, "tokens_used").cmp(&u64_of(b, "tokens_used")))
        .then_with(|| u64_of(a, "elapsed_ms").cmp(&u64_of(b, "elapsed_ms")))
        .then_with(|| provider_of(a).cmp(provider_of(b)))
}

fn describe_winner(w: &Value) -> String {
    if status_of(w) == "completed" {
        format!(
            "{} completed in {} tool calls / {} tokens / {} ms",
            provider_of(w),
            w.get("tool_calls").and_then(Value::as_u64).unwrap_or(0),
            w.get("tokens_used").and_then(Value::as_u64).unwrap_or(0),
            w.get("elapsed_ms").and_then(Value::as_u64).unwrap_or(0),
        )
    } else {
        format!("{} ended as '{}'", provider_of(w), status_of(w))
    }
}

pub fn run_tournament(opts: TournamentOptions) -> Result<i32, ExecError> {
    if opts.task.trim().is_empty() {
        return Err(ExecError::usage(
            "no task given: rex tournament --task \"...\"",
        ));
    }
    if opts.providers.is_empty() {
        return Err(ExecError::usage(
            "no providers given: rex tournament --providers anthropic,openai",
        ));
    }

    let mut contestants: Vec<Value> = Vec::with_capacity(opts.providers.len());
    let mut workspaces: Vec<Option<std::path::PathBuf>> = Vec::with_capacity(opts.providers.len());
    for provider in &opts.providers {
        if !opts.json {
            eprintln!("rex: tournament contestant: {provider}");
        }
        let exec_opts = ExecOptions {
            task: opts.task.clone(),
            provider: provider.clone(),
            model: opts.model.clone(),
            workspace: opts.workspace.clone(),
            max_steps: opts.max_steps,
            max_tool_calls: opts.max_tool_calls,
            max_tokens: opts.max_tokens,
            timeout_secs: opts.timeout_secs,
            json: true, // never print per-contestant; the bracket is the output
            yes: opts.yes,
            dry_run: false,
            bid: false,
            accept_bid: false,
            budget_usd: None,
            run_tag: Some(format!("-{provider}")),
            deadman_mins: None,
            deadman_file: None,
            skills: vec![],
            interactive: None,
            on_begin: None,
        };
        match execute(exec_opts) {
            Ok(out) => {
                workspaces.push(out.workspace);
                contestants.push(out.receipt);
            }
            Err(e) => {
                // A contestant that cannot even start still gets a seat at
                // the table, marked honestly, so the bracket is complete.
                eprintln!("rex: contestant {provider} failed to start: {}", e.message);
                workspaces.push(None);
                contestants.push(serde_json::json!({
                    "schema": "rex.exec.receipt/1",
                    "provider": provider,
                    "status": "failed",
                    "error": e.message,
                    "tool_calls": 0,
                    "tokens_used": 0,
                    "elapsed_ms": 0,
                }));
            }
        }
    }

    let mut ordered: Vec<usize> = (0..contestants.len()).collect();
    ordered.sort_by(|&a, &b| rank(&contestants[a], &contestants[b]));
    let winner_idx = ordered[0];
    let winner = &contestants[winner_idx];
    let winner_completed = status_of(winner) == "completed";

    let bracket_id = format!(
        "tournament-{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    );
    let receipt = serde_json::json!({
        "schema": SCHEMA,
        "run_id": bracket_id,
        "task": opts.task,
        "providers": opts.providers,
        "rule": RULE,
        "contestants": contestants,
        "finished_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "winner": {
            "provider": provider_of(winner),
            "run_id": winner.get("run_id"),
            "status": status_of(winner),
            "reason": describe_winner(winner),
        },
    });
    let mut map = receipt
        .as_object()
        .cloned()
        .expect("tournament receipt is an object");
    match load_or_generate(&state_dir()).and_then(|key| sign_receipt(&key, &mut map)) {
        Ok(_) => {}
        Err(e) => eprintln!("rex: warning: tournament receipt is unsigned: {e}"),
    }
    let out = Value::Object(map);
    ledger::append(&state_dir(), &out);

    if opts.json {
        println!("{}", serde_json::to_string(&out).unwrap());
    } else {
        println!(
            "tournament winner: {} ({})",
            provider_of(winner),
            describe_winner(winner)
        );
        for (i, idx) in ordered.iter().enumerate() {
            let c = &contestants[*idx];
            let ws = workspaces[*idx]
                .as_ref()
                .map(|w| format!(" ({})", w.display()))
                .unwrap_or_default();
            println!("  {}. {} — {}{}", i + 1, provider_of(c), status_of(c), ws);
        }
    }

    Ok(if winner_completed { 0 } else { 3 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contestant(provider: &str, status: &str, calls: u64, tokens: u64, ms: u64) -> Value {
        serde_json::json!({
            "provider": provider,
            "status": status,
            "tool_calls": calls,
            "tokens_used": tokens,
            "elapsed_ms": ms,
        })
    }

    #[test]
    fn completed_beats_failed_regardless_of_cost() {
        let a = contestant("a", "failed", 1, 1, 1);
        let b = contestant("b", "completed", 999, 999, 999);
        assert_eq!(rank(&a, &b), Ordering::Greater);
    }

    #[test]
    fn efficiency_breaks_ties_deterministically() {
        let a = contestant("b-provider", "completed", 10, 100, 1000);
        let b = contestant("a-provider", "completed", 10, 100, 1000);
        // Identical cost: alphabetical provider wins, always.
        assert_eq!(rank(&a, &b), Ordering::Greater);
        assert_eq!(rank(&b, &a), Ordering::Less);

        let cheap = contestant("x", "completed", 5, 100, 1000);
        let pricey = contestant("y", "completed", 10, 50, 500);
        // Tool calls outrank tokens and time.
        assert_eq!(rank(&cheap, &pricey), Ordering::Less);
    }
}
