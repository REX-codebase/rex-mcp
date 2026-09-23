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
        }
    }
}

impl std::error::Error for FableError {}
