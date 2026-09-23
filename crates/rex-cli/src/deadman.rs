//! Time-locked disconnected operation: dead-man custody (leapfrog bet 6).
//!
//! `rex exec --deadman-mins 30` arms a dead-man timer. The operator must
//! check in — `touch` the check-in file, or `rex checkin --file F` — at
//! least every 30 minutes. If the timer lapses, the run is cancelled at
//! the next drive-loop poll and the receipt records the trip. An
//! unwatched agent never keeps burning budget: walk away (or lose the
//! connection) and it parks itself instead of running open-loop.
//!
//! This is deliberately different from `--timeout-secs`, which is a
//! *budget* on total runtime. The dead-man switch is a *liveness*
//! signal: "a human is still watching."
//!
//! Verified rollback: runs always happen on a staged workspace copy —
//! the original is never touched. On a trip the staged copy is preserved
//! for inspection and the receipt records exactly where the run halted
//! (step, tool calls, tokens), so the operator can resume or discard
//! with full knowledge of what happened.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Pure trip predicate, so the timing logic is unit-testable without
/// faking file mtimes.
pub fn tripped(now: SystemTime, last_checkin: SystemTime, mins: u64) -> bool {
    now.duration_since(last_checkin)
        .map(|d| d > Duration::from_secs(mins.saturating_mul(60)))
        .unwrap_or(true)
}

pub struct Deadman {
    mins: u64,
    file: PathBuf,
    pub tripped: bool,
}

impl Deadman {
    /// Arm the switch: resolve the check-in file and record an initial
    /// check-in at arm time.
    pub fn arm(
        state_dir: &Path,
        run_id: &str,
        mins: u64,
        file_override: Option<PathBuf>,
    ) -> Result<Self, String> {
        if mins == 0 {
            return Err("--deadman-mins must be at least 1".to_string());
        }
        let file = match file_override {
            Some(f) => f,
            None => {
                let dir = state_dir.join("deadman");
                std::fs::create_dir_all(&dir)
                    .map_err(|e| format!("cannot create deadman dir: {e}"))?;
                dir.join(format!("{run_id}.checkin"))
            }
        };
        checkin(&file)?;
        Ok(Deadman {
            mins,
            file,
            tripped: false,
        })
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    /// Poll once per drive-loop iteration. Returns true on the trip.
    pub fn poll(&mut self, cancel: &dyn Fn() -> Result<(), String>) -> Result<bool, String> {
        if self.tripped {
            return Ok(true);
        }
        let mtime = std::fs::metadata(&self.file)
            .and_then(|m| m.modified())
            .map_err(|e| format!("cannot read check-in file {}: {e}", self.file.display()))?;
        if tripped(SystemTime::now(), mtime, self.mins) {
            cancel().map_err(|e| format!("dead-man trip fired but cancel failed: {e}"))?;
            self.tripped = true;
            return Ok(true);
        }
        Ok(false)
    }

    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "mins": self.mins,
            "checkin_file": self.file.display().to_string(),
            "tripped": self.tripped,
        })
    }
}

/// Record a check-in: create/touch the file.
pub fn checkin(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create check-in dir: {e}"))?;
        }
    }
    // The content is the check-in time, so the write always changes bytes
    // and mtime moves even on filesystems with coarse timestamp granularity.
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos().to_string())
        .unwrap_or_default();
    std::fs::write(path, now.as_bytes())
        .map_err(|e| format!("cannot check in at {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trip_predicate() {
        let now = SystemTime::now();
        let fresh = now - Duration::from_secs(60);
        let stale = now - Duration::from_secs(61 * 60);
        assert!(!tripped(now, fresh, 30));
        assert!(tripped(now, stale, 30));
        // Boundary: exactly at the limit is not a trip (strictly greater).
        let edge = now - Duration::from_secs(30 * 60);
        assert!(!tripped(now, edge, 30));
    }

    #[test]
    fn checkin_creates_file() {
        let dir = std::env::temp_dir().join("rex-deadman-test");
        let _ = std::fs::remove_dir_all(&dir);
        let f = dir.join("run.checkin");
        checkin(&f).unwrap();
        assert!(f.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn arm_rejects_zero() {
        let dir = std::env::temp_dir().join("rex-deadman-zero");
        assert!(Deadman::arm(&dir, "r", 0, None).is_err());
    }
}
