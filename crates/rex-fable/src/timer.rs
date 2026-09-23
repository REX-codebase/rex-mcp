//! The mechanical authority timer.
//!
//! Fable separates confidence from permission with a wall-clock gate: even
//! with perfect evidence, `unlock_execution` cannot succeed until the
//! configured deliberation budget has elapsed. The timer starts at session
//! creation and is enforced in Rust, not in the UI — a countdown display can
//! lie, the gate cannot.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::FableError;

/// Default deliberation budget, minutes.
pub const DEFAULT_BUDGET_MINUTES: u32 = 60;
/// Minimum budget: below this the "timer" is theater, not deliberation.
pub const MIN_BUDGET_MINUTES: u32 = 2;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A wall-clock gate attached to a Fable session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorityTimer {
    duration_minutes: u32,
    started_at_ms: u64,
}

impl AuthorityTimer {
    /// Start a timer with the given budget (minimum 2 minutes).
    pub fn new(duration_minutes: u32) -> Result<Self, FableError> {
        Self::new_at(duration_minutes, now_ms())
    }

    pub(crate) fn new_at(duration_minutes: u32, started_at_ms: u64) -> Result<Self, FableError> {
        if duration_minutes < MIN_BUDGET_MINUTES {
            return Err(FableError::BadTimeBudget {
                minutes: duration_minutes,
                minimum: MIN_BUDGET_MINUTES,
            });
        }
        Ok(AuthorityTimer {
            duration_minutes,
            started_at_ms,
        })
    }

    pub fn duration_minutes(&self) -> u32 {
        self.duration_minutes
    }

    pub fn started_at_ms(&self) -> u64 {
        self.started_at_ms
    }

    /// Milliseconds until the gate opens; 0 once elapsed.
    pub fn remaining_ms(&self) -> u64 {
        let total = self.duration_minutes as u64 * 60_000;
        let elapsed = now_ms().saturating_sub(self.started_at_ms);
        total.saturating_sub(elapsed)
    }

    /// Whether deliberation time has fully elapsed.
    pub fn elapsed(&self) -> bool {
        self.remaining_ms() == 0
    }

    /// Human countdown, e.g. "42m 10s" or "0s" when elapsed.
    pub fn remaining_human(&self) -> String {
        let ms = self.remaining_ms();
        if ms == 0 {
            return "0s".to_string();
        }
        let total_s = ms / 1000;
        let h = total_s / 3600;
        let m = (total_s % 3600) / 60;
        let s = total_s % 60;
        if h > 0 {
            format!("{h}h {m}m")
        } else if m > 0 {
            format!("{m}m {s:02}s")
        } else {
            format!("{s}s")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_theater_budgets() {
        assert!(AuthorityTimer::new(0).is_err());
        assert!(AuthorityTimer::new(1).is_err());
        assert!(AuthorityTimer::new(2).is_ok());
    }

    #[test]
    fn fresh_timer_has_full_remaining() {
        let t = AuthorityTimer::new(60).unwrap();
        // Allow a small scheduling slop.
        assert!(t.remaining_ms() > 59 * 60_000);
        assert!(!t.elapsed());
    }

    #[test]
    fn past_timer_is_elapsed() {
        let t = AuthorityTimer::new_at(2, now_ms() - 3 * 60_000).unwrap();
        assert!(t.elapsed());
        assert_eq!(t.remaining_ms(), 0);
        assert_eq!(t.remaining_human(), "0s");
    }

    #[test]
    fn remaining_human_formats() {
        let t = AuthorityTimer::new_at(90, now_ms()).unwrap();
        assert!(t.remaining_human().starts_with("1h "));
        let t2 = AuthorityTimer::new_at(5, now_ms()).unwrap();
        assert!(t2.remaining_human().starts_with("4m ") || t2.remaining_human().starts_with("5m "));
    }
}
