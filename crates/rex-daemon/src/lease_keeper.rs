//! Background lease keeper: renew every live custody lease before it
//! expires, with zero host-agent involvement.
//!
//! Why: a host that works longer than one lease window between daemon calls
//! (a long build, a long model turn, the REXCRAFT run) used to let the
//! lease lapse, `CustodyRegistry::sweep` suspended the grant, and the task
//! stalled until a resume. The keeper closes that gap from inside the
//! daemon process.
//!
//! What it touches: the shared `CustodyRegistry` only (which persists the
//! grant file and appends a hash-chained "heartbeat" audit event on every
//! renewal) plus its own append-only ledger, `<root>/lease-keeper.jsonl`.
//!
//! What it deliberately does not do: no `task.json` or `events.jsonl`
//! writes (the MCP thread does unlocked load-modify-persist on those files,
//! and a second writer would race), no resurrection of lapsed or suspended
//! grants (verified resume owns that), and no renewal past the wall budget
//! (lapse handling there is the failsafe's job).

use rex_custody::{CapabilityToken, CustodyError, CustodyPhase, CustodyRegistry};
use rex_protocol::{ProtocolError, TaskState};
use serde::Serialize;
use serde_json::Value;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::internal;
use super::now_ms;

/// Cadence and lead time of the keeper.
#[derive(Debug, Clone, Copy)]
pub struct LeaseKeeperConfig {
    /// How often one renewal pass runs.
    pub tick_ms: u64,
    /// Renew once this much or less remains of the lease. Kept well inside
    /// the lease so several ticks of slack remain before expiry.
    pub renew_ahead_ms: u64,
}

impl Default for LeaseKeeperConfig {
    fn default() -> Self {
        Self {
            tick_ms: 10_000,
            renew_ahead_ms: 120_000,
        }
    }
}

/// One lease extended by the keeper, before expiry and without the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenewedLease {
    pub task_id: String,
    pub grant_id: String,
    pub epoch: u64,
    pub previous_expires_ms: u128,
    pub expires_ms: u128,
}

/// Outcome of one renewal pass.
#[derive(Debug, Clone, Default)]
pub struct LeaseRenewalReport {
    /// Leases extended this pass, in scan order.
    pub renewed: Vec<RenewedLease>,
    /// Task records that needed no renewal (terminal, wall budget spent,
    /// lapsed, suspended, or not yet due) plus unreadable records.
    pub skipped: usize,
    /// Renewals or ledger writes that failed. A pass never panics.
    pub errors: Vec<String>,
}

/// One line of the keeper's own ledger, `<root>/lease-keeper.jsonl`.
#[derive(Serialize)]
struct LedgerLine<'a> {
    ts_ms: u128,
    task_id: &'a str,
    grant_id: &'a str,
    epoch: u64,
    previous_expires_ms: u128,
    expires_ms: u128,
    actor: &'a str,
}

/// The read-only slice of a task record a pass needs.
struct TaskFacts {
    task_id: String,
    terminal: bool,
    created_ms: u128,
    max_wall_ms: u64,
}

/// One renewal pass over `<root>/tasks/*/task.json` at `now_ms`.
fn renew_due(
    root: &Path,
    custody: &Mutex<CustodyRegistry>,
    cfg: &LeaseKeeperConfig,
    now_ms: u128,
) -> LeaseRenewalReport {
    let mut report = LeaseRenewalReport::default();
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
    for dir in dirs {
        let facts = match read_facts(&dir.join("task.json")) {
            Some(facts) => facts,
            None => {
                report.skipped += 1;
                continue;
            }
        };
        if facts.terminal || now_ms >= facts.created_ms + facts.max_wall_ms as u128 {
            report.skipped += 1;
            continue;
        }
        let mut reg = match custody.lock() {
            Ok(reg) => reg,
            Err(_) => {
                report.errors.push("custody registry poisoned".to_string());
                break;
            }
        };
        let outcome = renew_locked(&mut reg, &facts, now_ms, cfg.renew_ahead_ms);
        drop(reg);
        match outcome {
            Ok(Some(renewed)) => {
                if let Err(e) = append_ledger(root, &renewed, now_ms) {
                    report
                        .errors
                        .push(format!("{}: ledger write failed: {e}", facts.task_id));
                }
                report.renewed.push(renewed);
            }
            Ok(None) => report.skipped += 1,
            Err(e) => report.errors.push(format!("{}: {e}", facts.task_id)),
        }
    }
    report
}

fn read_facts(path: &Path) -> Option<TaskFacts> {
    let bytes = fs::read(path).ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    let task_id = v.get("task_id")?.as_str()?.to_string();
    let state: TaskState = serde_json::from_value(v.get("state")?.clone()).ok()?;
    let created_ms = v.get("created_ms")?.as_u64()? as u128;
    let max_wall_ms = v.get("max_wall_ms")?.as_u64()?;
    Some(TaskFacts {
        task_id,
        terminal: state.is_terminal(),
        created_ms,
        max_wall_ms,
    })
}

fn renew_locked(
    reg: &mut CustodyRegistry,
    facts: &TaskFacts,
    now_ms: u128,
    renew_ahead_ms: u64,
) -> Result<Option<RenewedLease>, CustodyError> {
    let grant = match reg.grant_for_task(&facts.task_id) {
        Some(grant) => grant.clone(),
        None => return Ok(None),
    };
    if grant.phase != CustodyPhase::Active || !grant.lease.is_live(now_ms) {
        return Ok(None);
    }
    if grant.lease.expires_ms.saturating_sub(now_ms) > renew_ahead_ms as u128 {
        return Ok(None);
    }
    let previous_expires_ms = grant.lease.expires_ms;
    let token = CapabilityToken {
        grant_id: grant.grant_id.clone(),
        epoch: grant.lease.epoch,
        secret: grant.token_secret.clone(),
        scope_hash: grant.capabilities.scope_hash(),
    };
    let lease = reg.heartbeat(&token, grant.lease.next_seq, now_ms)?;
    Ok(Some(RenewedLease {
        task_id: facts.task_id.clone(),
        grant_id: grant.grant_id,
        epoch: grant.lease.epoch,
        previous_expires_ms,
        expires_ms: lease.expires_ms,
    }))
}

fn append_ledger(root: &Path, renewed: &RenewedLease, ts_ms: u128) -> Result<(), String> {
    let line = LedgerLine {
        ts_ms,
        task_id: &renewed.task_id,
        grant_id: &renewed.grant_id,
        epoch: renewed.epoch,
        previous_expires_ms: renewed.previous_expires_ms,
        expires_ms: renewed.expires_ms,
        actor: "lease_keeper",
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

impl super::HarnessDaemon {
    /// Run one renewal pass with an injected clock, so tests and the
    /// desktop UI can drive a pass deterministically.
    pub fn renew_due_leases(&self, cfg: &LeaseKeeperConfig, now_ms: u128) -> LeaseRenewalReport {
        renew_due(&self.root, &self.custody, cfg, now_ms)
    }

    /// Start the background keeper thread. Dropping the returned handle
    /// always stops it.
    ///
    /// Lease upkeep is single-owner: `<root>/lease-keeper.lock` is held with an
    /// advisory `flock` for the life of the thread, so one process per state
    /// directory renews and sweeps. A second process starts in standby and
    /// takes over the moment the owner stops.
    pub fn spawn_lease_keeper(&self, cfg: LeaseKeeperConfig) -> Result<LeaseKeeper, ProtocolError> {
        let root = self.root.clone();
        let custody = Arc::clone(&self.custody);
        assert_send(&root);
        assert_send(&custody);
        let tick = Duration::from_millis(cfg.tick_ms.max(1));
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(root.join("lease-keeper.lock"))
            .map_err(internal)?;
        let (stop, stopped) = mpsc::channel::<()>();
        let owner = Arc::new(AtomicBool::new(false));
        let owner_flag = Arc::clone(&owner);
        let handle = thread::Builder::new()
            .name("rex-lease-keeper".into())
            .spawn(move || keeper_loop(root, custody, cfg, stopped, tick, lock, owner_flag))
            .map_err(internal)?;
        Ok(LeaseKeeper {
            stop: Some(stop),
            handle: Some(handle),
            owner,
        })
    }

    /// Copy the live lease view from custody into a freshly loaded task
    /// record.
    pub(crate) fn sync_lease_from_custody(
        &self,
        t: &mut super::DurableTask,
    ) -> Result<(), ProtocolError> {
        let synced = {
            let reg = self
                .custody
                .lock()
                .map_err(|_| internal("custody registry poisoned"))?;
            reg.grant(&t.grant_id)
                .filter(|g| g.lease.epoch == t.lease_epoch)
                .map(|g| (g.lease.expires_ms, g.lease.next_seq))
        };
        if let Some((expires_ms, next_seq)) = synced {
            t.lease_expires_ms = expires_ms;
            t.heartbeat_seq = next_seq;
        }
        Ok(())
    }
}

pub struct LeaseKeeper {
    stop: Option<Sender<()>>,
    handle: Option<JoinHandle<()>>,
    owner: Arc<AtomicBool>,
}

impl LeaseKeeper {
    /// Whether this keeper holds lease upkeep for the state directory right
    /// now. False while standing by behind another process's lock.
    pub fn is_owner(&self) -> bool {
        self.owner.load(Ordering::SeqCst)
    }

    /// Stop the keeper thread and wait for it to finish.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for LeaseKeeper {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn assert_send<T: Send>(_: &T) {}

fn keeper_loop(
    root: PathBuf,
    custody: Arc<Mutex<CustodyRegistry>>,
    cfg: LeaseKeeperConfig,
    stopped: Receiver<()>,
    tick: Duration,
    lock: File,
    owner: Arc<AtomicBool>,
) {
    let mut owns_upkeep = false;
    let mut standby_logged = false;
    loop {
        if !owns_upkeep {
            match lock.try_lock() {
                Ok(()) => {
                    owns_upkeep = true;
                    owner.store(true, Ordering::SeqCst);
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    if !standby_logged {
                        standby_logged = true;
                        eprintln!(
                            "rex-lease-keeper: another process owns lease upkeep for {}; standing by",
                            root.display()
                        );
                    }
                }
                Err(_) => {
                    if !standby_logged {
                        standby_logged = true;
                        eprintln!(
                            "rex-lease-keeper: lease upkeep lock unusable for {}; standing by",
                            root.display()
                        );
                    }
                }
            }
        }
        if owns_upkeep {
            let now = now_ms();
            let report = renew_due(&root, &custody, &cfg, now);
            for err in &report.errors {
                eprintln!("rex-lease-keeper: {err}");
            }
            let failsafe = crate::lease_failsafe::pause_lapsed(&root, &custody, now);
            for err in &failsafe.errors {
                eprintln!("rex-lease-keeper: {err}");
            }
        }
        match stopped.recv_timeout(tick) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                owner.store(false, Ordering::SeqCst);
                return;
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{DaemonPolicy, HarnessDaemon};
    use rex_custody::CustodyGrant;
    use rex_protocol::packets::OperationStatus;
    use rex_protocol::{
        CancelRequest, ExecuteRequest, HostKind, NextRequest, PlanStep, TaskRefRequest, TaskState,
    };
    use serde_json::Value;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::thread;
    use std::time::{Duration, Instant};
    use tempfile::tempdir;

    const DAY_MS: u64 = 24 * 60 * 60 * 1000;

    fn req(id: &str) -> ExecuteRequest {
        ExecuteRequest {
            request_id: id.into(),
            task: "make hello".into(),
            mobile_result_fields: None,
            task_id: None,
            resume_handle: None,
            follow_up: None,
            host: HostKind::ClaudeCode,
            operator_is_agent: true,
            ultra: false,
            budgets: None,
            proof: None,
            plan: Some(vec![PlanStep {
                instructions: "write hello".into(),
                acceptance: Some("file exists".into()),
            }]),
        }
    }

    fn setup() -> (tempfile::TempDir, PathBuf, HarnessDaemon) {
        let d = tempdir().unwrap();
        let w = d.path().join("ws");
        let root = d.path().join("state");
        let daemon = HarnessDaemon::open(&root, DaemonPolicy::conservative(&w)).unwrap();
        (d, root, daemon)
    }

    fn grant_of(daemon: &HarnessDaemon, task_id: &str) -> CustodyGrant {
        let reg = daemon.custody.lock().unwrap();
        reg.grant_for_task(task_id).unwrap().clone()
    }

    fn suspend_grant(daemon: &HarnessDaemon, grant_id: &str, now: u128) {
        let mut reg = daemon.custody.lock().unwrap();
        let suspended = reg.suspend(grant_id, "test", now);
        suspended.unwrap();
    }

    fn rewrite_u64(root: &Path, task_id: &str, field: &str, value: u64) {
        let file = root.join("tasks").join(task_id).join("task.json");
        let mut v: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        let obj = v.as_object_mut().unwrap();
        obj.insert(field.into(), Value::from(value));
        fs::write(&file, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    }

    fn ledger_has_line(path: &Path) -> bool {
        let text = fs::read_to_string(path).unwrap_or_default();
        text.lines().any(|l| l.contains("lease_keeper"))
    }

    #[test]
    fn renews_before_expiry_without_any_agent_call() {
        let (_d, root, daemon) = setup();
        let ex = daemon.execute(req("keeper-1")).unwrap();
        let task_id = ex.task_id;
        rewrite_u64(&root, &task_id, "max_wall_ms", DAY_MS);
        let grant = grant_of(&daemon, &task_id);
        let epoch = grant.lease.epoch;
        let original_expiry = grant.lease.expires_ms;
        let cfg = LeaseKeeperConfig::default();
        let mut expiry = original_expiry;
        for pass in 0..3u32 {
            let now = expiry - 60_000;
            let report = daemon.renew_due_leases(&cfg, now);
            assert_eq!(report.renewed.len(), 1, "pass {pass}: {report:?}");
            assert!(report.errors.is_empty(), "pass {pass}: {report:?}");
            assert_eq!(report.skipped, 0, "pass {pass}");
            let renewed = &report.renewed[0];
            assert_eq!(renewed.task_id, task_id);
            assert_eq!(renewed.grant_id, grant.grant_id);
            assert_eq!(renewed.epoch, epoch);
            assert_eq!(renewed.previous_expires_ms, expiry);
            let expected = now + crate::DEFAULT_LEASE_MS as u128;
            assert_eq!(renewed.expires_ms, expected);
            assert!(renewed.expires_ms > expiry);
            expiry = renewed.expires_ms;
        }
        let later = original_expiry + 600_001;
        assert!(later < expiry);
        let grant = grant_of(&daemon, &task_id);
        assert_eq!(grant.lease.epoch, epoch);
        assert!(grant.lease.is_live(later));
        {
            let mut reg = daemon.custody.lock().unwrap();
            reg.sweep(later).unwrap();
        }
        let grant = grant_of(&daemon, &task_id);
        assert_eq!(grant.phase, CustodyPhase::Active);
        let ledger = fs::read_to_string(root.join("lease-keeper.jsonl")).unwrap();
        assert_eq!(ledger.lines().count(), 3);
        for line in ledger.lines() {
            assert!(line.contains("\"actor\":\"lease_keeper\""), "{line}");
        }
    }

    #[test]
    fn not_due_is_left_alone() {
        let (_d, root, daemon) = setup();
        let ex = daemon.execute(req("keeper-2")).unwrap();
        let task_id = ex.task_id;
        rewrite_u64(&root, &task_id, "max_wall_ms", DAY_MS);
        let grant = grant_of(&daemon, &task_id);
        let seq = grant.lease.next_seq;
        let expiry = grant.lease.expires_ms;
        let cfg = LeaseKeeperConfig::default();
        let report = daemon.renew_due_leases(&cfg, expiry - 200_000);
        assert!(report.renewed.is_empty(), "{report:?}");
        assert!(report.errors.is_empty(), "{report:?}");
        assert_eq!(report.skipped, 1);
        let grant = grant_of(&daemon, &task_id);
        assert_eq!(grant.lease.next_seq, seq);
        assert_eq!(grant.lease.expires_ms, expiry);
    }

    #[test]
    fn agent_call_after_auto_renewal_still_works() {
        let (_d, root, daemon) = setup();
        let ex = daemon.execute(req("keeper-3")).unwrap();
        let task_id = ex.task_id;
        rewrite_u64(&root, &task_id, "max_wall_ms", DAY_MS);
        let epoch = ex.lease.epoch;
        let capability = ex.task_capability.unwrap();
        let before = grant_of(&daemon, &task_id);
        let cfg = LeaseKeeperConfig {
            tick_ms: 10_000,
            renew_ahead_ms: 300_000,
        };
        let report = daemon.renew_due_leases(&cfg, crate::now_ms());
        assert_eq!(report.renewed.len(), 1, "{report:?}");
        assert!(report.errors.is_empty(), "{report:?}");
        let after = grant_of(&daemon, &task_id);
        assert!(after.lease.next_seq > before.lease.next_seq);
        let next_req = NextRequest {
            task_id: task_id.clone(),
            capability,
            lease_epoch: epoch,
        };
        let n = daemon.next(next_req).unwrap();
        assert_eq!(n.state, TaskState::Active);
        assert!(n.lease.expires_ms_from_now > 0);
        let status_req = TaskRefRequest { task_id };
        let s = daemon.status(status_req).unwrap();
        assert!(!matches!(s.operation, OperationStatus::Stale));
        assert!(s.lease.expires_ms_from_now > 0);
    }

    #[test]
    fn stale_task_cache_does_not_expire_a_renewed_lease() {
        // The keeper never writes task.json, so its cached expiry can fall
        // behind custody. live() and status() must judge the custody view.
        let (_d, root, daemon) = setup();
        let ex = daemon.execute(req("keeper-7")).unwrap();
        let task_id = ex.task_id;
        rewrite_u64(&root, &task_id, "max_wall_ms", DAY_MS);
        let cfg = LeaseKeeperConfig {
            tick_ms: 10_000,
            renew_ahead_ms: 300_000,
        };
        let report = daemon.renew_due_leases(&cfg, crate::now_ms());
        assert_eq!(report.renewed.len(), 1, "{report:?}");
        rewrite_u64(&root, &task_id, "lease_expires_ms", 1);
        let status_req = TaskRefRequest {
            task_id: task_id.clone(),
        };
        let s = daemon.status(status_req).unwrap();
        assert!(!matches!(s.operation, OperationStatus::Stale));
        let next_req = NextRequest {
            task_id,
            capability: ex.task_capability.unwrap(),
            lease_epoch: ex.lease.epoch,
        };
        let n = daemon.next(next_req).unwrap();
        assert_eq!(n.state, TaskState::Active);
    }

    #[test]
    fn terminal_suspended_and_spent_tasks_are_skipped() {
        let (_d, root, daemon) = setup();
        let a = daemon.execute(req("keeper-4a")).unwrap();
        let a_cap = a.task_capability.unwrap();
        let a_id = a.task_id;
        let cancel_req = CancelRequest {
            task_id: a_id,
            capability: a_cap,
            reason: Some("test".into()),
        };
        daemon.cancel(cancel_req).unwrap();
        let b = daemon.execute(req("keeper-4b")).unwrap();
        let b_id = b.task_id;
        let b_grant = grant_of(&daemon, &b_id);
        suspend_grant(&daemon, &b_grant.grant_id, crate::now_ms());
        let c = daemon.execute(req("keeper-4c")).unwrap();
        let c_id = c.task_id;
        rewrite_u64(&root, &c_id, "max_wall_ms", 1);

        let cfg = LeaseKeeperConfig {
            tick_ms: 10_000,
            renew_ahead_ms: 300_000,
        };
        let report = daemon.renew_due_leases(&cfg, crate::now_ms());
        assert!(report.renewed.is_empty(), "{report:?}");
        assert!(report.errors.is_empty(), "{report:?}");
        assert_eq!(report.skipped, 3);
        let b_after = grant_of(&daemon, &b_id);
        assert_eq!(b_after.phase, CustodyPhase::Suspended);
        assert_eq!(b_after.lease.next_seq, b_grant.lease.next_seq);
    }

    #[test]
    fn background_thread_renews_and_stops_promptly() {
        let (_d, root, daemon) = setup();
        let ex = daemon.execute(req("keeper-5")).unwrap();
        let task_id = ex.task_id;
        rewrite_u64(&root, &task_id, "max_wall_ms", DAY_MS);
        let before = grant_of(&daemon, &task_id);
        let cfg = LeaseKeeperConfig {
            tick_ms: 20,
            renew_ahead_ms: 300_000,
        };
        let keeper = daemon.spawn_lease_keeper(cfg).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let ledger = root.join("lease-keeper.jsonl");
        let mut renewed = false;
        while Instant::now() < deadline {
            let after = grant_of(&daemon, &task_id);
            if after.lease.expires_ms > before.lease.expires_ms || ledger_has_line(&ledger) {
                renewed = true;
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(renewed, "keeper did not renew within 5s");
        let started = Instant::now();
        keeper.stop();
        let took = started.elapsed();
        assert!(took < Duration::from_secs(1), "stop took {took:?}");
    }

    #[test]
    fn corrupt_task_file_does_not_panic() {
        let (_d, root, daemon) = setup();
        let ex = daemon.execute(req("keeper-6")).unwrap();
        let task_id = ex.task_id;
        rewrite_u64(&root, &task_id, "max_wall_ms", DAY_MS);
        let bogus = root.join("tasks").join("bogus");
        fs::create_dir_all(&bogus).unwrap();
        fs::write(bogus.join("task.json"), "not json").unwrap();
        let cfg = LeaseKeeperConfig {
            tick_ms: 10_000,
            renew_ahead_ms: 300_000,
        };
        let report = daemon.renew_due_leases(&cfg, crate::now_ms());
        assert_eq!(report.renewed.len(), 1);
        assert_eq!(report.renewed[0].task_id, task_id);
        assert_eq!(report.skipped, 1, "{report:?}");
        assert!(report.errors.is_empty(), "{report:?}");
    }
}
