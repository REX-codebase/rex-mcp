//! Broad language packs generated from detected repository facts.
//!
//! Architecture section M: broad language support without false claims. A
//! generated pack is never Certified. When its toolchain is detected the
//! pack is Partial: probed capabilities are available and the uncertified
//! layers (diagnostic mapping, sandbox certification, certification
//! fixtures) remain visible gaps. When its toolchain is absent the pack is
//! DetectedUnsupported. Languages REX cannot attribute to any certified pack
//! or template are reported as Unknown on the unsupported list, never
//! silently ignored and never claimed as supported.

use crate::skills::{
    CertificationStatus, GateTemplate, RepositoryFacts, RuleModule, Selector, SkillPackManifest,
};
use serde::{Deserialize, Serialize};

/// Static description of a language REX can recognize from repository facts
/// and probe on PATH. First-class certified languages (Rust) deliberately
/// have no template: a template must never shadow a certified pack.
pub struct LanguageTemplate {
    pub language: &'static str,
    pub extensions: &'static [&'static str],
    pub manifests: &'static [&'static str],
    /// Tool probed on PATH by `collect_repository_facts`.
    pub tool: &'static str,
    pub test_gate: &'static str,
    pub lint_gate: Option<&'static str>,
}

pub const LANGUAGE_TEMPLATES: &[LanguageTemplate] = &[
    LanguageTemplate {
        language: "python",
        extensions: &["py"],
        manifests: &["pyproject.toml"],
        tool: "python3",
        test_gate: "python3 -m pytest",
        lint_gate: Some("python3 -m py_compile on changed files"),
    },
    LanguageTemplate {
        language: "javascript",
        extensions: &["js", "jsx", "mjs", "cjs"],
        manifests: &["package.json"],
        tool: "node",
        test_gate: "npm test",
        lint_gate: Some("node --check on changed files"),
    },
    LanguageTemplate {
        language: "typescript",
        extensions: &["ts", "tsx"],
        manifests: &["tsconfig.json"],
        tool: "npx",
        test_gate: "npm test",
        lint_gate: Some("npx tsc --noEmit"),
    },
    LanguageTemplate {
        language: "go",
        extensions: &["go"],
        manifests: &["go.mod"],
        tool: "go",
        test_gate: "go test ./...",
        lint_gate: Some("go vet ./..."),
    },
    LanguageTemplate {
        language: "ruby",
        extensions: &["rb"],
        manifests: &["Gemfile"],
        tool: "ruby",
        test_gate: "bundle exec rake test",
        lint_gate: Some("ruby -c on changed files"),
    },
    LanguageTemplate {
        language: "java",
        extensions: &["java"],
        manifests: &["pom.xml", "build.gradle"],
        tool: "javac",
        test_gate: "mvn test or gradle test per build file",
        lint_gate: Some("javac on changed files"),
    },
    LanguageTemplate {
        language: "c",
        extensions: &["c", "h"],
        manifests: &["Makefile"],
        tool: "gcc",
        test_gate: "make test",
        lint_gate: Some("gcc -fsyntax-only on changed files"),
    },
    LanguageTemplate {
        language: "cpp",
        extensions: &["cpp", "cc", "cxx", "hpp"],
        manifests: &["CMakeLists.txt"],
        tool: "g++",
        test_gate: "ctest",
        lint_gate: Some("g++ -fsyntax-only on changed files"),
    },
];

/// Recognizable programming languages REX has neither a certified pack nor a
/// broad template for. Detecting one produces an honest Unknown note.
const UNTEMPLATED_LANGUAGES: &[(&str, &str)] = &[
    ("kt", "kotlin"),
    ("swift", "swift"),
    ("cs", "csharp"),
    ("php", "php"),
    ("scala", "scala"),
    ("hs", "haskell"),
    ("lua", "lua"),
    ("pl", "perl"),
    ("r", "r"),
    ("dart", "dart"),
    ("ex", "elixir"),
    ("exs", "elixir"),
    ("clj", "clojure"),
    ("zig", "zig"),
    ("fs", "fsharp"),
    ("ml", "ocaml"),
];

fn template_matched(template: &LanguageTemplate, facts: &RepositoryFacts) -> bool {
    template
        .extensions
        .iter()
        .any(|extension| facts.file_extensions.contains(*extension))
        || template
            .manifests
            .iter()
            .any(|manifest| facts.manifests.contains(*manifest))
}

/// Generate broad pack manifests for every language detected in the
/// repository. TypeScript suppresses a manifest-only JavaScript match (a
/// shared package.json is not evidence of JavaScript work); real .js files
/// keep both packs. Detection is fact-based only - free text cannot
/// self-authorize a pack here either.
pub fn broad_registry(facts: &RepositoryFacts) -> Vec<SkillPackManifest> {
    let matched: Vec<&LanguageTemplate> = LANGUAGE_TEMPLATES
        .iter()
        .filter(|template| template_matched(template, facts))
        .collect();
    let typescript = matched.iter().any(|t| t.language == "typescript");
    let has_js_files = ["js", "jsx", "mjs", "cjs"]
        .iter()
        .any(|e| facts.file_extensions.contains(*e));
    matched
        .into_iter()
        .filter(|template| {
            !(template.language == "javascript" && typescript && !has_js_files)
        })
        .map(|template| {
            let tool_detected = facts.detected_tools.contains(template.tool);
            let mut gates = vec![GateTemplate {
                id: format!("generated-{}-test", template.language),
                command_hint: template.test_gate.to_string(),
                required: true,
            }];
            if let Some(lint) = template.lint_gate {
                gates.push(GateTemplate {
                    id: format!("generated-{}-lint", template.language),
                    command_hint: lint.to_string(),
                    required: false,
                });
            }
            SkillPackManifest {
                id: format!("generated-{}", template.language),
                version: "0.1.0".to_string(),
                schema: 1,
                domains: vec![format!("language-{}", template.language)],
                selectors: template
                    .extensions
                    .iter()
                    .map(|extension| Selector::FileExtension { extension: extension.to_string() })
                    .chain(template.manifests.iter().map(|manifest| Selector::Manifest {
                        filename: manifest.to_string(),
                    }))
                    .collect(),
                dependencies: Vec::new(),
                conflicts: Vec::new(),
                rules: vec![RuleModule {
                    id: "honest-partial-support".to_string(),
                    statement: format!(
                        "{} support is generated from repository facts and PATH probes, not certification fixtures; gaps are reported, never hidden",
                        template.language
                    ),
                }],
                gates,
                required_tools: vec![template.tool.to_string()],
                provenance: "generated from repository facts and PATH probes (architecture section M: broad support without false claims)".to_string(),
                certification: if tool_detected {
                    CertificationStatus::Partial
                } else {
                    CertificationStatus::DetectedUnsupported
                },
            }
        })
        .collect()
}

/// Honest Unknown notes for recognizable languages that have neither a
/// certified pack in the registry nor a broad template. Sorted for
/// deterministic plan hashes.
pub fn unknown_language_notes(facts: &RepositoryFacts) -> Vec<String> {
    let mut notes: Vec<String> = UNTEMPLATED_LANGUAGES
        .iter()
        .filter(|(extension, _)| facts.file_extensions.contains(*extension))
        .map(|(extension, language)| {
            format!(
                "{language}: detected (.{extension}) but no certified pack or broad template exists - status Unknown, never claimed as supported"
            )
        })
        .collect();
    notes.sort();
    notes.dedup();
    notes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::{compile_plan, RepositoryFacts};
    use std::collections::BTreeSet;

    fn facts(extensions: &[&str], manifests: &[&str], tools: &[&str]) -> RepositoryFacts {
        RepositoryFacts {
            file_extensions: extensions
                .iter()
                .map(|e| e.to_string())
                .collect::<BTreeSet<_>>(),
            manifests: manifests
                .iter()
                .map(|m| m.to_string())
                .collect::<BTreeSet<_>>(),
            shebangs: BTreeSet::new(),
            requested_medium: None,
            detected_tools: tools.iter().map(|t| t.to_string()).collect::<BTreeSet<_>>(),
        }
    }

    #[test]
    fn broad_pack_is_partial_when_its_tool_is_detected() {
        let registry = broad_registry(&facts(&["py"], &["pyproject.toml"], &["python3"]));
        let pack = registry
            .iter()
            .find(|p| p.id == "generated-python")
            .unwrap();
        assert_eq!(pack.certification, CertificationStatus::Partial);
        assert_eq!(pack.required_tools, vec!["python3".to_string()]);
        assert!(pack
            .gates
            .iter()
            .any(|g| g.id == "generated-python-test" && g.required));
        assert!(pack
            .gates
            .iter()
            .any(|g| g.id == "generated-python-lint" && !g.required));
    }

    #[test]
    fn broad_pack_is_detected_unsupported_without_its_tool() {
        let registry = broad_registry(&facts(&["py"], &[], &[]));
        let pack = registry
            .iter()
            .find(|p| p.id == "generated-python")
            .unwrap();
        assert_eq!(pack.certification, CertificationStatus::DetectedUnsupported);
    }

    #[test]
    fn typescript_suppresses_manifest_only_javascript() {
        let registry = broad_registry(&facts(&["ts"], &["package.json"], &["node", "npx"]));
        assert!(registry.iter().any(|p| p.id == "generated-typescript"));
        assert!(!registry.iter().any(|p| p.id == "generated-javascript"));
        let with_js = broad_registry(&facts(&["ts", "js"], &["package.json"], &["node", "npx"]));
        assert!(with_js.iter().any(|p| p.id == "generated-javascript"));
    }

    #[test]
    fn generated_packs_never_claim_certification() {
        for with_tool in [true, false] {
            for template in LANGUAGE_TEMPLATES {
                let tools = if with_tool {
                    vec![template.tool]
                } else {
                    vec![]
                };
                let registry =
                    broad_registry(&facts(template.extensions, template.manifests, &tools));
                let pack = registry
                    .iter()
                    .find(|p| p.id == format!("generated-{}", template.language))
                    .unwrap();
                assert_ne!(
                    pack.certification,
                    CertificationStatus::Certified,
                    "{} must never be certified by generation",
                    template.language
                );
            }
        }
    }

    #[test]
    fn templates_do_not_shadow_first_class_packs() {
        let first_class: BTreeSet<String> = crate::skill_packs::first_class_registry()
            .iter()
            .map(|p| p.id.clone())
            .collect();
        for template in LANGUAGE_TEMPLATES {
            assert!(
                !first_class.contains(template.language),
                "template {} shadows a certified pack",
                template.language
            );
        }
    }

    #[test]
    fn untemplated_languages_are_reported_unknown_never_supported() {
        let notes = unknown_language_notes(&facts(&["kt", "swift"], &[], &[]));
        assert_eq!(notes.len(), 2);
        assert!(notes
            .iter()
            .any(|n| n.contains("kotlin") && n.contains("Unknown")));
        assert!(notes
            .iter()
            .any(|n| n.contains("swift") && n.contains("Unknown")));
        assert!(unknown_language_notes(&facts(&["py", "rs"], &[], &[])).is_empty());
    }

    #[test]
    fn compile_plan_integrates_broad_packs_and_unknown_notes() {
        let python = facts(&["py"], &["pyproject.toml"], &["python3"]);
        let mut registry = crate::skill_packs::first_class_registry();
        registry.extend(broad_registry(&python));
        let plan = compile_plan(&python, &registry).unwrap();
        assert!(plan
            .selected
            .iter()
            .any(|p| p.id == "generated-python" && p.version == "0.1.0"));
        assert!(plan.gates.iter().any(|g| g.pack == "generated-python"
            && g.id == "generated-python-test"
            && g.required));
        let again = compile_plan(&python, &registry).unwrap();
        assert_eq!(plan.plan_hash, again.plan_hash);

        let toolless = facts(&["py"], &[], &[]);
        let mut registry = crate::skill_packs::first_class_registry();
        registry.extend(broad_registry(&toolless));
        let plan = compile_plan(&toolless, &registry).unwrap();
        assert!(!plan.selected.iter().any(|p| p.id == "generated-python"));
        assert!(plan
            .unsupported
            .iter()
            .any(|u| u.contains("generated-python")));

        let kotlin = facts(&["kt"], &[], &[]);
        let plan = compile_plan(&kotlin, &crate::skill_packs::first_class_registry()).unwrap();
        assert!(plan
            .unsupported
            .iter()
            .any(|u| u.contains("kotlin") && u.contains("Unknown")));
    }
}
