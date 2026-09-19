//! Phase 4: causal tracing, deterministic replay, fault injection, recovery,
//! exact rollback and bounded regression isolation.
//!
//! The machinery in this module is model-independent. It persists canonical,
//! hash-linked records and fails closed when a run cannot prove its history.

use crate::evidence::{sha256_hex, EvidenceStore};
use crate::ledger::{EpistemicLedger, FactClass};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const FORMAT_VERSION: u16 = 1;
pub const MAX_TRACE_EVENTS: usize = 20_000;
pub const MAX_REPLAY_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_CHECKPOINT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_SNAPSHOT_FILES: usize = 20_000;
pub const MAX_SNAPSHOT_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_BISECT_PROBES: usize = 32;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    PhaseDecision,
    ModelDecision,
    ToolRequest,
    ToolResult,
    CommandStart,
    CommandExit,
    ProcessStart,
    ProcessExit,
    FileMutation,
    TauriEvent,
    SidecarEvent,
    IpcEvent,
    VerifierEvidence,
    CompletionDecision,
    Checkpoint,
    Recovery,
    Rollback,
    Fault,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CausalEvent {
    pub id: String,
    pub sequence: u64,
    pub kind: EventKind,
    pub parents: Vec<String>,
    pub payload_sha256: String,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CausalTrace {
    pub format_version: u16,
    pub run_id: String,
    pub events: Vec<CausalEvent>,
}

fn canonical_json(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), canonical_json(v)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(canonical_json).collect()),
        v => v.clone(),
    }
}

fn event_id(
    run_id: &str,
    sequence: u64,
    kind: &EventKind,
    parents: &[String],
    payload: &Value,
) -> String {
    let bytes = serde_json::to_vec(&(run_id, sequence, kind, parents, canonical_json(payload)))
        .unwrap_or_default();
    format!("evt-{}", &sha256_hex(&bytes)[..24])
}

impl CausalTrace {
    pub fn new(run_id: impl Into<String>) -> Self {
        Self {
            format_version: FORMAT_VERSION,
            run_id: run_id.into(),
            events: Vec::new(),
        }
    }

    pub fn append(
        &mut self,
        kind: EventKind,
        mut parents: Vec<String>,
        payload: Value,
    ) -> Result<String, String> {
        if self.events.len() >= MAX_TRACE_EVENTS {
            return Err("causal trace event budget exceeded".into());
        }
        parents.sort();
        parents.dedup();
        let known: BTreeSet<_> = self.events.iter().map(|e| e.id.as_str()).collect();
        if parents.iter().any(|p| !known.contains(p.as_str())) {
            return Err("causal parent is missing or forged".into());
        }
        let sequence = self.events.len() as u64 + 1;
        let payload = canonical_json(&payload);
        let id = event_id(&self.run_id, sequence, &kind, &parents, &payload);
        let payload_sha256 = sha256_hex(&serde_json::to_vec(&payload).unwrap_or_default());
        self.events.push(CausalEvent {
            id: id.clone(),
            sequence,
            kind,
            parents,
            payload_sha256,
            payload,
        });
        Ok(id)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.format_version != FORMAT_VERSION {
            return Err("unsupported causal trace version".into());
        }
        if self.run_id.trim().is_empty() {
            return Err("trace has no run id".into());
        }
        if self.events.is_empty() {
            return Err("trace is empty".into());
        }
        if self.events.len() > MAX_TRACE_EVENTS {
            return Err("trace exceeds event budget".into());
        }
        let mut seen = BTreeSet::new();
        for (index, event) in self.events.iter().enumerate() {
            let expected_seq = index as u64 + 1;
            if event.sequence != expected_seq {
                return Err("trace sequence is ambiguous".into());
            }
            if event.parents.iter().any(|p| !seen.contains(p)) {
                return Err("event cites a missing, future or forged parent".into());
            }
            if event.parents.iter().collect::<BTreeSet<_>>().len() != event.parents.len() {
                return Err("event has duplicate parents".into());
            }
            let expected_payload = sha256_hex(
                &serde_json::to_vec(&canonical_json(&event.payload)).unwrap_or_default(),
            );
            if event.payload_sha256 != expected_payload {
                return Err("event payload hash mismatch".into());
            }
            let expected_id = event_id(
                &self.run_id,
                event.sequence,
                &event.kind,
                &event.parents,
                &event.payload,
            );
            if event.id != expected_id || !seen.insert(event.id.clone()) {
                return Err("event id is forged or duplicated".into());
            }
        }
        let completion = self
            .events
            .iter()
            .filter(|e| e.kind == EventKind::CompletionDecision)
            .count();
        if completion != 1 {
            return Err("trace must contain exactly one completion decision".into());
        }
        Ok(())
    }

    pub fn write_atomic(&self, path: &Path) -> Result<(), String> {
        self.validate()?;
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        atomic_write(path, &bytes)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplayInput {
    pub name: String,
    pub sha256: String,
    pub redacted: bool,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplayBundle {
    pub format_version: u16,
    pub run_id: String,
    pub trace_sha256: String,
    pub provider: String,
    pub model: String,
    pub seed: u64,
    pub inputs: Vec<ReplayInput>,
    pub expected_observables: BTreeMap<String, String>,
    pub reproducible: bool,
    pub non_reproducible_reasons: Vec<String>,
}

fn sensitive_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    [
        "key",
        "token",
        "secret",
        "password",
        "cookie",
        "authorization",
        "credential",
    ]
    .iter()
    .any(|needle| k.contains(needle))
}

pub fn redact_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        if sensitive_key(k) {
                            Value::String("[REDACTED]".into())
                        } else {
                            redact_value(v)
                        },
                    )
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(redact_value).collect()),
        v => v.clone(),
    }
}

impl ReplayBundle {
    pub fn build(
        trace: &CausalTrace,
        provider: &str,
        model: &str,
        seed: u64,
        inputs: Vec<(String, Value)>,
        expected_observables: BTreeMap<String, String>,
    ) -> Self {
        let mut total = 0usize;
        let mut reasons = Vec::new();
        let mut replay_inputs = Vec::new();
        for (name, value) in inputs {
            let redacted_value = redact_value(&value);
            let redacted = redacted_value != value;
            let bytes = serde_json::to_vec(&canonical_json(&redacted_value)).unwrap_or_default();
            total = total.saturating_add(bytes.len());
            if redacted {
                reasons.push(format!("input {name} contains redacted secret material"));
            }
            replay_inputs.push(ReplayInput {
                name,
                sha256: sha256_hex(&bytes),
                redacted,
                bytes,
            });
        }
        if total > MAX_REPLAY_BYTES {
            reasons.push("replay input budget exceeded".into());
        }
        let trace_sha256 = sha256_hex(&serde_json::to_vec(trace).unwrap_or_default());
        Self {
            format_version: FORMAT_VERSION,
            run_id: trace.run_id.clone(),
            trace_sha256,
            provider: provider.into(),
            model: model.into(),
            seed,
            inputs: replay_inputs,
            expected_observables,
            reproducible: reasons.is_empty(),
            non_reproducible_reasons: reasons,
        }
    }

    pub fn validate(&self, trace: &CausalTrace) -> Result<(), String> {
        trace.validate()?;
        if self.format_version != FORMAT_VERSION || self.run_id != trace.run_id {
            return Err("replay identity/version mismatch".into());
        }
        if self.trace_sha256 != sha256_hex(&serde_json::to_vec(trace).unwrap_or_default()) {
            return Err("replay trace drift".into());
        }
        let total: usize = self.inputs.iter().map(|i| i.bytes.len()).sum();
        if total > MAX_REPLAY_BYTES {
            return Err("replay input budget exceeded".into());
        }
        for input in &self.inputs {
            if input.sha256 != sha256_hex(&input.bytes) {
                return Err(format!("replay input drift: {}", input.name));
            }
        }
        if !self.reproducible || !self.non_reproducible_reasons.is_empty() {
            return Err("run is truthfully marked non-reproducible".into());
        }
        Ok(())
    }

    pub fn compare(&self, actual: &BTreeMap<String, String>) -> Result<(), String> {
        if &self.expected_observables == actual {
            Ok(())
        } else {
            Err("deterministic replay drift".into())
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum FaultSeam {
    ProcessDeath,
    Timeout,
    NetworkInterruption,
    TruncatedStream,
    ReorderedStream,
    DiskWriteFailure,
    CorruptState,
    CorruptCheckpoint,
    CancellationRace,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FaultSpec {
    pub seam: FaultSeam,
    pub occurrence: u32,
    pub seed: u64,
}

pub struct FaultInjector {
    specs: Vec<FaultSpec>,
    hits: BTreeMap<FaultSeam, u32>,
    deadline: Instant,
    cancelled: AtomicBool,
}

impl FaultInjector {
    pub fn new(specs: Vec<FaultSpec>, budget: Duration) -> Result<Self, String> {
        if specs.len() > 64 || budget > Duration::from_secs(600) {
            return Err("fault campaign budget exceeds limit".into());
        }
        if specs.iter().any(|s| s.occurrence == 0) {
            return Err("fault occurrence is one-based".into());
        }
        Ok(Self {
            specs,
            hits: BTreeMap::new(),
            deadline: Instant::now() + budget,
            cancelled: AtomicBool::new(false),
        })
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
    pub fn check(&mut self, seam: FaultSeam) -> Result<(), String> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err("fault campaign cancelled".into());
        }
        if Instant::now() > self.deadline {
            return Err("fault campaign timed out".into());
        }
        let hit = self.hits.entry(seam).or_insert(0);
        *hit += 1;
        if self
            .specs
            .iter()
            .any(|s| s.seam == seam && s.occurrence == *hit)
        {
            Err(format!("injected fault at {seam:?} occurrence {hit}"))
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EffectRecord {
    pub idempotency_key: String,
    pub request_sha256: String,
    pub result_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CheckpointState {
    pub phase: String,
    pub cursor: u64,
    pub effects: Vec<EffectRecord>,
    pub state: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct CheckpointEnvelope {
    format_version: u16,
    generation: u64,
    payload_sha256: String,
    payload: CheckpointState,
}

pub fn write_checkpoint(
    path: &Path,
    generation: u64,
    state: &CheckpointState,
) -> Result<(), String> {
    let payload = serde_json::to_vec(state).map_err(|e| e.to_string())?;
    if payload.len() > MAX_CHECKPOINT_BYTES {
        return Err("checkpoint exceeds size budget".into());
    }
    let env = CheckpointEnvelope {
        format_version: FORMAT_VERSION,
        generation,
        payload_sha256: sha256_hex(&payload),
        payload: state.clone(),
    };
    atomic_write(
        path,
        &serde_json::to_vec_pretty(&env).map_err(|e| e.to_string())?,
    )
}

pub fn read_checkpoint(path: &Path) -> Result<(u64, CheckpointState), String> {
    let bytes = fs::read(path).map_err(|e| format!("checkpoint read: {e}"))?;
    if bytes.len() > MAX_CHECKPOINT_BYTES {
        return Err("checkpoint exceeds size budget".into());
    }
    let env: CheckpointEnvelope =
        serde_json::from_slice(&bytes).map_err(|e| format!("checkpoint corrupt: {e}"))?;
    if env.format_version != FORMAT_VERSION {
        return Err("checkpoint version unsupported".into());
    }
    let payload = serde_json::to_vec(&env.payload).map_err(|e| e.to_string())?;
    if env.payload_sha256 != sha256_hex(&payload) {
        return Err("checkpoint integrity mismatch".into());
    }
    let mut keys = BTreeSet::new();
    if env
        .payload
        .effects
        .iter()
        .any(|e| e.idempotency_key.is_empty() || !keys.insert(&e.idempotency_key))
    {
        return Err("checkpoint has ambiguous/double-applied effects".into());
    }
    Ok((env.generation, env.payload))
}

pub fn apply_effect_once<T, F>(
    state: &mut CheckpointState,
    key: &str,
    request: &[u8],
    effect: F,
) -> Result<T, String>
where
    T: Serialize + Clone,
    F: FnOnce() -> Result<T, String>,
{
    let request_sha256 = sha256_hex(request);
    if let Some(prior) = state.effects.iter().find(|e| e.idempotency_key == key) {
        if prior.request_sha256 != request_sha256 {
            return Err("idempotency key reused for a different effect".into());
        }
        return Err("effect already applied; replay suppressed".into());
    }
    let result = effect()?;
    let bytes = serde_json::to_vec(&result).map_err(|e| e.to_string())?;
    state.effects.push(EffectRecord {
        idempotency_key: key.into(),
        request_sha256,
        result_sha256: sha256_hex(&bytes),
    });
    Ok(result)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotEntry {
    pub path: String,
    pub sha256: String,
    pub bytes: Vec<u8>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceSnapshot {
    pub entries: Vec<SnapshotEntry>,
    pub tree_sha256: String,
}

fn safe_relative(path: &Path) -> bool {
    !path.is_absolute() && path.components().all(|c| matches!(c, Component::Normal(_)))
}

fn collect_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    let mut queue = VecDeque::from([root.to_path_buf()]);
    while let Some(dir) = queue.pop_front() {
        let mut items: Vec<_> = fs::read_dir(&dir)
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .collect();
        items.sort_by_key(|e| e.file_name());
        for item in items {
            let path = item.path();
            let name = item.file_name();
            if name == ".git" || name == "target" || name == "node_modules" || name == "ultra" {
                continue;
            }
            let meta = item.metadata().map_err(|e| e.to_string())?;
            if meta.file_type().is_symlink() {
                return Err("workspace snapshot refuses symlinks".into());
            }
            if meta.is_dir() {
                queue.push_back(path);
            } else if meta.is_file() {
                out.push(path);
                if out.len() > MAX_SNAPSHOT_FILES {
                    return Err("snapshot file budget exceeded".into());
                }
            }
        }
    }
    Ok(out)
}

impl WorkspaceSnapshot {
    pub fn capture(root: &Path) -> Result<Self, String> {
        let mut entries = Vec::new();
        let mut total = 0u64;
        for path in collect_files(root)? {
            let rel = path
                .strip_prefix(root)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .replace('\\', "/");
            let bytes = fs::read(&path).map_err(|e| e.to_string())?;
            total = total.saturating_add(bytes.len() as u64);
            if total > MAX_SNAPSHOT_BYTES {
                return Err("snapshot byte budget exceeded".into());
            }
            entries.push(SnapshotEntry {
                path: rel,
                sha256: sha256_hex(&bytes),
                bytes,
            });
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        let tree_sha256 = sha256_hex(&serde_json::to_vec(&entries).unwrap_or_default());
        Ok(Self {
            entries,
            tree_sha256,
        })
    }

    pub fn restore(&self, root: &Path) -> Result<(), String> {
        if self.tree_sha256 != sha256_hex(&serde_json::to_vec(&self.entries).unwrap_or_default()) {
            return Err("rollback snapshot integrity mismatch".into());
        }
        let expected: BTreeSet<_> = self.entries.iter().map(|e| e.path.as_str()).collect();
        for path in collect_files(root)? {
            let rel = path
                .strip_prefix(root)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .replace('\\', "/");
            if !expected.contains(rel.as_str()) {
                fs::remove_file(path).map_err(|e| format!("rollback remove: {e}"))?;
            }
        }
        for entry in &self.entries {
            let rel = Path::new(&entry.path);
            if !safe_relative(rel) {
                return Err("unsafe path in rollback snapshot".into());
            }
            if entry.sha256 != sha256_hex(&entry.bytes) {
                return Err("rollback entry integrity mismatch".into());
            }
            let path = root.join(rel);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            atomic_write(&path, &entry.bytes)?;
        }
        let after = Self::capture(root)?;
        if after.tree_sha256 != self.tree_sha256 {
            return Err("rollback mismatch".into());
        }
        Ok(())
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    {
        let mut f = fs::File::create(&tmp).map_err(|e| e.to_string())?;
        f.write_all(bytes).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
    }
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        e.to_string()
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BisectReport {
    pub culprit: Option<String>,
    pub probes: Vec<String>,
    pub minimized_delta: Vec<String>,
    pub conclusive: bool,
    pub reason: String,
}

pub fn bounded_bisect<F>(
    ordered_changes: &[String],
    max_probes: usize,
    mut regresses: F,
) -> BisectReport
where
    F: FnMut(&[String]) -> Result<bool, String>,
{
    let limit = max_probes.min(MAX_BISECT_PROBES);
    let mut probes = Vec::new();
    if ordered_changes.is_empty() {
        return BisectReport {
            culprit: None,
            probes,
            minimized_delta: vec![],
            conclusive: false,
            reason: "no changes".into(),
        };
    }
    let mut probe = |slice: &[String]| -> Result<bool, String> {
        if probes.len() >= limit {
            return Err("probe budget exhausted".into());
        }
        probes.push(sha256_hex(&serde_json::to_vec(slice).unwrap_or_default()));
        regresses(slice)
    };
    if probe(&[]).unwrap_or(true) {
        return BisectReport {
            culprit: None,
            probes,
            minimized_delta: vec![],
            conclusive: false,
            reason: "baseline regresses; attribution refused".into(),
        };
    }
    if !probe(ordered_changes).unwrap_or(false) {
        return BisectReport {
            culprit: None,
            probes,
            minimized_delta: vec![],
            conclusive: false,
            reason: "head does not reproduce regression".into(),
        };
    }
    let (mut lo, mut hi) = (0usize, ordered_changes.len());
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        match probe(&ordered_changes[..mid]) {
            Ok(true) => hi = mid,
            Ok(false) => lo = mid,
            Err(e) => {
                return BisectReport {
                    culprit: None,
                    probes,
                    minimized_delta: vec![],
                    conclusive: false,
                    reason: e,
                }
            }
        }
    }
    let culprit = ordered_changes[hi - 1].clone();
    let single = vec![culprit.clone()];
    match probe(&single) {
        Ok(true) => BisectReport {
            culprit: Some(culprit.clone()),
            probes,
            minimized_delta: vec![culprit],
            conclusive: true,
            reason: "single causal change reproduces regression".into(),
        },
        Ok(false) => BisectReport {
            culprit: None,
            probes,
            minimized_delta: ordered_changes[..hi].to_vec(),
            conclusive: false,
            reason: "interaction regression; single-change attribution refused".into(),
        },
        Err(e) => BisectReport {
            culprit: None,
            probes,
            minimized_delta: vec![],
            conclusive: false,
            reason: e,
        },
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Phase4Gate {
    pub promotable: bool,
    pub reasons: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Phase4Bundle {
    pub trace: CausalTrace,
    pub replay: ReplayBundle,
    pub checkpoint_recovered: bool,
    pub effects_exactly_once: bool,
    pub rollback_verified: bool,
    pub bisect: Option<BisectReport>,
    pub cleanup_complete: bool,
    pub gate: Phase4Gate,
}

pub fn gate(bundle: &Phase4Bundle) -> Phase4Gate {
    let mut reasons = Vec::new();
    if let Err(e) = bundle.trace.validate() {
        reasons.push(e);
    }
    if let Err(e) = bundle.replay.validate(&bundle.trace) {
        reasons.push(e);
    }
    if !bundle.checkpoint_recovered {
        reasons.push("checkpoint recovery not proven".into());
    }
    if !bundle.effects_exactly_once {
        reasons.push("exactly-once effect recovery not proven".into());
    }
    if !bundle.rollback_verified {
        reasons.push("rollback to exact pre-run state not proven".into());
    }
    if !bundle.cleanup_complete {
        reasons.push("phase 4 cleanup incomplete".into());
    }
    if let Some(b) = &bundle.bisect {
        if !b.conclusive {
            reasons.push("regression attribution is inconclusive".into());
        }
    }
    Phase4Gate {
        promotable: reasons.is_empty(),
        reasons,
    }
}

pub fn seal_bundle(
    dir: &Path,
    bundle: &mut Phase4Bundle,
    ledger: &mut EpistemicLedger,
    evidence: &mut EvidenceStore,
) -> Result<PathBuf, String> {
    bundle.gate = gate(bundle);
    let path = dir.join("phase4.json");
    let bytes = serde_json::to_vec_pretty(bundle).map_err(|e| e.to_string())?;
    atomic_write(&path, &bytes)?;
    let ev = evidence.put_bytes("phase4_bundle", &bytes);
    ledger.record(
        if bundle.gate.promotable {
            "Phase 4 causal replay/recovery gate passed"
        } else {
            "Phase 4 causal replay/recovery gate failed closed"
        },
        FactClass::Observed { evidence_id: ev.id },
    );
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn trace() -> CausalTrace {
        let mut t = CausalTrace::new("run-1");
        let a = t
            .append(
                EventKind::PhaseDecision,
                vec![],
                json!({"phase":"contract"}),
            )
            .unwrap();
        let b = t
            .append(
                EventKind::ToolRequest,
                vec![a],
                json!({"tool":"write","path":"a"}),
            )
            .unwrap();
        let c = t
            .append(EventKind::ToolResult, vec![b], json!({"ok":true}))
            .unwrap();
        t.append(
            EventKind::CompletionDecision,
            vec![c],
            json!({"promote":true}),
        )
        .unwrap();
        t
    }
    #[test]
    fn stable_ids_and_valid_causality() {
        assert_eq!(trace(), trace());
        trace().validate().unwrap();
    }
    #[test]
    fn forged_causality_is_rejected() {
        let mut t = trace();
        t.events[2].parents = vec!["evt-forged".into()];
        assert!(t.validate().unwrap_err().contains("forged"));
    }
    #[test]
    fn payload_tamper_is_rejected() {
        let mut t = trace();
        t.events[1].payload = json!({"tool":"shell"});
        assert!(t.validate().is_err());
    }
    #[test]
    fn secrets_are_redacted_and_non_reproducible() {
        let t = trace();
        let b = ReplayBundle::build(
            &t,
            "p",
            "m",
            1,
            vec![("env".into(), json!({"api_key":"raw"}))],
            BTreeMap::new(),
        );
        assert!(!b.reproducible);
        assert!(!serde_json::to_string(&b).unwrap().contains("raw"));
        assert!(b.validate(&t).is_err());
    }
    #[test]
    fn replay_drift_fails() {
        let t = trace();
        let mut expected = BTreeMap::new();
        expected.insert("file".into(), "abc".into());
        let b = ReplayBundle::build(&t, "p", "m", 1, vec![], expected);
        let mut actual = BTreeMap::new();
        actual.insert("file".into(), "def".into());
        assert!(b.compare(&actual).is_err());
    }
    #[test]
    fn each_fault_seam_is_deterministic() {
        for seam in [
            FaultSeam::ProcessDeath,
            FaultSeam::Timeout,
            FaultSeam::NetworkInterruption,
            FaultSeam::TruncatedStream,
            FaultSeam::ReorderedStream,
            FaultSeam::DiskWriteFailure,
            FaultSeam::CorruptState,
            FaultSeam::CorruptCheckpoint,
            FaultSeam::CancellationRace,
        ] {
            let mut f = FaultInjector::new(
                vec![FaultSpec {
                    seam,
                    occurrence: 2,
                    seed: 7,
                }],
                Duration::from_secs(1),
            )
            .unwrap();
            assert!(f.check(seam).is_ok());
            assert!(f.check(seam).is_err());
        }
    }
    #[test]
    fn checkpoint_detects_corruption() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("cp");
        let s = CheckpointState {
            phase: "build".into(),
            cursor: 2,
            effects: vec![],
            state: json!({"x":1}),
        };
        write_checkpoint(&p, 3, &s).unwrap();
        let mut bytes = fs::read(&p).unwrap();
        let n = bytes.len() - 5;
        bytes[n] ^= 1;
        fs::write(&p, bytes).unwrap();
        assert!(read_checkpoint(&p).is_err());
    }
    #[test]
    fn restart_does_not_double_apply() {
        let mut s = CheckpointState {
            phase: "x".into(),
            cursor: 0,
            effects: vec![],
            state: json!({}),
        };
        let calls = std::cell::Cell::new(0);
        apply_effect_once(&mut s, "write:a", b"request", || {
            calls.set(calls.get() + 1);
            Ok(7u8)
        })
        .unwrap();
        assert!(apply_effect_once(&mut s, "write:a", b"request", || {
            calls.set(calls.get() + 1);
            Ok(8u8)
        })
        .is_err());
        assert_eq!(calls.get(), 1);
    }
    #[test]
    fn rollback_restores_exact_tree() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir(d.path().join("sub")).unwrap();
        fs::write(d.path().join("a"), "old").unwrap();
        let s = WorkspaceSnapshot::capture(d.path()).unwrap();
        fs::write(d.path().join("a"), "new").unwrap();
        fs::write(d.path().join("sub/new"), "junk").unwrap();
        s.restore(d.path()).unwrap();
        assert_eq!(fs::read_to_string(d.path().join("a")).unwrap(), "old");
        assert!(!d.path().join("sub/new").exists());
    }
    #[test]
    fn rollback_mismatch_fails_closed() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("a"), "old").unwrap();
        let mut s = WorkspaceSnapshot::capture(d.path()).unwrap();
        s.entries[0].bytes = b"forged".to_vec();
        assert!(s.restore(d.path()).is_err());
    }
    #[test]
    fn bisect_finds_smallest_causal_change() {
        let c = vec!["a".into(), "b".into(), "c".into()];
        let r = bounded_bisect(&c, 16, |xs| Ok(xs.iter().any(|x| x == "b")));
        assert!(r.conclusive);
        assert_eq!(r.culprit.as_deref(), Some("b"));
        assert_eq!(r.minimized_delta.len(), 1);
    }
    #[test]
    fn bisect_refuses_false_attribution() {
        let c = vec!["a".into(), "b".into()];
        let r = bounded_bisect(&c, 16, |xs| Ok(xs.len() >= 2));
        assert!(!r.conclusive);
        assert!(r.culprit.is_none());
    }
    #[test]
    fn gate_rejects_unproven_recovery() {
        let t = trace();
        let r = ReplayBundle::build(&t, "p", "m", 1, vec![], BTreeMap::new());
        let mut b = Phase4Bundle {
            trace: t,
            replay: r,
            checkpoint_recovered: false,
            effects_exactly_once: true,
            rollback_verified: true,
            bisect: None,
            cleanup_complete: true,
            gate: Phase4Gate {
                promotable: false,
                reasons: vec![],
            },
        };
        b.gate = gate(&b);
        assert!(!b.gate.promotable);
    }
}
