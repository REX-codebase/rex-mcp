//! Acceptance contracts: the spec compiler's machine-checkable output.
//!
//! The worker model drafts a contract; this module parses and validates it
//! deterministically. A contract that cannot be executed or inspected is
//! rejected - the model gets the validation errors back and must repair its
//! draft. Nothing in a contract is trusted until the verifier re-executes it.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// One machine-checkable proof attached to an obligation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Proof {
    /// Run an allowed command in the workspace; it must exit 0.
    CommandSucceeds {
        argv: Vec<String>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    /// Run an allowed command; stdout+stderr must contain the needle.
    CommandOutputContains {
        argv: Vec<String>,
        needle: String,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    /// A file must exist in the workspace and be non-empty.
    FileExists { path: String },
    /// A file must exist and contain the needle.
    FileContains { path: String, needle: String },
    /// Observable behavior that no command can check. Only the clean-room
    /// judge can pass it, and only by citing run evidence.
    BehaviorEvidence { description: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Obligation {
    pub id: String,
    pub statement: String,
    pub proof: Proof,
}

/// A change the run must not cause. Checked by the adversary and judge;
/// deterministic checks attach to them in later phases.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ForbiddenRegression {
    pub id: String,
    pub statement: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AcceptanceContract {
    pub task: String,
    pub obligations: Vec<Obligation>,
    #[serde(default)]
    pub forbidden_regressions: Vec<ForbiddenRegression>,
}

pub const MAX_OBLIGATIONS: usize = 24;
pub const MAX_REGRESSIONS: usize = 12;
const MAX_FIELD_CHARS: usize = 2_000;

/// Extract the contract JSON object from model text. The model is told to
/// answer with JSON only, but anything extra is cut away deterministically:
/// the largest balanced {...} span wins, and prose around it is discarded.
pub fn extract_json(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut best: Option<(usize, usize)> = None;
    let mut stack: Vec<usize> = Vec::new();
    let mut in_string = false;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate() {
        if in_string {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => stack.push(i),
            b'}' => {
                if let Some(start) = stack.pop() {
                    if stack.is_empty() {
                        let span = (start, i + 1);
                        if best.map(|(s, e)| e - s).unwrap_or(0) < span.1 - span.0 {
                            best = Some(span);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    best.map(|(s, e)| text[s..e].to_string())
}

/// Parse and validate a contract draft. Every error is deterministic and
/// explainable to the model for repair.
pub fn parse_contract(text: &str, task: &str) -> Result<AcceptanceContract, Vec<String>> {
    let mut errors = Vec::new();
    let json = match extract_json(text) {
        Some(j) => j,
        None => return Err(vec!["no JSON object found in the draft".into()]),
    };
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Raw {
        obligations: Vec<Obligation>,
        #[serde(default)]
        forbidden_regressions: Vec<ForbiddenRegression>,
    }
    let raw: Raw = match serde_json::from_str(&json) {
        Ok(r) => r,
        Err(e) => return Err(vec![format!("contract JSON is invalid: {e}")]),
    };
    let mut seen = HashSet::new();
    if raw.obligations.is_empty() {
        errors.push("contract has no obligations".into());
    }
    if raw.obligations.len() > MAX_OBLIGATIONS {
        errors.push(format!("contract has more than {MAX_OBLIGATIONS} obligations"));
    }
    if raw.forbidden_regressions.len() > MAX_REGRESSIONS {
        errors.push(format!("contract has more than {MAX_REGRESSIONS} forbidden regressions"));
    }
    for ob in &raw.obligations {
        if ob.id.trim().is_empty() {
            errors.push("an obligation has an empty id".into());
        } else if !seen.insert(ob.id.clone()) {
            errors.push(format!("duplicate obligation id {}", ob.id));
        }
        if ob.statement.trim().is_empty() {
            errors.push(format!("obligation {} has an empty statement", ob.id));
        }
        if ob.statement.chars().count() > MAX_FIELD_CHARS {
            errors.push(format!("obligation {} statement is too long", ob.id));
        }
        match &ob.proof {
            Proof::CommandSucceeds { argv, .. } | Proof::CommandOutputContains { argv, .. }
                if argv.is_empty() || argv[0].trim().is_empty() =>
            {
                errors.push(format!("obligation {} has an empty command", ob.id));
            }
            Proof::CommandOutputContains { needle, .. } if needle.is_empty() => {
                errors.push(format!("obligation {} has an empty needle", ob.id));
            }
            Proof::FileExists { path } if path.trim().is_empty() => {
                errors.push(format!("obligation {} has an empty path", ob.id));
            }
            Proof::FileContains { path, needle } if path.trim().is_empty() || needle.is_empty() => {
                errors.push(format!("obligation {} has an empty path or needle", ob.id));
            }
            Proof::BehaviorEvidence { description } if description.trim().is_empty() => {
                errors.push(format!("obligation {} has an empty behavior description", ob.id));
            }
            _ => {}
        }
    }
    for reg in &raw.forbidden_regressions {
        if reg.id.trim().is_empty() || reg.statement.trim().is_empty() {
            errors.push("a forbidden regression has an empty id or statement".into());
        }
    }
    if errors.is_empty() {
        Ok(AcceptanceContract {
            task: task.to_string(),
            obligations: raw.obligations,
            forbidden_regressions: raw.forbidden_regressions,
        })
    } else {
        Err(errors)
    }
}

/// The drafting prompt handed to the worker model.
pub fn drafting_prompt(task: &str) -> String {
    format!(
        "You are the REX Ultra spec compiler. Turn this task into an acceptance contract.\n\n\
TASK:\n{task}\n\n\
Answer with ONE JSON object and nothing else:\n\
{{\"obligations\": [{{\"id\": \"short-id\", \"statement\": \"what must be true\", \"proof\": PROOF}}...], \
\"forbidden_regressions\": [{{\"id\": \"...\", \"statement\": \"what must not break\"}}...]}}\n\n\
PROOF is one of:\n\
- {{\"kind\":\"command_succeeds\",\"argv\":[\"cmd\",\"arg\"...],\"cwd\":null,\"timeout_ms\":null}}\n\
- {{\"kind\":\"command_output_contains\",\"argv\":[...],\"needle\":\"text\",\"cwd\":null,\"timeout_ms\":null}}\n\
- {{\"kind\":\"file_exists\",\"path\":\"relative/path\"}}\n\
- {{\"kind\":\"file_contains\",\"path\":\"relative/path\",\"needle\":\"text\"}}\n\
- {{\"kind\":\"behavior_evidence\",\"description\":\"observable behavior no command can check\"}}\n\n\
Rules: every obligation must be provable; prefer executable proofs over behavior claims; \
commands run inside the task workspace with no shell; keep obligations independent and minimal; \
cover the task's failure modes, not just its happy path. {MAX_OBLIGATIONS} obligations maximum."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_contract_parses() {
        let draft = r#"{"obligations":[{"id":"builds","statement":"project compiles","proof":{"kind":"command_succeeds","argv":["cargo","check"]}}],"forbidden_regressions":[]}"#;
        let c = parse_contract(draft, "task").expect("valid");
        assert_eq!(c.obligations.len(), 1);
        assert_eq!(c.task, "task");
    }

    #[test]
    fn prose_around_json_is_stripped() {
        let draft = "Here is the contract:\n{\"obligations\":[{\"id\":\"a\",\"statement\":\"s\",\"proof\":{\"kind\":\"file_exists\",\"path\":\"x\"}}]}\nHope this helps.";
        assert!(parse_contract(draft, "t").is_ok());
    }

    #[test]
    fn empty_obligations_rejected() {
        let draft = r#"{"obligations":[]}"#;
        let errs = parse_contract(draft, "t").unwrap_err();
        assert!(errs.iter().any(|e| e.contains("no obligations")));
    }

    #[test]
    fn duplicate_ids_rejected() {
        let draft = r#"{"obligations":[{"id":"a","statement":"s","proof":{"kind":"file_exists","path":"x"}},{"id":"a","statement":"s2","proof":{"kind":"file_exists","path":"y"}}]}"#;
        let errs = parse_contract(draft, "t").unwrap_err();
        assert!(errs.iter().any(|e| e.contains("duplicate")));
    }

    #[test]
    fn unknown_proof_kind_rejected() {
        let draft = r#"{"obligations":[{"id":"a","statement":"s","proof":{"kind":"trust_me"}}]}"#;
        assert!(parse_contract(draft, "t").is_err());
    }

    #[test]
    fn unknown_fields_rejected() {
        let draft = r#"{"obligations":[{"id":"a","statement":"s","proof":{"kind":"file_exists","path":"x"}}],"extra":true}"#;
        assert!(parse_contract(draft, "t").is_err());
    }

    #[test]
    fn no_json_rejected() {
        assert!(parse_contract("no json here", "t").is_err());
    }
}
