//! Adversarial immune system, first cut (leapfrog bet 2).
//!
//! `rex redteam --task T` runs the task once (the builder), then runs a
//! second agent (the critic) whose only job is to attack the builder's
//! result: concrete flaws, wrong claims, untested paths, security holes,
//! bid violations. The redteam receipt binds both receipts plus a verdict,
//! and it is signed like everything else.
//!
//! The verdict is mechanical, not a quality judgment. The critic is
//! instructed to end its summary with `VERDICT: SOUND` or
//! `VERDICT: BROKEN: <reason>`; if it doesn't follow the protocol the
//! verdict is `inconclusive`, and the raw critic summary stays in the
//! receipt for a human to check.
//!
//! Requires `--yes`: an adversarial loop must never hang on interactive
//! approval. Exit 0 means the result survived the attack (builder
//! completed and verdict is sound); exit 3 means broken, inconclusive,
//! or incomplete — CI should treat that as "do not ship".

use crate::cert::{load_or_generate, sign_receipt};
use crate::exec::{execute, state_dir, ExecError, ExecOptions};
use crate::ledger;
use serde_json::Value;

const SCHEMA: &str = "rex.redteam.receipt/1";

#[derive(Debug, Clone)]
pub struct RedteamOptions {
    pub task: String,
    pub provider: String,
    pub model: Option<String>,
    pub workspace: Option<std::path::PathBuf>,
    pub max_steps: Option<usize>,
    pub max_tool_calls: Option<usize>,
    pub max_tokens: Option<u64>,
    pub timeout_secs: Option<u64>,
    pub json: bool,
    pub yes: bool,
    pub dry_run: bool,
}

fn summary_of(r: &Value) -> &str {
    // Real exec receipts store the summary under "result"; older fixtures
    // used "completion_summary". Read the real key first.
    r.get("result")
        .or_else(|| r.get("completion_summary"))
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("(no summary produced)")
}

fn status_of(r: &Value) -> &str {
    r.get("status").and_then(Value::as_str).unwrap_or("unknown")
}

fn critic_task(task: &str, builder: &Value) -> String {
    format!(
        "You are an adversarial reviewer. Another agent (the builder) was given this task:\n\n\
         {task}\n\n\
         The builder finished with status \"{status}\" and reported:\n\n\
         {summary}\n\n\
         Its workspace is your workspace: inspect the actual files, tests, and diffs.\n\
         Your job is to ATTACK the result, not to praise it. Find concrete flaws:\n\
         claims in the summary that the files don't support, code that doesn't do\n\
         what it says, untested paths, security holes, budget/bid violations,\n\
         missing edge cases. Verify before you accuse: read the code.\n\n\
         When you are done, end your final summary with exactly one of these lines:\n\
         VERDICT: SOUND\n\
         VERDICT: BROKEN: <one-line reason>",
        task = task,
        status = status_of(builder),
        summary = summary_of(builder),
    )
}

/// Parse the critic's verdict. Returns ("sound"|"broken"|"inconclusive", reason).
pub fn parse_verdict(summary: &str) -> (String, Option<String>) {
    for line in summary.lines().rev() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("VERDICT:") {
            let rest = rest.trim();
            if rest.eq_ignore_ascii_case("SOUND") {
                return ("sound".to_string(), None);
            }
            if let Some(reason) = rest.strip_prefix("BROKEN:") {
                return ("broken".to_string(), Some(reason.trim().to_string()));
            }
            if let Some(reason) = rest.strip_prefix("broken:") {
                return ("broken".to_string(), Some(reason.trim().to_string()));
            }
        }
    }
    ("inconclusive".to_string(), None)
}

fn exec_opts_for(
    opts: &RedteamOptions,
    task: String,
    workspace: Option<std::path::PathBuf>,
) -> ExecOptions {
    ExecOptions {
        task,
        provider: opts.provider.clone(),
        model: opts.model.clone(),
        workspace,
        max_steps: opts.max_steps,
        max_tool_calls: opts.max_tool_calls,
        max_tokens: opts.max_tokens,
        timeout_secs: opts.timeout_secs,
        json: true, // never print the legs; the redteam receipt is the output
        yes: true,  // --yes is required by run_redteam
        dry_run: false,
        detach: false,
        bid: false,
        accept_bid: false,
        budget_usd: None,
        run_tag: None,
        deadman_mins: None,
        deadman_file: None,
        skills: vec![],
        name: None,
        continued_from: None,
        interactive: None,
        on_begin: None,
    }
}

pub fn run_redteam(opts: RedteamOptions) -> Result<i32, ExecError> {
    if opts.task.trim().is_empty() {
        return Err(ExecError::usage(
            "no task given: rex redteam --task \"...\"",
        ));
    }
    if !opts.yes {
        return Err(ExecError::usage(
            "rex redteam requires --yes: an adversarial loop must not hang on approval",
        ));
    }

    if opts.dry_run {
        let out = serde_json::json!({
            "schema": "rex.redteam.dry_run/1",
            "task": opts.task,
            "provider": opts.provider,
            "model": opts.model,
            "builder_budgets": {
                "max_steps": opts.max_steps,
                "max_tool_calls": opts.max_tool_calls,
                "max_tokens": opts.max_tokens,
                "timeout_secs": opts.timeout_secs,
            },
            "critic_budgets": "half the builder's steps/tool calls (min 8/20), same token/time budgets",
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
        return Ok(2);
    }

    if !opts.json {
        eprintln!("rex: redteam builder starting");
    }
    let builder_out = execute(exec_opts_for(
        &opts,
        opts.task.clone(),
        opts.workspace.clone(),
    ))?; // config/usage failure: nothing to attack
    let builder = builder_out.receipt;

    if !opts.json {
        eprintln!("rex: redteam critic starting");
    }
    // The critic gets half the builder's step/tool-call budget (bounded
    // below): attacking should be cheaper than building.
    let half = |v: Option<usize>, floor: usize| v.map(|n| n.div_ceil(2).max(floor));
    let critic_opts = ExecOptions {
        max_steps: half(opts.max_steps, 8).or(Some(12)),
        max_tool_calls: half(opts.max_tool_calls, 20).or(Some(40)),
        ..exec_opts_for(
            &opts,
            critic_task(&opts.task, &builder),
            builder_out.workspace,
        )
    };
    let critic_out = match execute(critic_opts) {
        Ok(out) => out,
        Err(e) => {
            // The critic failing to start is an incomplete immune check,
            // recorded honestly rather than hidden.
            eprintln!("rex: redteam critic failed to start: {}", e.message);
            return Ok(finish(
                opts,
                builder,
                None,
                "inconclusive",
                Some(format!("critic failed to start: {}", e.message)),
            ));
        }
    };
    let critic = critic_out.receipt;

    let (verdict, reason) = parse_verdict(summary_of(&critic));
    if !opts.json {
        eprintln!("rex: redteam verdict: {verdict}");
    }
    Ok(finish(opts, builder, Some(critic), &verdict, reason))
}

fn finish(
    opts: RedteamOptions,
    builder: Value,
    critic: Option<Value>,
    verdict: &str,
    verdict_reason: Option<String>,
) -> i32 {
    let builder_completed = status_of(&builder) == "completed";
    let survived = builder_completed && verdict == "sound";

    let receipt = serde_json::json!({
        "schema": SCHEMA,
        "run_id": format!("redteam-{}", chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)),
        "task": opts.task,
        "provider": opts.provider,
        "model": opts.model,
        "builder": builder,
        "critic": critic,
        "verdict": verdict,
        "verdict_reason": verdict_reason,
        "survived": survived,
        "finished_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    });
    let mut map = receipt
        .as_object()
        .cloned()
        .expect("redteam receipt is an object");
    match load_or_generate(&state_dir()).and_then(|key| sign_receipt(&key, &mut map)) {
        Ok(_) => {}
        Err(e) => eprintln!("rex: warning: redteam receipt is unsigned: {e}"),
    }
    let out = Value::Object(map);
    ledger::append(&state_dir(), &out);

    if opts.json {
        println!("{}", serde_json::to_string(&out).unwrap());
    } else {
        println!("redteam verdict: {verdict}");
        if let Some(r) = &verdict_reason {
            println!("reason: {r}");
        }
    }

    if survived {
        0
    } else {
        3
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_sound() {
        let (v, r) = parse_verdict("some findings...\nVERDICT: SOUND");
        assert_eq!(v, "sound");
        assert!(r.is_none());
    }

    #[test]
    fn verdict_broken_with_reason() {
        let (v, r) = parse_verdict("blah\nVERDICT: BROKEN: tests never ran");
        assert_eq!(v, "broken");
        assert_eq!(r.as_deref(), Some("tests never ran"));
    }

    #[test]
    fn verdict_missing_is_inconclusive() {
        let (v, _) = parse_verdict("looks fine to me, ship it");
        assert_eq!(v, "inconclusive");
    }

    #[test]
    fn verdict_takes_last_line() {
        let (v, _) = parse_verdict("VERDICT: BROKEN: x\nVERDICT: SOUND");
        assert_eq!(v, "sound");
    }

    #[test]
    fn critic_task_contains_evidence() {
        // Regression: real exec receipts store the summary under "result",
        // not "completion_summary" (which summary_of used to read).
        let b = serde_json::json!({"status": "completed", "result": "built it"});
        assert_eq!(summary_of(&b), "built it");
        let legacy = serde_json::json!({"status": "completed", "completion_summary": "old"});
        assert_eq!(summary_of(&legacy), "old");
        let t = critic_task("make tea", &b);
        assert!(t.contains("make tea"));
        assert!(t.contains("built it"));
        assert!(t.contains("VERDICT: SOUND"));
    }
}
