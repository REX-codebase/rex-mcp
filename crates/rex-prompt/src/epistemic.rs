//! Epistemic state rendered to the model: what the run OBSERVED, what it
//! INFERRED, what it GUESSES. Observed facts require harness evidence ids
//! and inferences must name their sources, so model narration can never
//! mint knowledge. Anything the model itself said enters as a guess.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum EpistemicFact {
    Observed { statement: String, evidence_id: String },
    Inferred { statement: String, from: Vec<String> },
    Guess { statement: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpistemicError(pub String);

impl std::fmt::Display for EpistemicError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for EpistemicError {}

fn check_statement(statement: &str) -> Result<String, EpistemicError> {
    if statement.trim().is_empty() {
        return Err(EpistemicError("fact statement is empty".into()));
    }
    Ok(statement.trim().to_string())
}

/// Observed facts require the harness evidence that proves them.
pub fn observed(statement: &str, evidence_id: &str) -> Result<EpistemicFact, EpistemicError> {
    let statement = check_statement(statement)?;
    if evidence_id.trim().is_empty() {
        return Err(EpistemicError(
            "an observed fact needs the evidence id that proves it".into(),
        ));
    }
    Ok(EpistemicFact::Observed {
        statement,
        evidence_id: evidence_id.trim().to_string(),
    })
}

/// Inferences must name the fact ids they chain from.
pub fn inferred(statement: &str, from: &[String]) -> Result<EpistemicFact, EpistemicError> {
    let statement = check_statement(statement)?;
    if from.is_empty() || from.iter().any(|f| f.trim().is_empty()) {
        return Err(EpistemicError(
            "an inferred fact needs the fact ids it derives from".into(),
        ));
    }
    Ok(EpistemicFact::Inferred {
        statement,
        from: from.to_vec(),
    })
}

pub fn guess(statement: &str) -> Result<EpistemicFact, EpistemicError> {
    Ok(EpistemicFact::Guess {
        statement: check_statement(statement)?,
    })
}

/// Anything the model itself said enters as a guess. There is deliberately
/// no path from this constructor to Observed: prose cannot create facts.
pub fn from_model_prose(statement: &str) -> EpistemicFact {
    EpistemicFact::Guess {
        statement: statement.trim().to_string(),
    }
}

pub fn render_state(facts: &[EpistemicFact]) -> String {
    let mut observed_lines = Vec::new();
    let mut inferred_lines = Vec::new();
    let mut guess_lines = Vec::new();
    for fact in facts {
        match fact {
            EpistemicFact::Observed {
                statement,
                evidence_id,
            } => observed_lines.push(format!("- {statement} [evidence {evidence_id}]")),
            EpistemicFact::Inferred { statement, from } => {
                inferred_lines.push(format!("- {statement} [from {}]", from.join(", ")))
            }
            EpistemicFact::Guess { statement } => guess_lines.push(format!("- {statement}")),
        }
    }
    if observed_lines.is_empty() && inferred_lines.is_empty() && guess_lines.is_empty() {
        return "No epistemic state was supplied for this call.".to_string();
    }
    let mut out = String::new();
    if !observed_lines.is_empty() {
        out.push_str("OBSERVED (harness-verified):\n");
        out.push_str(&observed_lines.join("\n"));
        out.push('\n');
    }
    if !inferred_lines.is_empty() {
        out.push_str("INFERRED (derived, not directly verified):\n");
        out.push_str(&inferred_lines.join("\n"));
        out.push('\n');
    }
    if !guess_lines.is_empty() {
        out.push_str("GUESSES (unsupported; they unlock nothing):\n");
        out.push_str(&guess_lines.join("\n"));
        out.push('\n');
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observed_facts_require_evidence() {
        assert!(observed("build passes", "").is_err());
        assert!(observed("", "ev-1").is_err());
        assert!(observed("build passes", "ev-1").is_ok());
    }

    #[test]
    fn inferences_require_sources() {
        assert!(inferred("x follows", &[]).is_err());
        assert!(inferred("x follows", &["".to_string()]).is_err());
        assert!(inferred("x follows", &["fact-1".to_string()]).is_ok());
    }

    #[test]
    fn model_prose_can_only_be_a_guess() {
        let fact = from_model_prose("I definitely ran the tests");
        assert!(matches!(fact, EpistemicFact::Guess { .. }));
        let rendered = render_state(&[fact]);
        assert!(rendered.contains("GUESSES"));
        assert!(!rendered.contains("OBSERVED (harness-verified):\n- I definitely"));
    }

    #[test]
    fn render_separates_the_three_classes() {
        let facts = vec![
            observed("cargo check exit 0", "ev-1").unwrap(),
            inferred("workspace compiles", &["f-1".to_string()]).unwrap(),
            guess("probably fine").unwrap(),
        ];
        let rendered = render_state(&facts);
        assert!(rendered.contains("OBSERVED (harness-verified):\n- cargo check exit 0 [evidence ev-1]"));
        assert!(rendered.contains("INFERRED (derived, not directly verified):\n- workspace compiles [from f-1]"));
        assert!(rendered.contains("GUESSES (unsupported; they unlock nothing):\n- probably fine"));
    }
}
