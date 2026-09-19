//! The adversary pass.
//!
//! A separate run with one job: prove the builder's result wrong. It gets the
//! contract and the verification outcomes, reads the workspace through the
//! normal safe tools, and must answer with a strict defect list. An empty
//! list is the only clean answer. Claims the adversary cannot ground are
//! recorded as guesses in the epistemic ledger - they inform the judge but
//! discharge nothing.

use crate::contract::{extract_json, AcceptanceContract};
use crate::verify::VerificationReport;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Defect {
    pub title: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdversaryReport {
    pub defects: Vec<Defect>,
    /// True when the adversary's answer could not be parsed - reported
    /// honestly instead of being treated as either clean or dirty.
    pub inconclusive: bool,
    pub adversary_model: String,
}

impl AdversaryReport {
    pub fn clean(adversary_model: &str) -> Self {
        Self {
            defects: Vec::new(),
            inconclusive: false,
            adversary_model: adversary_model.to_string(),
        }
    }
}

pub fn adversary_brief(
    task: &str,
    contract: &AcceptanceContract,
    verification: &VerificationReport,
) -> String {
    let mut obligations = String::new();
    for ob in &contract.obligations {
        obligations.push_str(&format!("- {}: {}\n", ob.id, ob.statement));
    }
    let mut outcomes = String::new();
    for o in &verification.outcomes {
        outcomes.push_str(&format!("- {}: {:?} - {}\n", o.obligation_id, o.status, o.detail));
    }
    format!(
        "You are the adversary in REX Ultra. Another agent claims this task is complete:\n\n\
TASK:\n{task}\n\n\
ACCEPTANCE OBLIGATIONS:\n{obligations}\n\
HARNESS VERIFICATION OUTCOMES:\n{outcomes}\n\
Your only job is to prove the result wrong. Inspect the workspace with your tools. Attack edge \
cases the obligations miss, check that proofs measure the real behavior (not a cached or staged \
result), and look for anything that would embarrass this run in front of a hostile reviewer. \
Do NOT modify files; this is a read-only inspection.\n\n\
Finish with complete_task whose summary is ONE JSON object:\n\
{{\"defects\":[{{\"title\":\"...\",\"detail\":\"what is wrong and how you confirmed it\"}}...]}}\n\
An empty defects list is the only way to say the result survives your attack. If you cannot \
inspect enough to decide, list one defect explaining what you could not check."
    )
}

pub fn parse_defects(summary: &str, adversary_model: &str) -> AdversaryReport {
    let Some(json) = extract_json(summary) else {
        return AdversaryReport {
            defects: Vec::new(),
            inconclusive: true,
            adversary_model: adversary_model.to_string(),
        };
    };
    #[derive(Deserialize)]
    struct Raw {
        #[serde(default)]
        defects: Vec<Defect>,
    }
    match serde_json::from_str::<Raw>(&json) {
        Ok(raw) => AdversaryReport {
            defects: raw
                .defects
                .into_iter()
                .filter(|d| !d.title.trim().is_empty())
                .collect(),
            inconclusive: false,
            adversary_model: adversary_model.to_string(),
        },
        Err(_) => AdversaryReport {
            defects: Vec::new(),
            inconclusive: true,
            adversary_model: adversary_model.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_answer_parses() {
        let r = parse_defects("{\"defects\":[]}", "m");
        assert!(!r.inconclusive);
        assert!(r.defects.is_empty());
    }

    #[test]
    fn defects_parse() {
        let r = parse_defects(
            "I found problems.\n{\"defects\":[{\"title\":\"empty file\",\"detail\":\"x.txt is 0 bytes\"}]}",
            "m",
        );
        assert_eq!(r.defects.len(), 1);
    }

    #[test]
    fn unparseable_is_inconclusive_not_clean() {
        let r = parse_defects("looks fine to me", "m");
        assert!(r.inconclusive);
        assert!(r.defects.is_empty());
    }
}
