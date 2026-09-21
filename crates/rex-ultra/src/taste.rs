//! The visual taste contract.
//!
//! Visual work cannot qualify on code, DOM or build evidence alone. This
//! module defines the serializable contract a visual task must declare
//! before candidate theses are built: who it is for, the one central
//! invention, what to learn from and avoid, the criteria under review, the
//! viewports that must be screenshotted, and the forbidden patterns that
//! count as taste failures. Defaults are configurable and recorded in the
//! contract; the gate never claims objective beauty, only that the declared
//! direction, comparisons and review process were actually satisfied.

use serde::{Deserialize, Serialize};
use std::ops::RangeInclusive;

pub const MIN_COMPREHENSION_SECONDS: u8 = 3;
pub const MAX_COMPREHENSION_SECONDS: u8 = 10;
pub const MIN_DISTINCT_THESES: usize = 3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CreativeBrief {
    pub audience: String,
    pub central_invention: CentralInvention,
    pub references: Vec<VisualReference>,
    pub anti_references: Vec<VisualReference>,
    pub composition: CriterionSet,
    pub typography: CriterionSet,
    pub motion: CriterionSet,
    pub responsive: CriterionSet,
    pub forbidden_patterns: Vec<ForbiddenPattern>,
    pub required_viewports: Vec<ViewportSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CentralInvention {
    pub statement: String,
    /// Seconds a first-time viewer needs to grasp the central behavior.
    /// Must intersect 3..=10: faster claims are decoration, slower ones are
    /// comprehension failures.
    pub comprehension_window_seconds: RangeInclusive<u8>,
    pub proof_prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VisualReference {
    /// What is being learned (references) or avoided (anti-references).
    pub property: String,
    pub provenance: ReferenceProvenance,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReferenceProvenance {
    Url {
        url: String,
    },
    /// A registered local artifact; the hash binds the exact bytes, so a
    /// name alone is never evidence.
    LocalArtifact {
        artifact_id: String,
        content_hash: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CriterionSet {
    pub criteria: Vec<Criterion>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Criterion {
    pub id: String,
    pub statement: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ForbiddenPattern {
    pub id: String,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewportClass {
    Desktop,
    Phone,
    Tablet,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ViewportSpec {
    pub name: String,
    pub class: ViewportClass,
    pub width: u32,
    pub height: u32,
}

/// One candidate's declared visual thesis. Distinctness is judged on the
/// four semantic models, never on color tokens.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CandidateThesis {
    pub id: String,
    pub central_behavior: String,
    pub composition_model: String,
    pub motion_model: String,
    pub responsive_strategy: String,
    /// Recorded but deliberately excluded from distinctness checks.
    pub color_tokens: Vec<String>,
}

/// The default forbidden patterns. Configurable per brief; whatever list
/// the brief carries is recorded in the contract, so a waived default is an
/// explicit decision, not a silent one.
pub fn default_forbidden_patterns() -> Vec<ForbiddenPattern> {
    [
        (
            "generic-sci-fi-hud",
            "generic sci-fi HUD styling instead of a product-specific direction",
        ),
        (
            "dashboard-as-landing",
            "dashboard presented as the landing experience",
        ),
        (
            "dense-parameter-cards",
            "dense parameter cards as the primary composition",
        ),
        (
            "gratuitous-gradients-glow",
            "gratuitous gradients or glow substituting for design",
        ),
        (
            "control-overload",
            "control overload beyond the task's actual needs",
        ),
        (
            "generic-hero-plus-cards",
            "generic hero-plus-cards layout with no central behavior",
        ),
        (
            "polish-without-invention",
            "visual polish without a new central behavior",
        ),
    ]
    .iter()
    .map(|(id, description)| ForbiddenPattern {
        id: id.to_string(),
        description: description.to_string(),
    })
    .collect()
}

/// Validate a creative brief before any candidate is built. Every failure
/// is reported; none is silently repaired.
pub fn validate_brief(brief: &CreativeBrief) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    if brief.audience.trim().is_empty() {
        errors.push("audience is required".to_string());
    }
    let invention = &brief.central_invention;
    if invention.statement.trim().is_empty() {
        errors.push("central invention statement is required".to_string());
    }
    if invention.proof_prompt.trim().is_empty() {
        errors.push("central invention proof prompt is required".to_string());
    }
    let window = &invention.comprehension_window_seconds;
    if window.end() < &MIN_COMPREHENSION_SECONDS || window.start() > &MAX_COMPREHENSION_SECONDS {
        errors.push(format!(
            "comprehension window {window:?} must intersect {MIN_COMPREHENSION_SECONDS}..={MAX_COMPREHENSION_SECONDS} seconds"
        ));
    }
    for (label, set) in [
        ("composition", &brief.composition),
        ("typography", &brief.typography),
        ("motion", &brief.motion),
        ("responsive", &brief.responsive),
    ] {
        if set.criteria.is_empty() {
            errors.push(format!("{label} criterion set is empty"));
        }
        for criterion in &set.criteria {
            if criterion.id.trim().is_empty() || criterion.statement.trim().is_empty() {
                errors.push(format!("{label} criterion needs an id and a statement"));
            }
        }
    }
    for (label, references) in [
        ("reference", &brief.references),
        ("anti-reference", &brief.anti_references),
    ] {
        for reference in references {
            if reference.property.trim().is_empty() {
                errors.push(format!(
                    "{label} needs the property being learned or avoided"
                ));
            }
            match &reference.provenance {
                ReferenceProvenance::Url { url } => {
                    if url.trim().is_empty() {
                        errors.push(format!("{label} url is empty"));
                    }
                }
                ReferenceProvenance::LocalArtifact {
                    artifact_id,
                    content_hash,
                } => {
                    if artifact_id.trim().is_empty() || content_hash.trim().is_empty() {
                        errors.push(format!(
                            "{label} local artifact needs an id and a content hash"
                        ));
                    }
                }
            }
        }
    }
    if !brief
        .required_viewports
        .iter()
        .any(|v| v.class == ViewportClass::Desktop)
    {
        errors.push("a desktop viewport is required".to_string());
    }
    if !brief
        .required_viewports
        .iter()
        .any(|v| v.class == ViewportClass::Phone)
    {
        errors.push("a phone viewport is required".to_string());
    }
    for viewport in &brief.required_viewports {
        if viewport.name.trim().is_empty() || viewport.width == 0 || viewport.height == 0 {
            errors.push("viewport needs a name and non-zero dimensions".to_string());
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn normalize(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn thesis_signature(thesis: &CandidateThesis) -> (String, String, String, String) {
    (
        normalize(&thesis.central_behavior),
        normalize(&thesis.composition_model),
        normalize(&thesis.motion_model),
        normalize(&thesis.responsive_strategy),
    )
}

/// Visual tasks need at least three genuinely distinct candidate theses.
/// Distinctness is checked on the declared central behavior, composition,
/// motion and responsive models; a thesis that differs only in color tokens
/// is a near-duplicate and is rejected before build.
pub fn check_distinct_theses(theses: &[CandidateThesis]) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    if theses.len() < MIN_DISTINCT_THESES {
        errors.push(format!(
            "visual work needs at least {MIN_DISTINCT_THESES} distinct theses, got {}",
            theses.len()
        ));
    }
    let mut seen_ids = std::collections::BTreeSet::new();
    for thesis in theses {
        if thesis.id.trim().is_empty() || !seen_ids.insert(thesis.id.clone()) {
            errors.push("thesis ids must be non-empty and unique".to_string());
        }
        let signature = thesis_signature(thesis);
        if signature.0.is_empty()
            || signature.1.is_empty()
            || signature.2.is_empty()
            || signature.3.is_empty()
        {
            errors.push(format!(
                "thesis {:?} must declare all four semantic models",
                thesis.id
            ));
        }
    }
    for (index, left) in theses.iter().enumerate() {
        for right in theses.iter().skip(index + 1) {
            if thesis_signature(left) == thesis_signature(right) {
                errors.push(format!(
                    "theses {:?} and {:?} are near-duplicates: same behavior, composition, motion and responsive models",
                    left.id, right.id
                ));
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Pixel-level evidence that a visual candidate actually exists and works:
/// hash-bound screenshots at the mandatory viewports plus an interaction
/// replay artifact. Text claims are not pixel evidence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScreenshotEvidence {
    pub viewport: ViewportClass,
    pub artifact_hash: String,
}

/// The host's visual evidence for one candidate thesis.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TasteGateReport {
    pub thesis_id: String,
    pub screenshots: Vec<ScreenshotEvidence>,
    pub interaction_replay_hash: String,
    pub forbidden_patterns_hit: Vec<String>,
    pub critic_clean: bool,
}

/// The mandatory floor for visual evidence: hash-bound screenshots at both
/// desktop and phone, a hash-bound interaction replay, no forbidden pattern
/// hits, and a clean critic pass. Brief-specific viewports and criteria stay
/// with the spec compiler; the external kernel enforces this floor.
pub fn validate_taste_gate(report: &TasteGateReport) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    if report.thesis_id.trim().is_empty() {
        errors.push("thesis id is empty".to_string());
    }
    for class in [ViewportClass::Desktop, ViewportClass::Phone] {
        let covered = report
            .screenshots
            .iter()
            .any(|shot| shot.viewport == class && is_artifact_hash(&shot.artifact_hash));
        if !covered {
            errors.push(format!(
                "missing hash-bound screenshot evidence for the {class:?} viewport"
            ));
        }
    }
    if !is_artifact_hash(&report.interaction_replay_hash) {
        errors.push("interaction replay is not hash-bound".to_string());
    }
    if !report.forbidden_patterns_hit.is_empty() {
        errors.push(format!(
            "forbidden patterns hit: {}",
            report.forbidden_patterns_hit.join(", ")
        ));
    }
    if !report.critic_clean {
        errors.push("critic pass is not clean".to_string());
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn is_artifact_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn criterion(id: &str) -> Criterion {
        Criterion {
            id: id.into(),
            statement: format!("{id} holds"),
        }
    }

    fn brief() -> CreativeBrief {
        CreativeBrief {
            audience: "astronomy students".into(),
            central_invention: CentralInvention {
                statement: "orbit the answer".into(),
                comprehension_window_seconds: 3..=8,
                proof_prompt: "show the orbit responding".into(),
            },
            references: vec![VisualReference {
                property: "spatial depth".into(),
                provenance: ReferenceProvenance::Url {
                    url: "https://example.com/ref".into(),
                },
            }],
            anti_references: vec![VisualReference {
                property: "flat card grid".into(),
                provenance: ReferenceProvenance::LocalArtifact {
                    artifact_id: "old-cut".into(),
                    content_hash: "abc123".into(),
                },
            }],
            composition: CriterionSet {
                criteria: vec![criterion("focal-path")],
            },
            typography: CriterionSet {
                criteria: vec![criterion("measure")],
            },
            motion: CriterionSet {
                criteria: vec![criterion("easing")],
            },
            responsive: CriterionSet {
                criteria: vec![criterion("reflow")],
            },
            forbidden_patterns: default_forbidden_patterns(),
            required_viewports: vec![
                ViewportSpec {
                    name: "desktop".into(),
                    class: ViewportClass::Desktop,
                    width: 1440,
                    height: 900,
                },
                ViewportSpec {
                    name: "phone".into(),
                    class: ViewportClass::Phone,
                    width: 390,
                    height: 844,
                },
            ],
        }
    }

    fn thesis(id: &str, behavior: &str) -> CandidateThesis {
        CandidateThesis {
            id: id.into(),
            central_behavior: behavior.into(),
            composition_model: format!("composition-{id}"),
            motion_model: format!("motion-{id}"),
            responsive_strategy: format!("responsive-{id}"),
            color_tokens: vec!["#fff".into()],
        }
    }

    #[test]
    fn a_complete_brief_validates() {
        assert_eq!(validate_brief(&brief()), Ok(()));
    }

    #[test]
    fn comprehension_window_must_intersect_the_truthful_range() {
        let mut b = brief();
        b.central_invention.comprehension_window_seconds = 12..=30;
        assert!(validate_brief(&b)
            .unwrap_err()
            .iter()
            .any(|e| e.contains("comprehension window")));
        let mut b = brief();
        b.central_invention.comprehension_window_seconds = 1..=2;
        assert!(validate_brief(&b).is_err());
        let mut b = brief();
        b.central_invention.comprehension_window_seconds = 2..=4;
        assert!(validate_brief(&b).is_ok());
    }

    #[test]
    fn references_need_provenance_and_a_property() {
        let mut b = brief();
        b.references[0].property = "  ".into();
        assert!(validate_brief(&b)
            .unwrap_err()
            .iter()
            .any(|e| e.contains("property")));
        let mut b = brief();
        b.anti_references[0].provenance = ReferenceProvenance::LocalArtifact {
            artifact_id: "old-cut".into(),
            content_hash: String::new(),
        };
        assert!(validate_brief(&b)
            .unwrap_err()
            .iter()
            .any(|e| e.contains("content hash")));
    }

    #[test]
    fn both_viewport_classes_are_required() {
        let mut b = brief();
        b.required_viewports
            .retain(|v| v.class == ViewportClass::Desktop);
        assert!(validate_brief(&b)
            .unwrap_err()
            .iter()
            .any(|e| e.contains("phone viewport")));
        let mut b = brief();
        b.required_viewports.clear();
        let errors = validate_brief(&b).unwrap_err();
        assert!(errors.iter().any(|e| e.contains("desktop viewport")));
        assert!(errors.iter().any(|e| e.contains("phone viewport")));
    }

    #[test]
    fn every_criterion_set_must_be_declared() {
        let mut b = brief();
        b.motion.criteria.clear();
        assert!(validate_brief(&b)
            .unwrap_err()
            .iter()
            .any(|e| e.contains("motion criterion set is empty")));
    }

    #[test]
    fn defaults_are_explicit_and_recorded() {
        let patterns = default_forbidden_patterns();
        assert!(patterns.len() >= 7);
        assert!(patterns.iter().any(|p| p.id == "polish-without-invention"));
        let b = brief();
        assert_eq!(b.forbidden_patterns, patterns);
    }

    #[test]
    fn fewer_than_three_theses_is_rejected() {
        let theses = vec![thesis("a", "orbit"), thesis("b", "slice")];
        assert!(check_distinct_theses(&theses)
            .unwrap_err()
            .iter()
            .any(|e| e.contains("at least 3")));
    }

    #[test]
    fn color_only_differences_are_near_duplicates() {
        let mut a = thesis("a", "orbit the answer");
        let mut b = thesis("b", "orbit the answer");
        b.composition_model = a.composition_model.clone();
        b.motion_model = a.motion_model.clone();
        b.responsive_strategy = a.responsive_strategy.clone();
        a.color_tokens = vec!["#fff".into()];
        b.color_tokens = vec!["#000".into(), "#123456".into()];
        let theses = vec![a, b, thesis("c", "slice time")];
        assert!(check_distinct_theses(&theses)
            .unwrap_err()
            .iter()
            .any(|e| e.contains("near-duplicates")));
    }

    #[test]
    fn genuinely_distinct_theses_pass() {
        let theses = vec![
            thesis("a", "orbit the answer"),
            thesis("b", "slice time"),
            thesis("c", "grow a garden"),
        ];
        assert_eq!(check_distinct_theses(&theses), Ok(()));
    }

    #[test]
    fn blank_semantic_models_cannot_hide_duplication() {
        let mut b = thesis("b", "slice time");
        b.motion_model = "   ".into();
        let theses = vec![
            thesis("a", "orbit the answer"),
            b,
            thesis("c", "grow a garden"),
        ];
        assert!(check_distinct_theses(&theses)
            .unwrap_err()
            .iter()
            .any(|e| e.contains("all four semantic models")));
    }
    fn report() -> TasteGateReport {
        TasteGateReport {
            thesis_id: "thesis-a".into(),
            screenshots: vec![
                ScreenshotEvidence {
                    viewport: ViewportClass::Desktop,
                    artifact_hash: "a".repeat(64),
                },
                ScreenshotEvidence {
                    viewport: ViewportClass::Phone,
                    artifact_hash: "b".repeat(64),
                },
            ],
            interaction_replay_hash: "c".repeat(64),
            forbidden_patterns_hit: Vec::new(),
            critic_clean: true,
        }
    }

    #[test]
    fn taste_gate_accepts_only_hash_bound_pixel_evidence() {
        assert!(validate_taste_gate(&report()).is_ok());
        let mut missing_phone = report();
        missing_phone
            .screenshots
            .retain(|s| s.viewport == ViewportClass::Desktop);
        assert!(validate_taste_gate(&missing_phone)
            .unwrap_err()
            .iter()
            .any(|e| e.contains("Phone")));
        let mut unbound = report();
        unbound.screenshots[0].artifact_hash = "trust me".into();
        assert!(validate_taste_gate(&unbound).is_err());
        let mut no_replay = report();
        no_replay.interaction_replay_hash = String::new();
        assert!(validate_taste_gate(&no_replay).is_err());
    }

    #[test]
    fn taste_gate_fails_closed_on_forbidden_patterns_and_dirty_critics() {
        let mut hit = report();
        hit.forbidden_patterns_hit = vec!["generic-hero-plus-cards".into()];
        assert!(validate_taste_gate(&hit).is_err());
        let mut dirty = report();
        dirty.critic_clean = false;
        assert!(validate_taste_gate(&dirty).is_err());
    }
}
