//! The skill compiler: composable, version-pinned, evidence-backed skill
//! packs instead of one shallow prompt per language.
//!
//! Selection is capability-discovered from repository facts and task
//! semantics - never from free text, which cannot self-authorize a skill or
//! a tool. Support is claimed only where certification and detected tooling
//! back it; everything else lands on an honest unsupported list. Conflicts
//! resolve only through declared rules with recorded provenance; unresolved
//! safety, toolchain or gate conflicts fail compilation.

use rex_protocol::schema::canonical_hash;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const COMPILER_VERSION: &str = "rex-skill-compiler 0.1.0";
pub const SHARED_LAWS_DOMAIN: &str = "shared-laws";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CertificationStatus {
    /// Detection, native tooling, diagnosis and executable gates all exist.
    Certified,
    /// Some layers exist; the pack may be selected but its gaps are visible.
    Partial,
    /// Probed and found unsupported in this environment.
    DetectedUnsupported,
    /// Never probed. Claimed as nothing.
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Selector {
    FileExtension { extension: String },
    Manifest { filename: String },
    Shebang { program: String },
    Medium { medium: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictKind {
    Safety,
    Toolchain,
    Gate,
    Cosmetic,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillConflict {
    pub other_pack: String,
    pub kind: ConflictKind,
    /// The declared rule that resolves this conflict, with provenance.
    /// Safety, toolchain and gate conflicts without one fail compilation.
    pub resolution: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuleModule {
    pub id: String,
    pub statement: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GateTemplate {
    pub id: String,
    pub command_hint: String,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillPackManifest {
    pub id: String,
    pub version: String,
    pub schema: u16,
    pub domains: Vec<String>,
    pub selectors: Vec<Selector>,
    pub dependencies: Vec<String>,
    pub conflicts: Vec<SkillConflict>,
    pub rules: Vec<RuleModule>,
    pub gates: Vec<GateTemplate>,
    /// Native tools this pack's gates need. A pack whose tools are not
    /// detected is reported unsupported, never silently selected.
    pub required_tools: Vec<String>,
    pub provenance: String,
    pub certification: CertificationStatus,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepositoryFacts {
    pub file_extensions: BTreeSet<String>,
    pub manifests: BTreeSet<String>,
    pub shebangs: BTreeSet<String>,
    pub requested_medium: Option<String>,
    pub detected_tools: BTreeSet<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PinnedSkillPack {
    pub id: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedConflict {
    pub pack: String,
    pub other: String,
    pub kind: ConflictKind,
    pub resolution: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompiledGate {
    pub pack: String,
    pub id: String,
    pub command_hint: String,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompiledSkillPlan {
    pub plan_hash: String,
    pub compiler_version: String,
    pub selected: Vec<PinnedSkillPack>,
    pub resolved_conflicts: Vec<ResolvedConflict>,
    pub gates: Vec<CompiledGate>,
    pub unsupported: Vec<String>,
}

fn valid_semver(version: &str) -> bool {
    let parts: Vec<&str> = version.split('.').collect();
    parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

pub fn validate_manifest(manifest: &SkillPackManifest) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    if manifest.id.trim().is_empty() {
        errors.push("pack id is required".to_string());
    }
    if !valid_semver(&manifest.version) {
        errors.push(format!("pack {:?} version {:?} is not semver x.y.z", manifest.id, manifest.version));
    }
    if manifest.schema == 0 {
        errors.push(format!("pack {:?} needs a schema version", manifest.id));
    }
    if manifest.provenance.trim().is_empty() {
        errors.push(format!("pack {:?} needs provenance", manifest.id));
    }
    for gate in &manifest.gates {
        if gate.id.trim().is_empty() || gate.command_hint.trim().is_empty() {
            errors.push(format!("pack {:?} has a gate without id or command hint", manifest.id));
        }
    }
    for rule in &manifest.rules {
        if rule.id.trim().is_empty() || rule.statement.trim().is_empty() {
            errors.push(format!("pack {:?} has a rule without id or statement", manifest.id));
        }
    }
    if errors.is_empty() { Ok(()) } else { Err(errors) }
}

fn matches(pack: &SkillPackManifest, facts: &RepositoryFacts) -> usize {
    pack.selectors
        .iter()
        .filter(|selector| match selector {
            Selector::FileExtension { extension } => facts.file_extensions.contains(extension),
            Selector::Manifest { filename } => facts.manifests.contains(filename),
            Selector::Shebang { program } => facts.shebangs.contains(program),
            Selector::Medium { medium } => facts.requested_medium.as_deref() == Some(medium.as_str()),
        })
        .count()
}

/// Compile a task-specific, version-pinned skill plan. Deterministic: same
/// registry and facts always produce the same hash.
pub fn compile_plan(
    facts: &RepositoryFacts,
    registry: &[SkillPackManifest],
) -> Result<CompiledSkillPlan, Vec<String>> {
    let mut errors = Vec::new();
    let mut by_id: BTreeMap<&str, &SkillPackManifest> = BTreeMap::new();
    for pack in registry {
        if let Err(manifest_errors) = validate_manifest(pack) {
            errors.extend(manifest_errors);
            continue;
        }
        if by_id.insert(pack.id.as_str(), pack).is_some() {
            errors.push(format!("duplicate pack id {:?}", pack.id));
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }

    let mut selected: BTreeMap<&str, &SkillPackManifest> = BTreeMap::new();
    let mut unsupported: BTreeSet<String> = BTreeSet::new();
    for pack in registry {
        let shared = pack.domains.iter().any(|d| d == SHARED_LAWS_DOMAIN);
        if !shared && matches(pack, facts) == 0 {
            continue;
        }
        match pack.certification {
            CertificationStatus::DetectedUnsupported => {
                unsupported.insert(format!("{}: probed unsupported", pack.id));
            }
            CertificationStatus::Unknown => {
                unsupported.insert(format!("{}: never probed", pack.id));
            }
            CertificationStatus::Certified | CertificationStatus::Partial => {
                let missing: Vec<&String> = pack
                    .required_tools
                    .iter()
                    .filter(|tool| !facts.detected_tools.contains(*tool))
                    .collect();
                if missing.is_empty() {
                    selected.insert(pack.id.as_str(), pack);
                } else {
                    unsupported.insert(format!(
                        "{}: required tools not detected ({})",
                        pack.id,
                        missing.iter().map(|t| t.as_str()).collect::<Vec<_>>().join(", ")
                    ));
                }
            }
        }
    }

    // Dependencies pull in their packs; a selected pack with an unknown or
    // unselectable dependency fails compilation rather than half-applying.
    let mut queue: Vec<&str> = selected.keys().cloned().collect();
    while let Some(id) = queue.pop() {
        let Some(pack) = selected.get(id).copied() else { continue };
        for dependency in &pack.dependencies {
            match by_id.get(dependency.as_str()) {
                Some(dep) if matches_certified(dep) && dep.required_tools.iter().all(|t| facts.detected_tools.contains(t)) => {
                    if selected.insert(dep.id.as_str(), dep).is_none() {
                        queue.push(dep.id.as_str());
                    }
                }
                _ => errors.push(format!(
                    "pack {:?} depends on {:?}, which is unavailable or unsupported",
                    pack.id, dependency
                )),
            }
        }
    }

    // Conflicts: only declared rules resolve. Safety, toolchain and gate
    // conflicts without one fail; cosmetic conflicts pick the pack with more
    // repository-specific matches and record the choice.
    let mut resolved: BTreeSet<ResolvedConflictKey> = BTreeSet::new();
    let mut resolved_conflicts = Vec::new();
    let ids: Vec<&str> = selected.keys().cloned().collect();
    for id in &ids {
        let pack = selected[id];
        for conflict in &pack.conflicts {
            let Some(other) = selected.get(conflict.other_pack.as_str()).copied() else { continue };
            let key = ResolvedConflictKey(std::cmp::min(id, &other.id.as_str()).to_string()
                , std::cmp::max(id, &other.id.as_str()).to_string());
            if !resolved.insert(key) { continue; }
            match (conflict.kind, &conflict.resolution) {
                (ConflictKind::Cosmetic, _) => {
                    let (kept, dropped) = if matches(pack, facts) >= matches(other, facts) {
                        (pack, other)
                    } else {
                        (other, pack)
                    };
                    resolved_conflicts.push(ResolvedConflict {
                        pack: kept.id.clone(),
                        other: dropped.id.clone(),
                        kind: ConflictKind::Cosmetic,
                        resolution: format!(
                            "cosmetic conflict resolved for the more repository-specific pack {:?} (recorded choice)",
                            kept.id
                        ),
                    });
                }
                (kind, Some(rule)) => {
                    resolved_conflicts.push(ResolvedConflict {
                        pack: pack.id.clone(),
                        other: other.id.clone(),
                        kind,
                        resolution: rule.clone(),
                    });
                }
                (kind, None) => {
                    errors.push(format!(
                        "{kind:?} conflict between {:?} and {:?} has no declared resolution rule",
                        pack.id, other.id
                    ));
                }
            }
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }

    let mut selected_packs: Vec<PinnedSkillPack> = selected
        .values()
        .map(|p| PinnedSkillPack { id: p.id.clone(), version: p.version.clone() })
        .collect();
    selected_packs.sort_by(|a, b| a.id.cmp(&b.id));
    resolved_conflicts.sort_by(|a, b| (&a.pack, &a.other).cmp(&(&b.pack, &b.other)));
    let mut gates: Vec<CompiledGate> = selected
        .values()
        .flat_map(|p| p.gates.iter().map(move |g| CompiledGate {
            pack: p.id.clone(), id: g.id.clone(), command_hint: g.command_hint.clone(), required: g.required,
        }))
        .collect();
    gates.sort_by(|a, b| (&a.pack, &a.id).cmp(&(&b.pack, &b.id)));
    let mut unsupported: Vec<String> = unsupported.into_iter().collect();
    // Broad-support honesty: recognizable languages with no certified pack
    // or broad template are reported Unknown, never silently ignored.
    unsupported.extend(crate::generated_packs::unknown_language_notes(facts));
    unsupported.sort();
    unsupported.dedup();

    let plan_hash = canonical_hash(&(
        COMPILER_VERSION,
        &selected_packs,
        &resolved_conflicts,
        &gates,
        &unsupported,
    ))
    .map_err(|e| vec![format!("plan hashing failed: {e}")])?;

    Ok(CompiledSkillPlan {
        plan_hash,
        compiler_version: COMPILER_VERSION.to_string(),
        selected: selected_packs,
        resolved_conflicts,
        gates,
        unsupported,
    })
}

fn matches_certified(pack: &SkillPackManifest) -> bool {
    matches!(pack.certification, CertificationStatus::Certified | CertificationStatus::Partial)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ResolvedConflictKey(String, String);


/// Well-known native tools probed on PATH, without executing anything.
pub const PROBED_TOOLS: &[&str] = &["cargo", "rustc", "node", "npx", "python3", "go", "javac", "ruby", "gcc", "g++"];

/// Collect repository facts from a workspace directory without executing
/// any tool: file extensions and root manifests from a bounded walk, and
/// detected tools from a PATH directory listing. Detection is honest - a
/// tool is reported only when its executable file is present, never assumed.
pub fn collect_repository_facts(workspace: &std::path::Path) -> RepositoryFacts {
    let mut facts = RepositoryFacts::default();
    collect_dir(workspace, 0, &mut facts);
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            for tool in PROBED_TOOLS {
                if dir.join(tool).is_file() {
                    facts.detected_tools.insert((*tool).to_string());
                }
            }
        }
    }
    facts
}

const MANIFEST_NAMES: &[&str] = &[
    "Cargo.toml", "package.json", "pyproject.toml", "go.mod", "pom.xml",
    "build.gradle", "Gemfile", "composer.json", "Package.swift",
];
const MAX_WALK_DEPTH: u32 = 3;
const MAX_WALK_ENTRIES: usize = 4_096;

fn collect_dir(dir: &std::path::Path, depth: u32, facts: &mut RepositoryFacts) {
    if depth > MAX_WALK_DEPTH { return; }
    let Ok(entries) = std::fs::read_dir(dir) else { return; };
    let mut count = 0;
    for entry in entries.flatten() {
        count += 1;
        if count > MAX_WALK_ENTRIES { return; }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') { continue; }
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_symlink() { continue; }
        if kind.is_dir() {
            collect_dir(&entry.path(), depth + 1, facts);
        } else if kind.is_file() {
            if depth == 0 && MANIFEST_NAMES.contains(&name.as_str()) {
                facts.manifests.insert(name.clone());
            }
            if let Some((_, extension)) = name.rsplit_once('.') {
                if !extension.is_empty() && extension.len() <= 12
                    && extension.bytes().all(|b| b.is_ascii_alphanumeric()) {
                    facts.file_extensions.insert(extension.to_string());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(id: &str, domains: &[&str], selectors: Vec<Selector>) -> SkillPackManifest {
        SkillPackManifest {
            id: id.into(),
            version: "1.0.0".into(),
            schema: 1,
            domains: domains.iter().map(|d| d.to_string()).collect(),
            selectors,
            dependencies: vec![],
            conflicts: vec![],
            rules: vec![RuleModule { id: "r1".into(), statement: "evidence required".into() }],
            gates: vec![GateTemplate { id: "g1".into(), command_hint: "run tests".into(), required: true }],
            required_tools: vec![],
            provenance: "test registry".into(),
            certification: CertificationStatus::Certified,
        }
    }

    fn facts() -> RepositoryFacts {
        RepositoryFacts {
            file_extensions: ["rs"].iter().map(|s| s.to_string()).collect(),
            manifests: ["Cargo.toml"].iter().map(|s| s.to_string()).collect(),
            shebangs: BTreeSet::new(),
            requested_medium: None,
            detected_tools: ["cargo", "rustc"].iter().map(|s| s.to_string()).collect(),
        }
    }

    fn registry() -> Vec<SkillPackManifest> {
        vec![
            pack("shared-laws", &[SHARED_LAWS_DOMAIN], vec![]),
            pack("rust", &["language"], vec![
                Selector::Manifest { filename: "Cargo.toml".into() },
                Selector::FileExtension { extension: "rs".into() },
            ]),
            pack("three-d", &["medium"], vec![Selector::Medium { medium: "three-d".into() }]),
        ]
    }

    #[test]
    fn selection_is_capability_discovered_and_deterministic() {
        let plan = compile_plan(&facts(), &registry()).unwrap();
        let ids: Vec<&str> = plan.selected.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["rust", "shared-laws"]);
        assert!(plan.unsupported.is_empty());
        assert_eq!(plan, compile_plan(&facts(), &registry()).unwrap());
    }

    #[test]
    fn requested_medium_selects_medium_packs() {
        let mut f = facts();
        f.requested_medium = Some("three-d".into());
        let plan = compile_plan(&f, &registry()).unwrap();
        let ids: Vec<&str> = plan.selected.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["rust", "shared-laws", "three-d"]);
    }

    #[test]
    fn support_is_never_claimed_without_certification_and_tools() {
        let mut unprobed = pack("mystery", &["language"], vec![Selector::FileExtension { extension: "rs".into() }]);
        unprobed.certification = CertificationStatus::Unknown;
        let mut probed_out = pack("legacy", &["language"], vec![Selector::Manifest { filename: "Cargo.toml".into() }]);
        probed_out.certification = CertificationStatus::DetectedUnsupported;
        let mut needs_tool = pack("wasm-pack", &["ecosystem"], vec![Selector::FileExtension { extension: "rs".into() }]);
        needs_tool.required_tools = vec!["wasm32-toolchain".into()];
        let mut reg = registry();
        reg.extend([unprobed, probed_out, needs_tool]);
        let plan = compile_plan(&facts(), &reg).unwrap();
        let ids: Vec<&str> = plan.selected.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["rust", "shared-laws"]);
        assert_eq!(plan.unsupported.len(), 3);
        assert!(plan.unsupported.iter().any(|u| u.contains("never probed")));
        assert!(plan.unsupported.iter().any(|u| u.contains("probed unsupported")));
        assert!(plan.unsupported.iter().any(|u| u.contains("required tools not detected")));
    }

    #[test]
    fn dependencies_pull_in_certified_packs_and_fail_on_missing_ones() {
        let mut app = pack("app", &["ecosystem"], vec![Selector::Manifest { filename: "Cargo.toml".into() }]);
        app.dependencies = vec!["rust".into()];
        let mut reg = registry();
        reg.push(app.clone());
        let plan = compile_plan(&facts(), &reg).unwrap();
        assert!(plan.selected.iter().any(|p| p.id == "app"));
        app.dependencies = vec!["nonexistent".into()];
        let mut reg = registry();
        reg.push(app);
        assert!(compile_plan(&facts(), &reg).unwrap_err().iter().any(|e| e.contains("nonexistent")));
    }

    #[test]
    fn safety_conflicts_without_a_declared_rule_fail_compilation() {
        let mut a = pack("alpha", &["language"], vec![Selector::FileExtension { extension: "rs".into() }]);
        let mut b = pack("beta", &["language"], vec![Selector::FileExtension { extension: "rs".into() }]);
        a.conflicts = vec![SkillConflict { other_pack: "beta".into(), kind: ConflictKind::Safety, resolution: None }];
        b.conflicts = vec![SkillConflict { other_pack: "alpha".into(), kind: ConflictKind::Safety, resolution: None }];
        let mut reg = registry();
        reg.extend([a, b]);
        assert!(compile_plan(&facts(), &reg).unwrap_err().iter().any(|e| e.contains("no declared resolution")));
    }

    #[test]
    fn declared_rules_resolve_hard_conflicts_once() {
        let mut a = pack("alpha", &["language"], vec![Selector::FileExtension { extension: "rs".into() }]);
        let b = pack("beta", &["language"], vec![Selector::FileExtension { extension: "rs".into() }]);
        a.conflicts = vec![SkillConflict {
            other_pack: "beta".into(),
            kind: ConflictKind::Toolchain,
            resolution: Some("repo-pinned toolchain wins (owner decision 2026-09-21)".into()),
        }];
        let mut reg = registry();
        reg.extend([a, b]);
        let plan = compile_plan(&facts(), &reg).unwrap();
        assert_eq!(plan.resolved_conflicts.len(), 1);
        assert_eq!(plan.resolved_conflicts[0].kind, ConflictKind::Toolchain);
    }

    #[test]
    fn cosmetic_conflicts_pick_the_more_repository_specific_pack() {
        let mut general = pack("general-rust", &["language"], vec![Selector::FileExtension { extension: "rs".into() }]);
        let specific = pack("workspace-rust", &["repo-local"], vec![
            Selector::FileExtension { extension: "rs".into() },
            Selector::Manifest { filename: "Cargo.toml".into() },
        ]);
        general.conflicts = vec![SkillConflict { other_pack: "workspace-rust".into(), kind: ConflictKind::Cosmetic, resolution: None }];
        let mut reg = registry();
        reg.extend([general, specific]);
        let plan = compile_plan(&facts(), &reg).unwrap();
        assert_eq!(plan.resolved_conflicts.len(), 1);
        assert_eq!(plan.resolved_conflicts[0].pack, "workspace-rust");
        assert_eq!(plan.resolved_conflicts[0].other, "general-rust");
    }

    #[test]
    fn plan_hash_is_stable_and_changes_with_selection() {
        let one = compile_plan(&facts(), &registry()).unwrap();
        assert_eq!(one.plan_hash, compile_plan(&facts(), &registry()).unwrap().plan_hash);
        let mut f = facts();
        f.requested_medium = Some("three-d".into());
        let two = compile_plan(&f, &registry()).unwrap();
        assert_ne!(one.plan_hash, two.plan_hash);
    }

    #[test]
    fn invalid_manifests_are_rejected_before_selection() {
        let mut bad = pack("", &["language"], vec![]);
        bad.version = "1.0".into();
        let errors = compile_plan(&facts(), &[bad]).unwrap_err();
        assert!(errors.iter().any(|e| e.contains("pack id is required")));
        assert!(errors.iter().any(|e| e.contains("semver")));
    }
    #[test]
    fn facts_come_from_the_workspace_and_path_without_execution() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::write(root.join("Cargo.toml"), "[package]").unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src").join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(root.join("src").join("notes.txt"), "hi").unwrap();
        let facts = collect_repository_facts(root);
        assert!(facts.manifests.contains("Cargo.toml"));
        assert!(facts.file_extensions.contains("rs"));
        assert!(facts.file_extensions.contains("txt"));
        // PATH probing reports only what is actually present.
        let path_has_cargo = std::env::var_os("PATH")
            .map(|v| std::env::split_paths(&v).any(|d| d.join("cargo").is_file()))
            .unwrap_or(false);
        assert_eq!(facts.detected_tools.contains("cargo"), path_has_cargo);
        assert!(!facts.detected_tools.contains("definitely-not-a-tool"));
    }

    #[test]
    fn facts_feed_the_first_class_registry() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::write(root.join("Cargo.toml"), "[package]").unwrap();
        std::fs::write(root.join("lib.rs"), "").unwrap();
        let facts = collect_repository_facts(root);
        let plan = compile_plan(&facts, &crate::skill_packs::first_class_registry()).unwrap();
        let path_has_cargo = std::env::var_os("PATH")
            .map(|v| std::env::split_paths(&v).any(|d| d.join("cargo").is_file()))
            .unwrap_or(false);
        assert_eq!(plan.selected.iter().any(|p| p.id == "rust"), path_has_cargo && facts.detected_tools.contains("rustc"));
        assert!(plan.selected.iter().any(|p| p.id == "shared-laws"));
    }
}
