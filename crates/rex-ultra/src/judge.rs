//! The clean-room judge.
//!
//! A single-shot pass that sees the original task, the acceptance contract,
//! the verifier's fresh outcomes, the adversary's standing defects and the
//! evidence manifest - never the builder's narrative or self-assessment. It
//! answers with strict JSON: one verdict per obligation. Missing, unknown or
//! duplicated verdicts reject the whole report; a malformed answer rejects
//! the run rather than being waved through.

use crate::adversary::AdversaryReport;
use crate::contract::{extract_json, AcceptanceContract};
use crate::verify::{ObligationStatus, VerificationReport};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerdictKind {
    Pass,
    Fail,
    Unproven,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Verdict {
    pub obligation_id: String,
    pub verdict: VerdictKind,
    pub reason: String,
    /// Evidence ids the judge relied on. Required for passing a
    /// behavior_evidence obligation.
    #[serde(default)]
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JudgeReport {
    pub verdicts: Vec<Verdict>,
    pub all_passed: bool,
    pub judge_model: String,
    /// True when the judge model is the worker model. Recorded so nobody can
    /// later claim independent review that did not happen.
    pub same_model_as_worker: bool,
}

pub fn judge_prompt(
    task: &str,
    contract: &AcceptanceContract,
    verification: &VerificationReport,
    adversary: &AdversaryReport,
    evidence_manifest: &str,
) -> String {
    let mut obligations = String::new();
    for ob in &contract.obligations {
        let proof = serde_json::to_string(&ob.proof).unwrap_or_default();
        obligations.push_str(&format!(
            "- {}: {} (proof: {})\n",
            ob.id, ob.statement, proof
        ));
    }
    let mut outcomes = String::new();
    for o in &verification.outcomes {
        outcomes.push_str(&format!(
            "- {}: {:?} - {} (evidence: {})\n",
            o.obligation_id,
            o.status,
            o.detail,
            o.evidence_ids.join(",")
        ));
    }
    let mut defects = String::new();
    for d in &adversary.defects {
        defects.push_str(&format!("- {}: {}\n", d.title, d.detail));
    }
    if defects.is_empty() {
        defects.push_str("- none standing\n");
    }
    let mut regressions = String::new();
    for r in &contract.forbidden_regressions {
        regressions.push_str(&format!("- {}: {}\n", r.id, r.statement));
    }
    format!(
        "You are the clean-room judge in REX Ultra. You have never seen the builder's reasoning, \
only its artifacts and the harness's fresh verification. Be adversarial: a claim without evidence \
is unproven.\n\n\
ORIGINAL TASK:\n{task}\n\n\
ACCEPTANCE CONTRACT OBLIGATIONS:\n{obligations}\n\
FORBIDDEN REGRESSIONS:\n{regressions}\n\
DETERMINISTIC VERIFICATION OUTCOMES (fresh execution by the harness, not the builder):\n{outcomes}\n\
STANDING ADVERSARY DEFECTS:\n{defects}\n\
EVIDENCE MANIFEST (id, kind, path, sha256, bytes):\n{evidence_manifest}\n\n\
Answer with ONE JSON object and nothing else:\n\
{{\"verdicts\":[{{\"obligation_id\":\"...\",\"verdict\":\"pass|fail|unproven\",\"reason\":\"...\",\"evidence\":[\"ev-0001\"]}}...]}}\n\n\
Rules: one verdict for every obligation id, no extras; an obligation whose verification failed \
cannot pass; a behavior_evidence obligation can pass only if you cite manifest evidence ids that \
support it; any standing adversary defect must fail the obligation it undermines."
    )
}

/// Parse and strictly check a judge answer against the contract and the
/// verifier's outcomes. Any structural problem rejects the whole report.
pub fn parse_verdicts(
    text: &str,
    contract: &AcceptanceContract,
    verification: &VerificationReport,
    judge_model: &str,
    same_model_as_worker: bool,
) -> Result<JudgeReport, String> {
    let json = extract_json(text).ok_or_else(|| "judge returned no JSON".to_string())?;
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Raw {
        verdicts: Vec<Verdict>,
    }
    let raw: Raw = serde_json::from_str(&json).map_err(|e| format!("judge JSON invalid: {e}"))?;

    let mut seen = HashSet::new();
    for v in &raw.verdicts {
        if !contract.obligations.iter().any(|o| o.id == v.obligation_id) {
            return Err(format!(
                "judge returned a verdict for unknown obligation {}",
                v.obligation_id
            ));
        }
        if !seen.insert(v.obligation_id.clone()) {
            return Err(format!(
                "judge returned duplicate verdicts for {}",
                v.obligation_id
            ));
        }
    }
    for ob in &contract.obligations {
        if !seen.contains(&ob.id) {
            return Err(format!("judge gave no verdict for obligation {}", ob.id));
        }
    }
    // A verdict cannot overrule fresh deterministic failure.
    let mut verdicts = raw.verdicts;
    for v in &mut verdicts {
        if let Some(o) = verification
            .outcomes
            .iter()
            .find(|o| o.obligation_id == v.obligation_id)
        {
            if o.status == ObligationStatus::Failed && v.verdict == VerdictKind::Pass {
                return Err(format!(
                    "judge passed {} despite failed deterministic verification",
                    v.obligation_id
                ));
            }
        }
    }
    let all_passed = verdicts.iter().all(|v| v.verdict == VerdictKind::Pass);
    Ok(JudgeReport {
        verdicts,
        all_passed,
        judge_model: judge_model.to_string(),
        same_model_as_worker,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adversary::AdversaryReport;
    use crate::contract::parse_contract;
    use crate::verify::ObligationOutcome;

    fn fixture() -> (AcceptanceContract, VerificationReport) {
        let c = parse_contract(
            r#"{"obligations":[{"id":"a","statement":"s","proof":{"kind":"file_exists","path":"x"}},{"id":"b","statement":"ui","proof":{"kind":"behavior_evidence","description":"renders"}}]}"#,
            "t",
        ).unwrap();
        let v = VerificationReport {
            outcomes: vec![
                ObligationOutcome {
                    obligation_id: "a".into(),
                    status: ObligationStatus::Proven,
                    detail: "ok".into(),
                    evidence_ids: vec!["ev-1".into()],
                    duration_ms: 1,
                },
                ObligationOutcome {
                    obligation_id: "b".into(),
                    status: ObligationStatus::AwaitingJudge,
                    detail: "judge".into(),
                    evidence_ids: vec![],
                    duration_ms: 0,
                },
            ],
            executable_all_proven: true,
            verified_ms: 0,
        };
        (c, v)
    }

    #[test]
    fn complete_verdicts_parse() {
        let (c, v) = fixture();
        let report = parse_verdicts(
            r#"{"verdicts":[{"obligation_id":"a","verdict":"pass","reason":"proven"},{"obligation_id":"b","verdict":"pass","reason":"shot","evidence":["ev-2"]}]}"#,
            &c, &v, "judge-model", true,
        ).unwrap();
        assert!(report.all_passed);
        assert!(report.same_model_as_worker);
    }

    #[test]
    fn missing_obligation_rejects_the_report() {
        let (c, v) = fixture();
        let err = parse_verdicts(
            r#"{"verdicts":[{"obligation_id":"a","verdict":"pass","reason":"ok"}]}"#,
            &c,
            &v,
            "m",
            true,
        )
        .unwrap_err();
        assert!(err.contains("no verdict"), "{err}");
    }

    #[test]
    fn judge_cannot_overrule_failed_verification() {
        let (c, mut v) = fixture();
        v.outcomes[0].status = ObligationStatus::Failed;
        let err = parse_verdicts(
            r#"{"verdicts":[{"obligation_id":"a","verdict":"pass","reason":"i believe it"},{"obligation_id":"b","verdict":"fail","reason":"no evidence"}]}"#,
            &c, &v, "m", true,
        ).unwrap_err();
        assert!(
            err.contains("overrule") || err.contains("failed deterministic"),
            "{err}"
        );
    }

    #[test]
    fn unknown_obligation_rejected() {
        let (c, v) = fixture();
        assert!(parse_verdicts(
            r#"{"verdicts":[{"obligation_id":"zzz","verdict":"pass","reason":"?"}]}"#,
            &c,
            &v,
            "m",
            true,
        )
        .is_err());
    }

    #[test]
    fn malformed_json_rejected() {
        let (c, v) = fixture();
        assert!(parse_verdicts("the work looks good to me", &c, &v, "m", true).is_err());
    }

    #[test]
    fn adversary_report_default_is_clean() {
        let report = AdversaryReport::clean("m");
        assert!(report.defects.is_empty());
    }
}
