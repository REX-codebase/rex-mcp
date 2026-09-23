use std::fmt;

/// Errors from the Fable gate. Every variant names the violated rule so the
/// caller can report honestly instead of guessing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FableError {
    /// The session name is empty or not filesystem-safe.
    BadName(String),
    /// The objective is empty.
    EmptyObjective,
    /// The requested transition is not allowed from the current phase.
    IllegalTransition {
        from: String,
        attempted: String,
        detail: String,
    },
    /// The session is terminal; no further transitions are possible.
    Terminal(String),
    /// Persistence failed.
    Io(String),
    /// A stored session could not be parsed.
    Corrupt(String),
    /// A ledger claim was empty.
    EmptyClaim,
    /// A PROVEN item was logged without evidence.
    EvidenceRequired { claim: String },
    /// A HYPOTHESIS/UNKNOWN item was logged with evidence.
    EvidenceForbidden { status: String },
    /// An invariant was recorded without a falsifiable check.
    InvariantNeedsCheck,
    /// `unlock_execution` was attempted without meeting the prerequisites.
    UnlockDenied { unmet: Vec<String> },
    /// `unlock_execution` was attempted outside PROVE.
    UnlockWrongPhase { phase: String },
    /// The time budget was below the minimum.
    BadTimeBudget { minutes: u32, minimum: u32 },
}

impl fmt::Display for FableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FableError::BadName(n) => write!(
                f,
                "bad session name {n:?}: use letters, digits, '-' and '_'"
            ),
            FableError::EmptyObjective => write!(f, "objective must not be empty"),
            FableError::IllegalTransition {
                from,
                attempted,
                detail,
            } => write!(f, "cannot {attempted} from {from}: {detail}"),
            FableError::Terminal(name) => {
                write!(f, "session {name:?} is terminal; no further transitions")
            }
            FableError::Io(e) => write!(f, "fable persistence failed: {e}"),
            FableError::Corrupt(e) => write!(f, "stored fable session is corrupt: {e}"),
            FableError::EmptyClaim => write!(f, "ledger claim must not be empty"),
            FableError::EvidenceRequired { claim } => write!(
                f,
                "PROVEN claim {claim:?} needs evidence: file, command output, receipt, or source"
            ),
            FableError::EvidenceForbidden { status } => write!(
                f,
                "{status} items must not carry evidence; log a PROVEN item instead"
            ),
            FableError::InvariantNeedsCheck => write!(
                f,
                "invariant needs a falsifiable check: how could it be proven wrong?"
            ),
            FableError::UnlockDenied { unmet } => {
                write!(f, "unlock denied: {}", unmet.join("; "))
            }
            FableError::UnlockWrongPhase { phase } => write!(
                f,
                "unlock_execution is only valid in PROVE, not {phase}"
            ),
            FableError::BadTimeBudget { minutes, minimum } => write!(
                f,
                "time budget {minutes}m is below the {minimum}m minimum: a shorter timer is theater, not deliberation"
            ),
        }
    }
}

impl std::error::Error for FableError {}
