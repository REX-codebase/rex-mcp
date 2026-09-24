//! Lease-lapse failsafe: pause the task, persist where it stopped, and resume
//! only on a verified reacquire.
//!
//! Why this exists: the lease keeper renews every live custody lease with no
//! agent involvement, but a lease can still lapse. The machine sleeps or the
//! process is frozen and the keeper misses its deadline; the daemon crashes or
//! restarts and `CustodyRegistry::recover` finds dead leases; the keeper is
//! disabled with `REX_LEASE_KEEPER=0`; or the task's wall budget is spent and
//! renewal stops on purpose. Before this module a lapse was handled badly:
//! nothing durable recorded that the task stopped or why, and
//! `renew_lease_on_resume` treated an `Active` grant with a dead lease as never
//! swept and heartbeated it back to life with no epoch bump and no fencing. That
//! silent resurrection is exactly what docs/agent-operator-mode.md promises can
//! never happen: every lapse goes through suspension, and resume bumps the epoch
//! and rotates every secret so stale processes die.
//!
//! What it writes: an atomic pause record
//! `<root>/tasks/<task_id>/pause.json` holding exactly where the task stopped; a
//! custody `suspend` of the lapsed grant so no further work is allowed; one line
//! per transition in the shared upkeep ledger `<root>/lease-keeper.jsonl` with
//! `"actor":"lease_failsafe"`; and, only from the MCP thread, the task's own
//! `events.jsonl` and `task.json`. A background pass never writes `task.json` or
//! `events.jsonl`: the MCP thread owns those files and does unlocked
//! load-modify-persist on them.
//!
//! `pause.json` has two in-process writers, the keeper pass and the MCP
//! thread. Every read-modify-write of it happens under the custody lock, and
//! each write goes through a temp file unique to its writer, so neither can
//! overwrite a newer record with one decided from a stale read.
//!
//! What it never does: it never resumes a task without the host's current resume
//! handle (reacquire always goes through the verified `rex_execute` resume path
//! and fences the grant with epoch+1 and rotated secrets); it never writes
//! `task.json` or `events.jsonl` from a background thread; and it never turns a
//! task terminal when the grace window passes. Grace expiry only marks the pause
//! record `expired`; the task state is a separate decision.
//!
//! Known limitation, two processes on one state dir: the default state dir is
//! shared by every rex-mcp a host agent launches, and each process holds its own
//! in-memory `CustodyRegistry`. Lease upkeep is single-owner
//! (`<root>/lease-keeper.lock`, see `lease_keeper`), but tasks created by a
//! standby process are not in the owner's in-memory registry, so the owner
//! cannot renew them. They lapse, and their own process pauses them on its next
//! call (the MCP-thread path in `live`). The real fix is a cross-process custody
//! store lock or one daemon per state dir, which is out of scope here.

use rex_custody::{CustodyError, CustodyGrant, CustodyPhase, CustodyRegistry, ReleaseReason};
use rex_protocol::packets::OperationStatus;
use rex_protocol::{ErrorCode, ProtocolError, TaskState};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::{custody_err, internal, now_ms, perr, DurableTask, HarnessDaemon};

/// Persisted pause-record schema. Bump when `PauseRecord` changes shape; never
/// reuse the wire protocol version for storage decisions.
const PAUSE_SCHEMA: u32 = 1;

/// Where a paused task stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseStatus {
    /// Paused: no work allowed until a verified reacquire.
    Paused,
    /// Closed out by a successful verified resume.
    Resumed,
    /// The resume grace window passed without a resume.
    Expired,
}

/// Why the failsafe stopped the task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseReason {
    /// The custody lease passed its expiry without a heartbeat.
    LeaseLapsed,
    /// The wall budget was already spent when the lapse was seen.
    WallBudgetSpent,
    /// A restart found the grant suspended with no pause record.
    SuspendedOnRecovery,
}

/// Durable record of exactly where a task stopped and why. Written atomically
/// to `<root>/tasks/<task_id>/pause.json`; the resume path verifies it against
/// the task before any work continues, so a task can never resume at a step it
/// did not stop at.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PauseRecord {
    /// Persisted schema; see `PAUSE_SCHEMA`.
    pub schema: u32,
    pub task_id: String,
    pub grant_id: String,
    pub status: PauseStatus,
    pub reason: PauseReason,
    /// Lease epoch the grant carried when the task was paused.
    pub epoch_at_pause: u64,
    /// Grant lease expiry as it read when the lapse was seen.
    pub lease_expired_ms: u128,
    pub paused_at_ms: u128,
    /// `suspended_at_ms + resume_grace_ms`: the last moment a resume is
    /// accepted before custody releases the grant.
    pub resume_deadline_ms: u128,
    /// Read-only snapshot of where the task stopped.
    pub cursor: u64,
    pub plan_hash: String,
    pub open_action_id: Option<String>,
    pub operation_status: OperationStatus,
    pub used_tool_calls: u64,
    pub last_event_seq: u64,
    pub resumed_at_ms: Option<u128>,
    pub resumed_epoch: Option<u64>,
    pub expired_at_ms: Option<u128>,
    /// Whether the pause already landed in the task's own `events.jsonl` and
    /// `task.json`. Only the MCP thread may set it.
    #[serde(default)]
    pub absorbed: bool,
}

/// Outcome of one failsafe pass over `<root>/tasks/*`.
#[derive(Debug, Clone, Default)]
pub struct FailsafeReport {
    /// Tasks paused this pass, in scan order.
    pub paused: Vec<PauseRecord>,
    /// Task ids whose grace window passed; their record is now `expired`.
    pub expired: Vec<String>,
    /// Task records that needed nothing (terminal, live lease, already handled)
    /// plus unreadable records.
    pub skipped: usize,
    /// Recording or ledger failures. A pass never panics.
    pub errors: Vec<String>,
}

/// The read-only slice of a task record one pass needs.
struct TaskFacts {
    task_id: String,
    terminal: bool,
    created_ms: u128,
    max_wall_ms: u64,
    grant_id: String,
    cursor: u64,
    plan_hash: String,
    open_action_id: Option<String>,
    operation_status: OperationStatus,
    used_tool_calls: u64,
    last_event_seq: u64,
}

/// Parse the task facts from `task.json` without touching the record. A record
/// that does not parse is skipped by the caller; a pass never panics on it.
fn read_pause_facts(path: &Path) -> Option<TaskFacts> {
    let bytes = fs::read(path).ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    let state: TaskState = serde_json::from_value(v.get("state")?.clone()).ok()?;
    Some(TaskFacts {
        task_id: v.get("task_id")?.as_str()?.to_string(),
        terminal: state.is_terminal(),
        created_ms: v.get("created_ms")?.as_u64()? as u128,
        max_wall_ms: v.get("max_wall_ms")?.as_u64()?,
        grant_id: v.get("grant_id")?.as_str()?.to_string(),
        cursor: v.get("cursor")?.as_u64()?,
        plan_hash: v.get("plan_hash")?.as_str()?.to_string(),
        open_action_id: v
            .get("open_action")
            .and_then(|a| a.get("action_id"))
            .and_then(|s| s.as_str())
            .map(String::from),
        operation_status: v
            .get("operation_status")
            .cloned()
            .and_then(|s| serde_json::from_value(s).ok())
            .unwrap_or_default(),
        used_tool_calls: v.get("used_tool_calls")?.as_u64()?,
        last_event_seq: v.get("last_event_seq")?.as_u64()?,
    })
}

/// Read a pause record. A missing file is `Ok(None)`; a corrupt one is an
/// error, never a guess.
/// Atomic write through a temp file unique to this writer (process, thread,
/// call), so two writers never share, clobber or rename each other's temp.
fn write_pause_file(path: &Path, rec: &PauseRecord) -> Result<(), ProtocolError> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!(
        "{}.{:?}.{seq}.tmp",
        std::process::id(),
        std::thread::current().id()
    ));
    let bytes = serde_json::to_vec_pretty(rec).map_err(internal)?;
    let written = fs::write(&tmp, bytes).and_then(|()| fs::rename(&tmp, path));
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written.map_err(internal)
}

fn read_pause_file(path: &Path) -> Result<Option<PauseRecord>, ProtocolError> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| internal(format!("corrupt pause record: {e}"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(internal(format!("pause record unreadable: {e}"))),
    }
}

/// What one task needs from the pass. Read, decided and written under the
/// custody lock.
enum PassStep {
    Skip,
    Pause(PauseRecord),
    Expire(PauseRecord),
    Fail(String),
}

/// Wall budget first: a lapse seen after the budget is spent is recorded as
/// such, so the operator can tell "ran out of time" from "lost the lease".
fn pause_reason(created_ms: u128, max_wall_ms: u64, now: u128) -> PauseReason {
    if now >= created_ms + max_wall_ms as u128 {
        PauseReason::WallBudgetSpent
    } else {
        PauseReason::LeaseLapsed
    }
}

fn build_record(
    facts: &TaskFacts,
    grant: &CustodyGrant,
    reason: PauseReason,
    now: u128,
) -> PauseRecord {
    let suspended_at = grant.suspended_at_ms.unwrap_or(now);
    PauseRecord {
        schema: PAUSE_SCHEMA,
        task_id: facts.task_id.clone(),
        grant_id: grant.grant_id.clone(),
        status: PauseStatus::Paused,
        reason,
        epoch_at_pause: grant.lease.epoch,
        lease_expired_ms: grant.lease.expires_ms,
        paused_at_ms: now,
        resume_deadline_ms: suspended_at + grant.lease_terms.resume_grace_ms as u128,
        cursor: facts.cursor,
        plan_hash: facts.plan_hash.clone(),
        open_action_id: facts.open_action_id.clone(),
        operation_status: facts.operation_status.clone(),
        used_tool_calls: facts.used_tool_calls,
        last_event_seq: facts.last_event_seq,
        resumed_at_ms: None,
        resumed_epoch: None,
        expired_at_ms: None,
        absorbed: false,
    }
}

/// Decide one task's transition. Runs under the custody lock.
fn decide(
    reg: &mut CustodyRegistry,
    facts: &TaskFacts,
    existing: Option<&PauseRecord>,
    now: u128,
    swept: &mut bool,
) -> PassStep {
    let grant = match reg.grant(&facts.grant_id) {
        Some(grant) => grant.clone(),
        None => return PassStep::Skip,
    };
    if let Some(rec) = existing.filter(|rec| rec.status == PauseStatus::Paused) {
        // A resume has already fenced the grant back to life and is about to
        // close this record out; the record is not the keeper's to touch.
        if grant.phase == CustodyPhase::Active && grant.lease.is_live(now) {
            return PassStep::Skip;
        }
        let released = grant.release.as_ref() == Some(&ReleaseReason::LeaseExpired);
        let past_grace = now > rec.resume_deadline_ms;
        if !released && !past_grace {
            return PassStep::Skip;
        }
        if past_grace && !*swept {
            if let Err(e) = reg.sweep(now) {
                return PassStep::Fail(format!("{}: sweep failed: {e}", facts.task_id));
            }
            *swept = true;
        }
        return PassStep::Expire(rec.clone());
    }
    match grant.phase {
        CustodyPhase::Active if !grant.lease.is_live(now) => {
            if let Err(e) = reg.suspend(&facts.grant_id, "lease lapsed (failsafe)", now) {
                return PassStep::Fail(format!("{}: suspend failed: {e}", facts.task_id));
            }
            let grant = match reg.grant(&facts.grant_id) {
                Some(grant) => grant.clone(),
                None => {
                    return PassStep::Fail(format!(
                        "{}: grant missing after suspend",
                        facts.task_id
                    ));
                }
            };
            let reason = pause_reason(facts.created_ms, facts.max_wall_ms, now);
            PassStep::Pause(build_record(facts, &grant, reason, now))
        }
        CustodyPhase::Suspended => PassStep::Pause(build_record(
            facts,
            &grant,
            PauseReason::SuspendedOnRecovery,
            now,
        )),
        _ => PassStep::Skip,
    }
}

/// One failsafe pass over `<root>/tasks/*/task.json` at `now`. Read-only on the
/// task records: everything this writes lives in `pause.json`, the custody grant
/// files and the upkeep ledger.
pub(crate) fn pause_lapsed(
    root: &Path,
    custody: &Mutex<CustodyRegistry>,
    now: u128,
) -> FailsafeReport {
    let mut report = FailsafeReport::default();
    let entries = match fs::read_dir(root.join("tasks")) {
        Ok(entries) => entries,
        Err(e) => {
            report.errors.push(format!("task scan: {e}"));
            return report;
        }
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    let mut swept = false;
    for dir in dirs {
        let facts = match read_pause_facts(&dir.join("task.json")) {
            Some(facts) => facts,
            None => {
                report.skipped += 1;
                continue;
            }
        };
        if facts.terminal {
            report.skipped += 1;
            continue;
        }
        let path = dir.join("pause.json");
        let mut reg = match custody.lock() {
            Ok(reg) => reg,
            Err(_) => {
                report.errors.push("custody registry poisoned".to_string());
                break;
            }
        };
        // Read, decide and write under one hold of the custody lock: a resume
        // on the MCP thread cannot land between the read and the write, so
        // this pass never overwrites a newer record with a stale decision.
        let existing = match read_pause_file(&path) {
            Ok(existing) => existing,
            Err(e) => {
                drop(reg);
                report.errors.push(format!("{}: {e}", facts.task_id));
                report.skipped += 1;
                continue;
            }
        };
        let mut action = decide(&mut reg, &facts, existing.as_ref(), now, &mut swept);
        let written = match &mut action {
            PassStep::Pause(rec) => Some(write_pause_file(&path, rec)),
            PassStep::Expire(rec) => {
                rec.status = PauseStatus::Expired;
                rec.expired_at_ms = Some(now);
                Some(write_pause_file(&path, rec))
            }
            PassStep::Skip | PassStep::Fail(_) => None,
        };
        drop(reg);
        if let Some(Err(e)) = written {
            report
                .errors
                .push(format!("{}: pause record write failed: {e}", facts.task_id));
        }
        match action {
            PassStep::Skip => report.skipped += 1,
            PassStep::Pause(rec) => {
                if let Err(e) = append_failsafe_ledger(root, "paused", &rec, now) {
                    report
                        .errors
                        .push(format!("{}: ledger write failed: {e}", facts.task_id));
                }
                report.paused.push(rec);
            }
            PassStep::Expire(rec) => {
                if let Err(e) = append_failsafe_ledger(root, "expired", &rec, now) {
                    report
                        .errors
                        .push(format!("{}: ledger write failed: {e}", facts.task_id));
                }
                report.expired.push(facts.task_id.clone());
            }
            PassStep::Fail(message) => report.errors.push(message),
        }
    }
    report
}

/// One line of the shared upkeep ledger, `<root>/lease-keeper.jsonl`.
#[derive(Serialize)]
struct FailsafeLedgerLine<'a> {
    ts_ms: u128,
    task_id: &'a str,
    grant_id: &'a str,
    epoch: u64,
    event: &'a str,
    actor: &'a str,
}

fn append_failsafe_ledger(
    root: &Path,
    event: &str,
    rec: &PauseRecord,
    ts_ms: u128,
) -> Result<(), String> {
    let line = FailsafeLedgerLine {
        ts_ms,
        task_id: &rec.task_id,
        grant_id: &rec.grant_id,
        epoch: rec.epoch_at_pause,
        event,
        actor: "lease_failsafe",
    };
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("lease-keeper.jsonl"))
        .map_err(|e| e.to_string())?;
    serde_json::to_writer(&mut f, &line).map_err(|e| e.to_string())?;
    f.write_all(b"\n").map_err(|e| e.to_string())?;
    f.sync_data().map_err(|e| e.to_string())
}

impl HarnessDaemon {
    /// Run one failsafe pass with an injected clock, so tests and the desktop
    /// UI can drive a pass deterministically.
    pub fn pause_lapsed_leases(&self, now_ms: u128) -> FailsafeReport {
        pause_lapsed(&self.root, &self.custody, now_ms)
    }

    /// The durable pause record of a task, if any. Fail closed on corruption:
    /// a resume must never guess where a task stopped.
    pub(crate) fn read_pause_record(
        &self,
        task_id: &str,
    ) -> Result<Option<PauseRecord>, ProtocolError> {
        read_pause_file(&self.task_dir(task_id).join("pause.json"))
    }

    /// Write (or rewrite) the pause record atomically. `_held` is the
    /// caller's custody guard: every read-modify-write of `pause.json` runs
    /// under the custody lock, the same lock the keeper pass writes under.
    pub(crate) fn write_pause_record(
        &self,
        _held: &CustodyRegistry,
        rec: &PauseRecord,
    ) -> Result<(), ProtocolError> {
        write_pause_file(&self.task_dir(&rec.task_id).join("pause.json"), rec)
    }

    fn lock_custody(&self) -> Result<std::sync::MutexGuard<'_, CustodyRegistry>, ProtocolError> {
        self.custody
            .lock()
            .map_err(|_| internal("custody registry poisoned"))
    }

    /// Pause a task right now, from the MCP thread: suspend the grant if it is
    /// still active, then write the record. Idempotent; a live pause record
    /// wins, and an already suspended grant only gets its record ensured.
    pub(crate) fn pause_now(
        &self,
        t: &DurableTask,
        reason: PauseReason,
    ) -> Result<PauseRecord, ProtocolError> {
        let now = now_ms();
        let mut reg = self.lock_custody()?;
        if let Some(rec) = self.read_pause_record(&t.task_id)? {
            if rec.status == PauseStatus::Paused {
                return Ok(rec);
            }
        }
        let phase = reg.grant(&t.grant_id).map(|g| g.phase);
        match phase {
            Some(CustodyPhase::Active) => {
                reg.suspend(&t.grant_id, "lease lapsed (failsafe)", now)
                    .map_err(custody_err)?;
            }
            Some(_) => {}
            None => {
                return Err(perr(
                    ErrorCode::StaleLease,
                    "lease expired beyond the resume grace window",
                    &t.task_id,
                ));
            }
        }
        let grant = reg
            .grant(&t.grant_id)
            .cloned()
            .ok_or_else(|| internal("grant missing after suspend"))?;
        let rec = build_record_from_task(t, &grant, reason, now);
        self.write_pause_record(&reg, &rec)?;
        drop(reg);
        if let Err(e) = append_failsafe_ledger(&self.root, "paused", &rec, now) {
            eprintln!("lease-failsafe: ledger write failed: {e}");
        }
        Ok(rec)
    }

    /// Whether custody holds this task's grant suspended right now: the
    /// failsafe (or a restart sweep) has already paused it.
    pub(crate) fn grant_suspended(&self, t: &DurableTask) -> Result<bool, ProtocolError> {
        let reg = self
            .custody
            .lock()
            .map_err(|_| internal("custody registry poisoned"))?;
        Ok(matches!(
            reg.grant(&t.grant_id).map(|g| g.phase),
            Some(CustodyPhase::Suspended)
        ))
    }

    /// Fence a suspended grant back to life through custody: epoch bump,
    /// rotated resume and token secrets, fresh lease. Returns the epoch before
    /// the resume so the pause record can be matched to it.
    pub(crate) fn resume_suspended(
        &self,
        t: &mut DurableTask,
        now: u128,
    ) -> Result<u64, ProtocolError> {
        let mut reg = self
            .custody
            .lock()
            .map_err(|_| internal("custody registry poisoned"))?;
        let (epoch_before, secret) = match reg.grant(&t.grant_id) {
            Some(g) if g.phase == CustodyPhase::Suspended => {
                (g.lease.epoch, g.resume_secret.clone())
            }
            Some(_) => {
                return Err(perr(
                    ErrorCode::LeaseConflict,
                    "custody grant is not suspended",
                    &t.task_id,
                ));
            }
            None => {
                return Err(perr(
                    ErrorCode::StaleLease,
                    "lease expired beyond the resume grace window",
                    &t.task_id,
                ));
            }
        };
        // Refuse a drifted task before custody fences the grant back to life:
        // a refusal after the fence would leave an active grant behind a
        // paused record, and no later resume could get past it.
        if let Some(rec) = self.read_pause_record(&t.task_id)? {
            if rec.status == PauseStatus::Paused && record_drifted(&rec, t, epoch_before) {
                return Err(perr(
                    ErrorCode::LeaseConflict,
                    "task changed while paused; refusing to resume",
                    &t.task_id,
                ));
            }
        }
        let token = match reg.resume(&t.grant_id, &secret, now) {
            Ok(token) => token,
            Err(CustodyError::GrantReleased(_)) => {
                let _ = self.mark_record_expired(&reg, &t.task_id, now);
                return Err(perr(
                    ErrorCode::StaleLease,
                    "lease expired beyond the resume grace window",
                    &t.task_id,
                ));
            }
            Err(other) => return Err(custody_err(other)),
        };
        t.token = token;
        let grant = reg
            .grant(&t.grant_id)
            .ok_or_else(|| internal("grant missing after resume"))?;
        t.lease_epoch = grant.lease.epoch;
        t.lease_expires_ms = grant.lease.expires_ms;
        t.heartbeat_seq = grant.lease.next_seq;
        t.resume_nonce = grant.lease.next_seq;
        Ok(epoch_before)
    }

    /// Close out a pause record after a successful custody resume. The task
    /// must still be exactly where it stopped: any drift on the plan hash, the
    /// cursor or the open action refuses the resume and changes nothing.
    pub(crate) fn complete_resume_after_pause(
        &self,
        t: &mut DurableTask,
        epoch_before: u64,
    ) -> Result<Option<PauseRecord>, ProtocolError> {
        let reg = self.lock_custody()?;
        let Some(mut rec) = self.read_pause_record(&t.task_id)? else {
            return Ok(None);
        };
        if rec.status != PauseStatus::Paused {
            return Ok(None);
        }
        if record_drifted(&rec, t, epoch_before) {
            return Err(perr(
                ErrorCode::LeaseConflict,
                "task changed while paused; refusing to resume",
                &t.task_id,
            ));
        }
        let now = now_ms();
        t.operation_status = rec.operation_status.clone();
        self.append_event(
            t,
            "task_resumed_after_lapse",
            json!({
                "reason": rec.reason,
                "paused_at_ms": rec.paused_at_ms,
                "lease_expired_ms": rec.lease_expired_ms,
                "epoch_before": epoch_before,
                "epoch_after": t.lease_epoch,
                "paused_for_ms": now.saturating_sub(rec.paused_at_ms),
            }),
        )?;
        rec.status = PauseStatus::Resumed;
        rec.resumed_at_ms = Some(now);
        rec.resumed_epoch = Some(t.lease_epoch);
        self.write_pause_record(&reg, &rec)?;
        drop(reg);
        if let Err(e) = append_failsafe_ledger(&self.root, "resumed", &rec, now) {
            eprintln!("lease-failsafe: ledger write failed: {e}");
        }
        Ok(Some(rec))
    }

    /// The verified reacquire path for a lapse: pause the task and fence the
    /// grant back to life through custody suspend + resume, so a lapsed lease
    /// is never heartbeated back silently. The clock is injectable so tests can
    /// drive a lapse deterministically.
    pub(crate) fn reacquire_lapsed(
        &self,
        t: &mut DurableTask,
        now: u128,
    ) -> Result<(), ProtocolError> {
        let reason = pause_reason(t.created_ms, t.max_wall_ms, now);
        self.pause_now(t, reason)?;
        let epoch_before = self.resume_suspended(t, now)?;
        self.complete_resume_after_pause(t, epoch_before)?;
        Ok(())
    }

    /// Land a pause in the task's own durable state. MCP thread only:
    /// `events.jsonl` and `task.json` have exactly one writer. The record's
    /// `absorbed` marker keeps the event to exactly one per pause.
    pub(crate) fn absorb_pause(&self, t: &mut DurableTask) -> Result<(), ProtocolError> {
        let reg = self.lock_custody()?;
        let Some(mut rec) = self.read_pause_record(&t.task_id)? else {
            return Ok(());
        };
        if rec.status != PauseStatus::Paused || rec.absorbed {
            return Ok(());
        }
        self.append_event(
            t,
            "task_paused_on_lease_lapse",
            json!({
                "reason": rec.reason,
                "epoch_at_pause": rec.epoch_at_pause,
                "lease_expired_ms": rec.lease_expired_ms,
                "paused_at_ms": rec.paused_at_ms,
                "resume_deadline_ms": rec.resume_deadline_ms,
                "cursor": rec.cursor,
                "open_action_id": rec.open_action_id,
            }),
        )?;
        t.operation_status = OperationStatus::Stale;
        self.persist(t)?;
        rec.absorbed = true;
        self.write_pause_record(&reg, &rec)
    }

    /// Mark a still-paused record expired. Used when custody has already
    /// released the grant (grace passed) and no later resume can succeed.
    fn mark_record_expired(
        &self,
        held: &CustodyRegistry,
        task_id: &str,
        now: u128,
    ) -> Result<(), ProtocolError> {
        let Some(mut rec) = self.read_pause_record(task_id)? else {
            return Ok(());
        };
        if rec.status != PauseStatus::Paused {
            return Ok(());
        }
        rec.status = PauseStatus::Expired;
        rec.expired_at_ms = Some(now);
        self.write_pause_record(held, &rec)?;
        if let Err(e) = append_failsafe_ledger(&self.root, "expired", &rec, now) {
            eprintln!("lease-failsafe: ledger write failed: {e}");
        }
        Ok(())
    }
}

/// Whether the task no longer matches where its pause record says it stopped.
fn record_drifted(rec: &PauseRecord, t: &DurableTask, epoch_before: u64) -> bool {
    let open_action_id = t.open_action.as_ref().map(|a| a.action_id.clone());
    rec.task_id != t.task_id
        || rec.grant_id != t.grant_id
        || rec.epoch_at_pause != epoch_before
        || rec.plan_hash != t.plan_hash
        || rec.cursor != t.cursor as u64
        || rec.open_action_id != open_action_id
}

/// Snapshot where `t` stopped into a fresh pause record for `grant`.
fn build_record_from_task(
    t: &DurableTask,
    grant: &CustodyGrant,
    reason: PauseReason,
    now: u128,
) -> PauseRecord {
    let facts = TaskFacts {
        task_id: t.task_id.clone(),
        terminal: false,
        created_ms: t.created_ms,
        max_wall_ms: t.max_wall_ms,
        grant_id: t.grant_id.clone(),
        cursor: t.cursor as u64,
        plan_hash: t.plan_hash.clone(),
        open_action_id: t.open_action.as_ref().map(|a| a.action_id.clone()),
        operation_status: t.operation_status.clone(),
        used_tool_calls: t.used_tool_calls,
        last_event_seq: t.last_event_seq,
    };
    build_record(&facts, grant, reason, now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DaemonPolicy, HarnessDaemon, LeaseKeeperConfig};
    use rex_protocol::{ExecuteRequest, NextRequest, TaskRefRequest};
    use std::time::{Duration, Instant};
    use tempfile::tempdir;

    const DAY_MS: u64 = 24 * 60 * 60 * 1000;

    /// Build an execute request over the wire schema, the way a host agent
    /// sends one.
    fn wire(
        request_id: &str,
        task: &str,
        task_id: Option<&str>,
        handle: Option<&str>,
    ) -> ExecuteRequest {
        serde_json::from_value(json!({
            "protocol_version": "2.0",
            "task": task,
            "request_id": request_id,
            "task_id": task_id,
            "plan": [{
                "instructions": "write hello",
                "permitted_tools": [],
                "acceptance": "file exists",
                "max_wall_ms": 1_200_000
            }],
            "host": "claude_code",
            "operator_is_agent": true,
            "ultra": false,
            "follow_up": null,
            "resume_handle": handle
        }))
        .expect("execute request")
    }

    fn req(id: &str) -> ExecuteRequest {
        wire(&format!("create-{id}"), &format!("write {id}"), None, None)
    }

    fn req_resume(id: &str, n: u32, task_id: &str, handle: &str) -> ExecuteRequest {
        wire(
            &format!("resume-{id}-{n}"),
            &format!("write {id}"),
            Some(task_id),
            Some(handle),
        )
    }

    fn setup() -> (tempfile::TempDir, PathBuf, HarnessDaemon) {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path().join("state");
        let w = tmp.path().join("work");
        let daemon = HarnessDaemon::open(&root, DaemonPolicy::conservative(&w)).expect("open");
        (tmp, root, daemon)
    }

    fn grant_of(daemon: &HarnessDaemon, task_id: &str) -> CustodyGrant {
        let reg = daemon.custody.lock().expect("custody lock");
        reg.grant_for_task(task_id).expect("grant for task").clone()
    }

    fn grant_by_id(daemon: &HarnessDaemon, grant_id: &str) -> CustodyGrant {
        let reg = daemon.custody.lock().expect("custody lock");
        reg.grant(grant_id).expect("grant").clone()
    }

    fn rewrite_u64(root: &Path, task_id: &str, field: &str, value: u64) {
        let path = root.join("tasks").join(task_id).join("task.json");
        let mut v: Value =
            serde_json::from_slice(&fs::read(&path).expect("task bytes")).expect("task json");
        v[field] = json!(value);
        fs::write(&path, serde_json::to_vec_pretty(&v).expect("serialize")).expect("rewrite");
    }

    fn rewrite_state(root: &Path, task_id: &str, state: &str) {
        let path = root.join("tasks").join(task_id).join("task.json");
        let mut v: Value =
            serde_json::from_slice(&fs::read(&path).expect("task bytes")).expect("task json");
        v["state"] = json!(state);
        fs::write(&path, serde_json::to_vec_pretty(&v).expect("serialize")).expect("rewrite");
    }

    fn lapse_grant_file(root: &Path, grant_id: &str) {
        let path = root
            .join("custody")
            .join("grants")
            .join(format!("{grant_id}.json"));
        let mut v: Value =
            serde_json::from_slice(&fs::read(&path).expect("grant bytes")).expect("grant json");
        v["lease"]["expires_ms"] = json!(1);
        fs::write(&path, serde_json::to_vec_pretty(&v).expect("serialize")).expect("rewrite");
    }

    fn task_bytes(root: &Path, task_id: &str) -> Vec<u8> {
        fs::read(root.join("tasks").join(task_id).join("task.json")).expect("task bytes")
    }

    fn pause_file(root: &Path, task_id: &str) -> PathBuf {
        root.join("tasks").join(task_id).join("pause.json")
    }

    fn event_kinds(root: &Path, task_id: &str) -> Vec<String> {
        let path = root.join("tasks").join(task_id).join("events.jsonl");
        let mut kinds = Vec::new();
        if let Ok(text) = fs::read_to_string(path) {
            for line in text.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                let v: Value = serde_json::from_str(line).expect("event line");
                kinds.push(v["kind"].as_str().expect("kind").to_string());
            }
        }
        kinds
    }

    fn ledger(root: &Path) -> Vec<Value> {
        let mut lines = Vec::new();
        if let Ok(text) = fs::read_to_string(root.join("lease-keeper.jsonl")) {
            for line in text.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                lines.push(serde_json::from_str(line).expect("ledger line"));
            }
        }
        lines
    }

    /// Both the wire code and the message have to reach the caller.
    fn assert_error(err: ProtocolError, code: &str, needle: &str) {
        let debugged = format!("{err:?}");
        assert!(
            debugged.contains(code),
            "expected code {code} in {debugged}"
        );
        let rendered = err.to_string();
        assert!(rendered.contains(needle), "expected {needle} in {rendered}");
    }

    fn wait_until(ms: u64, mut cond: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < deadline {
            if cond() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("condition not met within {ms} ms");
    }

    #[test]
    fn f1_lapse_pauses_and_persists_without_touching_task_json() {
        let (_tmp, root, daemon) = setup();
        let r0 = daemon.execute(req("f1")).expect("create");
        rewrite_u64(&root, &r0.task_id, "max_wall_ms", DAY_MS);
        let grant = grant_of(&daemon, &r0.task_id);
        let before = task_bytes(&root, &r0.task_id);

        let report = daemon.pause_lapsed_leases(grant.lease.expires_ms + 1);
        assert_eq!(report.paused.len(), 1);
        assert!(report.expired.is_empty());
        assert!(report.errors.is_empty());
        let rec = &report.paused[0];
        assert_eq!(rec.reason, PauseReason::LeaseLapsed);
        assert_eq!(rec.status, PauseStatus::Paused);
        assert_eq!(rec.epoch_at_pause, grant.lease.epoch);
        let v: Value = serde_json::from_slice(&before).expect("task json");
        assert_eq!(rec.cursor, v["cursor"].as_u64().expect("cursor"));
        assert_eq!(rec.plan_hash, v["plan_hash"].as_str().expect("plan hash"));
        assert_eq!(
            rec.open_action_id.as_deref(),
            v["open_action"]["action_id"].as_str()
        );

        assert_eq!(
            grant_by_id(&daemon, &grant.grant_id).phase,
            CustodyPhase::Suspended
        );
        let stored = daemon
            .read_pause_record(&r0.task_id)
            .expect("read")
            .expect("record");
        assert_eq!(stored, *rec);
        assert_eq!(before, task_bytes(&root, &r0.task_id));
        assert!(ledger(&root)
            .iter()
            .any(|l| { l["actor"] == json!("lease_failsafe") && l["event"] == json!("paused") }));

        let second = daemon.pause_lapsed_leases(grant.lease.expires_ms + 1);
        assert!(second.paused.is_empty());
        assert!(second.expired.is_empty());
        assert!(second.errors.is_empty());
    }

    #[test]
    fn f2_live_lease_is_never_paused() {
        let (_tmp, root, daemon) = setup();
        let r0 = daemon.execute(req("f2")).expect("create");
        rewrite_u64(&root, &r0.task_id, "max_wall_ms", DAY_MS);
        let grant = grant_of(&daemon, &r0.task_id);

        let quiet = daemon.pause_lapsed_leases(grant.lease.expires_ms - 1);
        assert!(quiet.paused.is_empty());
        assert!(quiet.expired.is_empty());
        assert!(quiet.errors.is_empty());
        assert!(daemon
            .read_pause_record(&r0.task_id)
            .expect("read")
            .is_none());

        rewrite_state(&root, &r0.task_id, "cancelled");
        let after = daemon.pause_lapsed_leases(grant.lease.expires_ms + 1);
        assert!(after.paused.is_empty());
        assert_eq!(after.skipped, 1);
        assert!(after.errors.is_empty());
        assert!(daemon
            .read_pause_record(&r0.task_id)
            .expect("read")
            .is_none());
    }

    #[test]
    fn f3_agent_call_while_paused_fails_cleanly_and_records_pause() {
        let (_tmp, root, daemon) = setup();
        let r0 = daemon.execute(req("f3")).expect("create");
        rewrite_u64(&root, &r0.task_id, "max_wall_ms", DAY_MS);
        let grant = grant_of(&daemon, &r0.task_id);
        let cap = r0.task_capability.clone().expect("capability");
        let epoch = r0.lease.epoch;
        daemon.pause_lapsed_leases(grant.lease.expires_ms + 1);

        for _ in 0..2 {
            let err = daemon
                .next(NextRequest {
                    task_id: r0.task_id.clone(),
                    capability: cap.clone(),
                    lease_epoch: epoch,
                })
                .expect_err("paused task must refuse work");
            assert_error(err, "StaleLease", "paused");
        }
        let kinds = event_kinds(&root, &r0.task_id);
        assert_eq!(
            kinds
                .iter()
                .filter(|k| k.as_str() == "task_paused_on_lease_lapse")
                .count(),
            1
        );
        let status = daemon
            .status(TaskRefRequest {
                task_id: r0.task_id.clone(),
            })
            .expect("status");
        assert_eq!(status.operation, OperationStatus::Stale);
    }

    #[test]
    fn f4_resume_on_reacquire_restores_exact_step() {
        let (_tmp, root, daemon) = setup();
        let r0 = daemon.execute(req("f4")).expect("create");
        rewrite_u64(&root, &r0.task_id, "max_wall_ms", DAY_MS);
        let grant = grant_of(&daemon, &r0.task_id);
        let old_epoch = r0.lease.epoch;
        let old_cap = r0.task_capability.clone().expect("capability");
        let handle = r0.host_resume_handle.clone().expect("handle");
        let report = daemon.pause_lapsed_leases(grant.lease.expires_ms + 1);
        let paused_action = report.paused[0].open_action_id.clone();

        let r1 = daemon
            .execute(req_resume("f4", 1, &r0.task_id, &handle))
            .expect("resume");
        assert!(r1.resumed);
        assert_eq!(r1.lease.epoch, old_epoch + 1);
        assert_eq!(
            r1.next.as_ref().map(|a| a.action_id.as_str()),
            paused_action.as_deref()
        );
        let kinds = event_kinds(&root, &r0.task_id);
        assert!(kinds
            .iter()
            .any(|k| k.as_str() == "task_resumed_after_lapse"));
        let rec = daemon
            .read_pause_record(&r0.task_id)
            .expect("read")
            .expect("record");
        assert_eq!(rec.status, PauseStatus::Resumed);
        assert_eq!(rec.resumed_epoch, Some(r1.lease.epoch));
        assert_eq!(
            grant_by_id(&daemon, &grant.grant_id).phase,
            CustodyPhase::Active
        );

        let next = daemon
            .next(NextRequest {
                task_id: r0.task_id.clone(),
                capability: r1.task_capability.clone().expect("capability"),
                lease_epoch: r1.lease.epoch,
            })
            .expect("next on the new epoch");
        assert_eq!(next.next.map(|a| a.action_id), paused_action);

        let refused = daemon.next(NextRequest {
            task_id: r0.task_id.clone(),
            capability: old_cap,
            lease_epoch: old_epoch,
        });
        assert!(refused.is_err());
    }

    #[test]
    fn f5_same_process_lapse_no_longer_resurrects_silently() {
        let (_tmp, root, daemon) = setup();
        let r0 = daemon.execute(req("f5")).expect("create");
        rewrite_u64(&root, &r0.task_id, "max_wall_ms", DAY_MS);
        let grant = grant_of(&daemon, &r0.task_id);
        let epoch_before = grant.lease.epoch;

        let mut t = daemon.load(&r0.task_id).expect("load");
        daemon
            .reacquire_lapsed(&mut t, grant.lease.expires_ms + 1)
            .expect("reacquire");

        assert_eq!(t.lease_epoch, epoch_before + 1);
        assert_eq!(
            grant_by_id(&daemon, &grant.grant_id).lease.epoch,
            epoch_before + 1
        );
        let chain = daemon
            .custody
            .lock()
            .expect("custody lock")
            .audit_chain(&grant.grant_id)
            .expect("audit chain");
        let kinds: Vec<&str> = chain.iter().map(|e| e.kind.as_str()).collect();
        let suspended = kinds
            .iter()
            .position(|k| *k == "suspended")
            .expect("suspended event");
        let resumed = kinds
            .iter()
            .position(|k| *k == "resumed")
            .expect("resumed event");
        assert!(
            suspended < resumed,
            "a lapse must be fenced through suspend"
        );
        let rec = daemon
            .read_pause_record(&r0.task_id)
            .expect("read")
            .expect("record");
        assert_eq!(rec.status, PauseStatus::Resumed);
    }

    #[test]
    fn f6_restart_path_writes_record_and_resumes() {
        let (tmp, root, daemon) = setup();
        let w = tmp.path().join("work");
        let r0 = daemon.execute(req("f6")).expect("create");
        rewrite_u64(&root, &r0.task_id, "max_wall_ms", DAY_MS);
        let grant = grant_of(&daemon, &r0.task_id);
        let handle = r0.host_resume_handle.clone().expect("handle");
        lapse_grant_file(&root, &grant.grant_id);

        drop(daemon);
        let daemon = HarnessDaemon::open(&root, DaemonPolicy::conservative(&w)).expect("reopen");

        let rec = daemon
            .read_pause_record(&r0.task_id)
            .expect("read")
            .expect("record");
        assert_eq!(rec.reason, PauseReason::SuspendedOnRecovery);
        assert_eq!(rec.status, PauseStatus::Paused);

        let r1 = daemon
            .execute(req_resume("f6", 1, &r0.task_id, &handle))
            .expect("resume");
        assert!(r1.resumed);
        assert_eq!(r1.lease.epoch, rec.epoch_at_pause + 1);
        let rec = daemon
            .read_pause_record(&r0.task_id)
            .expect("read")
            .expect("record");
        assert_eq!(rec.status, PauseStatus::Resumed);
        assert_eq!(rec.resumed_epoch, Some(r1.lease.epoch));
    }

    #[test]
    fn f7_grace_expiry_marks_record_expired() {
        let (_tmp, root, daemon) = setup();
        let r0 = daemon.execute(req("f7")).expect("create");
        rewrite_u64(&root, &r0.task_id, "max_wall_ms", DAY_MS);
        let grant = grant_of(&daemon, &r0.task_id);
        let handle = r0.host_resume_handle.clone().expect("handle");
        let report = daemon.pause_lapsed_leases(grant.lease.expires_ms + 1);
        let deadline = report.paused[0].resume_deadline_ms;

        let late = daemon.pause_lapsed_leases(deadline + 1);
        assert!(late.paused.is_empty());
        assert_eq!(late.expired, vec![r0.task_id.clone()]);
        assert!(late.errors.is_empty());
        let rec = daemon
            .read_pause_record(&r0.task_id)
            .expect("read")
            .expect("record");
        assert_eq!(rec.status, PauseStatus::Expired);
        assert!(rec.expired_at_ms.is_some());
        assert_eq!(
            grant_by_id(&daemon, &grant.grant_id).release,
            Some(ReleaseReason::LeaseExpired)
        );

        let err = daemon
            .execute(req_resume("f7", 1, &r0.task_id, &handle))
            .expect_err("past grace");
        assert_error(err, "StaleLease", "beyond the resume grace window");
    }

    #[test]
    fn f8_tampered_pause_record_fails_closed() {
        let (_tmp, root, daemon) = setup();
        let r0 = daemon.execute(req("f8")).expect("create");
        rewrite_u64(&root, &r0.task_id, "max_wall_ms", DAY_MS);
        let grant = grant_of(&daemon, &r0.task_id);
        let handle = r0.host_resume_handle.clone().expect("handle");
        daemon.pause_lapsed_leases(grant.lease.expires_ms + 1);
        let path = pause_file(&root, &r0.task_id);

        let mut rec: PauseRecord =
            serde_json::from_slice(&fs::read(&path).expect("pause bytes")).expect("pause json");
        rec.cursor += 1;
        fs::write(&path, serde_json::to_vec_pretty(&rec).expect("serialize")).expect("rewrite");
        let err = daemon
            .execute(req_resume("f8", 1, &r0.task_id, &handle))
            .expect_err("tampered");
        assert_error(err, "LeaseConflict", "task changed while paused");
        let stored: PauseRecord =
            serde_json::from_slice(&fs::read(&path).expect("pause bytes")).expect("pause json");
        assert_eq!(stored.status, PauseStatus::Paused);

        fs::write(&path, b"{ not json").expect("corrupt");
        let err = daemon
            .execute(req_resume("f8", 2, &r0.task_id, &handle))
            .expect_err("corrupt");
        assert_error(err, "Internal", "corrupt pause record");
    }

    #[test]
    fn f9_only_one_keeper_owns_upkeep() {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path().join("state");
        let w = tmp.path().join("work");
        let d1 = HarnessDaemon::open(&root, DaemonPolicy::conservative(&w)).expect("open 1");
        let d2 = HarnessDaemon::open(&root, DaemonPolicy::conservative(&w)).expect("open 2");
        let cfg = LeaseKeeperConfig {
            tick_ms: 20,
            renew_ahead_ms: 120,
        };
        let k1 = d1.spawn_lease_keeper(cfg).expect("keeper 1");
        let k2 = d2.spawn_lease_keeper(cfg).expect("keeper 2");

        // Either keeper may win the lock; exactly one must own upkeep.
        wait_until(2_000, || k1.is_owner() || k2.is_owner());
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            k1.is_owner() != k2.is_owner(),
            "exactly one keeper owns upkeep; the other must stand by"
        );
        let (owner, standby) = if k1.is_owner() { (k1, k2) } else { (k2, k1) };

        owner.stop();
        wait_until(2_000, || standby.is_owner());
        assert!(standby.is_owner());
    }

    #[test]
    fn f10_wall_budget_spent_pauses_with_reason() {
        let (_tmp, root, daemon) = setup();
        let r0 = daemon.execute(req("f10")).expect("create");
        rewrite_u64(&root, &r0.task_id, "max_wall_ms", 1);
        let grant = grant_of(&daemon, &r0.task_id);

        let report = daemon.pause_lapsed_leases(grant.lease.expires_ms + 1);
        assert_eq!(report.paused.len(), 1);
        assert_eq!(report.paused[0].reason, PauseReason::WallBudgetSpent);
    }

    // --- Added during integration (not in the pasted output): guards for
    // paths the F-tests reach only indirectly; each one fails under a
    // targeted mutation of the code it covers.

    /// Lapse the grant in custody's in-memory view without any sweep, the
    /// state a long-running process reaches when no keeper runs.
    fn lapse_in_memory(daemon: &HarnessDaemon, root: &Path, grant_id: &str) {
        lapse_grant_file(root, grant_id);
        *daemon.custody.lock().expect("custody lock") =
            CustodyRegistry::open(root.join("custody")).expect("reopen custody");
    }

    fn fenced_through_suspend(daemon: &HarnessDaemon, grant_id: &str) -> bool {
        let chain = daemon
            .custody
            .lock()
            .expect("custody lock")
            .audit_chain(grant_id)
            .expect("audit chain");
        let kinds: Vec<&str> = chain.iter().map(|e| e.kind.as_str()).collect();
        match (
            kinds.iter().position(|k| *k == "suspended"),
            kinds.iter().rposition(|k| *k == "resumed"),
        ) {
            (Some(s), Some(r)) => s < r,
            _ => false,
        }
    }

    #[test]
    fn x1_resume_fences_a_lapse_recorded_on_the_task() {
        let (_tmp, root, daemon) = setup();
        let r0 = daemon.execute(req("x1")).expect("create");
        rewrite_u64(&root, &r0.task_id, "max_wall_ms", DAY_MS);
        rewrite_u64(&root, &r0.task_id, "lease_expires_ms", 1);
        let grant = grant_of(&daemon, &r0.task_id);
        let handle = r0.host_resume_handle.clone().expect("handle");

        let r1 = daemon
            .execute(req_resume("x1", 1, &r0.task_id, &handle))
            .expect("resume");
        assert_eq!(
            r1.lease.epoch,
            r0.lease.epoch + 1,
            "lapse must be re-fenced"
        );
        assert!(fenced_through_suspend(&daemon, &grant.grant_id));
    }

    #[test]
    fn x2_resume_fences_an_unswept_active_lapse() {
        let (_tmp, root, daemon) = setup();
        let r0 = daemon.execute(req("x2")).expect("create");
        rewrite_u64(&root, &r0.task_id, "max_wall_ms", DAY_MS);
        let grant = grant_of(&daemon, &r0.task_id);
        let handle = r0.host_resume_handle.clone().expect("handle");
        lapse_in_memory(&daemon, &root, &grant.grant_id);

        let r1 = daemon
            .execute(req_resume("x2", 1, &r0.task_id, &handle))
            .expect("resume");
        assert_eq!(
            r1.lease.epoch,
            r0.lease.epoch + 1,
            "lapse must be re-fenced"
        );
        assert!(fenced_through_suspend(&daemon, &grant.grant_id));
    }

    #[test]
    fn x3_live_call_that_discovers_a_lapse_pauses_durably() {
        let (_tmp, root, daemon) = setup();
        let r0 = daemon.execute(req("x3")).expect("create");
        rewrite_u64(&root, &r0.task_id, "max_wall_ms", DAY_MS);
        let grant = grant_of(&daemon, &r0.task_id);
        lapse_in_memory(&daemon, &root, &grant.grant_id);

        let err = daemon
            .next(NextRequest {
                task_id: r0.task_id.clone(),
                capability: r0.task_capability.clone().expect("capability"),
                lease_epoch: r0.lease.epoch,
            })
            .expect_err("lapsed task must refuse work");
        assert_error(err, "StaleLease", "paused");
        let rec = daemon
            .read_pause_record(&r0.task_id)
            .expect("read")
            .expect("record written by the live call");
        assert_eq!(rec.status, PauseStatus::Paused);
        assert!(event_kinds(&root, &r0.task_id)
            .iter()
            .any(|k| k == "task_paused_on_lease_lapse"));
    }

    #[test]
    fn x4_keeper_pass_pauses_a_lapsed_task() {
        let (_tmp, root, daemon) = setup();
        let r0 = daemon.execute(req("x4")).expect("create");
        rewrite_u64(&root, &r0.task_id, "max_wall_ms", DAY_MS);
        let grant = grant_of(&daemon, &r0.task_id);
        lapse_in_memory(&daemon, &root, &grant.grant_id);

        let keeper = daemon
            .spawn_lease_keeper(LeaseKeeperConfig {
                tick_ms: 20,
                renew_ahead_ms: 120,
            })
            .expect("keeper");
        let path = pause_file(&root, &r0.task_id);
        wait_until(2_000, || path.exists());
        keeper.stop();
        let rec = daemon
            .read_pause_record(&r0.task_id)
            .expect("read")
            .expect("record");
        assert_eq!(rec.status, PauseStatus::Paused);
    }

    // --- Added with the fixes for design holes A and B.

    #[test]
    fn a1_refused_resume_keeps_the_host_handle() {
        let (_tmp, root, daemon) = setup();
        let r0 = daemon.execute(req("a1")).expect("create");
        rewrite_u64(&root, &r0.task_id, "max_wall_ms", DAY_MS);
        let grant = grant_of(&daemon, &r0.task_id);
        let handle = r0.host_resume_handle.clone().expect("handle");
        daemon.pause_lapsed_leases(grant.lease.expires_ms + 1);
        let path = pause_file(&root, &r0.task_id);
        let original = fs::read(&path).expect("pause bytes");

        // A forged handle is refused before the gate and touches nothing.
        let err = daemon
            .execute(req_resume("a1", 1, &r0.task_id, "hrh-forged"))
            .expect_err("forged handle");
        assert_error(err, "ScopeDenied", "mismatch");
        assert_eq!(fs::read(&path).expect("pause bytes"), original);

        // The gate refuses (drifted record); the handle must survive it.
        let mut rec: PauseRecord = serde_json::from_slice(&original).expect("pause json");
        rec.cursor += 1;
        fs::write(&path, serde_json::to_vec_pretty(&rec).expect("serialize")).expect("rewrite");
        let err = daemon
            .execute(req_resume("a1", 2, &r0.task_id, &handle))
            .expect_err("gate refuses");
        assert_error(err, "LeaseConflict", "task changed while paused");
        let after = grant_by_id(&daemon, &grant.grant_id);
        assert_eq!(
            after.phase,
            CustodyPhase::Suspended,
            "refusal must not fence the grant"
        );
        assert_eq!(after.lease.epoch, grant.lease.epoch);

        // Once the cause is gone the same handle resumes, and only then rotates.
        fs::write(&path, &original).expect("restore");
        let r1 = daemon
            .execute(req_resume("a1", 3, &r0.task_id, &handle))
            .expect("same handle resumes after a refused attempt");
        assert_eq!(r1.lease.epoch, r0.lease.epoch + 1);
        let rotated = r1.host_resume_handle.clone().expect("rotated handle");
        assert_ne!(rotated, handle);
        let err = daemon
            .execute(req_resume("a1", 4, &r0.task_id, &handle))
            .expect_err("old handle is spent after a successful resume");
        assert_error(err, "ScopeDenied", "mismatch");
    }

    #[test]
    fn b1_keeper_never_expires_a_record_a_resume_has_fenced_back() {
        let (_tmp, root, daemon) = setup();
        let r0 = daemon.execute(req("b1")).expect("create");
        rewrite_u64(&root, &r0.task_id, "max_wall_ms", DAY_MS);
        let grant = grant_of(&daemon, &r0.task_id);
        daemon.pause_lapsed_leases(grant.lease.expires_ms + 1);
        // Put the grace deadline in the past (not a fenced field), then run
        // the resume only as far as the custody fence: the window in which a
        // keeper pass used to overwrite the record.
        let path = pause_file(&root, &r0.task_id);
        let mut rec: PauseRecord =
            serde_json::from_slice(&fs::read(&path).expect("pause bytes")).expect("pause json");
        rec.resume_deadline_ms = 1;
        fs::write(&path, serde_json::to_vec_pretty(&rec).expect("serialize")).expect("rewrite");
        let mut t = daemon.load(&r0.task_id).expect("load");
        let epoch_before = daemon.resume_suspended(&mut t, now_ms()).expect("fence");

        let report = daemon.pause_lapsed_leases(now_ms());
        assert!(report.expired.is_empty(), "keeper expired a resuming task");
        let rec = daemon
            .read_pause_record(&r0.task_id)
            .expect("read")
            .expect("record");
        assert_eq!(rec.status, PauseStatus::Paused);

        let closed = daemon
            .complete_resume_after_pause(&mut t, epoch_before)
            .expect("complete")
            .expect("record closed out");
        assert_eq!(closed.status, PauseStatus::Resumed);
    }

    /// Run `op` on another thread while this thread holds the custody lock:
    /// it must not write `pause.json` until the lock is released.
    fn assert_writes_under_custody_lock(
        daemon: &HarnessDaemon,
        path: &Path,
        op: impl FnOnce() -> Result<(), ProtocolError> + Send,
    ) {
        let before = fs::read(path).ok();
        std::thread::scope(|scope| {
            let guard = daemon.custody.lock().expect("custody lock");
            let worker = scope.spawn(op);
            std::thread::sleep(Duration::from_millis(200));
            assert!(
                !worker.is_finished(),
                "pause.json writer ran without the custody lock"
            );
            assert_eq!(
                fs::read(path).ok(),
                before,
                "pause.json changed without the lock"
            );
            drop(guard);
            worker.join().expect("worker").expect("op");
        });
        assert_ne!(
            fs::read(path).ok(),
            before,
            "op should have written pause.json"
        );
    }

    #[test]
    fn b2_every_mcp_thread_pause_write_holds_the_custody_lock() {
        let (_tmp, root, daemon) = setup();
        let r0 = daemon.execute(req("b2")).expect("create");
        rewrite_u64(&root, &r0.task_id, "max_wall_ms", DAY_MS);
        let path = pause_file(&root, &r0.task_id);

        let t = daemon.load(&r0.task_id).expect("load");
        assert_writes_under_custody_lock(&daemon, &path, || {
            daemon.pause_now(&t, PauseReason::LeaseLapsed).map(|_| ())
        });

        let mut t = daemon.load(&r0.task_id).expect("load");
        assert_writes_under_custody_lock(&daemon, &path, || daemon.absorb_pause(&mut t));

        let mut t = daemon.load(&r0.task_id).expect("load");
        let epoch_before = daemon.resume_suspended(&mut t, now_ms()).expect("fence");
        assert_writes_under_custody_lock(&daemon, &path, || {
            daemon
                .complete_resume_after_pause(&mut t, epoch_before)
                .map(|_| ())
        });
        let rec = daemon
            .read_pause_record(&r0.task_id)
            .expect("read")
            .expect("record");
        assert_eq!(rec.status, PauseStatus::Resumed);
    }

    #[test]
    fn b3_concurrent_pause_writers_never_share_a_temp_file() {
        let (_tmp, root, daemon) = setup();
        let r0 = daemon.execute(req("b3")).expect("create");
        rewrite_u64(&root, &r0.task_id, "max_wall_ms", DAY_MS);
        let grant = grant_of(&daemon, &r0.task_id);
        daemon.pause_lapsed_leases(grant.lease.expires_ms + 1);
        let path = pause_file(&root, &r0.task_id);
        let base: PauseRecord =
            serde_json::from_slice(&fs::read(&path).expect("pause bytes")).expect("pause json");

        std::thread::scope(|scope| {
            for writer in 0..4u64 {
                let (path, mut rec) = (path.clone(), base.clone());
                scope.spawn(move || {
                    for i in 0..300u64 {
                        rec.paused_at_ms = (writer * 1_000 + i) as u128;
                        write_pause_file(&path, &rec).expect("every write lands");
                    }
                });
            }
        });
        let last: PauseRecord =
            serde_json::from_slice(&fs::read(&path).expect("pause bytes")).expect("never torn");
        assert_eq!(last.task_id, base.task_id);
        let leftovers = fs::read_dir(path.parent().expect("task dir"))
            .expect("task dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0, "temp files left behind");
    }
}
