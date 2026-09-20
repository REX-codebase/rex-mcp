//! The least-authority tool contract. The model is told exactly the tools
//! enabled for THIS call, derived from the harness's actual offering -
//! never from a global list of what REX can do in general. A capability the
//! call does not have must read as nonexistent, not merely discouraged.

use crate::roles::ToolPolicy;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolSpec {
    pub name: String,
    pub summary: String,
    pub requires_approval: bool,
    pub mutates: bool,
}

impl ToolSpec {
    pub fn new(name: &str, summary: &str, requires_approval: bool, mutates: bool) -> Self {
        Self {
            name: name.to_string(),
            summary: summary.to_string(),
            requires_approval,
            mutates,
        }
    }
}

/// Tool names a read-only role keeps. Anything mutating is removed even if
/// it appears here; the `mutates` flag is the final word.
pub const READ_ONLY_TOOLS: &[&str] = &[
    "update_plan",
    "read_file",
    "search_files",
    "web_search",
    "complete_task",
];

/// Apply a role's tool policy to the call's actual offering.
pub fn filter_for_policy(specs: &[ToolSpec], policy: ToolPolicy) -> Vec<ToolSpec> {
    match policy {
        ToolPolicy::All => specs.to_vec(),
        ToolPolicy::None => Vec::new(),
        ToolPolicy::ReadOnly => specs
            .iter()
            .filter(|s| READ_ONLY_TOOLS.contains(&s.name.as_str()) && !s.mutates)
            .cloned()
            .collect(),
    }
}

/// Render the tool contract module body for the tools enabled at this call.
pub fn render_contract(specs: &[ToolSpec]) -> String {
    if specs.is_empty() {
        return "No tools are enabled for this call. Answer from the supplied context only. \
Do not claim to have read, written, run or searched anything beyond what is in front of you."
            .to_string();
    }
    let mut out = String::from(
        "You may use exactly the tools listed here. Tools not listed do not exist in this run: \
do not call them and do not claim their results. Tool outcomes come back from the harness as \
observations; a call you did not make did not happen.\n",
    );
    for spec in specs {
        let mut tags = Vec::new();
        tags.push(if spec.mutates { "mutating" } else { "read-only" });
        if spec.requires_approval {
            tags.push("requires trusted approval");
        }
        out.push_str(&format!("- {} ({}) - {}\n", spec.name, tags.join(", "), spec.summary));
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offering() -> Vec<ToolSpec> {
        vec![
            ToolSpec::new("update_plan", "plan", false, false),
            ToolSpec::new("read_file", "read", false, false),
            ToolSpec::new("create_file", "create", true, true),
            ToolSpec::new("edit_file", "edit", true, true),
            ToolSpec::new("search_files", "search", false, false),
            ToolSpec::new("run_command", "run", true, true),
            ToolSpec::new("web_search", "web", false, false),
            ToolSpec::new("complete_task", "done", false, false),
        ]
    }

    #[test]
    fn read_only_policy_strips_every_mutating_tool() {
        let scoped = filter_for_policy(&offering(), ToolPolicy::ReadOnly);
        let names: Vec<&str> = scoped.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            ["update_plan", "read_file", "search_files", "web_search", "complete_task"]
        );
        let contract = render_contract(&scoped);
        // tool overclaiming defence: mutating tools must not exist in the contract
        for gone in ["create_file", "edit_file", "run_command"] {
            assert!(!contract.contains(gone), "contract leaked {gone}");
        }
        assert!(contract.contains("do not exist in this run"));
    }

    #[test]
    fn none_policy_renders_an_explicit_no_tools_contract() {
        let scoped = filter_for_policy(&offering(), ToolPolicy::None);
        assert!(scoped.is_empty());
        let contract = render_contract(&scoped);
        assert!(contract.contains("No tools are enabled"));
        assert!(!contract.contains("read_file ("));
    }

    #[test]
    fn all_policy_keeps_the_full_offering_with_tags() {
        let scoped = filter_for_policy(&offering(), ToolPolicy::All);
        assert_eq!(scoped.len(), 8);
        let contract = render_contract(&scoped);
        assert!(contract.contains("create_file (mutating, requires trusted approval)"));
        assert!(contract.contains("read_file (read-only)"));
    }
}
