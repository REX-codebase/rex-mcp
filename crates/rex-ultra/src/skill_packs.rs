//! First-class certified deep skill packs.
//!
//! These are the deep packs the skill compiler selects from: 3D/WebGL,
//! HTML motion/video, SVG and Rust. Each carries the semantics, laws and
//! executable gate templates of its medium - never a shallow prompt. Broad
//! language coverage beyond these is generated and capability-discovered;
//! it is claimed only when detection, native tooling, diagnosis and
//! executable gates exist, per the certification states in `skills`.

use crate::skills::{CertificationStatus, GateTemplate, RuleModule, Selector, SkillPackManifest};

fn gate(
    id: &str,
    command_hint: &str,
    required: bool,
    exec: crate::skills::GateExec,
) -> GateTemplate {
    GateTemplate {
        id: id.into(),
        command_hint: command_hint.into(),
        required,
        exec,
    }
}

fn command_gate(id: &str, command_hint: &str, required: bool, argv: &[&str]) -> GateTemplate {
    gate(
        id,
        command_hint,
        required,
        crate::skills::GateExec::Command {
            argv: argv.iter().map(|a| a.to_string()).collect(),
        },
    )
}

fn unsupported_gate(id: &str, command_hint: &str, required: bool) -> GateTemplate {
    gate(
        id,
        command_hint,
        required,
        crate::skills::GateExec::Unsupported,
    )
}

fn rule(id: &str, statement: &str) -> RuleModule {
    RuleModule {
        id: id.into(),
        statement: statement.into(),
    }
}

fn manifest(
    id: &str,
    domains: &[&str],
    selectors: Vec<Selector>,
    required_tools: &[&str],
    rules: Vec<RuleModule>,
    gates: Vec<GateTemplate>,
) -> SkillPackManifest {
    SkillPackManifest {
        id: id.into(),
        version: "0.1.0".into(),
        schema: 1,
        domains: domains.iter().map(|d| d.to_string()).collect(),
        selectors,
        dependencies: vec!["shared-laws".into()],
        conflicts: vec![],
        rules,
        gates,
        required_tools: required_tools.iter().map(|t| t.to_string()).collect(),
        provenance: "first-class deep pack, frozen architecture 2026-09-20".into(),
        certification: CertificationStatus::Certified,
    }
}

/// 3D/WebGL/Three.js: scene, camera, lighting, materials, spatial
/// composition, interaction, performance and pixel evidence.
pub fn three_d() -> SkillPackManifest {
    manifest(
        "three-d",
        &["medium"],
        vec![
            Selector::Medium {
                medium: "three-d".into(),
            },
            Selector::Medium {
                medium: "webgl".into(),
            },
            Selector::Medium {
                medium: "three-js".into(),
            },
        ],
        &["node", "npx"],
        vec![
            rule(
                "scene-graph",
                "a real scene graph with named camera, lights and materials - not a textured div",
            ),
            rule(
                "spatial-composition",
                "depth, parallax and occlusion carry the composition",
            ),
            rule(
                "interaction",
                "the central behavior responds to pointer or device input",
            ),
            rule(
                "performance",
                "steady frame budget on the target device; no unbounded geometry or texture loads",
            ),
        ],
        vec![
            unsupported_gate("build", "bundle the scene with the pinned toolchain", true),
            unsupported_gate(
                "pixel-evidence-desktop",
                "headless screenshot at the declared desktop viewport, hash recorded",
                true,
            ),
            unsupported_gate(
                "pixel-evidence-phone",
                "headless screenshot at the declared phone viewport, hash recorded",
                true,
            ),
            unsupported_gate(
                "interaction-replay",
                "recorded replay of the central behavior input",
                true,
            ),
            unsupported_gate(
                "frame-budget",
                "measured frame times against the declared budget",
                false,
            ),
        ],
    )
}

/// HTML motion/video: continuous timeline, choreography and easing with
/// deterministic capture. Animated-slide output is rejected outright; any
/// original score stays inside its declared rights boundary.
pub fn html_motion() -> SkillPackManifest {
    manifest(
        "html-motion",
        &["medium"],
        vec![
            Selector::Medium { medium: "html-motion".into() },
            Selector::Medium { medium: "video".into() },
            Selector::Medium { medium: "animation".into() },
        ],
        &["node", "npx"],
        vec![
            rule("continuous-timeline", "one continuous timeline with choreography and easing - keyframed slides are rejected"),
            rule("not-slides", "frame analysis over a window must show continuous motion, not slide transitions"),
            rule("deterministic-capture", "the same run captures the same frames within tolerance"),
            rule("score-boundary", "any audio is original or inside its declared rights boundary"),
        ],
        vec![
            unsupported_gate("timeline-probe", "assert the declared timeline duration and easing curves exist", true),
            unsupported_gate("frame-difference", "sampled frame differences prove continuous motion over the window", true),
            unsupported_gate("deterministic-capture", "two captures of the same run match within tolerance", true),
            unsupported_gate("audio-boundary", "audio assets carry their declared rights metadata", false),
        ],
    )
}

/// SVG: path topology, viewBox, typography, animation, accessibility and
/// export correctness.
pub fn svg() -> SkillPackManifest {
    manifest(
        "svg",
        &["medium"],
        vec![
            Selector::Medium {
                medium: "svg".into(),
            },
            Selector::FileExtension {
                extension: "svg".into(),
            },
        ],
        &["node", "npx"],
        vec![
            rule(
                "topology",
                "paths and groups are structured for the drawing, not exported noise",
            ),
            rule(
                "viewbox",
                "a declared viewBox scales the artwork without distortion",
            ),
            rule(
                "accessible",
                "role, title and description make the graphic readable to assistive tech",
            ),
        ],
        vec![
            unsupported_gate("xml-parse", "the artwork parses as valid XML/SVG", true),
            unsupported_gate(
                "viewbox-check",
                "viewBox present and consistent with the declared aspect",
                true,
            ),
            unsupported_gate("a11y-check", "role/img, title and desc present", true),
            unsupported_gate(
                "raster-export",
                "rasterized export at the declared sizes matches within tolerance",
                false,
            ),
        ],
    )
}

/// Rust: ownership, async, unsafe boundaries, crates, clippy, fuzzing and
/// cross-platform builds.
pub fn rust() -> SkillPackManifest {
    manifest(
        "rust",
        &["language", "ecosystem"],
        vec![
            Selector::Manifest {
                filename: "Cargo.toml".into(),
            },
            Selector::FileExtension {
                extension: "rs".into(),
            },
        ],
        &["cargo", "rustc"],
        vec![
            rule(
                "ownership",
                "ownership and borrowing are the design, not an afterthought",
            ),
            rule(
                "unsafe-boundary",
                "unsafe is contained, justified and documented at the boundary",
            ),
            rule(
                "error-model",
                "fallible operations return typed errors; no silent unwrap in library paths",
            ),
        ],
        vec![
            command_gate(
                "cargo-test",
                "cargo test passes for the workspace",
                true,
                &["cargo", "test"],
            ),
            command_gate(
                "clippy-deny",
                "cargo clippy with warnings denied",
                true,
                &["cargo", "clippy", "--", "-D", "warnings"],
            ),
            command_gate(
                "fmt-check",
                "cargo fmt --check is clean",
                true,
                &["cargo", "fmt", "--check"],
            ),
            command_gate(
                "doc-build",
                "cargo doc builds without broken links",
                false,
                &["cargo", "doc", "--no-deps"],
            ),
        ],
    )
}

/// Shared engineering laws: always selected, whatever the medium.
pub fn shared_laws() -> SkillPackManifest {
    SkillPackManifest {
        id: "shared-laws".into(),
        version: "0.1.0".into(),
        schema: 1,
        domains: vec![crate::skills::SHARED_LAWS_DOMAIN.into()],
        selectors: vec![],
        dependencies: vec![],
        conflicts: vec![],
        rules: vec![
            rule(
                "evidence",
                "claims are backed by executable evidence, never prose",
            ),
            rule(
                "accessibility",
                "output is usable with assistive technology and keyboard alone",
            ),
            rule(
                "security",
                "no secrets in artifacts; untrusted input is validated at the boundary",
            ),
            rule(
                "scope",
                "only the task's declared scope changes; incidental edits are defects",
            ),
        ],
        gates: vec![gate(
            "scope-diff",
            "the changed surface matches the sealed bundle's declared paths",
            true,
            crate::skills::GateExec::ScopeDiffVsBundle,
        )],
        required_tools: vec![],
        provenance: "shared engineering laws, frozen architecture 2026-09-20".into(),
        certification: CertificationStatus::Certified,
    }
}

/// The certified first-class registry.
pub fn first_class_registry() -> Vec<SkillPackManifest> {
    vec![shared_laws(), three_d(), html_motion(), svg(), rust()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::{compile_plan, validate_manifest, RepositoryFacts};
    use std::collections::BTreeSet;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn every_first_class_pack_validates() {
        for pack in first_class_registry() {
            assert_eq!(validate_manifest(&pack), Ok(()), "pack {}", pack.id);
        }
    }

    #[test]
    fn rust_facts_select_the_rust_pack_with_its_gates() {
        let facts = RepositoryFacts {
            file_extensions: set(&["rs"]),
            manifests: set(&["Cargo.toml"]),
            shebangs: BTreeSet::new(),
            requested_medium: None,
            detected_tools: set(&["cargo", "rustc"]),
        };
        let plan = compile_plan(&facts, &first_class_registry()).unwrap();
        let ids: Vec<&str> = plan.selected.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["rust", "shared-laws"]);
        let gate_ids: Vec<&str> = plan.gates.iter().map(|g| g.id.as_str()).collect();
        assert!(gate_ids.contains(&"cargo-test"));
        assert!(gate_ids.contains(&"clippy-deny"));
        assert!(gate_ids.contains(&"scope-diff"));
    }

    #[test]
    fn medium_requests_select_their_deep_pack() {
        for (medium, pack_id, marker) in [
            ("three-d", "three-d", "pixel-evidence-desktop"),
            ("html-motion", "html-motion", "frame-difference"),
            ("svg", "svg", "viewbox-check"),
        ] {
            let facts = RepositoryFacts {
                requested_medium: Some(medium.into()),
                detected_tools: set(&["node", "npx"]),
                ..Default::default()
            };
            let plan = compile_plan(&facts, &first_class_registry()).unwrap();
            assert!(plan.selected.iter().any(|p| p.id == pack_id), "{medium}");
            assert!(plan.gates.iter().any(|g| g.id == marker), "{medium}");
        }
    }

    #[test]
    fn svg_files_select_the_svg_pack_without_a_medium_request() {
        let facts = RepositoryFacts {
            file_extensions: set(&["svg"]),
            detected_tools: set(&["node", "npx"]),
            ..Default::default()
        };
        let plan = compile_plan(&facts, &first_class_registry()).unwrap();
        assert!(plan.selected.iter().any(|p| p.id == "svg"));
    }

    #[test]
    fn missing_native_tools_keep_a_deep_pack_honestly_unsupported() {
        let facts = RepositoryFacts {
            requested_medium: Some("three-d".into()),
            detected_tools: BTreeSet::new(),
            ..Default::default()
        };
        let plan = compile_plan(&facts, &first_class_registry()).unwrap();
        assert!(!plan.selected.iter().any(|p| p.id == "three-d"));
        assert!(plan
            .unsupported
            .iter()
            .any(|u| u.contains("three-d") && u.contains("required tools not detected")));
    }
}
