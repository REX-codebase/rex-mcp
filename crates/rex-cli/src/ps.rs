//! Background/subagent dashboard: `rex ps`.
//!
//! The run ledger (`runs.jsonl`) is append-only history: a receipt lands
//! there only when a run finishes. It cannot show in-flight runs without
//! rewriting history, so live state lives in small marker files instead:
//! `$REX_STATE_DIR/active/<run_id>.json`, one per begun run.
//!
//! `execute()` writes the marker right after `begin` and removes it when
//! the final receipt is recorded; the drive loop refreshes `heartbeat_at`
//! every 30 seconds. A marker whose heartbeat is older than 120 seconds is
//! *stale* — its process is presumed dead. `rex ps` shows stale markers
//! honestly (never silently counting them as live), and the parallelism
//! cap counts only fresh ones. `rex ps --prune` drops stale markers.

use crate::exec::ExecError;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Heartbeat older than this means the owning process is presumed dead.
const STALE_AFTER: Duration = Duration::from_secs(120);
/// How often the drive loop refreshes the heartbeat.
pub const HEARTBEAT_EVERY: Duration = Duration::from_secs(30);

fn active_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("active")
}

/// Marker file for a run. The run id comes from the provider service; it is
/// sanitized defensively so a hostile id can never escape the active dir.
pub fn marker_path(state_dir: &Path, run_id: &str) -> PathBuf {
    let safe: String = run_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    active_dir(state_dir).join(format!("{safe}.json"))
}

#[derive(Debug, Clone)]
pub struct ActiveRun {
    pub run_id: String,
    pub task: String,
    pub provider: String,
    pub model: Option<String>,
    pub name: Option<String>,
    pub started_at: String,
    pub heartbeat_at: String,
    pub detached: bool,
    pub pid: u32,
    pub log: Option<String>,
    pub deadman: Option<Value>,
    pub nonce: Option<String>,
    /// Computed at read time: heartbeat older than STALE_AFTER.
    pub stale: bool,
}

pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn heartbeat_age_secs(heartbeat_at: &str) -> Option<u64> {
    let beat = chrono::DateTime::parse_from_rfc3339(heartbeat_at).ok()?;
    let age = chrono::Utc::now().signed_duration_since(beat.with_timezone(&chrono::Utc));
    Some(age.num_seconds().max(0) as u64)
}

fn is_fresh(heartbeat_at: &str) -> bool {
    heartbeat_age_secs(heartbeat_at).is_some_and(|age| age < STALE_AFTER.as_secs())
}

fn parse_marker(v: &Value) -> Option<ActiveRun> {
    let heartbeat_at = v.get("heartbeat_at")?.as_str()?.to_string();
    Some(ActiveRun {
        run_id: v.get("run_id")?.as_str()?.to_string(),
        task: v
            .get("task")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        provider: v
            .get("provider")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string(),
        model: v.get("model").and_then(Value::as_str).map(str::to_string),
        name: v.get("name").and_then(Value::as_str).map(str::to_string),
        started_at: v
            .get("started_at")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        heartbeat_at: heartbeat_at.clone(),
        detached: v.get("detached").and_then(Value::as_bool).unwrap_or(false),
        pid: v.get("pid").and_then(Value::as_u64).unwrap_or(0) as u32,
        log: v.get("log").and_then(Value::as_str).map(str::to_string),
        deadman: v.get("deadman").cloned(),
        nonce: v.get("nonce").and_then(Value::as_str).map(str::to_string),
        stale: !is_fresh(&heartbeat_at),
    })
}

impl ActiveRun {
    fn to_json(&self) -> Value {
        json!({
            "run_id": self.run_id,
            "task": self.task,
            "provider": self.provider,
            "model": self.model,
            "name": self.name,
            "status": if self.stale { "stale" } else { "running" },
            "stale": self.stale,
            "started_at": self.started_at,
            "heartbeat_at": self.heartbeat_at,
            "elapsed_secs": elapsed_secs(&self.started_at),
            "detached": self.detached,
            "pid": self.pid,
            "log": self.log,
            "deadman": deadman_json(self.deadman.as_ref()),
        })
    }
}

/// Best-effort: a marker failure must never fail the run it tracks.
pub fn write_marker(state_dir: &Path, run: &ActiveRun) {
    let path = marker_path(state_dir, &run.run_id);
    let v = json!({
        "run_id": run.run_id,
        "task": run.task,
        "provider": run.provider,
        "model": run.model,
        "name": run.name,
        "started_at": run.started_at,
        "heartbeat_at": run.heartbeat_at,
        "detached": run.detached,
        "pid": run.pid,
        "log": run.log,
        "deadman": run.deadman,
        "nonce": run.nonce,
    });
    if let Err(e) = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(active_dir(state_dir))?;
        std::fs::write(&path, serde_json::to_string(&v).unwrap())?;
        Ok(())
    })() {
        eprintln!("rex: warning: cannot write active-run marker: {e}");
    }
}

/// Refresh just the heartbeat, keeping the rest of the marker intact.
/// Missing marker (e.g. pruned mid-run) is not an error — the run
/// continues, it just goes quiet on the dashboard.
pub fn refresh_heartbeat(state_dir: &Path, run_id: &str) {
    let path = marker_path(state_dir, run_id);
    let mut v: Value = match std::fs::read_to_string(&path)
        .ok()
        .and_then(|r| serde_json::from_str(&r).ok())
    {
        Some(v) => v,
        None => return,
    };
    v["heartbeat_at"] = Value::String(now_rfc3339());
    if std::fs::write(&path, serde_json::to_string(&v).unwrap()).is_err() {
        // Best-effort; the next refresh will retry.
    }
}

pub fn remove_marker(state_dir: &Path, run_id: &str) {
    let path = marker_path(state_dir, run_id);
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!("rex: warning: cannot remove active-run marker: {e}"),
    }
}

/// All parseable markers, newest first. Corrupt files are skipped with a
/// warning — the dashboard degrades, it never lies.
pub fn active_runs(state_dir: &Path) -> Vec<ActiveRun> {
    let dir = active_dir(state_dir);
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let raw = match std::fs::read_to_string(&path) {
            Ok(r) => r,
            Err(_) => continue,
        };
        match serde_json::from_str::<Value>(&raw)
            .ok()
            .and_then(|v| parse_marker(&v))
        {
            Some(r) => out.push(r),
            None => eprintln!(
                "rex: warning: skipping unreadable active-run marker {}",
                path.display()
            ),
        }
    }
    out.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    out
}

/// Find the marker the detached parent is waiting for.
pub fn find_by_nonce(state_dir: &Path, nonce: &str) -> Option<ActiveRun> {
    active_runs(state_dir)
        .into_iter()
        .find(|r| r.nonce.as_deref() == Some(nonce))
}

/// Drop stale markers. Returns how many were removed.
pub fn prune_stale(state_dir: &Path) -> usize {
    let stale: Vec<ActiveRun> = active_runs(state_dir)
        .into_iter()
        .filter(|r| r.stale)
        .collect();
    let n = stale.len();
    for r in &stale {
        remove_marker(state_dir, &r.run_id);
    }
    n
}

/// What the detached parent tells the child through the environment:
/// a nonce so the parent can find the child's active-run marker, and the
/// log path so the marker can point `rex ps` at the child's output.
#[derive(Debug, Clone)]
pub struct DetachInfo {
    pub detached: bool,
    pub nonce: Option<String>,
    pub log: Option<String>,
}

impl DetachInfo {
    pub fn from_env() -> Self {
        let nonce = std::env::var("REX_DETACH_NONCE")
            .ok()
            .filter(|s| !s.is_empty());
        let log = std::env::var("REX_DETACH_LOG")
            .ok()
            .filter(|s| !s.is_empty());
        Self {
            detached: nonce.is_some(),
            nonce,
            log,
        }
    }
}

/// Unguessable-enough nonce for matching parent to child marker. Not a
/// security boundary — just a correlation id.
pub fn new_nonce() -> String {
    let nanos = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}-{:x}", std::process::id())
}

/// Fresh (non-stale) active runs: the set the parallelism cap counts.
pub fn live_runs(state_dir: &Path) -> Vec<ActiveRun> {
    active_runs(state_dir)
        .into_iter()
        .filter(|r| !r.stale)
        .collect()
}

fn elapsed_secs(started_at: &str) -> Option<u64> {
    let start = chrono::DateTime::parse_from_rfc3339(started_at).ok()?;
    let d = chrono::Utc::now().signed_duration_since(start.with_timezone(&chrono::Utc));
    Some(d.num_seconds().max(0) as u64)
}

pub fn fmt_elapsed(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

fn truncate_chars(s: &str, n: usize) -> String {
    let count = s.chars().count();
    if count <= n {
        return s.to_string();
    }
    let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Human line for the dead-man column: armed + last check-in age, or
/// TRIPPED. The check-in file's mtime is the last check-in — same signal
/// the drive loop polls.
fn deadman_status(deadman: Option<&Value>) -> String {
    let dm = match deadman {
        Some(d) => d,
        None => return "—".to_string(),
    };
    let mins = dm.get("mins").and_then(Value::as_u64).unwrap_or(0);
    if dm.get("tripped").and_then(Value::as_bool).unwrap_or(false) {
        return "TRIPPED".to_string();
    }
    match dm.get("checkin_file").and_then(Value::as_str) {
        Some(f) => match std::fs::metadata(f).and_then(|m| m.modified()) {
            Ok(mtime) => {
                let age = SystemTime::now()
                    .duration_since(mtime)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                format!("armed {mins}m · {} ago", fmt_elapsed(age))
            }
            Err(_) => format!("armed {mins}m · no checkin"),
        },
        None => format!("armed {mins}m"),
    }
}

fn deadman_json(deadman: Option<&Value>) -> Value {
    let dm = match deadman {
        Some(d) => d,
        None => return Value::Null,
    };
    let mut out = dm.clone();
    if let Some(f) = dm.get("checkin_file").and_then(Value::as_str) {
        out["last_checkin_secs_ago"] = match std::fs::metadata(f).and_then(|m| m.modified()) {
            Ok(mtime) => SystemTime::now()
                .duration_since(mtime)
                .map(|d| json!(d.as_secs()))
                .unwrap_or(Value::Null),
            Err(_) => Value::Null,
        };
    }
    out
}

/// The dashboard payload, shared by the human table and `--json`.
pub fn ps_payload(state_dir: &Path, with_recent: bool, recent_limit: usize) -> Value {
    let active: Vec<Value> = active_runs(state_dir)
        .iter()
        .map(ActiveRun::to_json)
        .collect();
    let mut out = json!({ "active": active });
    if with_recent {
        let mut entries = crate::ledger::read_all(state_dir);
        entries.reverse(); // newest first
        let recent: Vec<Value> = entries
            .into_iter()
            .filter(|v| v.get("finished_at").and_then(Value::as_str).is_some())
            .take(recent_limit)
            .map(|v| {
                json!({
                    "run_id": v.get("run_id").and_then(Value::as_str).unwrap_or("?"),
                    "name": v.get("name").and_then(Value::as_str),
                    "task": v.get("task").and_then(Value::as_str).unwrap_or(""),
                    "provider": v.get("provider").and_then(Value::as_str).unwrap_or("?"),
                    "model": v.get("model").and_then(Value::as_str),
                    "status": v.get("status").and_then(Value::as_str).unwrap_or("?"),
                    "elapsed_ms": v.get("elapsed_ms").and_then(Value::as_u64).unwrap_or(0),
                    "finished_at": v.get("finished_at").and_then(Value::as_str).unwrap_or(""),
                })
            })
            .collect();
        out["recent"] = Value::Array(recent);
    }
    out
}

fn print_table(state_dir: &Path, with_recent: bool, recent_limit: usize) {
    let runs = active_runs(state_dir);
    let (live, stale): (Vec<_>, Vec<_>) = runs.into_iter().partition(|r| !r.stale);
    let cap = crate::config::load(state_dir).max_parallel_runs;
    if live.is_empty() && stale.is_empty() {
        println!("no active runs.");
    } else {
        println!("ACTIVE RUNS ({}/{} slots used)", live.len(), cap);
        if !live.is_empty() {
            println!(
                "{:<10} {:<12} {:<42} {:<26} {:>7}  {:<28} BG",
                "RUN", "NAME", "TASK", "PROVIDER/MODEL", "ELAPSED", "DEADMAN"
            );
            for r in &live {
                let id: String = r.run_id.chars().take(8).collect();
                let name = r.name.as_deref().unwrap_or("—");
                let task = truncate_chars(&r.task.replace('\n', " "), 42);
                let pm = match &r.model {
                    Some(m) => format!("{}/{}", r.provider, m),
                    None => r.provider.clone(),
                };
                let pm = truncate_chars(&pm, 26);
                let elapsed = elapsed_secs(&r.started_at)
                    .map(fmt_elapsed)
                    .unwrap_or_else(|| "?".to_string());
                let dm = deadman_status(r.deadman.as_ref());
                let bg = if r.detached { "yes" } else { "no" };
                println!(
                    "{:<10} {:<12} {:<42} {:<26} {:>7}  {:<28} {}",
                    id,
                    truncate_chars(name, 12),
                    task,
                    pm,
                    elapsed,
                    truncate_chars(&dm, 28),
                    bg
                );
            }
        }
        if !stale.is_empty() {
            println!("\nSTALE (process may have died — `rex ps --prune` to clear):");
            for r in &stale {
                let id: String = r.run_id.chars().take(8).collect();
                let name = r.name.as_deref().unwrap_or("—");
                println!("  {id}  {name}  started {}", r.started_at);
            }
        }
    }
    if with_recent {
        let mut entries = crate::ledger::read_all(state_dir);
        entries.reverse();
        let recent: Vec<&Value> = entries
            .iter()
            .filter(|v| v.get("finished_at").and_then(Value::as_str).is_some())
            .take(recent_limit)
            .collect();
        println!();
        if recent.is_empty() {
            println!("no finished runs recorded yet.");
        } else {
            println!("RECENT ({}):", recent.len());
            for v in recent {
                let id: String = v
                    .get("run_id")
                    .and_then(Value::as_str)
                    .map(|s| s.chars().take(8).collect())
                    .unwrap_or_else(|| "?".to_string());
                let status = v.get("status").and_then(Value::as_str).unwrap_or("?");
                let ms = v.get("elapsed_ms").and_then(Value::as_u64).unwrap_or(0);
                let task = v
                    .get("task")
                    .and_then(Value::as_str)
                    .map(|t| truncate_chars(&t.replace('\n', " "), 50))
                    .unwrap_or_default();
                let name = v
                    .get("name")
                    .and_then(Value::as_str)
                    .map(|n| format!("[{n}] "))
                    .unwrap_or_default();
                println!(
                    "  {id}  [{status}]  {}  {name}{task}",
                    fmt_elapsed(ms / 1000)
                );
            }
        }
    }
}

/// `rex ps [--all] [--json] [--prune] [--limit N]`: the background-run
/// dashboard. Exit 0 even when empty — an empty dashboard is information,
/// not an error.
pub fn run_ps(args: &[String]) -> Result<i32, ExecError> {
    let mut with_recent = false;
    let mut as_json = false;
    let mut prune = false;
    let mut limit: usize = 10;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--all" => with_recent = true,
            "--json" => as_json = true,
            "--prune" => prune = true,
            "--limit" => {
                i += 1;
                limit = args.get(i).and_then(|s| s.parse().ok()).ok_or_else(|| {
                    ExecError::usage("usage: rex ps [--all] [--json] [--prune] [--limit N]")
                })?;
            }
            "--help" | "-h" => {
                return Err(ExecError::usage(
                    "usage: rex ps [--all] [--json] [--prune] [--limit N]",
                ))
            }
            other => {
                return Err(ExecError::usage(format!(
                    "unknown flag '{other}': usage: rex ps [--all] [--json] [--prune] [--limit N]"
                )))
            }
        }
        i += 1;
    }
    let state = crate::exec::state_dir();
    if prune {
        let n = prune_stale(&state);
        eprintln!(
            "rex: pruned {n} stale run marker{}",
            if n == 1 { "" } else { "s" }
        );
    }
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&ps_payload(&state, with_recent, limit)).unwrap()
        );
    } else {
        print_table(&state, with_recent, limit);
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static PS_ENV_LOCK: Mutex<()> = Mutex::new(());

    fn fake_state(tag: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("rex-ps-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    fn fake_run(id: &str, heartbeat_at: &str) -> ActiveRun {
        ActiveRun {
            run_id: id.to_string(),
            task: "do things".to_string(),
            provider: "anthropic".to_string(),
            model: Some("sonnet-5".to_string()),
            name: Some("alpha".to_string()),
            started_at: now_rfc3339(),
            heartbeat_at: heartbeat_at.to_string(),
            detached: true,
            pid: 1234,
            log: Some("/tmp/x.log".to_string()),
            deadman: None,
            nonce: Some("nonce-1".to_string()),
            stale: false,
        }
    }

    fn with_state_dir<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
        let _guard = PS_ENV_LOCK.lock().unwrap();
        let prev = std::env::var_os("REX_STATE_DIR");
        std::env::set_var("REX_STATE_DIR", dir);
        let out = f();
        match prev {
            Some(v) => std::env::set_var("REX_STATE_DIR", v),
            None => std::env::remove_var("REX_STATE_DIR"),
        }
        out
    }

    #[test]
    fn marker_roundtrip_and_freshness() {
        let state = fake_state("roundtrip");
        assert!(active_runs(&state).is_empty());
        write_marker(&state, &fake_run("run-1", &now_rfc3339()));
        let runs = active_runs(&state);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run_id, "run-1");
        assert!(!runs[0].stale, "fresh heartbeat must not be stale");
        assert_eq!(runs[0].name.as_deref(), Some("alpha"));
        assert!(runs[0].detached);

        // An ancient heartbeat reads back as stale.
        write_marker(&state, &fake_run("run-1", "2020-01-01T00:00:00Z"));
        let runs = active_runs(&state);
        assert_eq!(runs.len(), 1);
        assert!(runs[0].stale, "old heartbeat must be stale");

        // Heartbeat refresh revives it.
        refresh_heartbeat(&state, "run-1");
        let runs = active_runs(&state);
        assert!(!runs[0].stale, "refresh must clear staleness");

        // Nonce lookup finds the marker.
        assert!(find_by_nonce(&state, "nonce-1").is_some());
        assert!(find_by_nonce(&state, "nope").is_none());

        remove_marker(&state, "run-1");
        assert!(active_runs(&state).is_empty());
        // Removing twice is not an error.
        remove_marker(&state, "run-1");
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn live_runs_excludes_stale() {
        let state = fake_state("live");
        write_marker(&state, &fake_run("fresh-1", &now_rfc3339()));
        write_marker(&state, &fake_run("old-1", "2020-01-01T00:00:00Z"));
        assert_eq!(active_runs(&state).len(), 2);
        let live = live_runs(&state);
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].run_id, "fresh-1");
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn prune_removes_only_stale() {
        let state = fake_state("prune");
        write_marker(&state, &fake_run("fresh-1", &now_rfc3339()));
        write_marker(&state, &fake_run("old-1", "2020-01-01T00:00:00Z"));
        assert_eq!(prune_stale(&state), 1);
        let runs = active_runs(&state);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run_id, "fresh-1");
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn marker_path_never_escapes_active_dir() {
        let state = fake_state("escape");
        for evil in ["../../evil", "/etc/passwd", "a/b\\c", "x\0y"] {
            let p = marker_path(&state, evil);
            assert_eq!(
                p.parent().unwrap(),
                active_dir(&state).as_path(),
                "marker for {evil:?} must stay in the active dir"
            );
            let name = p.file_name().unwrap().to_string_lossy();
            assert!(
                !name.contains('/') && !name.contains('\\'),
                "no separators in {name:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn ps_json_shape() {
        let state = fake_state("json");
        write_marker(&state, &fake_run("run-9", &now_rfc3339()));
        let v = ps_payload(&state, false, 10);
        let active = v["active"].as_array().unwrap();
        assert_eq!(active.len(), 1);
        let a = &active[0];
        assert_eq!(a["run_id"], json!("run-9"));
        assert_eq!(a["status"], json!("running"));
        assert_eq!(a["stale"], json!(false));
        assert_eq!(a["detached"], json!(true));
        assert_eq!(a["name"], json!("alpha"));
        assert!(a["elapsed_secs"].as_u64().is_some());
        assert!(v.get("recent").is_none(), "recent only with --all");
        let v = ps_payload(&state, true, 10);
        assert_eq!(v["recent"].as_array().unwrap().len(), 0);
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn ps_empty_state_exits_zero() {
        let state = fake_state("empty");
        with_state_dir(&state, || {
            assert_eq!(run_ps(&[]).unwrap(), 0);
            assert_eq!(
                run_ps(&["--all".to_string(), "--json".to_string()]).unwrap(),
                0
            );
        });
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn deadman_display_covers_armed_tripped_and_missing() {
        assert_eq!(deadman_status(None), "—");
        let tripped = json!({"mins": 30, "checkin_file": "/tmp/x", "tripped": true});
        assert_eq!(deadman_status(Some(&tripped)), "TRIPPED");
        let armed =
            json!({"mins": 30, "checkin_file": "/nonexistent/rex-checkin", "tripped": false});
        let s = deadman_status(Some(&armed));
        assert!(
            s.contains("armed 30m") && s.contains("no checkin"),
            "got: {s}"
        );
        // A real check-in file shows an age.
        let dir = std::env::temp_dir().join("rex-ps-dm");
        let _ = std::fs::remove_dir_all(&dir);
        let f = dir.join("run.checkin");
        crate::deadman::checkin(&f).unwrap();
        let armed = json!({"mins": 15, "checkin_file": f.to_string_lossy(), "tripped": false});
        let s = deadman_status(Some(&armed));
        assert!(s.contains("armed 15m") && s.contains("ago"), "got: {s}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fmt_elapsed_shapes() {
        assert_eq!(fmt_elapsed(5), "5s");
        assert_eq!(fmt_elapsed(90), "1m30s");
        assert_eq!(fmt_elapsed(3720), "1h02m");
    }
}
