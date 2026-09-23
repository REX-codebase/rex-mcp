//! Fable session lifecycle: THINK → PROVE → ATTACK → WRITE.
//!
//! The phase gate is mechanical, not advisory. `advance_phase` moves the
//! session forward one step; the PROVE → ATTACK step is intentionally not
//! reachable here — it opens only through `unlock_execution` (epistemic
//! prerequisites + authority timer), which arrives with the ledger and timer
//! modules.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::FableError;
use crate::ledger::EpistemicLedger;

/// The four Fable gates plus terminal states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FablePhase {
    Think,
    Prove,
    Attack,
    Write,
    Complete,
    Abandoned,
}

impl FablePhase {
    /// The next phase reachable via `advance_phase`, or `None` when the phase
    /// is terminal or gated behind `unlock_execution`.
    pub fn next_advanceable(self) -> Option<FablePhase> {
        match self {
            FablePhase::Think => Some(FablePhase::Prove),
            // Prove -> Attack requires unlock_execution (ledger + timer).
            FablePhase::Prove => None,
            FablePhase::Attack => Some(FablePhase::Write),
            FablePhase::Write => Some(FablePhase::Complete),
            FablePhase::Complete | FablePhase::Abandoned => None,
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, FablePhase::Complete | FablePhase::Abandoned)
    }
}

impl std::fmt::Display for FablePhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            FablePhase::Think => "THINK",
            FablePhase::Prove => "PROVE",
            FablePhase::Attack => "ATTACK",
            FablePhase::Write => "WRITE",
            FablePhase::Complete => "COMPLETE",
            FablePhase::Abandoned => "ABANDONED",
        };
        f.write_str(s)
    }
}

/// A Fable session: one task moving through the four gates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FableSession {
    name: String,
    objective: String,
    phase: FablePhase,
    created_at_ms: u64,
    updated_at_ms: u64,
    /// Set when the session leaves PROVE via `unlock_execution`.
    #[serde(default)]
    unlocked: bool,
    /// The epistemic ledger: claims and invariants backing the unlock gate.
    #[serde(default)]
    ledger: EpistemicLedger,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl FableSession {
    /// Create a session in THINK. The name must be filesystem-safe because it
    /// becomes the persistence filename.
    pub fn create(
        name: impl Into<String>,
        objective: impl Into<String>,
    ) -> Result<Self, FableError> {
        let name = name.into();
        let objective = objective.into();
        if !valid_name(&name) {
            return Err(FableError::BadName(name));
        }
        if objective.trim().is_empty() {
            return Err(FableError::EmptyObjective);
        }
        let now = now_ms();
        Ok(FableSession {
            name,
            objective,
            phase: FablePhase::Think,
            created_at_ms: now,
            updated_at_ms: now,
            unlocked: false,
            ledger: EpistemicLedger::new(),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn objective(&self) -> &str {
        &self.objective
    }

    pub fn phase(&self) -> FablePhase {
        self.phase
    }

    pub fn unlocked(&self) -> bool {
        self.unlocked
    }

    /// The session's epistemic ledger.
    pub fn ledger(&self) -> &EpistemicLedger {
        &self.ledger
    }

    /// Mutable access to the ledger (log items, record invariants).
    pub fn ledger_mut(&mut self) -> &mut EpistemicLedger {
        &mut self.ledger
    }

    /// The PROVE → ATTACK gate. Succeeds only in PROVE, with a non-empty
    /// evidence-based rationale, and the ledger prerequisites met (2 PROVEN
    /// items + 1 invariant). The authority-timer check arrives with the
    /// timer module; until then the gate is ledger-only and says so.
    pub fn unlock_execution(&mut self, rationale: impl Into<String>) -> Result<(), FableError> {
        if self.phase != FablePhase::Prove {
            return Err(FableError::UnlockWrongPhase {
                phase: self.phase.to_string(),
            });
        }
        let rationale = rationale.into();
        if rationale.trim().is_empty() {
            return Err(FableError::EmptyClaim);
        }
        let unmet = self.ledger.unmet_prerequisites();
        if !unmet.is_empty() {
            return Err(FableError::UnlockDenied { unmet });
        }
        self.mark_unlocked();
        Ok(())
    }

    /// Move one step forward. PROVE → ATTACK is not reachable here on
    /// purpose: that gate opens only via `unlock_execution`.
    pub fn advance_phase(&mut self) -> Result<(), FableError> {
        if self.phase.is_terminal() {
            return Err(FableError::Terminal(self.name.clone()));
        }
        match self.phase.next_advanceable() {
            Some(next) => {
                self.phase = next;
                self.updated_at_ms = now_ms();
                Ok(())
            }
            None => Err(FableError::IllegalTransition {
                from: self.phase.to_string(),
                attempted: "advance_phase".to_string(),
                detail: "PROVE opens only through unlock_execution after the epistemic prerequisites and authority timer pass".to_string(),
            }),
        }
    }

    /// Abandon a non-terminal session. Abandoned sessions never resume.
    pub fn abandon(&mut self) -> Result<(), FableError> {
        if self.phase.is_terminal() {
            return Err(FableError::Terminal(self.name.clone()));
        }
        self.phase = FablePhase::Abandoned;
        self.updated_at_ms = now_ms();
        Ok(())
    }

    /// Transition used by `unlock_execution` once its prerequisites pass.
    /// `pub(crate)` so only the gate itself can move Prove → Attack.
    pub(crate) fn mark_unlocked(&mut self) {
        self.unlocked = true;
        self.phase = FablePhase::Attack;
        self.updated_at_ms = now_ms();
    }

    fn file_name(&self) -> String {
        format!("{}.json", self.name)
    }

    /// Persist the session under `dir`. Writes are atomic: write to a temp
    /// file, then rename.
    pub fn save(&self, dir: &Path) -> Result<(), FableError> {
        std::fs::create_dir_all(dir).map_err(|e| FableError::Io(e.to_string()))?;
        let path = dir.join(self.file_name());
        let tmp = dir.join(format!("{}.tmp", self.name));
        let json = serde_json::to_string_pretty(self).map_err(|e| FableError::Io(e.to_string()))?;
        std::fs::write(&tmp, json).map_err(|e| FableError::Io(e.to_string()))?;
        std::fs::rename(&tmp, &path).map_err(|e| FableError::Io(e.to_string()))?;
        Ok(())
    }

    /// Load a session by name.
    pub fn load(dir: &Path, name: &str) -> Result<Self, FableError> {
        if !valid_name(name) {
            return Err(FableError::BadName(name.to_string()));
        }
        let path = dir.join(format!("{name}.json"));
        let raw = std::fs::read_to_string(&path).map_err(|e| FableError::Io(e.to_string()))?;
        serde_json::from_str(&raw).map_err(|e| FableError::Corrupt(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_starts_in_think() {
        let s = FableSession::create("fix-auth", "Fix the auth bypass").unwrap();
        assert_eq!(s.phase(), FablePhase::Think);
        assert!(!s.unlocked());
    }

    #[test]
    fn bad_names_rejected() {
        assert!(FableSession::create("", "x").is_err());
        assert!(FableSession::create("../evil", "x").is_err());
        assert!(FableSession::create("has space", "x").is_err());
        assert!(FableSession::create("ok-name_1", "x").is_ok());
    }

    #[test]
    fn empty_objective_rejected() {
        assert!(FableSession::create("n", "   ").is_err());
    }

    #[test]
    fn think_advances_to_prove() {
        let mut s = FableSession::create("n", "o").unwrap();
        s.advance_phase().unwrap();
        assert_eq!(s.phase(), FablePhase::Prove);
    }

    #[test]
    fn prove_does_not_advance_without_unlock() {
        let mut s = FableSession::create("n", "o").unwrap();
        s.advance_phase().unwrap();
        let err = s.advance_phase().unwrap_err();
        assert!(matches!(err, FableError::IllegalTransition { .. }));
        assert_eq!(s.phase(), FablePhase::Prove);
    }

    #[test]
    fn attack_write_complete_chain() {
        let mut s = FableSession::create("n", "o").unwrap();
        s.advance_phase().unwrap(); // THINK -> PROVE
        s.mark_unlocked(); // (unlock_execution in a later commit)
        assert_eq!(s.phase(), FablePhase::Attack);
        assert!(s.unlocked());
        s.advance_phase().unwrap(); // ATTACK -> WRITE
        assert_eq!(s.phase(), FablePhase::Write);
        s.advance_phase().unwrap(); // WRITE -> COMPLETE
        assert_eq!(s.phase(), FablePhase::Complete);
        assert!(s.phase().is_terminal());
    }

    #[test]
    fn terminal_sessions_reject_transitions() {
        let mut s = FableSession::create("n", "o").unwrap();
        s.abandon().unwrap();
        assert_eq!(s.phase(), FablePhase::Abandoned);
        assert!(matches!(
            s.advance_phase().unwrap_err(),
            FableError::Terminal(_)
        ));
        assert!(matches!(s.abandon().unwrap_err(), FableError::Terminal(_)));
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = FableSession::create("roundtrip", "o").unwrap();
        s.advance_phase().unwrap();
        s.save(dir.path()).unwrap();
        let loaded = FableSession::load(dir.path(), "roundtrip").unwrap();
        assert_eq!(loaded.phase(), FablePhase::Prove);
        assert_eq!(loaded.objective(), "o");
    }

    fn proven_ledger() -> crate::ledger::EpistemicLedger {
        use crate::ledger::EpistemicStatus;
        let mut l = crate::ledger::EpistemicLedger::new();
        l.log_item(
            EpistemicStatus::Proven,
            "cli exit codes unchanged",
            Some("smoke.sh".to_string()),
        )
        .unwrap();
        l.log_item(
            EpistemicStatus::Proven,
            "no new network calls",
            Some("strace".to_string()),
        )
        .unwrap();
        l.record_invariant("public CLI compatible", "run smoke.sh")
            .unwrap();
        l
    }

    #[test]
    fn unlock_denied_without_prerequisites() {
        let mut s = FableSession::create("n", "o").unwrap();
        s.advance_phase().unwrap(); // THINK -> PROVE
        let err = s.unlock_execution("looks good").unwrap_err();
        assert!(matches!(err, FableError::UnlockDenied { .. }));
        assert_eq!(s.phase(), FablePhase::Prove);
        assert!(!s.unlocked());
    }

    #[test]
    fn unlock_succeeds_with_prerequisites_and_rationale() {
        let mut s = FableSession::create("n", "o").unwrap();
        s.advance_phase().unwrap();
        *s.ledger_mut() = proven_ledger();
        s.unlock_execution("two proven claims plus INV-01; timer check pending")
            .unwrap();
        assert_eq!(s.phase(), FablePhase::Attack);
        assert!(s.unlocked());
    }

    #[test]
    fn unlock_rejected_outside_prove() {
        let mut s = FableSession::create("n", "o").unwrap();
        let err = s.unlock_execution("rationale").unwrap_err();
        assert!(matches!(err, FableError::UnlockWrongPhase { .. }));
    }

    #[test]
    fn unlock_rejects_empty_rationale() {
        let mut s = FableSession::create("n", "o").unwrap();
        s.advance_phase().unwrap();
        *s.ledger_mut() = proven_ledger();
        assert!(matches!(
            s.unlock_execution("   ").unwrap_err(),
            FableError::EmptyClaim
        ));
    }

    #[test]
    fn ledger_survives_save_load() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = FableSession::create("ledgertest", "o").unwrap();
        *s.ledger_mut() = proven_ledger();
        s.save(dir.path()).unwrap();
        let loaded = FableSession::load(dir.path(), "ledgertest").unwrap();
        assert!(loaded.ledger().prerequisites_met());
        assert_eq!(loaded.ledger().invariants().len(), 1);
    }
}
