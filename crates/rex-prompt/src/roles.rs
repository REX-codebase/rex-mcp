//! Role cards: the mission and limits of the persona behind one model call.
//! Each card is fixed text; a role also declares which modules it may
//! receive (its allowlist) and its least-authority tool policy.

use crate::ModuleKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The Simple autonomous worker.
    Worker,
    /// Ultra's builder phase: same worker, inside the verification institution.
    Builder,
    /// Ultra phase 1: compiles a task into an acceptance contract.
    ContractDrafter,
    /// Read-only hostile inspection of a claimed-complete result.
    Adversary,
    /// Re-executes candidate work in a shadow world and reports differences.
    Shadow,
    /// Diagnoses a failed run from its trace and proposes minimal repair.
    Recovery,
    /// Verdicts from artifacts and fresh harness verification only; never
    /// sees builder narrative.
    CleanRoomJudge,
}

/// Least-authority tool policy for runs executed under a role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPolicy {
    /// The full enabled offering for this call.
    All,
    /// Observe and declare only: no writes, no edits, no commands.
    ReadOnly,
    /// No tools at all (one-shot reasoning calls).
    None,
}

impl Role {
    pub fn name(self) -> &'static str {
        match self {
            Role::Worker => "worker",
            Role::Builder => "builder",
            Role::ContractDrafter => "contract_drafter",
            Role::Adversary => "adversary",
            Role::Shadow => "shadow",
            Role::Recovery => "recovery",
            Role::CleanRoomJudge => "clean_room_judge",
        }
    }

    pub fn from_name(name: &str) -> Option<Role> {
        match name {
            "worker" => Some(Role::Worker),
            "builder" => Some(Role::Builder),
            "contract_drafter" => Some(Role::ContractDrafter),
            "adversary" => Some(Role::Adversary),
            "shadow" => Some(Role::Shadow),
            "recovery" => Some(Role::Recovery),
            "clean_room_judge" => Some(Role::CleanRoomJudge),
            _ => None,
        }
    }

    pub fn card(self) -> &'static str {
        match self {
            Role::Worker => "\
ROLE: REX WORKER
You are the worker agent in REX Simple mode. Work the task with the enabled \
tools until every part is verifiably done, then declare completion. Keep the \
plan honest: one step in progress at a time, steps marked done only when they \
are actually done. Ground yourself with reads before writes. The completion \
gate decides, not your confidence.",
            Role::Builder => "\
ROLE: REX ULTRA BUILDER
You are the builder inside REX Ultra. An acceptance contract was compiled \
before you started; build exactly what it obligates, no more and no less. \
Your work will be re-verified from scratch, attacked by an adversary and \
judged clean-room. Optimize for surviving hostile verification, not for \
looking done.",
            Role::ContractDrafter => "\
ROLE: REX ULTRA SPEC COMPILER
You compile a task into an acceptance contract: minimal, independent \
obligations, each with an executable proof where one exists. You do not \
solve the task. You define what 'done' must demonstrably mean. Answer with \
the JSON object only.",
            Role::Adversary => "\
ROLE: REX ULTRA ADVERSARY
Another agent claims the task is complete. Your only job is to prove it \
wrong: attack edge cases the obligations miss, check that proofs measure \
real behavior rather than cached or staged results, and hunt for anything \
that would embarrass the run in front of a hostile reviewer. You are \
read-only: no writes, no edits, no commands. An empty defect list is the \
only way to say the result survives you.",
            Role::Shadow => "\
ROLE: REX ULTRA SHADOW
You re-execute candidate work in a shadow copy of the world and report \
exactly what differs from the primary run's claims. You never trust the \
primary run's narrative; you trust only what you reproduced yourself. You \
are read-only against the primary workspace.",
            Role::Recovery => "\
ROLE: REX ULTRA RECOVERY
A run failed. Diagnose the root cause from the trace and the failure \
history, then propose the smallest repair that makes the acceptance \
contract provable. Do not rewrite working parts, do not expand scope, and \
do not paper over the failure with weaker checks.",
            Role::CleanRoomJudge => "\
ROLE: REX ULTRA CLEAN-ROOM JUDGE
You have never seen the builder's reasoning, plans or narration - only its \
artifacts and the harness's fresh re-verification. A claim without evidence \
is unproven. A failed verification cannot pass. A standing adversary defect \
fails whatever it undermines. Answer with the JSON object only.",
        }
    }

    /// The modules this role may receive. Anything else is a role leak and
    /// the assembler refuses it. Clean-room roles notably exclude failure
    /// history and repo twin context so no builder narrative or stale state
    /// can reach them.
    pub fn allowed_modules(self) -> &'static [ModuleKind] {
        use ModuleKind::*;
        match self {
            Role::Worker | Role::Builder | Role::Recovery => &[
                Constitution,
                RoleCard,
                RepoTwin,
                ToolContract,
                TaskContract,
                EpistemicState,
                FailureHistory,
                CompletionGate,
            ],
            Role::ContractDrafter => &[Constitution, RoleCard, RepoTwin],
            Role::Adversary => &[
                Constitution,
                RoleCard,
                RepoTwin,
                ToolContract,
                TaskContract,
                EpistemicState,
                CompletionGate,
            ],
            Role::Shadow => &[Constitution, RoleCard, ToolContract, TaskContract, EpistemicState],
            Role::CleanRoomJudge => &[Constitution, RoleCard, TaskContract, EpistemicState],
        }
    }

    pub fn allows(self, kind: ModuleKind) -> bool {
        self.allowed_modules().contains(&kind)
    }

    pub fn tool_policy(self) -> ToolPolicy {
        match self {
            Role::Worker | Role::Builder | Role::Recovery => ToolPolicy::All,
            Role::Adversary | Role::Shadow => ToolPolicy::ReadOnly,
            Role::ContractDrafter | Role::CleanRoomJudge => ToolPolicy::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_card_names_its_role() {
        for role in [
            Role::Worker,
            Role::Builder,
            Role::ContractDrafter,
            Role::Adversary,
            Role::Shadow,
            Role::Recovery,
            Role::CleanRoomJudge,
        ] {
            assert!(role.card().starts_with("ROLE: REX"), "{}", role.name());
            assert!(role.card().chars().count() <= ModuleKind::RoleCard.budget_chars());
        }
    }

    #[test]
    fn clean_room_roles_exclude_builder_narrative_modules() {
        for role in [Role::CleanRoomJudge, Role::Shadow] {
            assert!(!role.allows(ModuleKind::FailureHistory), "{}", role.name());
            assert!(!role.allows(ModuleKind::RepoTwin), "{}", role.name());
        }
    }

    #[test]
    fn adversary_is_read_only_and_judge_has_no_tools() {
        assert_eq!(Role::Adversary.tool_policy(), ToolPolicy::ReadOnly);
        assert_eq!(Role::Shadow.tool_policy(), ToolPolicy::ReadOnly);
        assert_eq!(Role::CleanRoomJudge.tool_policy(), ToolPolicy::None);
        assert_eq!(Role::ContractDrafter.tool_policy(), ToolPolicy::None);
        assert_eq!(Role::Builder.tool_policy(), ToolPolicy::All);
    }

    #[test]
    fn role_names_round_trip() {
        for role in [Role::Worker, Role::Builder, Role::Adversary, Role::CleanRoomJudge] {
            assert_eq!(Role::from_name(role.name()), Some(role));
        }
        assert_eq!(Role::from_name("nope"), None);
    }
}
