//! Daemon-executed verifier: the daemon runs every contract proof inside
//! an isolated candidate workspace and records the outcomes itself. Host
//! prose can request checks; it can never set a verdict. A proof that
//! cannot run records proven=false with the reason, never a silent pass.

use crate::contract::{AcceptanceContract, Proof};
use rex_tools::{ToolRequest, ToolRuntime};
use std::fs;
use std::path::Path;

/// Contract commands are checks, not builds: hard cap their runtime.
pub const MAX_PROOF_COMMAND_MS: u64 = 120_000;
const DEFAULT_PROOF_COMMAND_MS: u64 = 30_000;
/// File proofs read bounded bytes.
const MAX_FILE_PROOF_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProofOutcome {
    pub obligation_id: String,
    pub proven: bool,
    pub detail: String,
}

/// Execute every obligation proof against the candidate tree at
/// `candidate_root`. Deterministic order: contract order.
pub fn execute_proofs(contract: &AcceptanceContract, candidate_root: &Path) -> Vec<ProofOutcome> {
    contract
        .obligations
        .iter()
        .map(|o| {
            let (proven, detail) = execute_proof(&o.proof, candidate_root);
            ProofOutcome {
                obligation_id: o.id.clone(),
                proven,
                detail,
            }
        })
        .collect()
}

fn execute_proof(proof: &Proof, root: &Path) -> (bool, String) {
    match proof {
        Proof::FileExists { path } => match resolve(root, path) {
            Ok(full) => match fs::metadata(&full) {
                Ok(m) if m.is_file() && m.len() > 0 => (true, format!("{path} exists")),
                Ok(_) => (false, format!("{path} exists but is empty or not a file")),
                Err(e) => (false, format!("{path}: {e}")),
            },
            Err(d) => (false, d),
        },
        Proof::FileContains { path, needle } => match resolve(root, path) {
            Ok(full) => match fs::metadata(&full) {
                Ok(m) if m.len() > MAX_FILE_PROOF_BYTES => {
                    (false, format!("{path} exceeds the file-proof byte cap"))
                }
                Ok(_) => match fs::read(&full) {
                    Ok(bytes) => {
                        let text = String::from_utf8_lossy(&bytes);
                        if text.contains(needle.as_str()) {
                            (true, format!("{path} contains the needle"))
                        } else {
                            (false, format!("{path} does not contain the needle"))
                        }
                    }
                    Err(e) => (false, format!("{path}: {e}")),
                },
                Err(e) => (false, format!("{path}: {e}")),
            },
            Err(d) => (false, d),
        },
        Proof::CommandSucceeds {
            argv,
            cwd,
            timeout_ms,
        } => run_command(root, argv, cwd.as_deref(), *timeout_ms, None),
        Proof::CommandOutputContains {
            argv,
            needle,
            cwd,
            timeout_ms,
        } => run_command(root, argv, cwd.as_deref(), *timeout_ms, Some(needle)),
        Proof::BehaviorEvidence { .. } => (
            false,
            "host-judged behavior proofs are not executable by the daemon".into(),
        ),
    }
}

fn resolve(root: &Path, rel: &str) -> Result<std::path::PathBuf, String> {
    crate::promotion::validate_bundle_path(rel).map_err(|e| format!("path {rel:?}: {e:?}"))?;
    Ok(root.join(rel))
}

fn run_command(
    root: &Path,
    argv: &[String],
    cwd: Option<&str>,
    timeout_ms: Option<u64>,
    needle: Option<&str>,
) -> (bool, String) {
    if argv.is_empty() {
        return (false, "empty argv".into());
    }
    if let Some(c) = cwd {
        if let Err(e) = crate::promotion::validate_bundle_path(c) {
            return (false, format!("cwd {c:?}: {e:?}"));
        }
    }
    let runtime = match ToolRuntime::new(root) {
        Ok(r) => r,
        Err(e) => return (false, format!("tool runtime: {}", e.detail)),
    };
    let budget = timeout_ms
        .unwrap_or(DEFAULT_PROOF_COMMAND_MS)
        .min(MAX_PROOF_COMMAND_MS);
    let prepared = match runtime.prepare(ToolRequest::RunCommand {
        argv: argv.to_vec(),
        cwd: cwd.map(str::to_string),
        timeout_ms: Some(budget),
    }) {
        Ok(p) => p,
        Err(e) => return (false, format!("rejected by tool policy: {}", e.detail)),
    };
    // The daemon executes its own verifier checks; it is the trusted
    // approver for these isolated candidate-workspace runs.
    if prepared.approval_required {
        if let Err(e) = runtime.resolve_approval(&prepared.call_id, true) {
            return (false, format!("approval: {}", e.detail));
        }
    }
    let result = runtime.execute(&prepared.call_id);
    if !result.ok {
        let detail = result
            .error
            .map(|e| e.detail)
            .unwrap_or_else(|| "execution failed".into());
        return (false, detail);
    }
    let exit_code = result.receipt.exit_code;
    match needle {
        None => {
            if exit_code == Some(0) {
                (true, "command exited 0".into())
            } else {
                (false, format!("exit code {exit_code:?}"))
            }
        }
        Some(n) => {
            let output = result.output.unwrap_or_default();
            if exit_code == Some(0) && output.contains(n) {
                (true, "command exited 0 with the needle in output".into())
            } else {
                (false, format!("exit code {exit_code:?} or needle missing"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::Obligation;

    fn contract_with(proof: Proof) -> AcceptanceContract {
        AcceptanceContract {
            task: "t".into(),
            work_kind: crate::contract::WorkKind::General,
            obligations: vec![Obligation {
                id: "ob-1".into(),
                statement: "s".into(),
                proof,
            }],
            forbidden_regressions: Vec::new(),
        }
    }

    #[test]
    fn file_proofs_run_against_the_candidate_tree() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        fs::write(root.join("hello.txt"), "hello world").unwrap();
        let c = contract_with(Proof::FileContains {
            path: "hello.txt".into(),
            needle: "hello".into(),
        });
        let out = execute_proofs(&c, root);
        assert!(out[0].proven);
        let c = contract_with(Proof::FileContains {
            path: "hello.txt".into(),
            needle: "goodbye".into(),
        });
        assert!(!execute_proofs(&c, root)[0].proven);
        let c = contract_with(Proof::FileExists {
            path: "missing.txt".into(),
        });
        assert!(!execute_proofs(&c, root)[0].proven);
        // Traversal is rejected, never resolved outside the candidate tree.
        let c = contract_with(Proof::FileExists {
            path: "../escape.txt".into(),
        });
        assert!(!execute_proofs(&c, root)[0].proven);
    }

    #[test]
    fn command_proofs_execute_in_the_candidate_workspace() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        fs::write(root.join("marker.txt"), "x").unwrap();
        let c = contract_with(Proof::CommandSucceeds {
            argv: vec!["test".into(), "-f".into(), "marker.txt".into()],
            cwd: None,
            timeout_ms: None,
        });
        assert!(execute_proofs(&c, root)[0].proven);
        let c = contract_with(Proof::CommandSucceeds {
            argv: vec!["test".into(), "-f".into(), "absent.txt".into()],
            cwd: None,
            timeout_ms: None,
        });
        assert!(!execute_proofs(&c, root)[0].proven);
        let c = contract_with(Proof::CommandOutputContains {
            argv: vec!["echo".into(), "needle-here".into()],
            needle: "needle-here".into(),
            cwd: None,
            timeout_ms: None,
        });
        assert!(execute_proofs(&c, root)[0].proven);
    }

    #[test]
    fn behavior_prose_is_not_a_proof() {
        let d = tempfile::tempdir().unwrap();
        let c = contract_with(Proof::BehaviorEvidence {
            description: "trust me".into(),
        });
        assert!(!execute_proofs(&c, d.path())[0].proven);
    }
}
