//! The deterministic verifier. Not a model.
//!
//! Every executable proof in the contract is re-executed against the real
//! workspace, fresh, after the builder declares completion. The builder's own
//! test run does not count: a stale cache, a skipped test, or an invented
//! success all fail here. Command proofs go through the rex-tools policy;
//! hard-denied commands fail the obligation. Verification commands are
//! harness-initiated inside the disposable run workspace, so the runtime's
//! interactive approval is resolved by the harness itself and every command,
//! exit code and output hash lands in the evidence store and ledger for the
//! user to inspect.

use crate::contract::{AcceptanceContract, Obligation, Proof};
use crate::evidence::{sha256_hex, EvidenceStore};
use rex_tools::{RiskClass, ToolRequest, ToolRuntime};
use serde::{Deserialize, Serialize};
use std::path::Path;

const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const MAX_TIMEOUT_MS: u64 = 600_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ObligationStatus {
    /// Fresh execution passed.
    Proven,
    /// Fresh execution ran and failed.
    Failed,
    /// No executable proof exists; the clean-room judge must decide.
    AwaitingJudge,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObligationOutcome {
    pub obligation_id: String,
    pub status: ObligationStatus,
    pub detail: String,
    pub evidence_ids: Vec<String>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerificationReport {
    pub outcomes: Vec<ObligationOutcome>,
    /// True when every executable obligation passed. Behavior obligations do
    /// not block this flag; the judge owns them.
    pub executable_all_proven: bool,
    pub verified_ms: u128,
}

fn verify_one(
    obligation: &Obligation,
    runtime: &ToolRuntime,
    workspace: &Path,
    evidence: &mut EvidenceStore,
    scoring: Option<&[&str]>,
) -> ObligationOutcome {
    let started = std::time::Instant::now();
    let mut evidence_ids: Vec<String> = Vec::new();
    macro_rules! outcome {
        ($status:expr, $detail:expr $(,)?) => {
            ObligationOutcome {
                obligation_id: obligation.id.clone(),
                status: $status,
                detail: $detail,
                evidence_ids: evidence_ids.clone(),
                duration_ms: started.elapsed().as_millis() as u64,
            }
        };
    }

    match &obligation.proof {
        Proof::FileExists { path } => {
            let full = workspace.join(path);
            match std::fs::metadata(&full) {
                Ok(meta) if meta.is_file() && meta.len() > 0 => {
                    match evidence.put_file("proof_artifact", workspace, path) {
                        Ok(entry) => evidence_ids.push(entry.id),
                        Err(e) => return outcome!(ObligationStatus::Failed, e),
                    }
                    outcome!(
                        ObligationStatus::Proven,
                        format!("{path} exists and is non-empty")
                    )
                }
                Ok(_) => outcome!(
                    ObligationStatus::Failed,
                    format!("{path} is empty or not a file")
                ),
                Err(e) => outcome!(ObligationStatus::Failed, format!("{path}: {e}")),
            }
        }
        Proof::FileContains { path, needle } => {
            let full = workspace.join(path);
            match std::fs::read(&full) {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    match evidence.put_file("proof_artifact", workspace, path) {
                        Ok(entry) => evidence_ids.push(entry.id),
                        Err(e) => return outcome!(ObligationStatus::Failed, e),
                    }
                    if text.contains(needle.as_str()) {
                        outcome!(
                            ObligationStatus::Proven,
                            format!("{path} contains the needle")
                        )
                    } else {
                        outcome!(
                            ObligationStatus::Failed,
                            format!("{path} does not contain the expected text"),
                        )
                    }
                }
                Err(e) => outcome!(ObligationStatus::Failed, format!("{path}: {e}")),
            }
        }
        Proof::CommandSucceeds {
            argv,
            cwd,
            timeout_ms,
        }
        | Proof::CommandOutputContains {
            argv,
            cwd,
            timeout_ms,
            ..
        } => {
            let timeout = timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS).min(MAX_TIMEOUT_MS);
            let request = ToolRequest::RunCommand {
                argv: argv.clone(),
                cwd: cwd.clone(),
                timeout_ms: Some(timeout),
            };
            let prepared = match runtime.prepare(request) {
                Ok(p) => p,
                Err(e) => {
                    return outcome!(
                        ObligationStatus::Failed,
                        format!("prepare failed: {}", e.detail)
                    )
                }
            };
            // Two execution paths, one audit trail. Model-facing policy
            // denials stand UNLESS the caller pinned a scoring allowlist:
            // then the suite-authored command runs through the verifier's
            // trusted scoring path (same sandbox, same receipt). This never
            // widens model-issued command authority.
            let (result, via) = if prepared.risk == RiskClass::Denied {
                match scoring {
                    Some(allowed) => (
                        runtime.execute_trusted_scoring(argv, cwd.as_deref(), timeout, allowed),
                        "trusted_scoring",
                    ),
                    None => {
                        return outcome!(
                            ObligationStatus::Failed,
                            format!(
                                "verification command blocked by hard policy: {}",
                                prepared.policy_reason
                            ),
                        )
                    }
                }
            } else {
                // Harness-trusted approval inside the disposable run workspace;
                // recorded in evidence so the user can audit every command.
                if prepared.approval_required {
                    if let Err(e) = runtime.resolve_approval(&prepared.call_id, true) {
                        return outcome!(
                            ObligationStatus::Failed,
                            format!("approval failed: {}", e.detail)
                        );
                    }
                }
                (runtime.execute(&prepared.call_id), "runtime")
            };
            let output = result.output.clone().unwrap_or_default();
            let record = format!(
                "via={}\nargv={:?}\nexit={:?}\noutput_sha256={}\noutput:\n{}",
                via,
                argv,
                result.receipt.exit_code,
                sha256_hex(output.as_bytes()),
                output
            );
            let entry = evidence.put_bytes("command_verification", record.as_bytes());
            evidence_ids.push(entry.id);
            if !result.ok {
                let detail = result
                    .error
                    .map(|e| e.detail)
                    .unwrap_or_else(|| "command failed".into());
                return outcome!(ObligationStatus::Failed, detail);
            }
            if let Proof::CommandOutputContains { needle, .. } = &obligation.proof {
                if !output.contains(needle.as_str()) {
                    return outcome!(
                        ObligationStatus::Failed,
                        "command succeeded but its output lacked the expected text".into(),
                    );
                }
            }
            outcome!(
                ObligationStatus::Proven,
                format!("command exited 0 (receipt {})", result.call_id),
            )
        }
        Proof::BehaviorEvidence { description } => outcome!(
            ObligationStatus::AwaitingJudge,
            format!("behavior claim needs the clean-room judge: {description}"),
        ),
    }
}

pub fn verify_contract(
    contract: &AcceptanceContract,
    workspace: &Path,
    evidence: &mut EvidenceStore,
) -> VerificationReport {
    verify_contract_with_scoring(contract, workspace, evidence, None)
}

/// Verify with an optional trusted-scoring allowlist. When `scoring` names
/// executables (e.g. ["python3", "pytest"]), suite-authored command checks
/// that the model-facing policy denies still run - through
/// ToolRuntime::execute_trusted_scoring, inside the same sandbox. Callers:
/// the benchmark scorer passes the suite's pinned allowlist; every other
/// caller passes None and keeps the old behavior.
pub fn verify_contract_with_scoring(
    contract: &AcceptanceContract,
    workspace: &Path,
    evidence: &mut EvidenceStore,
    scoring: Option<&[&str]>,
) -> VerificationReport {
    let runtime = match ToolRuntime::new(workspace) {
        Ok(r) => r,
        Err(e) => {
            return VerificationReport {
                outcomes: contract
                    .obligations
                    .iter()
                    .map(|ob| ObligationOutcome {
                        obligation_id: ob.id.clone(),
                        status: ObligationStatus::Failed,
                        detail: format!("workspace cannot open: {}", e.detail),
                        evidence_ids: Vec::new(),
                        duration_ms: 0,
                    })
                    .collect(),
                executable_all_proven: false,
                verified_ms: crate::now_ms(),
            }
        }
    };
    let outcomes: Vec<ObligationOutcome> = contract
        .obligations
        .iter()
        .map(|ob| verify_one(ob, &runtime, workspace, evidence, scoring))
        .collect();
    let executable_all_proven = outcomes
        .iter()
        .filter(|o| o.status != ObligationStatus::AwaitingJudge)
        .all(|o| o.status == ObligationStatus::Proven);
    VerificationReport {
        outcomes,
        executable_all_proven,
        verified_ms: crate::now_ms(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::parse_contract;

    fn contract_with(proof: &str) -> AcceptanceContract {
        let draft = format!(r#"{{"obligations":[{{"id":"o1","statement":"s","proof":{proof}}}]}}"#);
        parse_contract(&draft, "t").unwrap()
    }

    #[test]
    fn forged_completion_fails_reexecution() {
        // The builder "claims" a passing test; the workspace has no such
        // file. Fresh verification must fail the obligation.
        let dir = tempfile::tempdir().unwrap();
        let mut ev = EvidenceStore::open(&dir.path().join("ev")).unwrap();
        let c = contract_with(r#"{"kind":"file_contains","path":"result.txt","needle":"PASS"}"#);
        let report = verify_contract(&c, dir.path(), &mut ev);
        assert!(!report.executable_all_proven);
        assert_eq!(report.outcomes[0].status, ObligationStatus::Failed);
    }

    #[test]
    fn real_artifact_passes_fresh_check() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("result.txt"), "PASS\n").unwrap();
        let mut ev = EvidenceStore::open(&dir.path().join("ev")).unwrap();
        let c = contract_with(r#"{"kind":"file_contains","path":"result.txt","needle":"PASS"}"#);
        let report = verify_contract(&c, dir.path(), &mut ev);
        assert!(report.executable_all_proven);
        assert!(!report.outcomes[0].evidence_ids.is_empty());
    }

    #[test]
    fn command_output_must_contain_the_needle() {
        let dir = tempfile::tempdir().unwrap();
        let mut ev = EvidenceStore::open(&dir.path().join("ev")).unwrap();
        let good = contract_with(
            r#"{"kind":"command_output_contains","argv":["printf","hello world"],"needle":"hello"}"#,
        );
        assert!(verify_contract(&good, dir.path(), &mut ev).executable_all_proven);
        let bad = contract_with(
            r#"{"kind":"command_output_contains","argv":["printf","hello world"],"needle":"goodbye"}"#,
        );
        assert!(!verify_contract(&bad, dir.path(), &mut ev).executable_all_proven);
    }

    #[test]
    fn policy_denied_commands_fail_the_obligation() {
        let dir = tempfile::tempdir().unwrap();
        let mut ev = EvidenceStore::open(&dir.path().join("ev")).unwrap();
        let c = contract_with(r#"{"kind":"command_succeeds","argv":["rm","-rf","/"]}"#);
        let report = verify_contract(&c, dir.path(), &mut ev);
        assert!(!report.executable_all_proven);
    }

    #[test]
    fn scoring_allowlist_runs_denied_interpreters_without_widening_model_policy() {
        use crate::contract::{AcceptanceContract, Obligation, Proof};
        use crate::evidence::EvidenceStore;
        let mk = |argv: Vec<&str>| AcceptanceContract {
            work_kind: Default::default(),
            task: "t".into(),
            obligations: vec![Obligation {
                id: "c1".into(),
                statement: "runs".into(),
                proof: Proof::CommandSucceeds {
                    argv: argv.iter().map(|s| s.to_string()).collect(),
                    cwd: None,
                    timeout_ms: Some(5_000),
                },
            }],
            forbidden_regressions: Vec::new(),
        };
        let dir = std::env::temp_dir().join(format!("rex-verify-scoring-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut ev = EvidenceStore::open(&dir.join("ev")).unwrap();

        // no allowlist: model-facing denial stands (old behavior preserved)
        let r = verify_contract(&mk(vec!["sh", "-c", "true"]), &dir, &mut ev);
        assert!(!r.executable_all_proven);
        assert!(r.outcomes[0].detail.contains("blocked by hard policy"));

        // allowlist with an exiting-zero command proves the obligation
        let r = verify_contract_with_scoring(&mk(vec!["true"]), &dir, &mut ev, Some(&["true"]));
        assert!(r.executable_all_proven, "{:?}", r.outcomes[0].detail);

        // allowlist with a nonzero exit fails the obligation but EXECUTED
        let r = verify_contract_with_scoring(&mk(vec!["false"]), &dir, &mut ev, Some(&["true"]));
        assert!(!r.executable_all_proven);
        assert!(!r.outcomes[0].detail.contains("blocked by hard policy"));

        // allowlist never admits shells even when named
        let r = verify_contract_with_scoring(
            &mk(vec!["sh", "-c", "true"]),
            &dir,
            &mut ev,
            Some(&["true"]),
        );
        assert!(!r.executable_all_proven);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn behavior_claims_wait_for_the_judge() {
        let dir = tempfile::tempdir().unwrap();
        let mut ev = EvidenceStore::open(&dir.path().join("ev")).unwrap();
        let c = contract_with(r#"{"kind":"behavior_evidence","description":"UI renders"}"#);
        let report = verify_contract(&c, dir.path(), &mut ev);
        assert_eq!(report.outcomes[0].status, ObligationStatus::AwaitingJudge);
        assert!(report.executable_all_proven);
    }
}
