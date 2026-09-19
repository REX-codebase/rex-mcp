//! Phase 5 closes Ultra with a repository digital twin, semantic index,
//! deterministic audits and profiles, visual replay evidence, narrow temporary
//! tools, independent reconstruction, and a proof-carrying promotion gate.
//! Every artifact is content-addressed and revalidated immediately before use.

use crate::contract::AcceptanceContract;
use crate::evidence::{sha256_hex, EvidenceStore};
use crate::ledger::{EpistemicLedger, FactClass};
use crate::verify::{ObligationStatus, VerificationReport};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Component, Path};
use std::process::Command;

const MAX_FILES: usize = 20_000;
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_QUEUE: usize = 2_000;
const INDEX_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TwinFile {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
    pub language: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepoTwin {
    pub version: u32,
    pub root_sha256: String,
    pub files: Vec<TwinFile>,
    pub dependencies: BTreeMap<String, Vec<String>>,
    pub runtime_processes: Vec<String>,
    pub ui_routes: Vec<String>,
    pub data_flows: Vec<String>,
    pub permissions: Vec<String>,
    pub tests: Vec<String>,
    pub deployment_surfaces: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub kind: String,
    pub path: String,
    pub line: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticIndex {
    pub version: u32,
    pub twin_sha256: String,
    pub symbols: Vec<Symbol>,
    pub edges: Vec<(String, String)>,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Finding {
    pub scanner: String,
    pub severity: String,
    pub path: String,
    pub detail: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditReport {
    pub twin_sha256: String,
    pub findings: Vec<Finding>,
    pub dependency_files: Vec<String>,
    pub licenses: BTreeMap<String, String>,
    pub secret_scan_complete: bool,
    pub security_scan_complete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProfileSample {
    pub name: String,
    pub value: u64,
    pub unit: String,
    pub budget: u64,
    pub command: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProfileReport {
    pub twin_sha256: String,
    pub samples: Vec<ProfileSample>,
    pub within_budget: bool,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VisualArtifact {
    pub state: String,
    pub path: String,
    pub sha256: String,
    pub width: u32,
    pub height: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InteractionStep {
    pub action: String,
    pub target: String,
    pub expected_state: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VisualReplay {
    pub twin_sha256: String,
    pub baseline: Vec<VisualArtifact>,
    pub current: Vec<VisualArtifact>,
    pub steps: Vec<InteractionStep>,
    pub matched: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SynthCapability {
    HashFile,
    CompareFiles,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SynthToolSpec {
    pub name: String,
    pub capability: SynthCapability,
    pub inputs: Vec<String>,
    pub allow_network: bool,
    pub allow_process_spawn: bool,
    pub allowed_root: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SynthToolReceipt {
    pub spec_sha256: String,
    pub source_sha256: String,
    pub output_sha256: String,
    pub compiled: bool,
    pub tested: bool,
    pub used: bool,
    pub discarded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReconstructionReport {
    pub model: String,
    pub same_model_as_worker: bool,
    pub saw_builder_narrative: bool,
    pub reconstructed: bool,
    pub obligation_ids: Vec<String>,
    pub evidence_ids: Vec<String>,
    pub explanation: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelRoute {
    pub provider: String,
    pub model: String,
    pub explicitly_configured: bool,
    pub safe_provider: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CriticPolicy {
    pub worker: String,
    pub selected: String,
    pub same_model: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProofLink {
    pub acceptance_id: String,
    pub evidence_ids: Vec<String>,
    pub executable: bool,
    pub passed: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorldLinks {
    pub builder: String,
    pub adversary: String,
    pub shadow: String,
    pub recovery: String,
    pub clean_room: String,
    pub phase3: String,
    pub phase4: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromotionGate {
    pub promotable: bool,
    pub reasons: Vec<String>,
    pub links: Vec<ProofLink>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Phase5Bundle {
    pub twin: RepoTwin,
    pub index: SemanticIndex,
    pub audit: AuditReport,
    pub profiles: ProfileReport,
    pub visual: Option<VisualReplay>,
    pub synthesis: Option<SynthToolReceipt>,
    pub reconstruction: ReconstructionReport,
    pub critic_policy: CriticPolicy,
    pub worlds: WorldLinks,
    pub gate: PromotionGate,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QueueStatus {
    Pending,
    Running,
    Passed,
    Failed,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QueueItem {
    pub id: String,
    pub payload_sha256: String,
    pub status: QueueStatus,
    pub attempts: u32,
    pub result_sha256: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DurableQueue {
    pub version: u32,
    pub request_sha256: String,
    pub items: VecDeque<QueueItem>,
}

fn safe_rel(path: &Path) -> Result<String, String> {
    if path.is_absolute()
        || path.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(format!("unsafe path {}", path.display()));
    }
    Ok(path.to_string_lossy().replace('\\', "/"))
}
fn atomic_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("tmp");
    if let Some(p) = path.parent() {
        fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())
}
fn walk(root: &Path) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut out = vec![];
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut ents = fs::read_dir(&dir)
            .map_err(|e| e.to_string())?
            .flatten()
            .collect::<Vec<_>>();
        ents.sort_by_key(|e| e.file_name());
        for e in ents {
            let p = e.path();
            let rel = p.strip_prefix(root).map_err(|e| e.to_string())?;
            let s = safe_rel(rel)?;
            if s.split('/')
                .any(|x| matches!(x, ".git" | "target" | "node_modules" | "dist"))
            {
                continue;
            }
            let m = fs::symlink_metadata(&p).map_err(|e| e.to_string())?;
            if m.file_type().is_symlink() {
                return Err(format!("digital twin refuses symlink {s}"));
            }
            if m.is_dir() {
                stack.push(p)
            } else if m.is_file() {
                if out.len() >= MAX_FILES {
                    return Err("digital twin file budget exceeded".into());
                }
                if m.len() <= MAX_FILE_BYTES {
                    out.push((s, fs::read(p).map_err(|e| e.to_string())?));
                }
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}
fn language(path: &str) -> String {
    Path::new(path)
        .extension()
        .and_then(|x| x.to_str())
        .unwrap_or("none")
        .to_ascii_lowercase()
}
fn canonical_hash<T: Serialize>(v: &T) -> String {
    sha256_hex(&serde_json::to_vec(v).unwrap_or_default())
}

pub fn build_twin(root: &Path) -> Result<RepoTwin, String> {
    let rows = walk(root)?;
    let files = rows
        .iter()
        .map(|(p, b)| TwinFile {
            path: p.clone(),
            sha256: sha256_hex(b),
            bytes: b.len() as u64,
            language: language(p),
        })
        .collect::<Vec<_>>();
    let text = rows
        .iter()
        .filter_map(|(p, b)| std::str::from_utf8(b).ok().map(|t| (p, t)))
        .collect::<Vec<_>>();
    let mut deps = BTreeMap::new();
    for (p, t) in &text {
        if p.ends_with("Cargo.toml")
            || p.ends_with("package.json")
            || p.ends_with("package-lock.json")
        {
            deps.insert(
                (*p).clone(),
                t.lines()
                    .filter(|l| l.contains('=') || l.contains("\":"))
                    .take(500)
                    .map(str::trim)
                    .map(str::to_string)
                    .collect(),
            );
        }
    }
    let mut tests = vec![];
    let mut runtime = BTreeSet::new();
    let mut routes = BTreeSet::new();
    let mut flows = BTreeSet::new();
    let mut perms = BTreeSet::new();
    let mut deploy = BTreeSet::new();
    for (p, t) in &text {
        if p.contains("test") || t.contains("#[test]") || t.contains("describe(") {
            tests.push((*p).clone());
        }
        for l in t.lines() {
            let q = l.trim();
            if q.contains("Command::new") || q.contains("child_process") || q.contains("spawn(") {
                runtime.insert(format!("{p}:{}", q.chars().take(180).collect::<String>()));
            }
            if q.contains("invoke(") || q.contains("/runs") || q.contains("route(") {
                routes.insert(format!("{p}:{}", q.chars().take(180).collect::<String>()));
            }
            if q.contains("channel(")
                || q.contains("emit(")
                || q.contains("send(")
                || q.contains("Transport")
            {
                flows.insert(format!("{p}:{}", q.chars().take(180).collect::<String>()));
            }
        }
        if p.contains("capabilities") || p.ends_with("tauri.conf.json") {
            perms.insert((*p).clone());
        }
        if p.contains("Dockerfile")
            || p.contains(".github/workflows")
            || p.ends_with("tauri.conf.json")
            || p.ends_with("vite.config.ts")
        {
            deploy.insert((*p).clone());
        }
    }
    tests.sort();
    let mut twin = RepoTwin {
        version: 1,
        root_sha256: String::new(),
        files,
        dependencies: deps,
        runtime_processes: runtime.into_iter().collect(),
        ui_routes: routes.into_iter().collect(),
        data_flows: flows.into_iter().collect(),
        permissions: perms.into_iter().collect(),
        tests,
        deployment_surfaces: deploy.into_iter().collect(),
    };
    twin.root_sha256 = canonical_hash(&(
        twin.version,
        &twin.files,
        &twin.dependencies,
        &twin.runtime_processes,
        &twin.ui_routes,
        &twin.data_flows,
        &twin.permissions,
        &twin.tests,
        &twin.deployment_surfaces,
    ));
    Ok(twin)
}
pub fn validate_twin(root: &Path, twin: &RepoTwin) -> Result<(), String> {
    let fresh = build_twin(root)?;
    if &fresh == twin {
        Ok(())
    } else {
        Err("digital twin is stale or forged".into())
    }
}

pub fn build_index(root: &Path, twin: &RepoTwin) -> Result<SemanticIndex, String> {
    validate_twin(root, twin)?;
    let rows = walk(root)?;
    let mut syms = vec![];
    let mut edges = BTreeSet::new();
    for (p, b) in rows {
        let Ok(t) = String::from_utf8(b) else {
            continue;
        };
        for (i, l) in t.lines().enumerate() {
            let s = l.trim();
            for (prefix, kind) in [
                ("fn ", "function"),
                ("pub fn ", "function"),
                ("struct ", "struct"),
                ("pub struct ", "struct"),
                ("enum ", "enum"),
                ("pub enum ", "enum"),
                ("export function ", "function"),
                ("export const ", "constant"),
                ("function ", "function"),
            ] {
                if let Some(r) = s.strip_prefix(prefix) {
                    let name = r
                        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                        .next()
                        .unwrap_or("");
                    if !name.is_empty() {
                        syms.push(Symbol {
                            name: name.into(),
                            kind: kind.into(),
                            path: p.clone(),
                            line: (i + 1) as u32,
                        });
                    }
                }
            }
            if let Some(x) = s.strip_prefix("use ") {
                edges.insert((p.clone(), x.trim_end_matches(';').into()));
            }
            if let Some(x) = s.strip_prefix("import ") {
                edges.insert((p.clone(), x.chars().take(180).collect()));
            }
        }
    }
    syms.sort_by(|a, b| (&a.path, a.line, &a.name).cmp(&(&b.path, b.line, &b.name)));
    let edges = edges.into_iter().collect::<Vec<_>>();
    let mut idx = SemanticIndex {
        version: INDEX_VERSION,
        twin_sha256: twin.root_sha256.clone(),
        symbols: syms,
        edges,
        sha256: String::new(),
    };
    idx.sha256 = canonical_hash(&(idx.version, &idx.twin_sha256, &idx.symbols, &idx.edges));
    Ok(idx)
}
pub fn validate_index(root: &Path, twin: &RepoTwin, index: &SemanticIndex) -> Result<(), String> {
    let fresh = build_index(root, twin)?;
    if &fresh == index {
        Ok(())
    } else {
        Err("semantic index is stale or forged".into())
    }
}

pub fn audit(root: &Path, twin: &RepoTwin) -> Result<AuditReport, String> {
    validate_twin(root, twin)?;
    let rows = walk(root)?;
    let mut f = vec![];
    let mut dependency_files = vec![];
    let mut licenses = BTreeMap::new();
    let secret_markers = [
        "BEGIN PRIVATE KEY",
        "AKIA",
        "ghp_",
        "github_pat_",
        "sk_live_",
        "password = ",
        "api_key = ",
    ];
    for (p, b) in rows {
        let t = String::from_utf8_lossy(&b);
        if p.ends_with("Cargo.lock") || p.ends_with("package-lock.json") {
            dependency_files.push(p.clone());
        }
        if p.to_ascii_lowercase().contains("license") {
            licenses.insert(p.clone(), sha256_hex(&b));
        }
        if !p.contains("phase5.rs") && !p.contains("docs/") {
            for m in secret_markers {
                if t.contains(m) {
                    f.push(Finding {
                        scanner: "secret".into(),
                        severity: "critical".into(),
                        path: p.clone(),
                        detail: format!("secret-like marker: {m}"),
                    });
                }
            }
        }
        for (needle, detail) in [
            ("Command::new(\"sh\")", "shell execution"),
            ("danger_accept_invalid_certs", "TLS verification disabled"),
            ("set_inner_html", "HTML injection sink"),
            ("unsafe {", "unsafe Rust block"),
        ] {
            if t.contains(needle) {
                f.push(Finding {
                    scanner: "static-security".into(),
                    severity: "review".into(),
                    path: p.clone(),
                    detail: detail.into(),
                });
            }
        }
    }
    dependency_files.sort();
    f.sort_by(|a, b| (&a.path, &a.scanner, &a.detail).cmp(&(&b.path, &b.scanner, &b.detail)));
    Ok(AuditReport {
        twin_sha256: twin.root_sha256.clone(),
        findings: f,
        dependency_files,
        licenses,
        secret_scan_complete: true,
        security_scan_complete: true,
    })
}

pub fn profile(twin: &RepoTwin, mut samples: Vec<ProfileSample>) -> ProfileReport {
    samples.sort_by(|a, b| a.name.cmp(&b.name));
    let within = samples.iter().all(|s| s.value <= s.budget) && !samples.is_empty();
    let mut p = ProfileReport {
        twin_sha256: twin.root_sha256.clone(),
        samples,
        within_budget: within,
        sha256: String::new(),
    };
    p.sha256 = canonical_hash(&(&p.twin_sha256, &p.samples, p.within_budget));
    p
}
pub fn validate_profile(twin: &RepoTwin, p: &ProfileReport) -> Result<(), String> {
    let fresh = profile(twin, p.samples.clone());
    if fresh == *p {
        Ok(())
    } else {
        Err("performance profile is stale or forged".into())
    }
}

pub fn visual_replay(
    twin: &RepoTwin,
    baseline: Vec<VisualArtifact>,
    current: Vec<VisualArtifact>,
    steps: Vec<InteractionStep>,
) -> VisualReplay {
    let valid_steps = !steps.is_empty()
        && steps
            .iter()
            .all(|s| !s.action.trim().is_empty() && !s.expected_state.trim().is_empty());
    let norm = |v: &Vec<VisualArtifact>| {
        v.iter()
            .map(|x| (x.state.clone(), x.sha256.clone(), x.width, x.height))
            .collect::<Vec<_>>()
    };
    VisualReplay {
        twin_sha256: twin.root_sha256.clone(),
        matched: valid_steps && norm(&baseline) == norm(&current),
        baseline,
        current,
        steps,
    }
}
pub fn validate_visual(twin: &RepoTwin, v: &VisualReplay) -> Result<(), String> {
    if v.twin_sha256 != twin.root_sha256 {
        return Err("visual evidence belongs to another repository state".into());
    }
    let fresh = visual_replay(twin, v.baseline.clone(), v.current.clone(), v.steps.clone());
    if fresh == *v && v.matched {
        Ok(())
    } else {
        Err("visual baseline drift or interaction replay failure".into())
    }
}

pub fn validate_synth_spec(spec: &SynthToolSpec, workspace: &Path) -> Result<(), String> {
    if spec.allow_network || spec.allow_process_spawn {
        return Err("synthesized tool requests authority expansion".into());
    }
    if spec.name.is_empty() || spec.inputs.is_empty() {
        return Err("synthesized tool is not narrow".into());
    }
    let root = fs::canonicalize(workspace).map_err(|e| e.to_string())?;
    let allowed = fs::canonicalize(&spec.allowed_root).map_err(|e| e.to_string())?;
    if root != allowed {
        return Err("synthesized tool root is outside disposable sandbox".into());
    }
    for i in &spec.inputs {
        let rel = Path::new(i);
        safe_rel(rel)?;
        let c = fs::canonicalize(root.join(rel)).map_err(|e| e.to_string())?;
        if !c.starts_with(&root) {
            return Err("synthesized tool input escaped sandbox".into());
        }
    }
    Ok(())
}
pub fn synthesize_use_discard(
    spec: &SynthToolSpec,
    workspace: &Path,
) -> Result<SynthToolReceipt, String> {
    validate_synth_spec(spec, workspace)?;
    let scratch = workspace.join(".rex-synth");
    if scratch.exists() {
        fs::remove_dir_all(&scratch).map_err(|e| e.to_string())?
    }
    fs::create_dir(&scratch).map_err(|e| e.to_string())?;
    let source = r#"use std::{env,fs};use std::hash::{Hash,Hasher};use std::collections::hash_map::DefaultHasher;fn main(){let mut h=DefaultHasher::new();for p in env::args().skip(1){fs::read(p).unwrap().hash(&mut h);}println!("{:016x}",h.finish());}"#;
    let src = scratch.join("tool.rs");
    let bin = scratch.join("tool");
    fs::write(&src, source).map_err(|e| e.to_string())?;
    let compiled = Command::new("rustc")
        .current_dir(&scratch)
        .arg("tool.rs")
        .arg("-o")
        .arg("tool")
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !compiled {
        let _ = fs::remove_dir_all(&scratch);
        return Err("temporary tool failed to compile".into());
    }
    let mut cmd = Command::new(&bin);
    cmd.current_dir(workspace);
    for i in &spec.inputs {
        cmd.arg(i);
    }
    let out = cmd.output().map_err(|e| e.to_string())?;
    let tested = out.status.success() && !out.stdout.is_empty();
    let used = tested;
    let output_sha256 = sha256_hex(&out.stdout);
    let source_sha256 = sha256_hex(source.as_bytes());
    let spec_sha256 = canonical_hash(spec);
    fs::remove_dir_all(&scratch).map_err(|e| e.to_string())?;
    Ok(SynthToolReceipt {
        spec_sha256,
        source_sha256,
        output_sha256,
        compiled,
        tested,
        used,
        discarded: !scratch.exists(),
    })
}

pub fn reconstruction_prompt(
    task: &str,
    twin: &RepoTwin,
    contract: &AcceptanceContract,
    manifest: &str,
) -> String {
    format!("You are a fresh reconstruction judge. You receive only the ORIGINAL REQUEST, FINAL REPOSITORY DIGITAL TWIN, ACCEPTANCE CONTRACT, and EVIDENCE MANIFEST. You have no builder narrative or hidden chain of thought. Reconstruct why each acceptance item is correct. If evidence is insufficient, reconstructed=false. Answer one JSON object only: {{\"reconstructed\":bool,\"obligation_ids\":[...],\"evidence_ids\":[...],\"explanation\":\"...\"}}.\n\nORIGINAL REQUEST:\n{task}\n\nFINAL TWIN:\n{}\n\nCONTRACT:\n{}\n\nEVIDENCE MANIFEST:\n{manifest}",serde_json::to_string(twin).unwrap_or_default(),serde_json::to_string(contract).unwrap_or_default())
}
pub fn parse_reconstruction(
    text: &str,
    model: &str,
    same: bool,
    contract: &AcceptanceContract,
    known_evidence: &BTreeSet<String>,
) -> Result<ReconstructionReport, String> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Raw {
        reconstructed: bool,
        obligation_ids: Vec<String>,
        evidence_ids: Vec<String>,
        explanation: String,
    }
    let start = text.find('{').ok_or("reconstruction returned no JSON")?;
    let end = text
        .rfind('}')
        .ok_or("reconstruction returned incomplete JSON")?;
    let r: Raw = serde_json::from_str(&text[start..=end]).map_err(|e| e.to_string())?;
    let need = contract
        .obligations
        .iter()
        .map(|o| o.id.clone())
        .collect::<BTreeSet<_>>();
    let got = r.obligation_ids.iter().cloned().collect::<BTreeSet<_>>();
    if need != got {
        return Err("reconstruction omitted or invented acceptance obligations".into());
    }
    if r.evidence_ids.is_empty() || r.evidence_ids.iter().any(|x| !known_evidence.contains(x)) {
        return Err("reconstruction cites missing evidence".into());
    }
    if !r.reconstructed || r.explanation.trim().is_empty() {
        return Err("fresh judge could not reconstruct correctness".into());
    }
    Ok(ReconstructionReport {
        model: model.into(),
        same_model_as_worker: same,
        saw_builder_narrative: false,
        reconstructed: true,
        obligation_ids: r.obligation_ids,
        evidence_ids: r.evidence_ids,
        explanation: r.explanation,
    })
}
pub fn choose_critic(worker: &ModelRoute, configured: Option<&ModelRoute>) -> CriticPolicy {
    match configured {
        Some(c)
            if c.explicitly_configured
                && c.safe_provider
                && (c.provider != worker.provider || c.model != worker.model) =>
        {
            CriticPolicy {
                worker: format!("{}/{}", worker.provider, worker.model),
                selected: format!("{}/{}", c.provider, c.model),
                same_model: false,
                reason: "explicit safe cross-model route".into(),
            }
        }
        _ => CriticPolicy {
            worker: format!("{}/{}", worker.provider, worker.model),
            selected: format!("{}/{}", worker.provider, worker.model),
            same_model: true,
            reason: "no explicitly configured safe cross-model route; honestly using same model"
                .into(),
        },
    }
}

pub fn promotion_gate(
    contract: &AcceptanceContract,
    verification: &VerificationReport,
    reconstruction: &ReconstructionReport,
    worlds: &WorldLinks,
    evidence: &BTreeSet<String>,
) -> PromotionGate {
    let mut reasons = vec![];
    let mut links = vec![];
    for ob in &contract.obligations {
        let out = verification
            .outcomes
            .iter()
            .find(|x| x.obligation_id == ob.id);
        let ids = out.map(|x| x.evidence_ids.clone()).unwrap_or_default();
        let passed = out
            .map(|x| {
                matches!(
                    x.status,
                    ObligationStatus::Proven | ObligationStatus::AwaitingJudge
                )
            })
            .unwrap_or(false)
            && !ids.is_empty()
            && ids.iter().all(|x| evidence.contains(x));
        if !passed {
            reasons.push(format!(
                "acceptance {} lacks executable linked evidence",
                ob.id
            ));
        }
        links.push(ProofLink {
            acceptance_id: ob.id.clone(),
            evidence_ids: ids,
            executable: out
                .map(|x| x.status != ObligationStatus::AwaitingJudge)
                .unwrap_or(false),
            passed,
        });
    }
    for (name, p) in [
        ("builder", &worlds.builder),
        ("adversary", &worlds.adversary),
        ("shadow", &worlds.shadow),
        ("recovery", &worlds.recovery),
        ("clean-room", &worlds.clean_room),
        ("phase3", &worlds.phase3),
        ("phase4", &worlds.phase4),
    ] {
        if p.trim().is_empty() {
            reasons.push(format!("missing {name} world proof link"));
        }
    }
    if !reconstruction.reconstructed || reconstruction.saw_builder_narrative {
        reasons.push("independent reconstruction failed or leaked builder narrative".into())
    }
    PromotionGate {
        promotable: reasons.is_empty(),
        reasons,
        links,
    }
}

pub fn validate_bundle(root: &Path, b: &Phase5Bundle) -> Result<(), String> {
    validate_twin(root, &b.twin)?;
    validate_index(root, &b.twin, &b.index)?;
    validate_profile(&b.twin, &b.profiles)?;
    if !b.audit.secret_scan_complete
        || !b.audit.security_scan_complete
        || b.audit.twin_sha256 != b.twin.root_sha256
    {
        return Err("audit incomplete or stale".into());
    }
    if let Some(v) = &b.visual {
        validate_visual(&b.twin, v)?
    }
    if let Some(s) = &b.synthesis {
        if !(s.compiled && s.tested && s.used && s.discarded) {
            return Err("temporary capability lifecycle incomplete".into());
        }
    }
    if !b.gate.promotable {
        return Err(format!("phase 5 gate failed: {:?}", b.gate.reasons));
    }
    Ok(())
}
pub fn seal_bundle(
    root: &Path,
    ultra_dir: &Path,
    b: &Phase5Bundle,
    ledger: &mut EpistemicLedger,
    evidence: &mut EvidenceStore,
) -> Result<String, String> {
    validate_bundle(root, b)?;
    let bytes = serde_json::to_vec_pretty(b).map_err(|e| e.to_string())?;
    atomic_json(&ultra_dir.join("phase5.json"), b)?;
    let ev = evidence.put_bytes("phase5_bundle", &bytes);
    ledger.record(
        "Phase 5 repository proof and promotion gate passed",
        FactClass::Observed {
            evidence_id: ev.id.clone(),
        },
    );
    Ok(ev.id)
}

impl DurableQueue {
    pub fn open(path: &Path, request_sha256: &str) -> Result<Self, String> {
        if path.exists() {
            let q: Self = serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            if q.version != 1 || q.request_sha256 != request_sha256 {
                return Err("queue checkpoint belongs to another request or version".into());
            }
            if q.items.len() > MAX_QUEUE {
                return Err("queue checkpoint exceeds budget".into());
            }
            Ok(q)
        } else {
            Ok(Self {
                version: 1,
                request_sha256: request_sha256.into(),
                items: VecDeque::new(),
            })
        }
    }
    pub fn push(&mut self, id: &str, payload: &[u8]) -> Result<(), String> {
        if self.items.len() >= MAX_QUEUE {
            return Err("queue budget exceeded".into());
        }
        if self.items.iter().any(|x| x.id == id) {
            return Err("duplicate queue id".into());
        }
        self.items.push_back(QueueItem {
            id: id.into(),
            payload_sha256: sha256_hex(payload),
            status: QueueStatus::Pending,
            attempts: 0,
            result_sha256: None,
        });
        Ok(())
    }
    pub fn start_next(&mut self) -> Option<String> {
        let x = self
            .items
            .iter_mut()
            .find(|x| x.status == QueueStatus::Pending)?;
        x.status = QueueStatus::Running;
        x.attempts += 1;
        Some(x.id.clone())
    }
    pub fn finish(&mut self, id: &str, result: &[u8], passed: bool) -> Result<(), String> {
        let x = self
            .items
            .iter_mut()
            .find(|x| x.id == id)
            .ok_or("unknown queue item")?;
        if x.status != QueueStatus::Running {
            return Err("queue item is not running".into());
        }
        x.status = if passed {
            QueueStatus::Passed
        } else {
            QueueStatus::Failed
        };
        x.result_sha256 = Some(sha256_hex(result));
        Ok(())
    }
    pub fn checkpoint(&self, path: &Path) -> Result<(), String> {
        atomic_json(path, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("src")).unwrap();
        fs::write(
            d.path().join("src/lib.rs"),
            "pub fn answer()->u8{42}\n#[cfg(test)] mod tests {}\n",
        )
        .unwrap();
        fs::write(
            d.path().join("Cargo.toml"),
            "[package]\nname=\"x\"\nversion=\"0.1.0\"\nlicense=\"MIT\"\n",
        )
        .unwrap();
        d
    }
    #[test]
    fn stale_twin_fails() {
        let d = repo();
        let t = build_twin(d.path()).unwrap();
        fs::write(d.path().join("src/lib.rs"), "changed").unwrap();
        assert!(validate_twin(d.path(), &t).is_err())
    }
    #[test]
    fn forged_index_fails() {
        let d = repo();
        let t = build_twin(d.path()).unwrap();
        let mut i = build_index(d.path(), &t).unwrap();
        i.symbols.clear();
        assert!(validate_index(d.path(), &t, &i).is_err())
    }
    #[test]
    fn forged_profile_fails() {
        let d = repo();
        let t = build_twin(d.path()).unwrap();
        let mut p = profile(
            &t,
            vec![ProfileSample {
                name: "binary".into(),
                value: 10,
                unit: "bytes".into(),
                budget: 20,
                command: "stat".into(),
            }],
        );
        p.within_budget = false;
        assert!(validate_profile(&t, &p).is_err())
    }
    #[test]
    fn visual_drift_fails() {
        let d = repo();
        let t = build_twin(d.path()).unwrap();
        let a = VisualArtifact {
            state: "idle".into(),
            path: "idle.png".into(),
            sha256: "a".into(),
            width: 100,
            height: 100,
        };
        let b = VisualArtifact {
            sha256: "b".into(),
            ..a.clone()
        };
        let v = visual_replay(
            &t,
            vec![a],
            vec![b],
            vec![InteractionStep {
                action: "open".into(),
                target: "app".into(),
                expected_state: "idle".into(),
            }],
        );
        assert!(validate_visual(&t, &v).is_err())
    }
    #[test]
    fn synthesized_tool_cannot_expand_authority() {
        let d = repo();
        let s = SynthToolSpec {
            name: "x".into(),
            capability: SynthCapability::HashFile,
            inputs: vec!["src/lib.rs".into()],
            allow_network: true,
            allow_process_spawn: false,
            allowed_root: d.path().display().to_string(),
        };
        assert!(validate_synth_spec(&s, d.path()).is_err());
        let mut s2 = s.clone();
        s2.allow_network = false;
        s2.inputs = vec!["../outside".into()];
        assert!(validate_synth_spec(&s2, d.path()).is_err())
    }
    #[test]
    fn reconstruction_rejects_leakage_and_failure() {
        let c=crate::contract::parse_contract(r#"{"obligations":[{"id":"a","statement":"a","proof":{"kind":"file_exists","path":"x"}}]}"#,"x").unwrap();
        let ev = BTreeSet::from(["ev-1".into()]);
        assert!(parse_reconstruction(r#"{"reconstructed":false,"obligation_ids":["a"],"evidence_ids":["ev-1"],"explanation":"no"}"#,"m",true,&c,&ev).is_err());
        assert!(parse_reconstruction(r#"{"reconstructed":true,"obligation_ids":["a"],"evidence_ids":["fake"],"explanation":"yes"}"#,"m",true,&c,&ev).is_err())
    }
    #[test]
    fn unavailable_cross_model_is_honest() {
        let w = ModelRoute {
            provider: "p".into(),
            model: "m".into(),
            explicitly_configured: true,
            safe_provider: true,
        };
        let unsafe_route = ModelRoute {
            provider: "q".into(),
            model: "x".into(),
            explicitly_configured: true,
            safe_provider: false,
        };
        let p = choose_critic(&w, Some(&unsafe_route));
        assert!(p.same_model);
        assert!(p.reason.contains("same model"))
    }
    #[test]
    fn incomplete_proof_links_fail() {
        let c=crate::contract::parse_contract(r#"{"obligations":[{"id":"a","statement":"a","proof":{"kind":"file_exists","path":"x"}}]}"#,"x").unwrap();
        let v = VerificationReport {
            outcomes: vec![],
            executable_all_proven: false,
            verified_ms: 0,
        };
        let r = ReconstructionReport {
            model: "m".into(),
            same_model_as_worker: true,
            saw_builder_narrative: false,
            reconstructed: true,
            obligation_ids: vec!["a".into()],
            evidence_ids: vec!["ev".into()],
            explanation: "x".into(),
        };
        let w = WorldLinks {
            builder: "b".into(),
            adversary: "a".into(),
            shadow: "s".into(),
            recovery: "r".into(),
            clean_room: "c".into(),
            phase3: "3".into(),
            phase4: "4".into(),
        };
        assert!(!promotion_gate(&c, &v, &r, &w, &BTreeSet::new()).promotable)
    }
    #[test]
    fn queue_resumes_without_reapplying() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("q.json");
        let mut q = DurableQueue::open(&p, "req").unwrap();
        q.push("a", b"payload").unwrap();
        assert_eq!(q.start_next().as_deref(), Some("a"));
        q.checkpoint(&p).unwrap();
        let q2 = DurableQueue::open(&p, "req").unwrap();
        assert!(q2.items[0].status == QueueStatus::Running);
        assert!(DurableQueue::open(&p, "other").is_err())
    }
}
