//! The completion gate: what "done" must mean, plus a validator that
//! rejects completion claims without real harness evidence before they can
//! reach any scorer. False completion is the failure mode this module
//! exists to make expensive.

use std::collections::BTreeSet;

pub const COMPLETION_GATE: &str = "\
Before you declare completion:
1. Every obligation in the task's acceptance contract must be backed by a \
fresh harness observation with an evidence id. Your say-so is not evidence.
2. The harness re-verifies after you finish. A false completion claim is \
worse than an honest \"not done yet\": it fails the run and is recorded.
3. If something cannot be verified, say exactly what and why - that is a \
valid outcome; inventing proof is not.
4. Declaring completion ends your turn. Declare it only when you would bet \
the run on hostile re-verification.";

/// Check a completion claim against the evidence the harness actually
/// registered. Any gap fails the whole claim.
pub fn validate_completion_claim(
    summary: &str,
    cited_evidence: &[String],
    registered_evidence: &BTreeSet<String>,
) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    if summary.trim().is_empty() {
        errors.push("completion summary is empty".to_string());
    }
    if cited_evidence.is_empty() {
        errors.push(
            "completion claim cites no evidence; a claim without evidence is unproven".to_string(),
        );
    }
    for id in cited_evidence {
        if !registered_evidence.contains(id) {
            errors.push(format!(
                "cited evidence \"{id}\" was never registered by the harness"
            ));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registered() -> BTreeSet<String> {
        ["ev-1".to_string(), "ev-2".to_string()]
            .into_iter()
            .collect()
    }

    #[test]
    fn false_completion_is_rejected() {
        // no evidence cited at all
        let err = validate_completion_claim("done", &[], &registered()).unwrap_err();
        assert!(err.iter().any(|e| e.contains("cites no evidence")));
        // invented evidence ids
        let err =
            validate_completion_claim("done", &["ev-999".to_string()], &registered()).unwrap_err();
        assert!(err.iter().any(|e| e.contains("never registered")));
        // empty summary
        assert!(validate_completion_claim("", &["ev-1".to_string()], &registered()).is_err());
    }

    #[test]
    fn evidenced_completion_passes() {
        assert!(validate_completion_claim(
            "all obligations verified",
            &["ev-1".to_string()],
            &registered()
        )
        .is_ok());
    }

    #[test]
    fn gate_fits_its_budget() {
        assert!(
            COMPLETION_GATE.chars().count() <= crate::ModuleKind::CompletionGate.budget_chars()
        );
    }
}
