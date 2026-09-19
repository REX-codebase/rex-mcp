//! The Ultra orchestrator: the phase machine that makes the mode real.
//!
//! Contracting -> Building -> Verifying -> Adversary -> Judging, with a
//! bounded repair loop. One shared disposable workspace serves the builder,
//! the adversary and the verifier, so every claim is checked against the same
//! bytes. Completion (Promoted) requires all three: fresh deterministic
//! verification, no standing adversary defect, and a clean-room judge pass.
//! Every phase transition and fact lands in the epistemic ledger and every
//! artifact in the evidence store; the proof bundle on disk is the deliverable.

use crate::adversary::{self, AdversaryReport};
use crate::contract::{self, AcceptanceContract};
use crate::evidence::EvidenceStore;
use crate::judge::{self, JudgeReport};
use crate::ledger::{EpistemicLedger, FactClass};
use crate::phase4::{
    self, CausalTrace, CheckpointState, EventKind, Phase4Bundle, Phase4Gate, ReplayBundle,
    WorkspaceSnapshot,
};
use crate::verify::{self, ObligationStatus, VerificationReport};
use rex_providers::autonomous::{AgentSnapshot, AutonomousRunService, Budgets};
use rex_providers::http::Transport;
use rex_providers::oneshot;
use rex_providers::secrets::SecretStore;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const CONTRACT_ATTEMPTS: u8 = 3;
const JUDGE_ATTEMPTS: u8 = 2;
const MAX_REPAIRS: u8 = 2;
const MAX_ACTIVE_RUNS: usize = 2;
const POLL_MS: u64 = 250;

/// Ultra budgets are materially larger than Simple's defaults, still bounded.
pub fn ultra_budgets() -> Budgets {
    Budgets {
        max_steps: 48,
        max_tool_calls: 160,
        max_wall_ms: 45 * 60 * 1000,
        max_tokens: 1_000_000,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UltraPhase {
    Contracting,
    Building,
    Verifying,
    Adversary,
    Judging,
    Repairing,
    Promoted,
    Rejected,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UltraTerminal {
    /// Every gate agreed, with the proof bundle on disk.
    Promoted,
    /// Verification, adversary or judge still disagree after the repair loop.
    Rejected {
        reasons: Vec<String>,
    },
    /// The builder run itself ended without completing.
    BuilderFailed {
        detail: String,
    },
    /// No valid contract after bounded drafting.
    ContractFailed {
        errors: Vec<String>,
    },
    ProviderError {
        detail: String,
    },
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
pub struct UltraSnapshot {
    pub id: String,
    pub task: String,
    pub phase: UltraPhase,
    pub provider: String,
    pub model: String,
    pub judge_model: String,
    pub adversary_model: String,
    pub repair: u8,
    pub max_repairs: u8,
    pub contract: Option<AcceptanceContract>,
    pub verification: Option<VerificationReport>,
    pub adversary: Option<AdversaryReport>,
    pub judge: Option<JudgeReport>,
    pub builder: Option<AgentSnapshot>,
    pub terminal: Option<UltraTerminal>,
    /// Run directory holding the proof bundle; the UI links it for audit.
    pub run_dir: String,
}

#[derive(Debug, Clone, Default)]
pub struct UltraOptions {
    /// Judge identity. Defaults to the worker model; the report then says
    /// same_model_as_worker = true instead of implying independent review.
    pub judge_provider: Option<String>,
    pub judge_model: Option<String>,
    pub adversary_provider: Option<String>,
    pub adversary_model: Option<String>,
    /// Disable the adversary world (benchmark ablation). Default on.
    pub adversary_enabled: Option<bool>,
}

struct Shared {
    phase: UltraPhase,
    contract: Option<AcceptanceContract>,
    verification: Option<VerificationReport>,
    adversary: Option<AdversaryReport>,
    judge: Option<JudgeReport>,
    builder: Option<AgentSnapshot>,
    builder_run_id: Option<String>,
    terminal: Option<UltraTerminal>,
    repair: u8,
    model: String,
    judge_model: String,
    adversary_model: String,
}

struct RunHandle {
    shared: Mutex<Shared>,
    cancel: AtomicBool,
}

pub struct UltraRunService<S: SecretStore + 'static, T: Transport + 'static> {
    inner: Arc<AutonomousRunService<S, T>>,
    runs: Arc<Mutex<HashMap<String, Arc<RunHandle>>>>,
    runs_root: PathBuf,
}

impl<S: SecretStore + 'static, T: Transport + 'static> UltraRunService<S, T> {
    /// Wrap the shared autonomous run service. Simple and Ultra share one
    /// provider core, one approval channel and one runs root; Ultra adds its
    /// own phases on top.
    pub fn new(inner: Arc<AutonomousRunService<S, T>>, runs_root: PathBuf) -> Self {
        Self {
            inner,
            runs: Arc::new(Mutex::new(HashMap::new())),
            runs_root,
        }
    }

    fn run_dir(&self, id: &str) -> PathBuf {
        self.runs_root.join(id)
    }

    pub fn begin(
        &self,
        task: &str,
        provider: &str,
        model: Option<&str>,
        options: UltraOptions,
    ) -> Result<UltraSnapshot, String> {
        let id = format!("ultra-{}-{:x}", std::process::id(), crate::now_ms());
        let run_dir = self.run_dir(&id);
        let workspace = run_dir.join("workspace");
        self.begin_in(&id, task, provider, model, options, run_dir, workspace)
    }

    /// Start one Ultra pipeline in a caller-owned isolated workspace. Phase 2
    /// uses this to give every candidate the same provider/model/options while
    /// keeping proof bundles and filesystem effects separate.
    pub fn begin_in(
        &self,
        id: &str,
        task: &str,
        provider: &str,
        model: Option<&str>,
        options: UltraOptions,
        run_dir: PathBuf,
        workspace: PathBuf,
    ) -> Result<UltraSnapshot, String> {
        let task = task.trim();
        if task.is_empty() {
            return Err("task is empty".into());
        }
        if task.chars().count() > 8_000 {
            return Err("task is too long".into());
        }
        if id.trim().is_empty() {
            return Err("run id is empty".into());
        }
        {
            let runs = self.runs.lock().map_err(|_| "run registry poisoned")?;
            if runs.contains_key(id) {
                return Err("run id already exists".into());
            }
            let active = runs
                .values()
                .filter(|h| {
                    h.shared
                        .lock()
                        .map(|s| s.terminal.is_none())
                        .unwrap_or(false)
                })
                .count();
            if active >= MAX_ACTIVE_RUNS {
                return Err("too many active Ultra runs; finish one first".into());
            }
        }
        fs::create_dir_all(&workspace).map_err(|e| e.to_string())?;
        fs::create_dir_all(run_dir.join("ultra")).map_err(|e| e.to_string())?;
        let id = id.to_string();

        let handle = Arc::new(RunHandle {
            shared: Mutex::new(Shared {
                phase: UltraPhase::Contracting,
                contract: None,
                verification: None,
                adversary: None,
                judge: None,
                builder: None,
                builder_run_id: None,
                terminal: None,
                repair: 0,
                model: model.unwrap_or_default().to_string(),
                judge_model: String::new(),
                adversary_model: String::new(),
            }),
            cancel: AtomicBool::new(false),
        });
        self.runs
            .lock()
            .map_err(|_| "run registry poisoned")?
            .insert(id.clone(), handle.clone());

        let ctx = DriveCtx {
            id: id.clone(),
            task: task.to_string(),
            provider: provider.to_string(),
            requested_model: model.map(str::to_string),
            options,
            run_dir,
            workspace,
            handle,
        };
        let inner = self.inner.clone();
        std::thread::spawn(move || drive(ctx, inner));
        self.snapshot(&id).ok_or_else(|| "run vanished".to_string())
    }

    pub fn snapshot(&self, run_id: &str) -> Option<UltraSnapshot> {
        let runs = self.runs.lock().ok()?;
        let handle = runs.get(run_id)?;
        let state = handle.shared.lock().ok()?;
        Some(UltraSnapshot {
            id: run_id.to_string(),
            task: String::new(),
            phase: state.phase.clone(),
            provider: String::new(),
            model: state.model.clone(),
            judge_model: state.judge_model.clone(),
            adversary_model: state.adversary_model.clone(),
            repair: state.repair,
            max_repairs: MAX_REPAIRS,
            contract: state.contract.clone(),
            verification: state.verification.clone(),
            adversary: state.adversary.clone(),
            judge: state.judge.clone(),
            builder: state.builder.clone(),
            terminal: state.terminal.clone(),
            run_dir: self.run_dir(run_id).to_string_lossy().to_string(),
        })
    }

    /// Trusted approval for the currently active builder/adversary run.
    pub fn decide(&self, run_id: &str, approved: bool) -> Result<UltraSnapshot, String> {
        let builder_id = {
            let runs = self.runs.lock().map_err(|_| "run registry poisoned")?;
            let handle = runs.get(run_id).ok_or_else(|| "unknown run".to_string())?;
            let state = handle.shared.lock().map_err(|_| "run state poisoned")?;
            state.builder_run_id.clone()
        };
        if let Some(builder_id) = builder_id {
            self.inner.decide(&builder_id, approved)?;
        }
        self.snapshot(run_id)
            .ok_or_else(|| "run vanished".to_string())
    }

    pub fn cancel(&self, run_id: &str) -> Result<UltraSnapshot, String> {
        let builder_id = {
            let runs = self.runs.lock().map_err(|_| "run registry poisoned")?;
            let handle = runs.get(run_id).ok_or_else(|| "unknown run".to_string())?;
            handle.cancel.store(true, Ordering::SeqCst);
            let state = handle.shared.lock().map_err(|_| "run state poisoned")?;
            state.builder_run_id.clone()
        };
        if let Some(builder_id) = builder_id {
            let _ = self.inner.cancel(&builder_id);
        }
        self.snapshot(run_id)
            .ok_or_else(|| "run vanished".to_string())
    }
}

struct RollbackGuard {
    workspace: PathBuf,
    snapshot: WorkspaceSnapshot,
    armed: bool,
}

impl RollbackGuard {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for RollbackGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.snapshot.restore(&self.workspace);
        }
    }
}

struct DriveCtx {
    id: String,
    task: String,
    provider: String,
    requested_model: Option<String>,
    options: UltraOptions,
    run_dir: PathBuf,
    workspace: PathBuf,
    handle: Arc<RunHandle>,
}

fn set_phase(handle: &RunHandle, phase: UltraPhase) {
    handle.shared.lock().expect("state poisoned").phase = phase;
}

fn cancelled(handle: &RunHandle) -> bool {
    handle.cancel.load(Ordering::SeqCst)
}

fn finish(handle: &RunHandle, terminal: UltraTerminal) {
    let mut state = handle.shared.lock().expect("state poisoned");
    state.phase = match &terminal {
        UltraTerminal::Promoted => UltraPhase::Promoted,
        UltraTerminal::Rejected { .. } => UltraPhase::Rejected,
        UltraTerminal::Cancelled => UltraPhase::Cancelled,
        _ => UltraPhase::Failed,
    };
    state.terminal = Some(terminal);
}

fn sha256_seed(text: &str) -> u64 {
    let hex = crate::evidence::sha256_hex(text.as_bytes());
    u64::from_str_radix(&hex[..16], 16).unwrap_or(0)
}

fn write_json(path: &PathBuf, value: &impl Serialize) {
    if let Ok(text) = serde_json::to_string_pretty(value) {
        let _ = fs::write(path, text);
    }
}

/// Poll a builder/adversary sub-run to its terminal state, mirroring the
/// snapshot into the Ultra run for the UI.
fn await_subrun<S: SecretStore + 'static, T: Transport + 'static>(
    inner: &Arc<AutonomousRunService<S, T>>,
    handle: &Arc<RunHandle>,
    run_id: &str,
) -> AgentSnapshot {
    loop {
        if cancelled(handle) {
            let _ = inner.cancel(run_id);
        }
        match inner.snapshot(run_id) {
            Some(snap) => {
                let terminal = snap.terminal_reason.is_some();
                handle.shared.lock().expect("state poisoned").builder = Some(snap.clone());
                if terminal {
                    return snap;
                }
            }
            None => {
                // Run not visible yet (just registered) - keep polling.
            }
        }
        std::thread::sleep(Duration::from_millis(POLL_MS));
    }
}

fn drive<S: SecretStore + 'static, T: Transport + 'static>(
    ctx: DriveCtx,
    inner: Arc<AutonomousRunService<S, T>>,
) {
    let pre_run = match WorkspaceSnapshot::capture(&ctx.workspace) {
        Ok(snapshot) => snapshot,
        Err(e) => {
            finish(
                &ctx.handle,
                UltraTerminal::ProviderError {
                    detail: format!("pre-run snapshot failed: {e}"),
                },
            );
            return;
        }
    };
    let mut rollback = RollbackGuard {
        workspace: ctx.workspace.clone(),
        snapshot: pre_run.clone(),
        armed: true,
    };
    let ultra_dir = ctx.run_dir.join("ultra");
    let mut ledger = match EpistemicLedger::open(&ultra_dir) {
        Ok(l) => l,
        Err(e) => {
            finish(
                &ctx.handle,
                UltraTerminal::ProviderError {
                    detail: format!("ledger failed: {e}"),
                },
            );
            return;
        }
    };
    let mut evidence = match EvidenceStore::open(&ultra_dir.join("evidence")) {
        Ok(e) => e,
        Err(e) => {
            finish(
                &ctx.handle,
                UltraTerminal::ProviderError {
                    detail: format!("evidence store failed: {e}"),
                },
            );
            return;
        }
    };
    ledger.record(
        "ultra run opened; builder, adversary, verifier and judge share one disposable workspace",
        FactClass::Observed {
            evidence_id: "run-dir".into(),
        },
    );

    // Phase 1: contract. The worker model drafts; deterministic validation
    // accepts or explains. Up to CONTRACT_ATTEMPTS rounds.
    set_phase(&ctx.handle, UltraPhase::Contracting);
    let worker_model = match oneshot::resolve_model(
        inner.service(),
        &ctx.provider,
        ctx.requested_model.as_deref(),
    ) {
        Ok(m) => m,
        Err(e) => {
            finish(&ctx.handle, UltraTerminal::ProviderError { detail: e });
            return;
        }
    };
    ctx.handle.shared.lock().expect("state poisoned").model = worker_model.clone();
    ledger.record(
        &format!("worker model resolved to {worker_model}"),
        FactClass::Observed {
            evidence_id: "provider-catalog".into(),
        },
    );

    let mut errors: Vec<String> = Vec::new();
    let mut contract: Option<AcceptanceContract> = None;
    for _ in 0..CONTRACT_ATTEMPTS {
        if cancelled(&ctx.handle) {
            finish(&ctx.handle, UltraTerminal::Cancelled);
            return;
        }
        let mut prompt = contract::drafting_prompt(&ctx.task);
        if !errors.is_empty() {
            prompt.push_str(&format!(
                "\n\nYour previous draft was rejected. Fix every error:\n- {}",
                errors.join("\n- ")
            ));
        }
        let draft = match oneshot::complete_text(
            inner.service(),
            &ctx.provider,
            &worker_model,
            "You compile tasks into acceptance contracts. Answer with JSON only.",
            &prompt,
        ) {
            Ok(out) => out.text,
            Err(e) => {
                errors = vec![format!("provider error: {e}")];
                continue;
            }
        };
        let entry = evidence.put_bytes("contract_draft", draft.as_bytes());
        match contract::parse_contract(&draft, &ctx.task) {
            Ok(c) => {
                let fact = ledger.record(
                    &format!(
                        "acceptance contract compiled with {} obligations",
                        c.obligations.len()
                    ),
                    FactClass::Observed {
                        evidence_id: entry.id.clone(),
                    },
                );
                let _ = fact;
                contract = Some(c);
                break;
            }
            Err(errs) => errors = errs,
        }
    }
    let Some(acceptance) = contract else {
        finish(&ctx.handle, UltraTerminal::ContractFailed { errors });
        return;
    };
    write_json(&ultra_dir.join("contract.json"), &acceptance);
    ctx.handle.shared.lock().expect("state poisoned").contract = Some(acceptance.clone());

    let judge_provider = ctx
        .options
        .judge_provider
        .clone()
        .unwrap_or_else(|| ctx.provider.clone());
    let judge_model = ctx
        .options
        .judge_model
        .clone()
        .unwrap_or_else(|| worker_model.clone());
    let adversary_provider = ctx
        .options
        .adversary_provider
        .clone()
        .unwrap_or_else(|| ctx.provider.clone());
    let adversary_model = ctx
        .options
        .adversary_model
        .clone()
        .unwrap_or_else(|| worker_model.clone());
    {
        let mut state = ctx.handle.shared.lock().expect("state poisoned");
        state.judge_model = format!("{judge_provider}/{judge_model}");
        state.adversary_model = format!("{adversary_provider}/{adversary_model}");
    }
    let adversary_enabled = ctx.options.adversary_enabled.unwrap_or(true);

    // Build -> verify -> adversary -> judge, with bounded repairs.
    let mut repair_digest = String::new();
    loop {
        let phase3_seed = sha256_seed(&format!("{}|{}|{}", ctx.task, ctx.provider, worker_model));
        let phase3_old_trace = crate::phase3::capture_trace(
            &ctx.provider,
            &worker_model,
            phase3_seed,
            &ctx.workspace,
            None,
        );
        // Building.
        set_phase(&ctx.handle, UltraPhase::Building);
        let mut brief = format!(
            "{}\n\nULTRA ACCEPTANCE CONTRACT - your completion claim is re-verified against this by \
a deterministic verifier, an adversary and a clean-room judge:\n",
            ctx.task
        );
        for ob in &acceptance.obligations {
            brief.push_str(&format!("- {}: {}\n", ob.id, ob.statement));
        }
        if !repair_digest.is_empty() {
            brief.push_str(&format!(
                "\nPREVIOUS ATTEMPT FAILED THESE GATES - repair them:\n{repair_digest}"
            ));
        }
        let builder_run = match inner.begin_in_workspace(
            &brief,
            &ctx.provider,
            Some(&worker_model),
            Some(ultra_budgets()),
            Some(ctx.workspace.clone()),
        ) {
            Ok(snap) => snap,
            Err(e) => {
                finish(&ctx.handle, UltraTerminal::ProviderError { detail: e });
                return;
            }
        };
        ctx.handle
            .shared
            .lock()
            .expect("state poisoned")
            .builder_run_id = Some(builder_run.id.clone());
        let built = await_subrun(&inner, &ctx.handle, &builder_run.id);
        if cancelled(&ctx.handle) {
            finish(&ctx.handle, UltraTerminal::Cancelled);
            return;
        }
        match built.terminal_reason {
            Some(rex_providers::autonomous::TerminalReason::Completed) => {
                ledger.record(
                    "builder declared completion; claim untrusted until verification",
                    FactClass::Guess,
                );
            }
            other => {
                let detail = format!("builder ended without completing: {other:?}");
                let mut state = ctx.handle.shared.lock().expect("state poisoned");
                if state.repair < MAX_REPAIRS {
                    state.repair += 1;
                    drop(state);
                    set_phase(&ctx.handle, UltraPhase::Repairing);
                    repair_digest = format!("- {detail}\n");
                    continue;
                }
                drop(state);
                finish(&ctx.handle, UltraTerminal::BuilderFailed { detail });
                return;
            }
        }

        // Verifying: fresh deterministic re-execution of every executable proof.
        set_phase(&ctx.handle, UltraPhase::Verifying);
        let artifacts = evidence.snapshot_workspace(&ctx.workspace);
        let report = verify::verify_contract(&acceptance, &ctx.workspace, &mut evidence);
        let phase3_new_trace = crate::phase3::capture_trace(
            &ctx.provider,
            &worker_model,
            phase3_seed,
            &ctx.workspace,
            Some(&report),
        );
        write_json(&ultra_dir.join("verification.json"), &report);
        for outcome in &report.outcomes {
            ledger.record(
                &format!(
                    "obligation {} verified fresh: {:?} - {}",
                    outcome.obligation_id, outcome.status, outcome.detail
                ),
                FactClass::Observed {
                    evidence_id: outcome
                        .evidence_ids
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "verification".into()),
                },
            );
        }
        ledger.record(
            &format!(
                "workspace artifact manifest hashed: {} files",
                artifacts.len()
            ),
            FactClass::Observed {
                evidence_id: "evidence-manifest".into(),
            },
        );
        ctx.handle
            .shared
            .lock()
            .expect("state poisoned")
            .verification = Some(report.clone());

        // When fresh verification already failed, repair immediately; the
        // adversary and judge worlds only run on a verified-clean workspace.
        let mut adversary_report: Option<AdversaryReport> = None;
        let mut judge_result: Option<JudgeReport> = None;
        if report.executable_all_proven {
            // Adversary world: a separate run with one job - prove it wrong.
            let mut adversary_report_inner =
                AdversaryReport::clean(&format!("{adversary_provider}/{adversary_model}"));
            if adversary_enabled {
                set_phase(&ctx.handle, UltraPhase::Adversary);
                let brief = adversary::adversary_brief(&ctx.task, &acceptance, &report);
                match inner.begin_in_workspace(
                    &brief,
                    &adversary_provider,
                    Some(&adversary_model),
                    Some(ultra_budgets()),
                    Some(ctx.workspace.clone()),
                ) {
                    Ok(run) => {
                        ctx.handle
                            .shared
                            .lock()
                            .expect("state poisoned")
                            .builder_run_id = Some(run.id.clone());
                        let attacked = await_subrun(&inner, &ctx.handle, &run.id);
                        if cancelled(&ctx.handle) {
                            finish(&ctx.handle, UltraTerminal::Cancelled);
                            return;
                        }
                        let summary = attacked.completion_summary.unwrap_or_default();
                        adversary_report_inner = parse_and_record(
                            &summary,
                            &format!("{adversary_provider}/{adversary_model}"),
                            &mut ledger,
                        );
                    }
                    Err(e) => {
                        adversary_report_inner = AdversaryReport {
                            defects: Vec::new(),
                            inconclusive: true,
                            adversary_model: format!(
                                "{adversary_provider}/{adversary_model} (failed to start: {e})"
                            ),
                        };
                    }
                }
                write_json(&ultra_dir.join("adversary.json"), &adversary_report_inner);
                ctx.handle.shared.lock().expect("state poisoned").adversary =
                    Some(adversary_report_inner.clone());
            }
            adversary_report = Some(adversary_report_inner);

            // Judging: the clean-room pass over contract + evidence, never the
            // builder's narrative.
            set_phase(&ctx.handle, UltraPhase::Judging);
            let manifest = evidence.manifest_text();
            let adversary_for_judge = adversary_report
                .clone()
                .unwrap_or_else(|| AdversaryReport::clean("not-run"));
            let prompt = judge::judge_prompt(
                &ctx.task,
                &acceptance,
                &report,
                &adversary_for_judge,
                &manifest,
            );
            let mut judge_report: Option<JudgeReport> = None;
            let mut judge_error = String::new();
            for _ in 0..JUDGE_ATTEMPTS {
                if cancelled(&ctx.handle) {
                    finish(&ctx.handle, UltraTerminal::Cancelled);
                    return;
                }
                let mut p = prompt.clone();
                if !judge_error.is_empty() {
                    p.push_str(&format!(
                    "\n\nYour previous answer was rejected: {judge_error}. Answer with the JSON object only."
                ));
                }
                match oneshot::complete_text(
                    inner.service(),
                    &judge_provider,
                    &judge_model,
                    "You are a hostile clean-room judge. Answer with JSON only.",
                    &p,
                ) {
                    Ok(out) => {
                        let entry = evidence.put_bytes("judge_answer", out.text.as_bytes());
                        let _ = entry;
                        match judge::parse_verdicts(
                            &out.text,
                            &acceptance,
                            &report,
                            &format!("{judge_provider}/{judge_model}"),
                            judge_provider == ctx.provider && judge_model == worker_model,
                        ) {
                            Ok(r) => {
                                judge_report = Some(r);
                                break;
                            }
                            Err(e) => judge_error = e,
                        }
                    }
                    Err(e) => {
                        finish(&ctx.handle, UltraTerminal::ProviderError { detail: e });
                        return;
                    }
                }
            }
            let Some(judge_found) = judge_report else {
                finish(
                    &ctx.handle,
                    UltraTerminal::Rejected {
                        reasons: vec![format!("judge answer unusable: {judge_error}")],
                    },
                );
                return;
            };
            judge_result = Some(judge_found);
            let judge_ref = judge_result.clone().expect("judge");
            write_json(&ultra_dir.join("verdicts.json"), &judge_ref);
            for v in &judge_ref.verdicts {
                ledger.record(
                    &format!("judge {:?} on {}: {}", v.verdict, v.obligation_id, v.reason),
                    FactClass::Inferred {
                        from: v.evidence.clone(),
                    },
                );
            }
            ctx.handle.shared.lock().expect("state poisoned").judge = judge_result.clone();
        } // end if executable_all_proven

        // Promotion decision: every world that ran must agree.
        let mut reasons: Vec<String> = Vec::new();
        for outcome in &report.outcomes {
            if outcome.status == ObligationStatus::Failed {
                reasons.push(format!(
                    "verification failed for {}: {}",
                    outcome.obligation_id, outcome.detail
                ));
            }
        }
        if let Some(adversary_report) = &adversary_report {
            for d in &adversary_report.defects {
                reasons.push(format!("adversary defect: {} - {}", d.title, d.detail));
            }
            if adversary_report.inconclusive {
                reasons.push("adversary pass was inconclusive".into());
            }
        }
        if let Some(judge_ref) = &judge_result {
            for v in &judge_ref.verdicts {
                if v.verdict != judge::VerdictKind::Pass {
                    reasons.push(format!(
                        "judge {:?} on {}: {}",
                        v.verdict, v.obligation_id, v.reason
                    ));
                }
            }
        }

        if reasons.is_empty() {
            // Phase 3 is entirely deterministic and fail closed. The old trace is the
            // acceptance baseline (pre-build verification); the new trace is the fresh
            // post-build verification. Provider/model/seed remain fixed.
            let manifest = crate::phase3::generate_manifest(
                &acceptance,
                &ctx.provider,
                &worker_model,
                phase3_seed,
            );
            write_json(&ultra_dir.join("phase3-manifest.json"), &manifest);
            let new_trace = phase3_new_trace.clone();
            let allowed = crate::phase3::contract_allowed_differences(&acceptance);
            let differential =
                crate::phase3::compare_traces(&phase3_old_trace, &new_trace, &allowed, &acceptance);
            let mutations = crate::phase3::execute_mutations(
                &acceptance,
                &ctx.workspace,
                &ultra_dir.join("phase3"),
                &manifest,
                &ctx.handle.cancel,
                None,
            );
            let gate =
                crate::phase3::gate(&mutations, &differential, manifest.counterexamples.len());
            let phase3_bundle = crate::phase3::Phase3Bundle {
                manifest,
                mutations,
                differential,
                gate,
            };
            let _ =
                crate::phase3::seal_bundle(&ultra_dir, &phase3_bundle, &mut ledger, &mut evidence);
            if !phase3_bundle.gate.promotable {
                #[cfg(test)]
                eprintln!(
                    "phase3 gate reasons: {:?}; differential={:?}; mutations={:?}",
                    phase3_bundle.gate.reasons, phase3_bundle.differential, phase3_bundle.mutations
                );
                reasons.extend(phase3_bundle.gate.reasons.clone());
            }
        }

        if reasons.is_empty() {
            let mut trace = CausalTrace::new(&ctx.id);
            let contract_event = trace.append(
                EventKind::PhaseDecision,
                vec![],
                serde_json::json!({"phase":"contracting","contract":acceptance}),
            );
            let model_event = contract_event.and_then(|parent| trace.append(EventKind::ModelDecision, vec![parent], serde_json::json!({"provider":ctx.provider,"model":worker_model,"judge":judge_model,"adversary":adversary_model})));
            let builder_event = model_event.and_then(|parent| {
                trace.append(
                    EventKind::ToolResult,
                    vec![parent],
                    serde_json::to_value(&built).unwrap_or_default(),
                )
            });
            let verifier_event = builder_event.and_then(|parent| {
                trace.append(
                    EventKind::VerifierEvidence,
                    vec![parent],
                    serde_json::to_value(&report).unwrap_or_default(),
                )
            });
            let completion_event = verifier_event.and_then(|parent| trace.append(EventKind::CompletionDecision, vec![parent], serde_json::json!({"decision":"promote","repairs":ctx.handle.shared.lock().map(|s|s.repair).unwrap_or(MAX_REPAIRS)})));
            if let Err(e) = completion_event {
                reasons.push(format!("phase 4 causal trace failed: {e}"));
            }

            if reasons.is_empty() {
                let replay = ReplayBundle::build(
                    &trace,
                    &ctx.provider,
                    &worker_model,
                    phase3_seed,
                    vec![
                        ("task".into(), serde_json::json!({"task":ctx.task})),
                        (
                            "contract".into(),
                            serde_json::to_value(&acceptance).unwrap_or_default(),
                        ),
                    ],
                    phase3_new_trace
                        .observables
                        .iter()
                        .map(|o| {
                            (
                                format!("{:?}:{}", o.kind, o.key),
                                format!("{}:{}", o.sha256, o.outcome),
                            )
                        })
                        .collect(),
                );
                let checkpoint_path = ultra_dir.join("phase4-checkpoint.json");
                let mut checkpoint = CheckpointState {
                    phase: "completion".into(),
                    cursor: trace.events.len() as u64,
                    effects: Vec::new(),
                    state: serde_json::json!({"run_id":ctx.id}),
                };
                let mut effects_exactly_once = true;
                for event in &trace.events {
                    if phase4::apply_effect_once(
                        &mut checkpoint,
                        &event.id,
                        event.payload_sha256.as_bytes(),
                        || Ok(event.id.clone()),
                    )
                    .is_err()
                    {
                        effects_exactly_once = false;
                        break;
                    }
                }
                if phase4::write_checkpoint(&checkpoint_path, 1, &checkpoint).is_err() {
                    effects_exactly_once = false;
                }
                let checkpoint_recovered = phase4::read_checkpoint(&checkpoint_path)
                    .map(|(_, recovered)| recovered == checkpoint)
                    .unwrap_or(false);
                if let Some(first) = trace.events.first() {
                    let duplicate_suppressed = phase4::apply_effect_once(
                        &mut checkpoint,
                        &first.id,
                        first.payload_sha256.as_bytes(),
                        || Ok(first.id.clone()),
                    )
                    .is_err();
                    effects_exactly_once &= duplicate_suppressed;
                }
                let rehearsal = ultra_dir.join("rollback-rehearsal");
                let rollback_verified = fs::create_dir_all(&rehearsal).is_ok()
                    && fs::write(rehearsal.join("mutation"), b"phase4").is_ok()
                    && pre_run.restore(&rehearsal).is_ok()
                    && WorkspaceSnapshot::capture(&rehearsal)
                        .map(|s| s.tree_sha256 == pre_run.tree_sha256)
                        .unwrap_or(false);
                let cleanup_complete =
                    fs::remove_dir_all(&rehearsal).is_ok() || !rehearsal.exists();
                let mut phase4_bundle = Phase4Bundle {
                    trace,
                    replay,
                    checkpoint_recovered,
                    effects_exactly_once,
                    rollback_verified,
                    bisect: None,
                    cleanup_complete,
                    gate: Phase4Gate {
                        promotable: false,
                        reasons: vec![],
                    },
                };
                if let Err(e) =
                    phase4::seal_bundle(&ultra_dir, &mut phase4_bundle, &mut ledger, &mut evidence)
                {
                    reasons.push(format!("phase 4 seal failed: {e}"));
                } else if !phase4_bundle.gate.promotable {
                    reasons.extend(phase4_bundle.gate.reasons);
                }
            }
        }

        if reasons.is_empty() {
            // Phase 5 binds the whole institution to a fresh repository model and
            // independent reconstruction. It sees no builder narrative.
            let phase5_result = (|| -> Result<(), String> {
                use crate::phase5;
                use std::collections::BTreeSet;
                let twin = phase5::build_twin(&ctx.workspace)?;
                let index = phase5::build_index(&ctx.workspace, &twin)?;
                let audit = phase5::audit(&ctx.workspace, &twin)?;
                if audit.findings.iter().any(|f| f.severity == "critical") {
                    return Err(format!(
                        "critical static/security findings: {:?}",
                        audit.findings
                    ));
                }
                let source_bytes = twin.files.iter().map(|f| f.bytes).sum::<u64>();
                let profiles = phase5::profile(
                    &twin,
                    vec![
                        phase5::ProfileSample {
                            name: "repository_bytes".into(),
                            value: source_bytes,
                            unit: "bytes".into(),
                            budget: 512 * 1024 * 1024,
                            command: "digital-twin deterministic byte census".into(),
                        },
                        phase5::ProfileSample {
                            name: "repository_files".into(),
                            value: twin.files.len() as u64,
                            unit: "files".into(),
                            budget: 20_000,
                            command: "digital-twin deterministic file census".into(),
                        },
                        phase5::ProfileSample {
                            name: "semantic_symbols".into(),
                            value: index.symbols.len() as u64,
                            unit: "symbols".into(),
                            budget: 100_000,
                            command: "semantic-index bounded parse".into(),
                        },
                    ],
                );
                let synth_input = twin
                    .files
                    .iter()
                    .find(|f| f.path == "Cargo.toml")
                    .or_else(|| twin.files.first())
                    .ok_or_else(|| "empty repository".to_string())?
                    .path
                    .clone();
                let synthesis = phase5::synthesize_use_discard(
                    &phase5::SynthToolSpec {
                        name: "phase5-content-hasher".into(),
                        capability: phase5::SynthCapability::HashFile,
                        inputs: vec![synth_input],
                        allow_network: false,
                        allow_process_spawn: false,
                        allowed_root: ctx
                            .workspace
                            .canonicalize()
                            .map_err(|e| e.to_string())?
                            .display()
                            .to_string(),
                    },
                    &ctx.workspace,
                )?;
                let manifest = evidence.manifest_text();
                let known_evidence = manifest
                    .lines()
                    .filter_map(|line| {
                        serde_json::from_str::<crate::evidence::EvidenceEntry>(line)
                            .ok()
                            .map(|e| e.id)
                    })
                    .collect::<BTreeSet<_>>();
                let worker_route = phase5::ModelRoute {
                    provider: ctx.provider.clone(),
                    model: worker_model.clone(),
                    explicitly_configured: true,
                    safe_provider: true,
                };
                let configured_route = match (&ctx.options.judge_provider, &ctx.options.judge_model)
                {
                    (Some(p), Some(m)) => Some(phase5::ModelRoute {
                        provider: p.clone(),
                        model: m.clone(),
                        explicitly_configured: true,
                        safe_provider: true,
                    }),
                    _ => None,
                };
                let critic_policy = phase5::choose_critic(&worker_route, configured_route.as_ref());
                let (recon_provider, recon_model) = if critic_policy.same_model {
                    (&ctx.provider, &worker_model)
                } else {
                    let r = configured_route
                        .as_ref()
                        .ok_or_else(|| "cross-model route disappeared".to_string())?;
                    (&r.provider, &r.model)
                };
                let prompt =
                    phase5::reconstruction_prompt(&ctx.task, &twin, &acceptance, &manifest);
                let answer = oneshot::complete_text(inner.service(), recon_provider, recon_model,
                    "You are a fresh reconstruction judge. JSON only. You have no access to builder reasoning.", &prompt)?;
                let answer_ev = evidence.put_bytes("reconstruction_answer", answer.text.as_bytes());
                let mut reconstruction_evidence = known_evidence.clone();
                reconstruction_evidence.insert(answer_ev.id);
                let reconstruction = phase5::parse_reconstruction(
                    &answer.text,
                    &format!("{recon_provider}/{recon_model}"),
                    critic_policy.same_model,
                    &acceptance,
                    &reconstruction_evidence,
                )?;
                let worlds = phase5::WorldLinks {
                    builder: "ultra/evidence/manifest.jsonl".into(),
                    adversary: "ultra/adversary.json".into(),
                    shadow: "ultra/phase3.json".into(),
                    recovery: "ultra/phase4.json".into(),
                    clean_room: "ultra/reconstruction.json".into(),
                    phase3: "ultra/phase3.json".into(),
                    phase4: "ultra/phase4.json".into(),
                };
                let mut proof_evidence = reconstruction_evidence;
                if let Some(j) = &judge_result {
                    for v in &j.verdicts {
                        for id in &v.evidence {
                            proof_evidence.insert(id.clone());
                        }
                    }
                }
                let mut linked_report = report.clone();
                if let Some(j) = &judge_result {
                    for outcome in &mut linked_report.outcomes {
                        if outcome.evidence_ids.is_empty() {
                            if let Some(v) = j
                                .verdicts
                                .iter()
                                .find(|v| v.obligation_id == outcome.obligation_id)
                            {
                                outcome.evidence_ids = v.evidence.clone();
                            }
                        }
                    }
                }
                let gate = phase5::promotion_gate(
                    &acceptance,
                    &linked_report,
                    &reconstruction,
                    &worlds,
                    &proof_evidence,
                );
                let bundle = phase5::Phase5Bundle {
                    twin,
                    index,
                    audit,
                    profiles,
                    visual: None,
                    synthesis: Some(synthesis),
                    reconstruction,
                    critic_policy,
                    worlds,
                    gate,
                };
                let _ = phase5::seal_bundle(
                    &ctx.workspace,
                    &ultra_dir,
                    &bundle,
                    &mut ledger,
                    &mut evidence,
                )?;
                Ok(())
            })();
            if let Err(e) = phase5_result {
                reasons.push(format!("phase 5 failed: {e}"));
            }
        }

        if reasons.is_empty() {
            let bundle = serde_json::json!({
                "task": ctx.task,
                "worker_model": format!("{}/{}", ctx.provider, worker_model),
                "contract": "ultra/contract.json",
                "verification": "ultra/verification.json",
                "adversary": "ultra/adversary.json",
                "verdicts": "ultra/verdicts.json",
                "ledger": "ultra/ledger.jsonl",
                "evidence": "ultra/evidence/manifest.jsonl",
                "phase3": "ultra/phase3.json",
                "phase4": "ultra/phase4.json",
                "phase5": "ultra/phase5.json",
            });
            let entry = evidence.put_bytes("proof_bundle", bundle.to_string().as_bytes());
            ledger.record(
                "PROMOTED: builder, verifier, adversary and judge all agree; proof bundle sealed",
                FactClass::Observed {
                    evidence_id: entry.id,
                },
            );
            rollback.disarm();
            finish(&ctx.handle, UltraTerminal::Promoted);
            return;
        }

        let mut state = ctx.handle.shared.lock().expect("state poisoned");
        if state.repair < MAX_REPAIRS {
            state.repair += 1;
            let n = state.repair;
            drop(state);
            set_phase(&ctx.handle, UltraPhase::Repairing);
            repair_digest = reasons.iter().map(|r| format!("- {r}\n")).collect();
            ledger.record(
                &format!(
                    "repair pass {n} started on {} gate failure(s)",
                    reasons.len()
                ),
                FactClass::Observed {
                    evidence_id: "gate-failures".into(),
                },
            );
            continue;
        }
        drop(state);
        ledger.record(
            "REJECTED: repair budget exhausted with standing gate failures",
            FactClass::Observed {
                evidence_id: "gate-failures".into(),
            },
        );
        finish(&ctx.handle, UltraTerminal::Rejected { reasons });
        return;
    }
}

fn parse_and_record(summary: &str, model: &str, ledger: &mut EpistemicLedger) -> AdversaryReport {
    let report = adversary::parse_defects(summary, model);
    if report.inconclusive {
        ledger.record(
            "adversary answer was unparseable; treated as inconclusive, not clean",
            FactClass::Guess,
        );
    } else if report.defects.is_empty() {
        ledger.record(
            "adversary found no standing defect",
            FactClass::Inferred { from: vec![] },
        );
    } else {
        for d in &report.defects {
            ledger.record(
                &format!("adversary defect: {} - {}", d.title, d.detail),
                FactClass::Guess,
            );
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use rex_providers::error::ProviderError;
    use rex_providers::secrets::MemorySecretStore;
    use rex_providers::service::ProviderService;
    use serde_json::{json, Value};
    use std::collections::VecDeque;
    use std::path::Path;
    use std::time::Instant;

    struct Script {
        turns: Mutex<VecDeque<String>>,
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
            _body: &str,
        ) -> Result<(u16, String), ProviderError> {
            let next = self.turns.lock().unwrap().pop_front();
            Ok((
                200,
                next.unwrap_or_else(|| text_turn("nothing more to say")),
            ))
        }
    }

    fn text_turn(text: &str) -> String {
        json!({"candidates":[{"content":{"parts":[{"text": text}]},"finishReason":"STOP"}],"usageMetadata":{"totalTokenCount":120}}).to_string()
    }

    fn call_turn(calls: Vec<Value>) -> String {
        json!({"candidates":[{"content":{"parts": calls},"finishReason":"STOP"}],"usageMetadata":{"totalTokenCount":240}}).to_string()
    }

    fn plan_call(items: Vec<(&str, &str, &str)>) -> Value {
        json!({"functionCall":{"name":"update_plan","args":{"items": items.iter().map(|(id, title, status)| json!({"id": id, "title": title, "status": status})).collect::<Vec<_>>()}}})
    }

    fn create_call(path: &str, content: &str) -> Value {
        json!({"functionCall":{"name":"create_file","args":{"path": path, "content": content, "overwrite": true}}})
    }

    fn complete_call(summary: &str) -> Value {
        json!({"functionCall":{"name":"complete_task","args":{"summary": summary}}})
    }

    type Ultra = UltraRunService<MemorySecretStore, Script>;
    type Inner = AutonomousRunService<MemorySecretStore, Script>;

    fn ultra(root: &Path, script: Script) -> Arc<Ultra> {
        let store = MemorySecretStore::new();
        store.set_key("gemini", "test-key").unwrap();
        let service = ProviderService::new(store, script);
        let inner = Arc::new(Inner::new(service, None, root.join("runs")));
        Arc::new(Ultra::new(inner, root.join("runs")))
    }

    fn wait_ultra(svc: &Arc<Ultra>, id: &str, timeout_ms: u64) -> UltraSnapshot {
        let start = Instant::now();
        loop {
            let snap = svc.snapshot(id).expect("snapshot");
            if let Some(builder) = &snap.builder {
                if builder.pending_approval.is_some() && snap.terminal.is_none() {
                    let _ = svc.decide(id, true);
                }
            }
            if snap.terminal.is_some() {
                return snap;
            }
            if start.elapsed().as_millis() as u64 > timeout_ms {
                panic!("ultra run stuck in {:?}", snap.phase);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    const CONTRACT: &str = r#"{"obligations":[{"id":"page","statement":"index.html exists with the tea house heading","proof":{"kind":"file_contains","path":"index.html","needle":"Tea House"}}],"forbidden_regressions":[]}"#;
    const PAGE: &str = "<!doctype html><html><head><style>body{margin:0}</style></head><body><header><h1>Tea House</h1></header></body></html>";

    #[test]
    fn full_pipeline_promotes_with_proof_bundle() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = ultra(
            tmp.path(),
            Script {
                turns: Mutex::new(VecDeque::from(vec![
                    // Contract draft.
                    text_turn(CONTRACT),
                    // Builder: plan + create, then plan-done + complete.
                    call_turn(vec![
                        plan_call(vec![("1", "build page", "in_progress")]),
                        create_call("index.html", PAGE),
                    ]),
                    call_turn(vec![
                        plan_call(vec![("1", "build page", "done")]),
                        complete_call("page built"),
                    ]),
                    // Adversary: read-only inspection + clean report.
                    call_turn(vec![
                        plan_call(vec![("1", "attack", "in_progress")]),
                        create_call("adversary-notes.md", "inspected the page"),
                    ]),
                    call_turn(vec![
                        plan_call(vec![("1", "attack", "done")]),
                        complete_call("{\"defects\":[]}"),
                    ]),
                    // Judge: pass.
                    text_turn(
                        r#"{"verdicts":[{"obligation_id":"page","verdict":"pass","reason":"fresh verification passed","evidence":[]}]}"#,
                    ),
                    // Fresh reconstruction sees only task, final twin and evidence manifest.
                    text_turn(
                        r#"{"reconstructed":true,"obligation_ids":["page"],"evidence_ids":["ev-0001"],"explanation":"The final repository twin and content-addressed evidence cover the requested page."}"#,
                    ),
                ])),
            },
        );
        let snap = svc
            .begin(
                "build a tea house page",
                "gemini",
                None,
                UltraOptions::default(),
            )
            .unwrap();
        let done = wait_ultra(&svc, &snap.id, 180_000);
        assert_eq!(
            done.terminal,
            Some(UltraTerminal::Promoted),
            "phase {:?} builder {:?}",
            done.phase,
            done.builder.as_ref().map(|b| b.terminal_reason.clone())
        );
        // The proof bundle is on disk.
        let ultra_dir = tmp.path().join("runs").join(&snap.id).join("ultra");
        assert!(ultra_dir.join("contract.json").is_file());
        assert!(ultra_dir.join("verification.json").is_file());
        assert!(ultra_dir.join("adversary.json").is_file());
        assert!(ultra_dir.join("verdicts.json").is_file());
        assert!(ultra_dir.join("ledger.jsonl").is_file());
        assert!(ultra_dir.join("phase4.json").is_file());
        assert!(ultra_dir.join("phase4-checkpoint.json").is_file());
        assert!(ultra_dir.join("phase5.json").is_file());
        assert!(ultra_dir.join("evidence").join("manifest.jsonl").is_file());
        let judge = done.judge.expect("judge report");
        assert!(judge.same_model_as_worker);
    }

    #[test]
    fn forged_completion_is_rejected_after_repairs() {
        // The contract demands a marker the builder never writes. Fresh
        // verification fails every cycle; repairs exhaust; run rejects.
        let tmp = tempfile::tempdir().unwrap();
        let bad_contract = r#"{"obligations":[{"id":"marker","statement":"result.txt contains PASS","proof":{"kind":"file_contains","path":"result.txt","needle":"PASS"}}]}"#;
        let mut turns = vec![text_turn(bad_contract)];
        for _ in 0..3 {
            turns.push(call_turn(vec![
                plan_call(vec![("1", "write result", "in_progress")]),
                create_call("result.txt", "almost"),
            ]));
            turns.push(call_turn(vec![
                plan_call(vec![("1", "write result", "done")]),
                complete_call("done, honest"),
            ]));
        }
        let svc = ultra(
            tmp.path(),
            Script {
                turns: Mutex::new(VecDeque::from(turns)),
            },
        );
        let snap = svc
            .begin("write the marker", "gemini", None, UltraOptions::default())
            .unwrap();
        let done = wait_ultra(&svc, &snap.id, 180_000);
        match done.terminal {
            Some(UltraTerminal::Rejected { ref reasons }) => {
                assert!(
                    reasons.iter().any(|r| r.contains("verification failed")),
                    "{reasons:?}"
                );
            }
            other => panic!("expected rejection, got {other:?}"),
        }
        assert_eq!(done.repair, 2);
        // The adversary and judge never ran: verification already failed.
        assert!(done.adversary.is_none());
        assert!(done.judge.is_none());
    }

    #[test]
    fn contract_failure_is_terminal_and_honest() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = ultra(
            tmp.path(),
            Script {
                turns: Mutex::new(VecDeque::from(vec![
                    text_turn("I cannot produce JSON"),
                    text_turn("still no JSON"),
                    text_turn("nope"),
                ])),
            },
        );
        let snap = svc
            .begin("anything", "gemini", None, UltraOptions::default())
            .unwrap();
        let done = wait_ultra(&svc, &snap.id, 60_000);
        assert!(matches!(
            done.terminal,
            Some(UltraTerminal::ContractFailed { .. })
        ));
    }
}
