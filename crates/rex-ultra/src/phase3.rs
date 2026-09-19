//! Phase 3: executable counterexamples, semantic mutation and behavioral differential oracles.
//!
//! This module is deliberately independent of model narration. Inputs are a validated
//! acceptance contract plus captured observables; outputs are replayable manifests and a
//! fail-closed promotion decision. Candidate worlds are private copies and are always removed.

use crate::contract::{AcceptanceContract, Obligation, Proof};
use crate::evidence::{sha256_hex, EvidenceStore};
use crate::ledger::{EpistemicLedger, FactClass};
use crate::verify::{self, ObligationStatus, VerificationReport};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const FORMAT_VERSION: u8 = 1;
const MAX_MUTATIONS: usize = 64;
const MAX_WORLD_BYTES: u64 = 256 * 1024 * 1024;
const DEFAULT_BUDGET_MS: u64 = 10 * 60 * 1_000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SemanticMutation {
    StaleCache,
    WrongAccountOrTarget,
    PartialWrite,
    DuplicateEvent,
    ReorderedEvents,
    ExpiredAuth,
    CorruptCheckpoint,
    CancellationRace,
    UnsupportedProviderResponse,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Counterexample {
    pub id: String,
    pub obligation_id: String,
    pub falsifiable_claim: String,
    pub mutation: SemanticMutation,
    pub seed: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplayManifest {
    pub format_version: u8,
    pub seed: u64,
    pub provider: String,
    pub model: String,
    pub contract_sha256: String,
    pub counterexamples: Vec<Counterexample>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MutationVerdict {
    Killed,
    Survived,
    Inconclusive,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MutationResult {
    pub counterexample_id: String,
    pub obligation_id: String,
    pub mutation: SemanticMutation,
    pub verdict: MutationVerdict,
    pub baseline_status: ObligationStatus,
    pub mutated_status: ObligationStatus,
    pub detail: String,
    pub evidence_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MutationReport {
    pub manifest_sha256: String,
    pub results: Vec<MutationResult>,
    pub killed: usize,
    pub survived: usize,
    pub inconclusive: usize,
    pub reproducible: bool,
    pub cancelled: bool,
    pub cleanup_complete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ObservableKind {
    File,
    Command,
    Process,
    UiRuntime,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Observable {
    pub kind: ObservableKind,
    pub key: String,
    pub sha256: String,
    pub outcome: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObservableTrace {
    pub provider: String,
    pub model: String,
    pub run_seed: u64,
    pub observables: Vec<Observable>,
    pub complete: bool,
    pub replayed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AllowedDifference {
    pub kind: ObservableKind,
    pub key: String,
    pub obligation_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Difference {
    pub kind: ObservableKind,
    pub key: String,
    pub before: Option<Observable>,
    pub after: Option<Observable>,
    pub obligation_id: Option<String>,
    pub allowed_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DifferentialReport {
    pub differences: Vec<Difference>,
    pub unexplained: usize,
    pub same_provider_model: bool,
    pub complete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Phase3Gate {
    pub promotable: bool,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Phase3Bundle {
    pub manifest: ReplayManifest,
    pub mutations: MutationReport,
    pub differential: DifferentialReport,
    pub gate: Phase3Gate,
}

fn obligation_mutation(ob: &Obligation, index: usize) -> SemanticMutation {
    match &ob.proof {
        Proof::FileExists { .. } => SemanticMutation::PartialWrite,
        Proof::FileContains { .. } => SemanticMutation::StaleCache,
        Proof::CommandSucceeds { .. } => [
            SemanticMutation::ExpiredAuth,
            SemanticMutation::CorruptCheckpoint,
            SemanticMutation::CancellationRace,
            SemanticMutation::UnsupportedProviderResponse,
        ][index % 4],
        Proof::CommandOutputContains { .. } => [
            SemanticMutation::WrongAccountOrTarget,
            SemanticMutation::DuplicateEvent,
            SemanticMutation::ReorderedEvents,
        ][index % 3],
        Proof::BehaviorEvidence { .. } => SemanticMutation::UnsupportedProviderResponse,
    }
}

pub fn generate_manifest(
    contract: &AcceptanceContract,
    provider: &str,
    model: &str,
    seed: u64,
) -> ReplayManifest {
    let contract_bytes = serde_json::to_vec(contract).unwrap_or_default();
    let counterexamples = contract
        .obligations
        .iter()
        .enumerate()
        .map(|(i, ob)| Counterexample {
            id: format!("cx-{seed:016x}-{i:03}"),
            obligation_id: ob.id.clone(),
            falsifiable_claim: format!(
                "{} must fail when {:?} is introduced",
                ob.statement,
                obligation_mutation(ob, i)
            ),
            mutation: obligation_mutation(ob, i),
            seed: seed ^ ((i as u64 + 1).wrapping_mul(0x9e3779b97f4a7c15)),
        })
        .collect();
    ReplayManifest {
        format_version: FORMAT_VERSION,
        seed,
        provider: provider.into(),
        model: model.into(),
        contract_sha256: sha256_hex(&contract_bytes),
        counterexamples,
    }
}

fn copy_world(src: &Path, dst: &Path, bytes: &mut u64) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        if name == "target" || name == "node_modules" || name == ".git" {
            continue;
        }
        let ty = entry.file_type().map_err(|e| e.to_string())?;
        let out = dst.join(&name);
        if ty.is_symlink() {
            return Err(format!(
                "symlink refused in candidate world: {}",
                entry.path().display()
            ));
        }
        if ty.is_dir() {
            copy_world(&entry.path(), &out, bytes)?;
        } else if ty.is_file() {
            *bytes = bytes.saturating_add(entry.metadata().map_err(|e| e.to_string())?.len());
            if *bytes > MAX_WORLD_BYTES {
                return Err("candidate world byte budget exceeded".into());
            }
            fs::copy(entry.path(), out).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn apply_counterexample(
    ob: &Obligation,
    world: &Path,
    cx: &Counterexample,
) -> Result<bool, String> {
    match &ob.proof {
        Proof::FileExists { path } => {
            let p = world.join(path);
            if p.exists() {
                fs::remove_file(p).map_err(|e| e.to_string())?;
                Ok(true)
            } else {
                Ok(false)
            }
        }
        Proof::FileContains { path, needle } => {
            let p = world.join(path);
            let text = fs::read_to_string(&p).map_err(|e| e.to_string())?;
            if !text.contains(needle) {
                return Ok(false);
            }
            fs::write(p, text.replacen(needle, &format!("MUTATED-{}", cx.seed), 1))
                .map_err(|e| e.to_string())?;
            Ok(true)
        }
        // Command mutation needs an explicit fixture seam. Never claim a kill from an
        // environment trick that the command may not consume.
        Proof::CommandSucceeds { .. }
        | Proof::CommandOutputContains { .. }
        | Proof::BehaviorEvidence { .. } => Ok(false),
    }
}

fn status(report: &VerificationReport, id: &str) -> Option<ObligationStatus> {
    report
        .outcomes
        .iter()
        .find(|o| o.obligation_id == id)
        .map(|o| o.status.clone())
}

pub fn execute_mutations(
    contract: &AcceptanceContract,
    workspace: &Path,
    phase3_dir: &Path,
    manifest: &ReplayManifest,
    cancel: &AtomicBool,
    budget_ms: Option<u64>,
) -> MutationReport {
    let started = Instant::now();
    let budget = Duration::from_millis(
        budget_ms
            .unwrap_or(DEFAULT_BUDGET_MS)
            .min(DEFAULT_BUDGET_MS),
    );
    let manifest_bytes = serde_json::to_vec(manifest).unwrap_or_default();
    let mut results = Vec::new();
    let mut cleanup_complete = true;
    let mut cancelled = false;
    let baseline_dir = phase3_dir.join("baseline-evidence");
    let baseline = EvidenceStore::open(&baseline_dir)
        .map(|mut e| verify::verify_contract(contract, workspace, &mut e));
    let baseline = match baseline {
        Ok(v) => v,
        Err(_e) => {
            return MutationReport {
                manifest_sha256: sha256_hex(&manifest_bytes),
                results,
                killed: 0,
                survived: 0,
                inconclusive: 1,
                reproducible: false,
                cancelled: false,
                cleanup_complete: false,
            }
        }
    };
    for cx in manifest.counterexamples.iter().take(MAX_MUTATIONS) {
        if cancel.load(Ordering::SeqCst) || started.elapsed() > budget {
            cancelled = true;
            break;
        }
        let world = phase3_dir.join("worlds").join(&cx.id);
        let mut bytes = 0;
        let ob = contract
            .obligations
            .iter()
            .find(|o| o.id == cx.obligation_id);
        let base = status(&baseline, &cx.obligation_id).unwrap_or(ObligationStatus::Failed);
        let (mutated, detail, ids) = match ob {
            None => (
                ObligationStatus::Failed,
                "counterexample references missing obligation".into(),
                vec![],
            ),
            Some(ob) => match copy_world(workspace, &world, &mut bytes)
                .and_then(|_| apply_counterexample(ob, &world, cx))
            {
                Ok(false) => (
                    base.clone(),
                    "mutation has no explicit executable seam; inconclusive".into(),
                    vec![],
                ),
                Err(e) => (base.clone(), format!("mutation setup failed: {e}"), vec![]),
                Ok(true) => {
                    match EvidenceStore::open(&phase3_dir.join("mutation-evidence").join(&cx.id)) {
                        Err(e) => (base.clone(), format!("evidence store failed: {e}"), vec![]),
                        Ok(mut evidence) => {
                            let r = verify::verify_contract(contract, &world, &mut evidence);
                            let s =
                                status(&r, &cx.obligation_id).unwrap_or(ObligationStatus::Failed);
                            let ids = r
                                .outcomes
                                .iter()
                                .find(|o| o.obligation_id == cx.obligation_id)
                                .map(|o| o.evidence_ids.clone())
                                .unwrap_or_default();
                            (s, "isolated candidate world executed".into(), ids)
                        }
                    }
                }
            },
        };
        let verdict = if detail.contains("inconclusive")
            || detail.contains("failed:")
            || base != ObligationStatus::Proven
        {
            MutationVerdict::Inconclusive
        } else if mutated == ObligationStatus::Failed {
            MutationVerdict::Killed
        } else {
            MutationVerdict::Survived
        };
        results.push(MutationResult {
            counterexample_id: cx.id.clone(),
            obligation_id: cx.obligation_id.clone(),
            mutation: cx.mutation,
            verdict,
            baseline_status: base,
            mutated_status: mutated,
            detail,
            evidence_ids: ids,
        });
        if fs::remove_dir_all(&world).is_err() && world.exists() {
            cleanup_complete = false;
        }
    }
    let killed = results
        .iter()
        .filter(|r| r.verdict == MutationVerdict::Killed)
        .count();
    let survived = results
        .iter()
        .filter(|r| r.verdict == MutationVerdict::Survived)
        .count();
    let inconclusive = results
        .iter()
        .filter(|r| r.verdict == MutationVerdict::Inconclusive)
        .count();
    MutationReport {
        manifest_sha256: sha256_hex(&manifest_bytes),
        results,
        killed,
        survived,
        inconclusive,
        reproducible: manifest.format_version == FORMAT_VERSION
            && manifest.contract_sha256
                == sha256_hex(&serde_json::to_vec(contract).unwrap_or_default()),
        cancelled,
        cleanup_complete,
    }
}

pub fn capture_trace(
    provider: &str,
    model: &str,
    seed: u64,
    workspace: &Path,
    verification: Option<&VerificationReport>,
) -> ObservableTrace {
    let mut observables = Vec::new();
    let mut stack = vec![workspace.to_path_buf()];
    let mut complete = true;
    while let Some(dir) = stack.pop() {
        let read = match fs::read_dir(&dir) {
            Ok(v) => v,
            Err(_) => {
                complete = false;
                continue;
            }
        };
        for entry in read.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            if name == "target" || name == "node_modules" || name == ".git" {
                continue;
            }
            match entry.file_type() {
                Ok(t) if t.is_dir() => stack.push(path),
                Ok(t) if t.is_file() => match fs::read(&path) {
                    Ok(bytes) if bytes.len() as u64 <= MAX_WORLD_BYTES => {
                        let key = path
                            .strip_prefix(workspace)
                            .unwrap_or(&path)
                            .to_string_lossy()
                            .to_string();
                        observables.push(Observable {
                            kind: ObservableKind::File,
                            key,
                            sha256: sha256_hex(&bytes),
                            outcome: format!("{} bytes", bytes.len()),
                        });
                    }
                    _ => complete = false,
                },
                Ok(t) if t.is_symlink() => complete = false,
                _ => {}
            }
        }
    }
    if let Some(report) = verification {
        for outcome in &report.outcomes {
            let body = serde_json::to_vec(outcome).unwrap_or_default();
            observables.push(Observable {
                kind: ObservableKind::Command,
                key: outcome.obligation_id.clone(),
                sha256: sha256_hex(&body),
                outcome: format!("{:?}: {}", outcome.status, outcome.detail),
            });
        }
        let body = serde_json::to_vec(report).unwrap_or_default();
        observables.push(Observable {
            kind: ObservableKind::Process,
            key: "verification".into(),
            sha256: sha256_hex(&body),
            outcome: format!("all_proven={}", report.executable_all_proven),
        });
    }
    observables.sort();
    ObservableTrace {
        provider: provider.into(),
        model: model.into(),
        run_seed: seed,
        observables,
        complete,
        replayed: true,
    }
}

pub fn contract_allowed_differences(contract: &AcceptanceContract) -> Vec<AllowedDifference> {
    let mut out = Vec::new();
    for ob in &contract.obligations {
        // Every verifier outcome is observable even when the underlying proof is a file.
        out.push(AllowedDifference {
            kind: ObservableKind::Command,
            key: ob.id.clone(),
            obligation_id: ob.id.clone(),
            reason: format!("fresh verifier outcome for acceptance obligation {}", ob.id),
        });
        match &ob.proof {
            Proof::FileExists { path } | Proof::FileContains { path, .. } => {
                out.push(AllowedDifference {
                    kind: ObservableKind::File,
                    key: path.clone(),
                    obligation_id: ob.id.clone(),
                    reason: format!("acceptance obligation {} requires this artifact", ob.id),
                })
            }
            Proof::CommandSucceeds { .. } | Proof::CommandOutputContains { .. } => {
                out.push(AllowedDifference {
                    kind: ObservableKind::Command,
                    key: ob.id.clone(),
                    obligation_id: ob.id.clone(),
                    reason: format!(
                        "acceptance obligation {} requires this command outcome",
                        ob.id
                    ),
                })
            }
            Proof::BehaviorEvidence { .. } => out.push(AllowedDifference {
                kind: ObservableKind::UiRuntime,
                key: ob.id.clone(),
                obligation_id: ob.id.clone(),
                reason: format!(
                    "acceptance obligation {} allows this runtime evidence",
                    ob.id
                ),
            }),
        }
    }
    out.push(AllowedDifference {
        kind: ObservableKind::Process,
        key: "verification".into(),
        obligation_id: contract
            .obligations
            .first()
            .map(|o| o.id.clone())
            .unwrap_or_default(),
        reason: "fresh verification outcome required by the acceptance contract".into(),
    });
    out
}

pub fn compare_traces(
    old: &ObservableTrace,
    new: &ObservableTrace,
    allowed: &[AllowedDifference],
    contract: &AcceptanceContract,
) -> DifferentialReport {
    let same_provider_model =
        old.provider == new.provider && old.model == new.model && old.run_seed == new.run_seed;
    let complete = old.complete && new.complete && old.replayed && new.replayed;
    let old_map: BTreeMap<_, _> = old
        .observables
        .iter()
        .map(|o| ((o.kind.clone(), o.key.clone()), o.clone()))
        .collect();
    let new_map: BTreeMap<_, _> = new
        .observables
        .iter()
        .map(|o| ((o.kind.clone(), o.key.clone()), o.clone()))
        .collect();
    let keys: BTreeSet<_> = old_map.keys().chain(new_map.keys()).cloned().collect();
    let obligations: BTreeSet<_> = contract.obligations.iter().map(|o| o.id.as_str()).collect();
    let mut differences = Vec::new();
    for (kind, key) in keys {
        let before = old_map.get(&(kind.clone(), key.clone())).cloned();
        let after = new_map.get(&(kind.clone(), key.clone())).cloned();
        if before == after {
            continue;
        }
        let rule = allowed.iter().find(|a| {
            a.kind == kind
                && a.key == key
                && obligations.contains(a.obligation_id.as_str())
                && !a.reason.trim().is_empty()
        });
        differences.push(Difference {
            kind,
            key,
            before,
            after,
            obligation_id: rule.map(|r| r.obligation_id.clone()),
            allowed_reason: rule.map(|r| r.reason.clone()),
        });
    }
    let unexplained = differences
        .iter()
        .filter(|d| d.obligation_id.is_none())
        .count();
    DifferentialReport {
        differences,
        unexplained,
        same_provider_model,
        complete,
    }
}

pub fn gate(m: &MutationReport, d: &DifferentialReport, expected: usize) -> Phase3Gate {
    let mut reasons = Vec::new();
    if m.results.len() != expected {
        reasons.push("missing mutation results".into());
    }
    if m.survived > 0 {
        reasons.push("semantic mutations survived".into());
    }
    if m.inconclusive > 0 {
        reasons.push("semantic mutations were inconclusive".into());
    }
    if !m.reproducible {
        reasons.push("mutation run is not reproducible".into());
    }
    if m.cancelled {
        reasons.push("mutation run was cancelled".into());
    }
    if !m.cleanup_complete {
        reasons.push("candidate world cleanup incomplete".into());
    }
    if !d.same_provider_model {
        reasons.push("old/new runs did not use identical provider, model and seed".into());
    }
    if !d.complete {
        reasons.push("observable traces are incomplete or were not replayed".into());
    }
    if d.unexplained > 0 {
        reasons.push("behavioral differential contains unexplained differences".into());
    }
    Phase3Gate {
        promotable: reasons.is_empty(),
        reasons,
    }
}

pub fn seal_bundle(
    dir: &Path,
    bundle: &Phase3Bundle,
    ledger: &mut EpistemicLedger,
    evidence: &mut EvidenceStore,
) -> Result<PathBuf, String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec_pretty(bundle).map_err(|e| e.to_string())?;
    let path = dir.join("phase3.json");
    let tmp = dir.join("phase3.json.tmp");
    fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
    fs::rename(tmp, &path).map_err(|e| e.to_string())?;
    let ev = evidence.put_bytes("phase3_bundle", &bytes);
    ledger.record(
        if bundle.gate.promotable {
            "Phase 3 gate passed"
        } else {
            "Phase 3 gate failed closed"
        },
        FactClass::Observed { evidence_id: ev.id },
    );
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::parse_contract;
    fn contract() -> AcceptanceContract {
        parse_contract(r#"{"obligations":[{"id":"marker","statement":"marker exists","proof":{"kind":"file_contains","path":"result.txt","needle":"PASS"}}],"forbidden_regressions":[]}"#, "make marker").unwrap()
    }
    fn obs(v: &str) -> Observable {
        Observable {
            kind: ObservableKind::File,
            key: "result.txt".into(),
            sha256: sha256_hex(v.as_bytes()),
            outcome: v.into(),
        }
    }
    #[test]
    fn deterministic_manifest_and_real_world_kill() {
        let t = tempfile::tempdir().unwrap();
        let ws = t.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        fs::write(ws.join("result.txt"), "PASS").unwrap();
        let c = contract();
        let m = generate_manifest(&c, "gemini", "fixed", 7);
        assert_eq!(m, generate_manifest(&c, "gemini", "fixed", 7));
        let r = execute_mutations(
            &c,
            &ws,
            &t.path().join("p3"),
            &m,
            &AtomicBool::new(false),
            Some(10_000),
        );
        assert_eq!(r.killed, 1);
        assert_eq!(r.inconclusive, 0);
        assert!(r.cleanup_complete);
    }
    #[test]
    fn forged_report_cannot_promote() {
        let m = MutationReport {
            manifest_sha256: "forged".into(),
            results: vec![],
            killed: 1,
            survived: 0,
            inconclusive: 0,
            reproducible: false,
            cancelled: false,
            cleanup_complete: true,
        };
        let d = DifferentialReport {
            differences: vec![],
            unexplained: 0,
            same_provider_model: true,
            complete: true,
        };
        assert!(!gate(&m, &d, 1).promotable);
    }
    #[test]
    fn flaky_inconclusive_cannot_promote() {
        let m = MutationReport {
            manifest_sha256: "x".into(),
            results: vec![MutationResult {
                counterexample_id: "x".into(),
                obligation_id: "marker".into(),
                mutation: SemanticMutation::StaleCache,
                verdict: MutationVerdict::Inconclusive,
                baseline_status: ObligationStatus::Proven,
                mutated_status: ObligationStatus::Proven,
                detail: "flaky".into(),
                evidence_ids: vec![],
            }],
            killed: 0,
            survived: 0,
            inconclusive: 1,
            reproducible: true,
            cancelled: false,
            cleanup_complete: true,
        };
        let d = DifferentialReport {
            differences: vec![],
            unexplained: 0,
            same_provider_model: true,
            complete: true,
        };
        assert!(!gate(&m, &d, 1).promotable);
    }
    #[test]
    fn unexplained_difference_cannot_promote() {
        let c = contract();
        let old = ObservableTrace {
            provider: "g".into(),
            model: "m".into(),
            run_seed: 1,
            observables: vec![obs("old")],
            complete: true,
            replayed: true,
        };
        let mut new = old.clone();
        new.observables = vec![obs("new")];
        let d = compare_traces(&old, &new, &[], &c);
        assert_eq!(d.unexplained, 1);
        let m = MutationReport {
            manifest_sha256: "x".into(),
            results: vec![],
            killed: 0,
            survived: 0,
            inconclusive: 0,
            reproducible: true,
            cancelled: false,
            cleanup_complete: true,
        };
        assert!(!gate(&m, &d, 0).promotable);
    }
    #[test]
    fn allowed_difference_requires_real_obligation() {
        let c = contract();
        let old = ObservableTrace {
            provider: "g".into(),
            model: "m".into(),
            run_seed: 1,
            observables: vec![obs("old")],
            complete: true,
            replayed: true,
        };
        let mut new = old.clone();
        new.observables = vec![obs("new")];
        let bad = AllowedDifference {
            kind: ObservableKind::File,
            key: "result.txt".into(),
            obligation_id: "invented".into(),
            reason: "trust me".into(),
        };
        assert_eq!(compare_traces(&old, &new, &[bad], &c).unexplained, 1);
    }
}
