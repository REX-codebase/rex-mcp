//! Deterministic state machine for an external host. There is no provider,
//! prompt, or managed-inference fallback in this module.

use rex_protocol::schema::canonical_hash;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use crate::contract::AcceptanceContract;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KernelState {
    New,
    Leased,
    Running,
    AwaitingEvidence,
    Completed,
    Failed,
    Revoked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KernelEvent {
    Acquire { lease_epoch: u64 },
    Begin { action_id: String },
    SubmitEvidence { digest: String },
    Complete,
    Fail { reason: String },
    Revoke,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelSnapshot {
    pub task_id: String,
    pub state: KernelState,
    pub step: u64,
    pub lease_epoch: u64,
    pub transcript_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KernelError {
    InvalidTransition { state: KernelState, event: String },
    EmptyValue,
    NonMonotonicLease,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostKernelStatus {
    HostRequired,
    Collecting,
    Ready,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostCandidateRequest {
    /// Visual contracts require pixel evidence at the taste gate; the host
    /// must know before generating candidates.
    pub work_kind: crate::contract::WorkKind,
    pub candidate_id: String,
    pub contract_hash: String,
    pub task: String,
    pub obligation_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CandidateResponse {
    pub candidate_id: String,
    pub response_hash: String,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Adversary,
    Verifier,
    /// Pixel-level taste gate evidence; issued only for visual contracts.
    Visual,
}

impl EvidenceKind {
    fn label(self) -> &'static str {
        match self {
            EvidenceKind::Adversary => "adversary",
            EvidenceKind::Verifier => "verifier",
            EvidenceKind::Visual => "visual",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostEvidenceRequest {
    pub request_id: String,
    pub candidate_id: String,
    pub contract_hash: String,
    pub kind: EvidenceKind,
    pub obligation_ids: Vec<String>,
    pub candidate_response_hash: String,
}

/// Request id carried by daemon-executed verifier records: proves the
/// outcomes were produced by the daemon running the contract, never by a
/// host claiming "proven".
pub const DAEMON_VERIFIER_REQUEST: &str = "daemon-verifier";

/// Request id carried by daemon-executed adversary records: proves the
/// scan was produced by the daemon walking the candidate tree, never by a
/// host claiming "clean".
pub const DAEMON_ADVERSARY_REQUEST: &str = "daemon-adversary";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerifierEvidence {
    pub request_id: String,
    pub candidate_id: String,
    pub response_hash: String,
    pub content: String,
}

/// Visual evidence: content is a JSON crate::taste::TasteGateReport.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VisualEvidence {
    pub request_id: String,
    pub candidate_id: String,
    pub response_hash: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaemonAdversaryRecord {
    pub request_id: String,
    pub response_hash: String,
    pub report: crate::daemon_adversary::DaemonAdversaryReport,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerifierEvidenceRecord {
    pub request_id: String,
    pub response_hash: String,
    /// obligation id -> proven by fresh host execution
    pub outcomes: BTreeMap<String, bool>,
    pub all_proven: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VisualEvidenceRecord {
    pub request_id: String,
    pub response_hash: String,
    pub report: crate::taste::TasteGateReport,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CandidateEvidence {
    #[serde(default)]
    pub daemon_adversary: Option<DaemonAdversaryRecord>,
    pub verifier: Option<VerifierEvidenceRecord>,
    #[serde(default)]
    pub visual: Option<VisualEvidenceRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ExternalAdapterState {
    contract: AcceptanceContract,
    kernel: KernelSnapshot,
    minimum_candidates: usize,
    host_attached: bool,
    responses: BTreeMap<String, CandidateResponse>,
    #[serde(default)]
    evidence: BTreeMap<String, CandidateEvidence>,
    /// The candidate whose evidence passed every gate, recorded at finalize.
    #[serde(default)]
    qualified_candidate: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdapterError {
    InvalidContract,
    InvalidMinimum,
    HostRequired,
    DuplicateCandidate,
    DuplicateResponse,
    UnknownCandidate,
    EvidenceStageNotOpen,
    UnknownEvidenceRequest,
    DuplicateEvidence,
    InvalidEvidence(String),
    Io(String),
    Encoding(String),
    Kernel(KernelError),
}

pub struct ExternalHostAdapter {
    path: Option<PathBuf>,
    state: ExternalAdapterState,
}

impl ExternalHostAdapter {
    pub fn new(
        contract: AcceptanceContract,
        minimum_candidates: usize,
    ) -> Result<Self, AdapterError> {
        if contract.obligations.is_empty() {
            return Err(AdapterError::InvalidContract);
        }
        if minimum_candidates == 0 {
            return Err(AdapterError::InvalidMinimum);
        }
        let task_id =
            canonical_hash(&contract).map_err(|e| AdapterError::Encoding(e.to_string()))?;
        Ok(Self {
            path: None,
            state: ExternalAdapterState {
                contract,
                kernel: KernelSnapshot {
                    task_id,
                    state: KernelState::New,
                    step: 0,
                    lease_epoch: 0,
                    transcript_hash: "genesis".into(),
                },
                minimum_candidates,
                host_attached: false,
                responses: BTreeMap::new(),
                evidence: BTreeMap::new(),
                qualified_candidate: None,
            },
        })
    }

    pub fn open(path: impl Into<PathBuf>) -> Result<Self, AdapterError> {
        let path = path.into();
        let state = serde_json::from_slice(&fs::read(&path).map_err(io_error)?)
            .map_err(|e| AdapterError::Encoding(e.to_string()))?;
        Ok(Self {
            path: Some(path),
            state,
        })
    }

    pub fn save_as(&mut self, path: impl Into<PathBuf>) -> Result<(), AdapterError> {
        self.path = Some(path.into());
        self.persist()
    }

    pub fn status(&self) -> HostKernelStatus {
        if !self.state.host_attached {
            HostKernelStatus::HostRequired
        } else if self.state.responses.len() < self.state.minimum_candidates {
            HostKernelStatus::Collecting
        } else {
            HostKernelStatus::Ready
        }
    }

    pub fn kernel(&self) -> &KernelSnapshot {
        &self.state.kernel
    }

    /// The acceptance contract frozen at kernel creation.
    pub fn contract(&self) -> &AcceptanceContract {
        &self.state.contract
    }

    pub fn requests(&self) -> Result<Vec<HostCandidateRequest>, AdapterError> {
        let contract_hash = canonical_hash(&self.state.contract)
            .map_err(|e| AdapterError::Encoding(e.to_string()))?;
        let obligation_ids: Vec<String> = self
            .state
            .contract
            .obligations
            .iter()
            .map(|o| o.id.clone())
            .collect();
        (0..self.state.minimum_candidates)
            .map(|index| {
                let candidate_id = canonical_hash(&(contract_hash.as_str(), index))
                    .map_err(|e| AdapterError::Encoding(e.to_string()))?;
                Ok(HostCandidateRequest {
                    work_kind: self.state.contract.work_kind,
                    candidate_id,
                    contract_hash: contract_hash.clone(),
                    task: self.state.contract.task.clone(),
                    obligation_ids: obligation_ids.clone(),
                })
            })
            .collect()
    }

    pub fn attach_host(&mut self, lease_epoch: u64) -> Result<(), AdapterError> {
        if self.state.host_attached {
            return Ok(());
        }
        self.state.kernel = transition(
            self.state.kernel.clone(),
            KernelEvent::Acquire { lease_epoch },
        )
        .map_err(AdapterError::Kernel)?;
        self.state.kernel = transition(
            self.state.kernel.clone(),
            KernelEvent::Begin {
                action_id: "candidate-generation".into(),
            },
        )
        .map_err(AdapterError::Kernel)?;
        self.state.host_attached = true;
        self.persist()
    }

    pub fn record_response(&mut self, response: CandidateResponse) -> Result<(), AdapterError> {
        if !self.state.host_attached {
            return Err(AdapterError::HostRequired);
        }
        let requests = self.requests()?;
        if !requests
            .iter()
            .any(|request| request.candidate_id == response.candidate_id)
        {
            return Err(AdapterError::UnknownCandidate);
        }
        if self.state.responses.contains_key(&response.candidate_id) {
            return Err(AdapterError::DuplicateCandidate);
        }
        if self
            .state
            .responses
            .values()
            .any(|existing| existing.response_hash == response.response_hash)
        {
            return Err(AdapterError::DuplicateResponse);
        }
        let expected_hash =
            canonical_hash(&response.content).map_err(|e| AdapterError::Encoding(e.to_string()))?;
        if expected_hash != response.response_hash {
            return Err(AdapterError::Encoding("response hash mismatch".into()));
        }
        self.state
            .responses
            .insert(response.candidate_id.clone(), response);
        self.persist()
    }

    pub fn responses(&self) -> impl Iterator<Item = &CandidateResponse> {
        self.state.responses.values()
    }

    pub fn advance(&mut self) -> Result<HostKernelStatus, AdapterError> {
        if !self.state.host_attached {
            return Err(AdapterError::HostRequired);
        }
        if self.state.responses.len() < self.state.minimum_candidates {
            return Ok(HostKernelStatus::Collecting);
        }
        self.state.kernel = transition(
            self.state.kernel.clone(),
            KernelEvent::SubmitEvidence {
                digest: canonical_hash(&self.state.responses)
                    .map_err(|e| AdapterError::Encoding(e.to_string()))?,
            },
        )
        .map_err(AdapterError::Kernel)?;
        self.persist()?;
        Ok(HostKernelStatus::Ready)
    }

    /// Deterministic adversary and verifier requests for every collected
    /// candidate. Only issued once the candidate stage has advanced; before
    /// that the truthful answer is an empty list.
    pub fn evidence_requests(&self) -> Result<Vec<HostEvidenceRequest>, AdapterError> {
        if self.state.kernel.state != KernelState::AwaitingEvidence {
            return Ok(Vec::new());
        }
        let contract_hash = canonical_hash(&self.state.contract)
            .map_err(|e| AdapterError::Encoding(e.to_string()))?;
        let obligation_ids: Vec<String> = self
            .state
            .contract
            .obligations
            .iter()
            .map(|o| o.id.clone())
            .collect();
        // The verifier and the adversary are executed by the daemon inside
        // each candidate workspace; hosts are only ever asked for (visual)
        // pixel inputs, never for verdicts on their own work.
        let mut kinds = Vec::new();
        if self.state.contract.work_kind == crate::contract::WorkKind::Visual {
            kinds.push(EvidenceKind::Visual);
        }
        let mut requests = Vec::new();
        for (candidate_id, response) in &self.state.responses {
            for kind in kinds.iter().copied() {
                let request_id =
                    canonical_hash(&(contract_hash.as_str(), candidate_id.as_str(), kind.label()))
                        .map_err(|e| AdapterError::Encoding(e.to_string()))?;
                requests.push(HostEvidenceRequest {
                    request_id,
                    candidate_id: candidate_id.clone(),
                    contract_hash: contract_hash.clone(),
                    kind,
                    obligation_ids: obligation_ids.clone(),
                    candidate_response_hash: response.response_hash.clone(),
                });
            }
        }
        Ok(requests)
    }

    fn expect_evidence_request(
        &self,
        kind: EvidenceKind,
        candidate_id: &str,
    ) -> Result<String, AdapterError> {
        if !self.state.host_attached || self.state.kernel.state != KernelState::AwaitingEvidence {
            return Err(AdapterError::EvidenceStageNotOpen);
        }
        if !self.state.responses.contains_key(candidate_id) {
            return Err(AdapterError::UnknownCandidate);
        }
        let contract_hash = canonical_hash(&self.state.contract)
            .map_err(|e| AdapterError::Encoding(e.to_string()))?;
        canonical_hash(&(contract_hash.as_str(), candidate_id, kind.label()))
            .map_err(|e| AdapterError::Encoding(e.to_string()))
    }

    /// Record the daemon's own adversary scan for one candidate. The
    /// daemon walks the sealed candidate tree itself; host-submitted
    /// adversary verdicts do not exist on this path.
    pub fn record_daemon_adversary(
        &mut self,
        candidate_id: &str,
        report: crate::daemon_adversary::DaemonAdversaryReport,
        receipts_hash: String,
    ) -> Result<(), AdapterError> {
        if !self.state.host_attached || self.state.kernel.state != KernelState::AwaitingEvidence {
            return Err(AdapterError::EvidenceStageNotOpen);
        }
        if !self.state.responses.contains_key(candidate_id) {
            return Err(AdapterError::UnknownCandidate);
        }
        if self
            .state
            .evidence
            .get(candidate_id)
            .and_then(|e| e.daemon_adversary.as_ref())
            .is_some()
        {
            return Err(AdapterError::DuplicateEvidence);
        }
        self.state
            .evidence
            .entry(candidate_id.to_string())
            .or_default()
            .daemon_adversary = Some(DaemonAdversaryRecord {
            request_id: DAEMON_ADVERSARY_REQUEST.into(),
            response_hash: receipts_hash,
            report,
        });
        self.persist()
    }

    /// Record the daemon's own verifier execution for one candidate. The
    /// daemon runs every contract proof inside the candidate workspace and
    /// records the outcomes itself; host-submitted verifier verdicts do not
    /// exist on this path.
    pub fn record_daemon_verifier(
        &mut self,
        candidate_id: &str,
        outcomes: BTreeMap<String, bool>,
        receipts_hash: String,
    ) -> Result<(), AdapterError> {
        if !self.state.host_attached || self.state.kernel.state != KernelState::AwaitingEvidence {
            return Err(AdapterError::EvidenceStageNotOpen);
        }
        if !self.state.responses.contains_key(candidate_id) {
            return Err(AdapterError::UnknownCandidate);
        }
        if self
            .state
            .evidence
            .get(candidate_id)
            .and_then(|e| e.verifier.as_ref())
            .is_some()
        {
            return Err(AdapterError::DuplicateEvidence);
        }
        let all_proven = !self.state.contract.obligations.is_empty()
            && self
                .state
                .contract
                .obligations
                .iter()
                .all(|o| outcomes.get(&o.id) == Some(&true));
        self.state
            .evidence
            .entry(candidate_id.to_string())
            .or_default()
            .verifier = Some(VerifierEvidenceRecord {
            request_id: DAEMON_VERIFIER_REQUEST.into(),
            response_hash: receipts_hash,
            outcomes,
            all_proven,
        });
        self.persist()
    }

    /// Record one visual evidence item. Only visual contracts ever issue
    /// visual requests; the report must pass the taste gate floor and its
    /// thesis must be distinct from every other candidate's recorded thesis.
    pub fn record_visual(&mut self, evidence: VisualEvidence) -> Result<(), AdapterError> {
        if self.state.contract.work_kind != crate::contract::WorkKind::Visual {
            return Err(AdapterError::InvalidEvidence(
                "visual evidence submitted for a non-visual contract".into(),
            ));
        }
        let expected =
            self.expect_evidence_request(EvidenceKind::Visual, &evidence.candidate_id)?;
        if evidence.request_id != expected {
            return Err(AdapterError::UnknownEvidenceRequest);
        }
        if self
            .state
            .evidence
            .get(&evidence.candidate_id)
            .and_then(|e| e.visual.as_ref())
            .is_some()
        {
            return Err(AdapterError::DuplicateEvidence);
        }
        let expected_hash =
            canonical_hash(&evidence.content).map_err(|e| AdapterError::Encoding(e.to_string()))?;
        if expected_hash != evidence.response_hash {
            return Err(AdapterError::Encoding("response hash mismatch".into()));
        }
        let report: crate::taste::TasteGateReport = serde_json::from_str(&evidence.content)
            .map_err(|e| AdapterError::InvalidEvidence(e.to_string()))?;
        crate::taste::validate_taste_gate(&report)
            .map_err(|errors| AdapterError::InvalidEvidence(errors.join("; ")))?;
        let thesis_taken = self.state.evidence.values().any(|existing| {
            existing
                .visual
                .as_ref()
                .map(|v| v.report.thesis_id == report.thesis_id)
                .unwrap_or(false)
        });
        if thesis_taken {
            return Err(AdapterError::InvalidEvidence(
                "thesis already claimed by another candidate".into(),
            ));
        }
        self.state
            .evidence
            .entry(evidence.candidate_id.clone())
            .or_default()
            .visual = Some(VisualEvidenceRecord {
            request_id: evidence.request_id,
            response_hash: evidence.response_hash,
            report,
        });
        self.persist()
    }

    pub fn candidate_evidence(&self, candidate_id: &str) -> Option<&CandidateEvidence> {
        self.state.evidence.get(candidate_id)
    }

    /// The candidate recorded as qualifying at finalize, if any.
    pub fn qualified_candidate(&self) -> Option<&str> {
        self.state.qualified_candidate.as_deref()
    }

    /// Evidence-gated transition. A candidate qualifies only when its
    /// adversary report is conclusive and clean and its verifier outcomes
    /// prove every contract obligation. Any qualified candidate completes
    /// the kernel; when every candidate is fully evidenced and none
    /// qualifies the kernel fails closed. Anything else stays truthfully
    /// pending in AwaitingEvidence.
    pub fn finalize(&mut self) -> Result<KernelState, AdapterError> {
        if !self.state.host_attached {
            return Err(AdapterError::HostRequired);
        }
        if self.state.kernel.state != KernelState::AwaitingEvidence {
            return Ok(self.state.kernel.state);
        }
        let mut qualified: Option<String> = None;
        let mut fully_evidenced = true;
        for candidate_id in self.state.responses.keys() {
            let evidence = self.state.evidence.get(candidate_id);
            let adversary_clean = evidence
                .and_then(|e| e.daemon_adversary.as_ref())
                .map(|a| a.request_id == DAEMON_ADVERSARY_REQUEST && a.report.clean())
                .unwrap_or(false);
            let verifier_proven = evidence
                .and_then(|e| e.verifier.as_ref())
                .map(|v| v.all_proven && v.request_id == DAEMON_VERIFIER_REQUEST)
                .unwrap_or(false);
            let visual_ok = self.state.contract.work_kind != crate::contract::WorkKind::Visual
                || evidence.and_then(|e| e.visual.as_ref()).is_some();
            if adversary_clean && verifier_proven && visual_ok && qualified.is_none() {
                qualified = Some(candidate_id.clone());
            }
            let complete_record = evidence
                .map(|e| {
                    e.daemon_adversary.is_some()
                        && e.verifier.is_some()
                        && (self.state.contract.work_kind != crate::contract::WorkKind::Visual
                            || e.visual.is_some())
                })
                .unwrap_or(false);
            if !complete_record {
                fully_evidenced = false;
            }
        }
        if let Some(candidate_id) = qualified {
            self.state.qualified_candidate = Some(candidate_id);
            self.state.kernel = transition(self.state.kernel.clone(), KernelEvent::Complete)
                .map_err(AdapterError::Kernel)?;
            self.persist()?;
        } else if fully_evidenced && !self.state.responses.is_empty() {
            self.state.kernel = transition(
                self.state.kernel.clone(),
                KernelEvent::Fail {
                    reason: "every candidate rejected by adversary or verifier evidence".into(),
                },
            )
            .map_err(AdapterError::Kernel)?;
            self.persist()?;
        }
        Ok(self.state.kernel.state)
    }

    fn persist(&self) -> Result<(), AdapterError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let temporary = path.with_extension("json.tmp");
        let bytes =
            serde_json::to_vec(&self.state).map_err(|e| AdapterError::Encoding(e.to_string()))?;
        fs::write(&temporary, bytes).map_err(io_error)?;
        fs::rename(&temporary, path).map_err(io_error)
    }
}

fn io_error(error: std::io::Error) -> AdapterError {
    AdapterError::Io(error.to_string())
}

pub fn transition(
    mut snapshot: KernelSnapshot,
    event: KernelEvent,
) -> Result<KernelSnapshot, KernelError> {
    let next = match (&snapshot.state, &event) {
        (KernelState::New, KernelEvent::Acquire { lease_epoch: 1 }) => KernelState::Leased,
        (KernelState::Leased, KernelEvent::Begin { action_id }) if !action_id.trim().is_empty() => {
            KernelState::Running
        }
        (KernelState::Running, KernelEvent::SubmitEvidence { digest })
            if !digest.trim().is_empty() =>
        {
            KernelState::AwaitingEvidence
        }
        (KernelState::AwaitingEvidence, KernelEvent::Complete) => KernelState::Completed,
        (
            KernelState::Leased | KernelState::Running | KernelState::AwaitingEvidence,
            KernelEvent::Fail { reason },
        ) if !reason.trim().is_empty() => KernelState::Failed,
        (
            KernelState::New
            | KernelState::Leased
            | KernelState::Running
            | KernelState::AwaitingEvidence,
            KernelEvent::Revoke,
        ) => KernelState::Revoked,
        (KernelState::New, KernelEvent::Acquire { .. }) => {
            return Err(KernelError::NonMonotonicLease)
        }
        _ => {
            return Err(KernelError::InvalidTransition {
                state: snapshot.state,
                event: format!("{event:?}"),
            })
        }
    };
    if let KernelEvent::Acquire { lease_epoch } = event {
        snapshot.lease_epoch = lease_epoch;
    }
    snapshot.step = snapshot.step.saturating_add(1);
    snapshot.state = next;
    snapshot.transcript_hash = canonical_hash(&(&snapshot.step, &event, &snapshot.transcript_hash))
        .map_err(|_| KernelError::EmptyValue)?;
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{Obligation, Proof};
    use tempfile::tempdir;

    fn initial() -> KernelSnapshot {
        KernelSnapshot {
            task_id: "task".into(),
            state: KernelState::New,
            step: 0,
            lease_epoch: 0,
            transcript_hash: "genesis".into(),
        }
    }

    #[test]
    fn external_host_sequence_is_deterministic() {
        let events = [
            KernelEvent::Acquire { lease_epoch: 1 },
            KernelEvent::Begin {
                action_id: "a".into(),
            },
            KernelEvent::SubmitEvidence {
                digest: "evidence".into(),
            },
            KernelEvent::Complete,
        ];
        let left = events
            .iter()
            .cloned()
            .try_fold(initial(), transition)
            .unwrap();
        let right = events
            .iter()
            .cloned()
            .try_fold(initial(), transition)
            .unwrap();
        assert_eq!(left, right);
        assert_eq!(left.state, KernelState::Completed);
    }

    #[test]
    fn managed_fallback_and_illegal_order_are_impossible() {
        assert!(transition(initial(), KernelEvent::Complete).is_err());
        assert!(transition(initial(), KernelEvent::Acquire { lease_epoch: 2 }).is_err());
    }

    fn contract() -> AcceptanceContract {
        AcceptanceContract {
            task: "build it".into(),
            obligations: vec![Obligation {
                id: "builds".into(),
                statement: "it builds".into(),
                proof: Proof::BehaviorEvidence {
                    description: "host evidence".into(),
                },
            }],
            forbidden_regressions: vec![],
            work_kind: crate::contract::WorkKind::General,
        }
    }

    #[test]
    fn no_host_stall_is_truthful_and_provider_free() {
        let adapter = ExternalHostAdapter::new(contract(), 2).unwrap();
        assert_eq!(adapter.status(), HostKernelStatus::HostRequired);
        assert_eq!(adapter.kernel().state, KernelState::New);
        assert_eq!(adapter.requests().unwrap().len(), 2);
    }

    #[test]
    fn candidates_are_independent_and_duplicates_rejected() {
        let mut adapter = ExternalHostAdapter::new(contract(), 2).unwrap();
        let requests = adapter.requests().unwrap();
        assert_ne!(requests[0].candidate_id, requests[1].candidate_id);
        assert_eq!(
            adapter.record_response(CandidateResponse {
                candidate_id: requests[0].candidate_id.clone(),
                response_hash: canonical_hash(&"left").unwrap(),
                content: "left".into()
            }),
            Err(AdapterError::HostRequired)
        );
        adapter.attach_host(1).unwrap();
        adapter
            .record_response(CandidateResponse {
                candidate_id: requests[0].candidate_id.clone(),
                response_hash: canonical_hash(&"left").unwrap(),
                content: "left".into(),
            })
            .unwrap();
        assert_eq!(
            adapter.record_response(CandidateResponse {
                candidate_id: requests[0].candidate_id.clone(),
                response_hash: canonical_hash(&"other").unwrap(),
                content: "other".into()
            }),
            Err(AdapterError::DuplicateCandidate)
        );
        assert_eq!(
            adapter.record_response(CandidateResponse {
                candidate_id: requests[1].candidate_id.clone(),
                response_hash: canonical_hash(&"left").unwrap(),
                content: "left".into()
            }),
            Err(AdapterError::DuplicateResponse)
        );
    }

    #[test]
    fn deterministic_replay_and_restart_preserve_candidates() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("external.json");
        let mut adapter = ExternalHostAdapter::new(contract(), 2).unwrap();
        adapter.save_as(&path).unwrap();
        let requests = adapter.requests().unwrap();
        adapter.attach_host(1).unwrap();
        for (index, request) in requests.iter().enumerate() {
            let content = format!("candidate-{index}");
            adapter
                .record_response(CandidateResponse {
                    candidate_id: request.candidate_id.clone(),
                    response_hash: canonical_hash(&content).unwrap(),
                    content,
                })
                .unwrap();
        }
        assert_eq!(adapter.advance().unwrap(), HostKernelStatus::Ready);
        let expected = adapter.kernel().clone();
        let recovered = ExternalHostAdapter::open(&path).unwrap();
        assert_eq!(recovered.kernel(), &expected);
        assert_eq!(recovered.status(), HostKernelStatus::Ready);
        assert_eq!(recovered.requests().unwrap(), requests);
        assert_eq!(recovered.responses().count(), 2);
    }
    fn ready_adapter() -> (ExternalHostAdapter, Vec<HostCandidateRequest>) {
        let mut adapter = ExternalHostAdapter::new(contract(), 2).unwrap();
        adapter.attach_host(1).unwrap();
        let requests = adapter.requests().unwrap();
        for (index, request) in requests.iter().enumerate() {
            let content = format!("candidate-{index}");
            adapter
                .record_response(CandidateResponse {
                    candidate_id: request.candidate_id.clone(),
                    response_hash: canonical_hash(&content).unwrap(),
                    content,
                })
                .unwrap();
        }
        assert_eq!(adapter.advance().unwrap(), HostKernelStatus::Ready);
        (adapter, requests)
    }


    fn clean_adversary_report() -> crate::daemon_adversary::DaemonAdversaryReport {
        crate::daemon_adversary::DaemonAdversaryReport {
            defects: Vec::new(),
            scanned_files: 1,
            scanned_bytes: 1,
            tree_hash: "tree".into(),
            truncated: false,
        }
    }

    /// The daemon's own adversary scan, recorded without any host verdict.
    fn daemon_adversary(adapter: &mut ExternalHostAdapter, candidate_id: &str, clean: bool) {
        let mut report = clean_adversary_report();
        if !clean {
            report.defects.push(crate::adversary::Defect {
                title: "placeholder content".into(),
                detail: "index.html contains \"lorem ipsum\"".into(),
            });
        }
        adapter
            .record_daemon_adversary(candidate_id, report, canonical_hash(&"daemon-scan").unwrap())
            .unwrap();
    }

    /// The daemon's own verifier run: outcomes it produced by executing the
    /// contract, recorded without any host verdict.
    fn daemon_verify(adapter: &mut ExternalHostAdapter, candidate_id: &str, proven: bool) {
        let outcomes = std::collections::BTreeMap::from([("builds".to_string(), proven)]);
        adapter
            .record_daemon_verifier(candidate_id, outcomes, canonical_hash(&proven).unwrap())
            .unwrap();
    }

    #[test]
    fn evidence_requests_are_deterministic_per_candidate_and_kind() {
        let (adapter, _) = ready_adapter();
        // General contracts issue no host evidence requests at all: the
        // daemon executes the verifier and the adversary itself.
        assert!(adapter.evidence_requests().unwrap().is_empty());
    }

    #[test]
    fn evidence_stage_opens_only_after_candidates_advance() {
        let mut adapter = ExternalHostAdapter::new(contract(), 1).unwrap();
        adapter.attach_host(1).unwrap();
        assert!(adapter.evidence_requests().unwrap().is_empty());
        let request = adapter.requests().unwrap().remove(0);
        let content = "only".to_string();
        adapter
            .record_response(CandidateResponse {
                candidate_id: request.candidate_id.clone(),
                response_hash: canonical_hash(&content).unwrap(),
                content,
            })
            .unwrap();
        assert_eq!(
            adapter.record_daemon_adversary(
                &request.candidate_id,
                clean_adversary_report(),
                "receipts".into()
            ),
            Err(AdapterError::EvidenceStageNotOpen)
        );
    }

    #[test]
    fn adversary_and_verifier_evidence_gates_completion() {
        let (mut adapter, _) = ready_adapter();
        let candidate = adapter.responses().next().unwrap().candidate_id.clone();
        // The daemon's scan is the only adversary evidence that exists.
        assert_eq!(
            adapter.record_daemon_adversary("nobody", clean_adversary_report(), "r".into()),
            Err(AdapterError::UnknownCandidate)
        );
        daemon_adversary(&mut adapter, &candidate, true);
        assert_eq!(
            adapter.record_daemon_adversary(&candidate, clean_adversary_report(), "r".into()),
            Err(AdapterError::DuplicateEvidence)
        );
        // A clean adversary scan alone does not qualify.
        assert!(matches!(
            adapter.finalize().unwrap(),
            KernelState::AwaitingEvidence
        ));

        daemon_verify(&mut adapter, &candidate, true);
        assert_eq!(
            adapter.record_daemon_verifier(
                &candidate,
                std::collections::BTreeMap::from([("builds".to_string(), true)]),
                "receipts".into()
            ),
            Err(AdapterError::DuplicateEvidence)
        );

        assert_eq!(adapter.finalize().unwrap(), KernelState::Completed);
        assert_eq!(adapter.kernel().state, KernelState::Completed);
    }

    #[test]
    fn unproven_verifier_blocks_completion_until_every_candidate_fails() {
        let (mut adapter, _) = ready_adapter();
        let candidates: Vec<String> = adapter.responses().map(|r| r.candidate_id.clone()).collect();
        for candidate in candidates {
            daemon_adversary(&mut adapter, &candidate, true);
            daemon_verify(&mut adapter, &candidate, false);
        }
        assert_eq!(adapter.finalize().unwrap(), KernelState::Failed);
        assert_eq!(adapter.kernel().state, KernelState::Failed);
    }

    #[test]
    fn standing_defects_fail_closed_and_truncated_never_cleans() {
        let (mut adapter, _) = ready_adapter();
        let mut candidates = adapter.responses().map(|r| r.candidate_id.clone());
        let first = candidates.next().unwrap();
        let second = candidates.next().unwrap();
        drop(candidates);
        // A scan with standing defects blocks qualification even with a
        // proven verifier.
        daemon_adversary(&mut adapter, &first, false);
        daemon_verify(&mut adapter, &first, true);
        let record = adapter
            .candidate_evidence(&first)
            .unwrap()
            .daemon_adversary
            .as_ref()
            .unwrap();
        assert_eq!(record.report.defects.len(), 1);
        // A truncated scan is never clean, so the second candidate cannot
        // qualify either; fully evidenced with no qualifier fails closed.
        let mut truncated = clean_adversary_report();
        truncated.truncated = true;
        adapter
            .record_daemon_adversary(&second, truncated, "r".into())
            .unwrap();
        daemon_verify(&mut adapter, &second, true);
        assert_eq!(adapter.finalize().unwrap(), KernelState::Failed);
    }

    #[test]
    fn evidence_stage_survives_restart_replay() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("external.json");
        let mut adapter = ExternalHostAdapter::new(contract(), 1).unwrap();
        adapter.save_as(&path).unwrap();
        adapter.attach_host(1).unwrap();
        let request = adapter.requests().unwrap().remove(0);
        let content = "candidate-0".to_string();
        adapter
            .record_response(CandidateResponse {
                candidate_id: request.candidate_id.clone(),
                response_hash: canonical_hash(&content).unwrap(),
                content,
            })
            .unwrap();
        assert_eq!(adapter.advance().unwrap(), HostKernelStatus::Ready);
        let requests = adapter.evidence_requests().unwrap();
        daemon_adversary(&mut adapter, &request.candidate_id, true);

        let mut recovered = ExternalHostAdapter::open(&path).unwrap();
        assert_eq!(recovered.evidence_requests().unwrap(), requests);
        daemon_verify(&mut recovered, &request.candidate_id, true);
        assert_eq!(recovered.finalize().unwrap(), KernelState::Completed);
        let reloaded = ExternalHostAdapter::open(&path).unwrap();
        assert_eq!(reloaded.kernel().state, KernelState::Completed);
    }
    fn visual_contract() -> AcceptanceContract {
        let mut contract = contract();
        contract.work_kind = crate::contract::WorkKind::Visual;
        contract
    }

    fn ready_visual_adapter() -> (ExternalHostAdapter, Vec<HostCandidateRequest>) {
        let mut adapter = ExternalHostAdapter::new(visual_contract(), 2).unwrap();
        adapter.attach_host(1).unwrap();
        let requests = adapter.requests().unwrap();
        for (index, request) in requests.iter().enumerate() {
            let content = format!("candidate-{index}");
            adapter
                .record_response(CandidateResponse {
                    candidate_id: request.candidate_id.clone(),
                    response_hash: canonical_hash(&content).unwrap(),
                    content,
                })
                .unwrap();
        }
        assert_eq!(adapter.advance().unwrap(), HostKernelStatus::Ready);
        (adapter, requests)
    }

    fn visual_report(thesis: &str) -> String {
        let report = crate::taste::TasteGateReport {
            thesis_id: thesis.into(),
            screenshots: vec![
                crate::taste::ScreenshotEvidence {
                    viewport: crate::taste::ViewportClass::Desktop,
                    artifact_hash: "a".repeat(64),
                },
                crate::taste::ScreenshotEvidence {
                    viewport: crate::taste::ViewportClass::Phone,
                    artifact_hash: "b".repeat(64),
                },
            ],
            interaction_replay_hash: "c".repeat(64),
            forbidden_patterns_hit: Vec::new(),
            critic_clean: true,
        };
        serde_json::to_string(&report).unwrap()
    }

    #[test]
    fn visual_contracts_issue_visual_evidence_requests() {
        let (adapter, _) = ready_visual_adapter();
        let requests = adapter.evidence_requests().unwrap();
        // Visual contracts issue exactly one visual request per candidate;
        // adversary and verifier are daemon-executed, never requested.
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.kind == EvidenceKind::Visual)
                .count(),
            2
        );
        let ids: std::collections::BTreeSet<&str> =
            requests.iter().map(|r| r.request_id.as_str()).collect();
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn visual_evidence_is_mandatory_for_visual_completion() {
        let (mut adapter, _) = ready_visual_adapter();
        let requests = adapter.evidence_requests().unwrap();
        let candidate = requests[0].candidate_id.clone();
        let visual = requests
            .iter()
            .find(|r| r.candidate_id == candidate && r.kind == EvidenceKind::Visual)
            .unwrap()
            .clone();
        daemon_adversary(&mut adapter, &candidate, true);
        daemon_verify(&mut adapter, &candidate, true);
        // clean adversary + proven verifier is not enough for a visual task
        assert!(matches!(
            adapter.finalize().unwrap(),
            KernelState::AwaitingEvidence
        ));
        let mut bad: crate::taste::TasteGateReport =
            serde_json::from_str(&visual_report("thesis-a")).unwrap();
        bad.screenshots
            .retain(|s| s.viewport == crate::taste::ViewportClass::Desktop);
        let bad_content = serde_json::to_string(&bad).unwrap();
        assert!(matches!(
            adapter.record_visual(VisualEvidence {
                request_id: visual.request_id.clone(),
                candidate_id: candidate.clone(),
                response_hash: canonical_hash(&bad_content).unwrap(),
                content: bad_content,
            }),
            Err(AdapterError::InvalidEvidence(_))
        ));
        let good = visual_report("thesis-a");
        adapter
            .record_visual(VisualEvidence {
                request_id: visual.request_id.clone(),
                candidate_id: candidate.clone(),
                response_hash: canonical_hash(&good).unwrap(),
                content: good,
            })
            .unwrap();
        assert!(matches!(
            adapter.finalize().unwrap(),
            KernelState::Completed
        ));
    }

    #[test]
    fn visual_evidence_rejects_forbidden_hits_dirty_critics_and_shared_theses() {
        let (mut adapter, _) = ready_visual_adapter();
        let requests = adapter.evidence_requests().unwrap();
        let candidate = requests[0].candidate_id.clone();
        let visual = requests
            .iter()
            .find(|r| r.candidate_id == candidate && r.kind == EvidenceKind::Visual)
            .unwrap()
            .clone();
        let mut hit: crate::taste::TasteGateReport =
            serde_json::from_str(&visual_report("thesis-a")).unwrap();
        hit.forbidden_patterns_hit = vec!["generic-hero-plus-cards".into()];
        let hit_content = serde_json::to_string(&hit).unwrap();
        assert!(matches!(
            adapter.record_visual(VisualEvidence {
                request_id: visual.request_id.clone(),
                candidate_id: candidate.clone(),
                response_hash: canonical_hash(&hit_content).unwrap(),
                content: hit_content,
            }),
            Err(AdapterError::InvalidEvidence(_))
        ));
        let mut dirty: crate::taste::TasteGateReport =
            serde_json::from_str(&visual_report("thesis-a")).unwrap();
        dirty.critic_clean = false;
        let dirty_content = serde_json::to_string(&dirty).unwrap();
        assert!(matches!(
            adapter.record_visual(VisualEvidence {
                request_id: visual.request_id.clone(),
                candidate_id: candidate.clone(),
                response_hash: canonical_hash(&dirty_content).unwrap(),
                content: dirty_content,
            }),
            Err(AdapterError::InvalidEvidence(_))
        ));
        let good = visual_report("thesis-a");
        adapter
            .record_visual(VisualEvidence {
                request_id: visual.request_id.clone(),
                candidate_id: candidate.clone(),
                response_hash: canonical_hash(&good).unwrap(),
                content: good,
            })
            .unwrap();
        let other = requests
            .iter()
            .find(|r| r.candidate_id != candidate && r.kind == EvidenceKind::Visual)
            .unwrap()
            .clone();
        let shared = visual_report("thesis-a");
        assert!(matches!(
            adapter.record_visual(VisualEvidence {
                request_id: other.request_id,
                candidate_id: other.candidate_id,
                response_hash: canonical_hash(&shared).unwrap(),
                content: shared,
            }),
            Err(AdapterError::InvalidEvidence(_))
        ));
    }

    #[test]
    fn general_contracts_reject_visual_evidence() {
        let (mut adapter, _) = ready_adapter();
        assert!(adapter.evidence_requests().unwrap().is_empty());
        let candidate = adapter.responses().next().unwrap().candidate_id.clone();
        let content = visual_report("thesis-a");
        assert!(matches!(
            adapter.record_visual(VisualEvidence {
                request_id: "anything".into(),
                candidate_id: candidate,
                response_hash: canonical_hash(&content).unwrap(),
                content,
            }),
            Err(AdapterError::InvalidEvidence(_))
        ));
    }
    #[test]
    fn candidate_requests_carry_the_work_kind() {
        let general = ExternalHostAdapter::new(contract(), 1).unwrap();
        assert_eq!(
            general.requests().unwrap()[0].work_kind,
            crate::contract::WorkKind::General
        );
        let visual = ExternalHostAdapter::new(visual_contract(), 1).unwrap();
        assert_eq!(
            visual.requests().unwrap()[0].work_kind,
            crate::contract::WorkKind::Visual
        );
    }
    #[test]
    fn finalize_records_the_qualified_candidate() {
        let (mut adapter, _) = ready_adapter();
        assert_eq!(adapter.qualified_candidate(), None);
        let candidate = adapter.responses().next().unwrap().candidate_id.clone();
        daemon_adversary(&mut adapter, &candidate, true);
        daemon_verify(&mut adapter, &candidate, true);
        assert!(matches!(
            adapter.finalize().unwrap(),
            KernelState::Completed
        ));
        assert_eq!(adapter.qualified_candidate(), Some(candidate.as_str()));
    }
}
