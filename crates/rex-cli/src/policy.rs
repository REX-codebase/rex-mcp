//! Cross-repo proof contracts (leapfrog bet 10).
//!
//! A repo declares its proof contract in `.rex/policy.json`. When
//! `rex exec` runs with `--workspace W`, the contract is loaded from `W`
//! *before* staging and enforced as a gate: the run either satisfies the
//! contract or it does not start. The receipt records the policy and the
//! evaluation, so anyone can check compliance offline from the receipt
//! alone — no need to re-read the repo.
//!
//! Fail-closed: a malformed contract file blocks the run. A contract that
//! caps cost but names a model with no known price also blocks, because
//! the cap cannot be proven.

use serde_json::Value;
use std::path::Path;

const SCHEMA: &str = "rex.policy/1";

#[derive(Debug, Clone, Default)]
pub struct Policy {
    /// The run must be executed under an accepted bid (`--accept-bid`).
    pub require_bid: bool,
    /// Worst-case cost ceiling in USD; enforced against the bid's ceiling.
    pub max_cost_usd: Option<f64>,
    /// If set, the provider must be one of these.
    pub allowed_providers: Option<Vec<String>>,
}

#[derive(Debug)]
pub struct PolicyError(pub String);

impl std::fmt::Display for PolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Load `.rex/policy.json` from a workspace root. `Ok(None)` when the
/// repo declares no contract.
pub fn load(workspace: &Path) -> Result<Option<Policy>, PolicyError> {
    let path = workspace.join(".rex").join("policy.json");
    let raw = match std::fs::read_to_string(&path) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(PolicyError(format!("cannot read {}: {e}", path.display()))),
    };
    let v: Value = serde_json::from_str(&raw)
        .map_err(|e| PolicyError(format!("{} is not valid JSON: {e}", path.display())))?;
    if v.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(PolicyError(format!(
            "{}: expected schema {SCHEMA:?}",
            path.display()
        )));
    }
    let get_bool = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
    let get_f64 = |k: &str| v.get(k).and_then(Value::as_f64).filter(|x| *x > 0.0);
    let allowed_providers = v.get("allowed_providers").and_then(|a| {
        a.as_array().map(|xs| {
            xs.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
    });
    Ok(Some(Policy {
        require_bid: get_bool("require_bid"),
        max_cost_usd: get_f64("max_cost_usd"),
        allowed_providers,
    }))
}

impl Policy {
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "schema": SCHEMA,
            "require_bid": self.require_bid,
            "max_cost_usd": self.max_cost_usd,
            "allowed_providers": self.allowed_providers,
        })
    }

    /// Enforce the contract against a planned run. `bid_ceiling_usd` is
    /// `Some` when the bid produced a worst-case ceiling, `None` when the
    /// model price is unknown. `bid_accepted` says whether `--accept-bid`
    /// was given.
    pub fn check(
        &self,
        provider: &str,
        bid_accepted: bool,
        bid_ceiling_usd: Option<f64>,
    ) -> Result<(), PolicyError> {
        if let Some(allowed) = &self.allowed_providers {
            if !allowed.iter().any(|p| p == provider) {
                return Err(PolicyError(format!(
                    "policy allows providers {allowed:?}; '{provider}' is not one of them"
                )));
            }
        }
        if self.require_bid && !bid_accepted {
            return Err(PolicyError(
                "policy requires an accepted bid: re-run with --accept-bid".to_string(),
            ));
        }
        if let Some(cap) = self.max_cost_usd {
            match bid_ceiling_usd {
                Some(ceiling) if ceiling <= cap => {}
                Some(ceiling) => {
                    return Err(PolicyError(format!(
                        "policy caps worst-case cost at ${cap}; this run's ceiling is ${ceiling}"
                    )))
                }
                None => {
                    return Err(PolicyError(format!(
                        "policy caps worst-case cost at ${cap}, but the model has no known price: \
                         the cap cannot be proven (see rex exec --bid)"
                    )))
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_policy(dir: &std::path::Path, body: &str) {
        let rex = dir.join(".rex");
        std::fs::create_dir_all(&rex).unwrap();
        let mut f = std::fs::File::create(rex.join("policy.json")).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn no_policy_file_is_no_contract() {
        let dir = std::env::temp_dir().join("rex-policy-none");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(load(&dir).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_full_policy() {
        let dir = std::env::temp_dir().join("rex-policy-full");
        let _ = std::fs::remove_dir_all(&dir);
        write_policy(
            &dir,
            r#"{"schema":"rex.policy/1","require_bid":true,"max_cost_usd":5.0,"allowed_providers":["anthropic"]}"#,
        );
        let p = load(&dir).unwrap().unwrap();
        assert!(p.require_bid);
        assert_eq!(p.max_cost_usd, Some(5.0));
        assert_eq!(
            p.allowed_providers.as_deref(),
            Some(["anthropic".to_string()].as_slice())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wrong_schema_fails_closed() {
        let dir = std::env::temp_dir().join("rex-policy-bad");
        let _ = std::fs::remove_dir_all(&dir);
        write_policy(&dir, r#"{"schema":"other/1"}"#);
        assert!(load(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_json_fails_closed() {
        let dir = std::env::temp_dir().join("rex-policy-malformed");
        let _ = std::fs::remove_dir_all(&dir);
        write_policy(&dir, r#"{"schema": "#);
        assert!(load(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_require_bid() {
        let p = Policy {
            require_bid: true,
            ..Default::default()
        };
        assert!(p.check("anthropic", false, None).is_err());
        assert!(p.check("anthropic", true, None).is_ok());
    }

    #[test]
    fn check_cost_cap() {
        let p = Policy {
            max_cost_usd: Some(5.0),
            ..Default::default()
        };
        assert!(p.check("anthropic", true, Some(3.75)).is_ok());
        assert!(p.check("anthropic", true, Some(5.01)).is_err());
        // Unknown ceiling cannot prove the cap: fail closed.
        assert!(p.check("anthropic", true, None).is_err());
    }

    #[test]
    fn check_provider_allowlist() {
        let p = Policy {
            allowed_providers: Some(vec!["anthropic".to_string()]),
            ..Default::default()
        };
        assert!(p.check("anthropic", true, None).is_ok());
        assert!(p.check("openai", true, None).is_err());
    }
}
