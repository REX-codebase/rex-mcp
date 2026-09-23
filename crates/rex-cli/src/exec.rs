//! `rex exec`: headless, scriptable agent runs for CI and automation.
//!
//! Contract:
//! - stdout carries exactly one JSON receipt when `--json` is passed
//!   (nothing else, so CI can parse it). All logs go to stderr.
//! - Approvals are never silently granted: `--yes` is the explicit,
//!   auditable opt-in that auto-approves tool calls and plans.
//! - The agent works on a disposable copy of `--workspace`, never on the
//!   caller's original directory.
//!
//! Exit codes: 0 = run completed; 2 = usage/config/approval needed;
//! 3 = run ended in a non-completed terminal state; 1 = internal error.

use rex_providers::{
    find_spec, AgentSnapshot, AgentStatus, AutonomousRunService, Budgets, FileSecretStore,
    ProviderError, ProviderService, SecretStore, TerminalReason, UreqTransport,
};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::cert::{load_or_generate, sign_receipt};
use crate::ledger;

const RECEIPT_SCHEMA: &str = "rex.exec.receipt/1";

/// Secret store that prefers `REX_<PROVIDER>_API_KEY` env vars (CI convention)
/// and falls back to the file store the desktop app uses. Writes always go
/// to the file store; the CLI never persists anything into the environment.
struct EnvSecretStore<S: SecretStore> {
    inner: S,
}

impl<S: SecretStore> EnvSecretStore<S> {
    fn env_name(provider: &str) -> String {
        format!("REX_{}_API_KEY", provider.to_uppercase().replace('-', "_"))
    }
}

impl<S: SecretStore> SecretStore for EnvSecretStore<S> {
    fn get_key(&self, provider: &str) -> Result<Option<String>, ProviderError> {
        let name = Self::env_name(provider);
        if let Ok(v) = std::env::var(&name) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                return Ok(Some(v));
            }
        }
        // Generic fallback so CI can bind the secret via `env:` without
        // interpolating it into shell text (avoids quoting/injection bugs).
        if let Ok(v) = std::env::var("REX_API_KEY") {
            let v = v.trim().to_string();
            if !v.is_empty() {
                return Ok(Some(v));
            }
        }
        self.inner.get_key(provider)
    }

    fn set_key(&self, provider: &str, key: &str) -> Result<(), ProviderError> {
        self.inner.set_key(provider, key)
    }

    fn clear_key(&self, provider: &str) -> Result<(), ProviderError> {
        self.inner.clear_key(provider)
    }
}

#[derive(Debug, Clone, Default)]
pub struct ExecOptions {
    pub task: String,
    pub provider: String,
    pub model: Option<String>,
    pub workspace: Option<PathBuf>,
    pub max_steps: Option<usize>,
    pub max_tool_calls: Option<usize>,
    pub max_tokens: Option<u64>,
    pub timeout_secs: Option<u64>,
    pub json: bool,
    pub yes: bool,
    pub dry_run: bool,
    /// Print the binding bid and stop unless `accept_bid` is also given.
    pub bid: bool,
    pub accept_bid: bool,
    /// Dollar cap: converted worst-case into a token budget. Needs --model.
    pub budget_usd: Option<f64>,
    /// Internal disambiguator for parallel-family runs (e.g. tournament
    /// contestants): each tag gets its own staged workspace directory.
    /// Not a CLI flag.
    pub run_tag: Option<String>,
    /// Dead-man custody: check in at least this often (minutes) or the
    /// run is cancelled. A liveness signal, not a runtime budget.
    pub deadman_mins: Option<u64>,
    /// Override the dead-man check-in file location.
    pub deadman_file: Option<PathBuf>,
    /// Skill packs to load into the run, by library name. Repeatable.
    pub skills: Vec<String>,
}

#[derive(Debug)]
pub struct ExecError {
    pub code: i32,
    pub message: String,
}

impl ExecError {
    pub(crate) fn usage(msg: impl Into<String>) -> Self {
        Self {
            code: 2,
            message: msg.into(),
        }
    }
    pub(crate) fn internal(msg: impl Into<String>) -> Self {
        Self {
            code: 1,
            message: msg.into(),
        }
    }
}

impl From<String> for ExecError {
    fn from(message: String) -> Self {
        Self::internal(message)
    }
}

pub(crate) fn state_dir() -> PathBuf {
    std::env::var_os("REX_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".rex")
                .join("harness")
        })
}

/// Copy a directory tree into `dst`, skipping version-control metadata and
/// build output directories. The agent always works on the copy.
fn copy_workspace(src: &Path, dst: &Path) -> Result<(), String> {
    const SKIP: &[&str] = &[
        ".git",
        ".hg",
        ".svn",
        "target",
        "node_modules",
        ".venv",
        "venv",
        "__pycache__",
        ".tox",
    ];
    if !src.is_dir() {
        return Err(format!("workspace is not a directory: {}", src.display()));
    }
    std::fs::create_dir_all(dst).map_err(|e| format!("cannot create {}: {e}", dst.display()))?;
    let mut stack = vec![(src.to_path_buf(), dst.to_path_buf())];
    while let Some((s, d)) = stack.pop() {
        let entries =
            std::fs::read_dir(&s).map_err(|e| format!("cannot read {}: {e}", s.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("cannot list {}: {e}", s.display()))?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if SKIP.iter().any(|s| *s == name_str.as_ref()) {
                continue;
            }
            let from = entry.path();
            let to = d.join(&name);
            let ftype = entry
                .file_type()
                .map_err(|e| format!("cannot stat {}: {e}", from.display()))?;
            if ftype.is_dir() {
                std::fs::create_dir_all(&to)
                    .map_err(|e| format!("cannot create {}: {e}", to.display()))?;
                stack.push((from, to));
            } else if ftype.is_file() {
                std::fs::copy(&from, &to)
                    .map_err(|e| format!("cannot copy {}: {e}", from.display()))?;
            }
            // Symlinks and other special files are intentionally skipped:
            // the sandbox copy must not escape through them.
        }
    }
    Ok(())
}

fn terminal_reason_value(t: &Option<TerminalReason>) -> serde_json::Value {
    // TerminalReason serializes with a `kind` tag and snake_case names,
    // so the receipt stays in sync with the enum automatically.
    serde_json::to_value(t).unwrap_or(serde_json::Value::Null)
}

fn receipt(
    opts: &ExecOptions,
    snap: &AgentSnapshot,
    workspace: Option<&Path>,
    accepted_bid: Option<&crate::bid::Bid>,
    policy: Option<&crate::policy::Policy>,
    deadman: Option<Value>,
    skills: &[crate::skill::LoadedSkill],
) -> Map<String, Value> {
    let status_str = serde_json::to_value(&snap.status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string());

    let (cost_usd_ceiling, cost_usd_estimate) = match accepted_bid {
        Some(b) => (
            b.max_cost_usd,
            b.model
                .as_deref()
                .and_then(|m| crate::bid::worst_case_cost(snap.tokens_used, &opts.provider, m)),
        ),
        None => (None, None),
    };
    serde_json::json!({
        "schema": RECEIPT_SCHEMA,
        "run_id": snap.id,
        "task": opts.task,
        "provider": snap.provider,
        "model": snap.model,
        "status": snap.status,
        "terminal_reason": terminal_reason_value(&snap.terminal_reason),
        "steps": snap.step,
        "max_steps": snap.max_steps,
        "tool_calls": snap.tool_calls,
        "max_tool_calls": snap.max_tool_calls,
        "tokens_used": snap.tokens_used,
        "max_tokens": snap.max_tokens,
        "elapsed_ms": snap.elapsed_ms,
        "max_wall_ms": snap.max_wall_ms,
        "result": snap.completion_summary,
        "error": snap.error,
        "prompt_version": snap.prompt_version,
        "prompt_hash": snap.prompt_hash,
        "workspace": workspace.map(|w| w.display().to_string()),
        "provenance": crate::provenance::build(&snap.events),
        "finished_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "bid": accepted_bid.map(|b| b.to_json()),
        "cost_usd_ceiling": cost_usd_ceiling,
        "cost_usd_estimate": cost_usd_estimate,
        "cost_basis": accepted_bid.map(|_| "worst-case: all tokens at the output price"),
        "bid_met": accepted_bid.map(|b| b.proof_gaps(&status_str, snap.completion_summary.as_deref()).is_empty()),
        "bid_gaps": accepted_bid.map(|b| b.proof_gaps(&status_str, snap.completion_summary.as_deref())),
        "policy": policy.map(|p| p.to_json()),
        "deadman": deadman,
        "skills": skills.iter().map(crate::skill::to_json).collect::<Vec<_>>(),
    })
    .as_object()
    .cloned()
    .expect("receipt literal is an object")
}

type Agent = AutonomousRunService<EnvSecretStore<FileSecretStore>, UreqTransport>;

fn build_agent() -> Result<(Agent, PathBuf), ExecError> {
    let state = state_dir();
    std::fs::create_dir_all(&state)
        .map_err(|e| ExecError::internal(format!("cannot create state dir: {e}")))?;
    let runs_root = state.join("agent-runs");
    std::fs::create_dir_all(&runs_root)
        .map_err(|e| ExecError::internal(format!("cannot create runs root: {e}")))?;
    let store = FileSecretStore::new(state.clone())
        .map_err(|e| ExecError::internal(format!("cannot open credential store: {e}")))?;
    let service = ProviderService::new(EnvSecretStore { inner: store }, UreqTransport::new());
    Ok((
        AutonomousRunService::new(service, None, runs_root.clone()),
        runs_root,
    ))
}

fn drive(
    agent: &Agent,
    opts: &ExecOptions,
    run_id: &str,
    deadman: &mut Option<crate::deadman::Deadman>,
) -> Result<AgentSnapshot, ExecError> {
    loop {
        std::thread::sleep(Duration::from_millis(500));
        if let Some(dm) = deadman {
            let rid = run_id.to_string();
            let tripped = dm
                .poll(&|| agent.cancel(&rid).map(|_| ()).map_err(|e| e.to_string()))
                .map_err(ExecError::internal)?;
            if tripped && !opts.json {
                eprintln!("rex: dead-man tripped: no operator check-in — run cancelled");
            }
        }
        let snap = agent
            .snapshot(run_id)
            .ok_or_else(|| ExecError::internal("run vanished from the registry"))?;
        match snap.status {
            AgentStatus::AwaitingApproval => {
                let call = snap.pending_approval.clone().unwrap_or_else(|| {
                    // Should not happen, but never unwrap across a trust boundary.
                    panic_missing_approval()
                });
                if opts.yes {
                    if !opts.json {
                        eprintln!(
                            "rex: auto-approving [{}] {} ({})",
                            call.tool, call.summary, call.policy_reason
                        );
                    }
                    agent
                        .decide(run_id, true)
                        .map_err(|e| ExecError::internal(format!("approval failed: {e}")))?;
                } else {
                    eprintln!("rex: run is waiting for approval:");
                    eprintln!("  tool:   {}", call.tool);
                    eprintln!("  summary: {}", call.summary);
                    eprintln!("  reason: {}", call.policy_reason);
                    return Err(ExecError::usage(
                        "approval required: re-run with --yes to auto-approve (CI mode)",
                    ));
                }
            }
            AgentStatus::AwaitingPlan => {
                if opts.yes {
                    if !opts.json {
                        eprintln!("rex: auto-approving plan");
                    }
                    agent
                        .decide_plan(run_id, true)
                        .map_err(|e| ExecError::internal(format!("plan approval failed: {e}")))?;
                } else {
                    return Err(ExecError::usage(
                        "plan approval required: re-run with --yes to auto-approve (CI mode)",
                    ));
                }
            }
            AgentStatus::Completed
            | AgentStatus::Blocked
            | AgentStatus::Denied
            | AgentStatus::Cancelled
            | AgentStatus::Failed => return Ok(snap),
            _ => {}
        }
    }
}

fn panic_missing_approval() -> ! {
    // Unreachable in practice: AwaitingApproval always carries a call.
    // A loud failure beats a silent wrong approval.
    eprintln!("rex: internal error: approval gate with no pending call");
    std::process::exit(1);
}

pub fn execute(mut opts: ExecOptions) -> Result<ExecOutput, ExecError> {
    if opts.task.trim().is_empty() {
        return Err(ExecError::usage("no task given: rex exec --task \"...\""));
    }
    if opts.provider.trim().is_empty() {
        opts.provider = std::env::var("REX_PROVIDER").unwrap_or_else(|_| "anthropic".to_string());
    }
    if opts.model.is_none() {
        opts.model = std::env::var("REX_MODEL")
            .ok()
            .filter(|m| !m.trim().is_empty());
    }
    if find_spec(&opts.provider).is_none() {
        return Err(ExecError::usage(format!(
            "unknown provider '{}': see provider registry",
            opts.provider
        )));
    }

    // Proof contract: load `.rex/policy.json` from the source workspace
    // before anything else, and fail closed on a malformed contract.
    let source_workspace: Option<std::path::PathBuf> = match &opts.workspace {
        Some(w) => {
            let p = if w.is_absolute() {
                w.clone()
            } else {
                std::env::current_dir()
                    .map_err(|e| ExecError::internal(format!("cannot resolve cwd: {e}")))?
                    .join(w)
            };
            Some(p)
        }
        None => None,
    };
    let policy: Option<crate::policy::Policy> = match &source_workspace {
        Some(src) => {
            crate::policy::load(src).map_err(|e| ExecError::usage(format!("policy: {e}")))?
        }
        None => None,
    };
    if let Some(p) = &policy {
        // Provider allowlist can be checked before any budgets are built.
        if let Some(allowed) = &p.allowed_providers {
            if !allowed.iter().any(|a| a == &opts.provider) {
                return Err(ExecError::usage(format!(
                    "policy allows providers {allowed:?}; '{}' is not one of them",
                    opts.provider
                )));
            }
        }
    }

    if let Some(mins) = opts.deadman_mins {
        if mins == 0 {
            return Err(ExecError::usage("--deadman-mins must be at least 1"));
        }
    }

    // Skills marketplace: load (and re-verify) each requested skill
    // before the agent starts. A missing or tampered skill fails the run;
    // it never silently degrades to an unskilled run.
    let state_for_skills = state_dir();
    let mut loaded_skills = Vec::new();
    for name in &opts.skills {
        loaded_skills
            .push(crate::skill::load(&state_for_skills, name).map_err(ExecError::internal)?);
    }
    let effective_task = if loaded_skills.is_empty() {
        opts.task.clone()
    } else {
        format!(
            "{}\n\n{}",
            crate::skill::prompt_block(&loaded_skills),
            opts.task
        )
    };

    let mut budgets = Budgets::default();
    if let Some(s) = opts.max_steps {
        budgets.max_steps = s;
    }
    if let Some(c) = opts.max_tool_calls {
        budgets.max_tool_calls = c;
    }
    if let Some(t) = opts.max_tokens {
        budgets.max_tokens = t;
    }
    if let Some(secs) = opts.timeout_secs {
        budgets.max_wall_ms = secs.saturating_mul(1000);
    }

    // A dollar budget binds the token budget: the cap buys tokens at the
    // expensive (output) price, so the spend can never exceed it.
    if let Some(usd) = opts.budget_usd {
        if usd <= 0.0 {
            return Err(ExecError::usage("--budget-usd must be positive"));
        }
        let model = opts.model.as_deref().ok_or_else(|| {
            ExecError::usage("--budget-usd needs --model with a known price (see rex exec --bid)")
        })?;
        let tokens =
            crate::bid::tokens_for_budget(usd, &opts.provider, model).ok_or_else(|| {
                ExecError::usage(format!(
                    "--budget-usd: no price for model '{model}' (see rex exec --bid)"
                ))
            })?;
        if opts.max_tokens.is_some_and(|t| t != tokens) {
            eprintln!("rex: --budget-usd ${usd} overrides --max-tokens with {tokens} tokens");
        }
        budgets.max_tokens = tokens;
    }

    // The bid gate: print the binding bid and stop unless it is accepted.
    // Works offline — no key, no provider contact needed for a bid.
    let accepted_bid: Option<crate::bid::Bid> = if opts.bid {
        let bid = crate::bid::build_bid(
            &opts.provider,
            opts.model.as_deref(),
            budgets.max_steps,
            budgets.max_tool_calls,
            budgets.max_tokens,
            budgets.max_wall_ms,
        );
        if !opts.accept_bid {
            let out = serde_json::json!({
                "schema": "rex.exec.bid/1",
                "task": opts.task,
                "provider": opts.provider,
                "accepted": false,
                "bid": bid.to_json(),
            });
            return Ok(ExecOutput {
                code: 2,
                receipt: out,
                workspace: None,
            });
        }
        Some(bid)
    } else {
        None
    };

    // The rest of the contract: bid requirement and cost cap, enforced
    // against the accepted bid before the run starts.
    if let Some(p) = &policy {
        let ceiling = accepted_bid.as_ref().and_then(|b| b.max_cost_usd);
        p.check(&opts.provider, accepted_bid.is_some(), ceiling)
            .map_err(|e| ExecError::usage(format!("policy: {e}")))?;
    }

    if opts.dry_run {
        let config = serde_json::json!({
            "schema": "rex.exec.dry_run/1",
            "task": opts.task,
            "provider": opts.provider,
            "model": opts.model,
            "max_steps": budgets.max_steps,
            "max_tool_calls": budgets.max_tool_calls,
            "max_tokens": budgets.max_tokens,
            "max_wall_ms": budgets.max_wall_ms,
            "auto_approve": opts.yes,
            "workspace": opts.workspace.as_ref().map(|w| w.display().to_string()),
            "policy": policy.as_ref().map(|p| p.to_json()),
            "deadman_mins": opts.deadman_mins,
        });
        return Ok(ExecOutput {
            code: 0,
            receipt: config,
            workspace: None,
        });
    }

    let (agent, runs_root) = build_agent()?;

    // Stage the workspace copy under the runs root (the service requires it).
    let staged_workspace: Option<PathBuf> = match &opts.workspace {
        Some(src) => {
            let src = if src.is_absolute() {
                src.clone()
            } else {
                std::env::current_dir()
                    .map_err(|e| ExecError::internal(format!("cannot resolve cwd: {e}")))?
                    .join(src)
            };
            let suffix = opts.run_tag.as_deref().unwrap_or("");
            let dst = runs_root.join(format!("cli-workspace{suffix}"));
            if dst.exists() {
                std::fs::remove_dir_all(&dst)
                    .map_err(|e| ExecError::internal(format!("cannot clear staging dir: {e}")))?;
            }
            copy_workspace(&src, &dst)?;
            Some(dst)
        }
        None => None,
    };

    let snap = agent
        .begin_in_workspace(
            &effective_task,
            &opts.provider,
            opts.model.as_deref(),
            Some(budgets),
            staged_workspace.clone(),
        )
        .map_err(ExecError::internal)?;
    let run_id = snap.id.clone();
    if !opts.json {
        eprintln!("rex: run {run_id} started (provider {})", opts.provider);
    }

    // Dead-man custody: arm the switch before the first drive poll.
    let state_for_dm = state_dir();
    let mut deadman = match opts.deadman_mins {
        Some(mins) => Some(
            crate::deadman::Deadman::arm(&state_for_dm, &run_id, mins, opts.deadman_file.clone())
                .map_err(ExecError::internal)?,
        ),
        None => None,
    };
    if let Some(dm) = &deadman {
        if !opts.json {
            eprintln!(
                "rex: dead-man armed: check in at {} at least every {} min",
                dm.file().display(),
                opts.deadman_mins.unwrap_or(0),
            );
        }
    }

    let final_snap = drive(&agent, &opts, &run_id, &mut deadman)?;
    let completed = final_snap.status == AgentStatus::Completed;
    let mut receipt_map = receipt(
        &opts,
        &final_snap,
        staged_workspace.as_deref(),
        accepted_bid.as_ref(),
        policy.as_ref(),
        deadman.as_ref().map(|d| d.to_json()),
        &loaded_skills,
    );

    // Leapfrog bet 1: every receipt is signed. A failed signature must
    // never block the run's own output, so it degrades to a warning.
    let state = state_dir();
    match load_or_generate(&state).and_then(|key| sign_receipt(&key, &mut receipt_map)) {
        Ok(_) => {}
        Err(e) => eprintln!("rex: warning: receipt is unsigned: {e}"),
    }
    let out = Value::Object(receipt_map);
    let code = if completed { 0 } else { 3 };
    ledger::append(&state, &out);

    if opts.json {
        println!("{}", serde_json::to_string(&out).unwrap());
    } else {
        println!("run {}: {}", final_snap.id, status_line(&final_snap));
        if let Some(summary) = &final_snap.completion_summary {
            println!("{summary}");
        }
        if let Some(err) = &final_snap.error {
            eprintln!("error: {err}");
        }
        if let Some(ws) = staged_workspace.as_deref() {
            println!("workspace: {}", ws.display());
        }
    }

    Ok(ExecOutput {
        code,
        receipt: out,
        workspace: staged_workspace,
    })
}

/// A single headless run without any printing: the tournament driver.
pub struct ExecOutput {
    pub code: i32,
    pub receipt: Value,
    pub workspace: Option<PathBuf>,
}

pub fn run_exec(opts: ExecOptions) -> Result<i32, ExecError> {
    let dry = opts.dry_run;
    let bid_only = opts.bid && !opts.accept_bid;
    let out = execute(opts)?;
    // Dry runs always show their config: that is the whole point of them.
    // A bare --bid prints the binding bid the same way.
    if dry || bid_only {
        println!("{}", serde_json::to_string_pretty(&out.receipt).unwrap());
    }
    if bid_only {
        eprintln!("rex: bid printed; re-run with --accept-bid to execute under it.");
    }
    Ok(out.code)
}

fn status_line(snap: &AgentSnapshot) -> String {
    format!(
        "{:?} after {} steps, {} tool calls, {} tokens, {} ms",
        snap.status, snap.step, snap.tool_calls, snap.tokens_used, snap.elapsed_ms
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_name_uppercases_and_underscores() {
        assert_eq!(
            EnvSecretStore::<FileSecretStore>::env_name("anthropic"),
            "REX_ANTHROPIC_API_KEY"
        );
        assert_eq!(
            EnvSecretStore::<FileSecretStore>::env_name("kimi-coding"),
            "REX_KIMI_CODING_API_KEY"
        );
    }

    #[test]
    fn terminal_reason_serializes_with_kind_tag() {
        let v = terminal_reason_value(&Some(TerminalReason::BudgetSteps { max_steps: 10 }));
        assert_eq!(v["kind"], serde_json::json!("budget_steps"));
        assert_eq!(v["max_steps"], serde_json::json!(10));
        assert_eq!(terminal_reason_value(&None), serde_json::Value::Null);
        // Every variant must survive the receipt path, including the ones
        // added after the first draft of this CLI.
        for reason in [
            TerminalReason::Completed,
            TerminalReason::Cancelled,
            TerminalReason::Denied,
            TerminalReason::ApprovalTimeout,
            TerminalReason::ModelStalled,
            TerminalReason::ProviderError {
                detail: "boom".into(),
            },
            TerminalReason::GatesFailed {
                failures: vec!["f".into()],
            },
            TerminalReason::NoProgress { turns: 3 },
        ] {
            let v = terminal_reason_value(&Some(reason));
            assert!(v.get("kind").is_some(), "missing kind tag: {v}");
        }
    }

    #[test]
    fn copy_workspace_skips_vcs_and_build_dirs() {
        let base = std::env::temp_dir().join(format!("rex-cli-test-{}", std::process::id()));
        let src = base.join("src");
        let dst = base.join("dst");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(src.join(".git")).unwrap();
        std::fs::create_dir_all(src.join("target")).unwrap();
        std::fs::create_dir_all(src.join("node_modules")).unwrap();
        std::fs::write(src.join(".git").join("HEAD"), "ref").unwrap();
        std::fs::write(src.join("main.rs"), "fn main() {}").unwrap();
        copy_workspace(&src, &dst).unwrap();
        assert!(dst.join("main.rs").exists());
        assert!(!dst.join(".git").exists());
        assert!(!dst.join("target").exists());
        assert!(!dst.join("node_modules").exists());
        let _ = std::fs::remove_dir_all(&base);
    }
}
