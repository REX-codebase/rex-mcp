//! Grounded repository/runtime state ("digital twin") with provenance and
//! freshness. Every fact enters with its source; a fact whose freshness
//! window has passed is omitted with a visible stale count, never silently
//! asserted. Model-originated statements render as unverified claims, never
//! as grounded facts.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Provenance {
    /// The harness observed this directly; carries the evidence id.
    HarnessObservation { evidence_id: String },
    /// A file the harness read; carries path and content hash.
    FileRead { path: String, sha256: String },
    /// Output of a command the harness ran.
    RuntimeProbe { command: String },
    /// Something a model said. Informs, never proves.
    ModelClaim,
}

impl Provenance {
    fn describe(&self) -> String {
        match self {
            Provenance::HarnessObservation { evidence_id } => {
                format!("harness evidence {evidence_id}")
            }
            Provenance::FileRead { path, sha256 } => {
                let short: String = sha256.chars().take(8).collect();
                format!("file {path} sha256 {short}")
            }
            Provenance::RuntimeProbe { command } => format!("probe `{command}`"),
            Provenance::ModelClaim => "model claim".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TwinFact {
    pub statement: String,
    pub provenance: Provenance,
    pub observed_ms: u128,
    pub fresh_for_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TwinError(pub String);

impl std::fmt::Display for TwinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for TwinError {}

impl TwinFact {
    fn checked(
        statement: &str,
        provenance: Provenance,
        observed_ms: u128,
        fresh_for_ms: u64,
    ) -> Result<Self, TwinError> {
        if statement.trim().is_empty() {
            return Err(TwinError("twin fact statement is empty".into()));
        }
        match &provenance {
            Provenance::HarnessObservation { evidence_id } if evidence_id.trim().is_empty() => {
                return Err(TwinError(
                    "an observed twin fact needs the evidence id that proves it".into(),
                ))
            }
            Provenance::FileRead { path, sha256 }
                if path.trim().is_empty() || sha256.trim().is_empty() =>
            {
                return Err(TwinError(
                    "a file twin fact needs its path and content hash".into(),
                ))
            }
            Provenance::RuntimeProbe { command } if command.trim().is_empty() => {
                return Err(TwinError(
                    "a probe twin fact needs the command that produced it".into(),
                ))
            }
            _ => {}
        }
        Ok(Self {
            statement: statement.trim().to_string(),
            provenance,
            observed_ms,
            fresh_for_ms,
        })
    }

    pub fn harness(
        statement: &str,
        evidence_id: &str,
        observed_ms: u128,
        fresh_for_ms: u64,
    ) -> Result<Self, TwinError> {
        Self::checked(
            statement,
            Provenance::HarnessObservation {
                evidence_id: evidence_id.to_string(),
            },
            observed_ms,
            fresh_for_ms,
        )
    }

    pub fn file(
        statement: &str,
        path: &str,
        sha256: &str,
        observed_ms: u128,
        fresh_for_ms: u64,
    ) -> Result<Self, TwinError> {
        Self::checked(
            statement,
            Provenance::FileRead {
                path: path.to_string(),
                sha256: sha256.to_string(),
            },
            observed_ms,
            fresh_for_ms,
        )
    }

    pub fn probe(
        statement: &str,
        command: &str,
        observed_ms: u128,
        fresh_for_ms: u64,
    ) -> Result<Self, TwinError> {
        Self::checked(
            statement,
            Provenance::RuntimeProbe {
                command: command.to_string(),
            },
            observed_ms,
            fresh_for_ms,
        )
    }

    /// A model-originated statement. There is deliberately no path from
    /// this constructor to a grounded provenance.
    pub fn model_claim(
        statement: &str,
        observed_ms: u128,
        fresh_for_ms: u64,
    ) -> Result<Self, TwinError> {
        Self::checked(statement, Provenance::ModelClaim, observed_ms, fresh_for_ms)
    }

    pub fn is_fresh(&self, now_ms: u128) -> bool {
        now_ms.saturating_sub(self.observed_ms) <= self.fresh_for_ms as u128
    }
}

/// Render the twin module. Fresh grounded facts first, then unverified
/// model claims in their own labeled section, then the stale-omission note.
pub fn render_twin(facts: &[TwinFact], now_ms: u128) -> String {
    let mut grounded = Vec::new();
    let mut claims = Vec::new();
    let mut stale = 0usize;
    for fact in facts {
        if !fact.is_fresh(now_ms) {
            stale += 1;
            continue;
        }
        match fact.provenance {
            Provenance::ModelClaim => claims.push(fact),
            _ => grounded.push(fact),
        }
    }
    let mut out = String::new();
    if grounded.is_empty() && claims.is_empty() {
        out.push_str("No grounded repository or runtime state was supplied for this call.");
    } else {
        if !grounded.is_empty() {
            out.push_str(
                "Grounded state (each fact carries its source; anything not listed here is unknown):\n",
            );
            for fact in grounded {
                out.push_str(&format!(
                    "- {} [source: {}]\n",
                    fact.statement,
                    fact.provenance.describe()
                ));
            }
        }
        if !claims.is_empty() {
            out.push_str("Unverified model claims (NOT facts - do not rely on them):\n");
            for fact in claims {
                out.push_str(&format!("- {}\n", fact.statement));
            }
        }
    }
    if stale > 0 {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&format!(
            "({stale} stale fact(s) omitted because their freshness window expired; do not reconstruct them from memory)"
        ));
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provenance_is_mandatory() {
        assert!(TwinFact::harness("x", "", 0, 1000).is_err());
        assert!(TwinFact::file("x", "", "abc", 0, 1000).is_err());
        assert!(TwinFact::file("x", "p", "", 0, 1000).is_err());
        assert!(TwinFact::probe("x", "", 0, 1000).is_err());
        assert!(TwinFact::harness("", "ev-1", 0, 1000).is_err());
        assert!(TwinFact::harness("x", "ev-1", 0, 1000).is_ok());
    }

    #[test]
    fn stale_facts_are_omitted_with_a_visible_count() {
        let facts = vec![
            TwinFact::harness("fresh fact", "ev-1", 1000, 500).unwrap(),
            TwinFact::harness("ancient observation", "ev-2", 0, 100).unwrap(),
        ];
        let rendered = render_twin(&facts, 1200);
        assert!(rendered.contains("fresh fact"));
        assert!(!rendered.contains("ancient observation"));
        assert!(rendered.contains("1 stale fact(s) omitted"));
    }

    #[test]
    fn model_claims_never_render_as_grounded() {
        let facts = vec![
            TwinFact::model_claim("the build passes", 1000, 5000).unwrap(),
            TwinFact::harness("cargo check exit 0", "ev-9", 1000, 5000).unwrap(),
        ];
        let rendered = render_twin(&facts, 1200);
        assert!(rendered.contains("Grounded state"));
        assert!(rendered.contains("cargo check exit 0 [source: harness evidence ev-9]"));
        assert!(rendered.contains("Unverified model claims (NOT facts"));
        let grounded_at = rendered.find("Grounded state").unwrap();
        let claims_at = rendered.find("Unverified model claims").unwrap();
        let claim_at = rendered.find("the build passes").unwrap();
        assert!(grounded_at < claims_at && claim_at > claims_at);
    }

    #[test]
    fn empty_twin_says_so() {
        assert_eq!(
            render_twin(&[], 0),
            "No grounded repository or runtime state was supplied for this call."
        );
    }
}
