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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ExternalAdapterState {
    contract: AcceptanceContract,
    kernel: KernelSnapshot,
    minimum_candidates: usize,
    host_attached: bool,
    responses: BTreeMap<String, CandidateResponse>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdapterError {
    InvalidContract,
    InvalidMinimum,
    HostRequired,
    DuplicateCandidate,
    DuplicateResponse,
    UnknownCandidate,
    Io(String),
    Encoding(String),
    Kernel(KernelError),
}

pub struct ExternalHostAdapter {
    path: Option<PathBuf>,
    state: ExternalAdapterState,
}

impl ExternalHostAdapter {
    pub fn new(contract: AcceptanceContract, minimum_candidates: usize) -> Result<Self, AdapterError> {
        if contract.obligations.is_empty() { return Err(AdapterError::InvalidContract); }
        if minimum_candidates == 0 { return Err(AdapterError::InvalidMinimum); }
        let task_id = canonical_hash(&contract).map_err(|e| AdapterError::Encoding(e.to_string()))?;
        Ok(Self {
            path: None,
            state: ExternalAdapterState {
                contract, kernel: KernelSnapshot { task_id, state: KernelState::New, step: 0,
                    lease_epoch: 0, transcript_hash: "genesis".into() },
                minimum_candidates, host_attached: false, responses: BTreeMap::new(),
            },
        })
    }

    pub fn open(path: impl Into<PathBuf>) -> Result<Self, AdapterError> {
        let path = path.into();
        let state = serde_json::from_slice(&fs::read(&path).map_err(io_error)?)
            .map_err(|e| AdapterError::Encoding(e.to_string()))?;
        Ok(Self { path: Some(path), state })
    }

    pub fn save_as(&mut self, path: impl Into<PathBuf>) -> Result<(), AdapterError> {
        self.path = Some(path.into());
        self.persist()
    }

    pub fn status(&self) -> HostKernelStatus {
        if !self.state.host_attached { HostKernelStatus::HostRequired }
        else if self.state.responses.len() < self.state.minimum_candidates { HostKernelStatus::Collecting }
        else { HostKernelStatus::Ready }
    }

    pub fn kernel(&self) -> &KernelSnapshot { &self.state.kernel }

    pub fn requests(&self) -> Result<Vec<HostCandidateRequest>, AdapterError> {
        let contract_hash = canonical_hash(&self.state.contract).map_err(|e| AdapterError::Encoding(e.to_string()))?;
        let obligation_ids: Vec<String> = self.state.contract.obligations.iter().map(|o| o.id.clone()).collect();
        (0..self.state.minimum_candidates).map(|index| {
            let candidate_id = canonical_hash(&(contract_hash.as_str(), index))
                .map_err(|e| AdapterError::Encoding(e.to_string()))?;
            Ok(HostCandidateRequest { candidate_id, contract_hash: contract_hash.clone(),
                task: self.state.contract.task.clone(), obligation_ids: obligation_ids.clone() })
        }).collect()
    }

    pub fn attach_host(&mut self, lease_epoch: u64) -> Result<(), AdapterError> {
        if self.state.host_attached { return Ok(()); }
        self.state.kernel = transition(self.state.kernel.clone(), KernelEvent::Acquire { lease_epoch })
            .map_err(AdapterError::Kernel)?;
        self.state.kernel = transition(self.state.kernel.clone(), KernelEvent::Begin { action_id: "candidate-generation".into() })
            .map_err(AdapterError::Kernel)?;
        self.state.host_attached = true;
        self.persist()
    }

    pub fn record_response(&mut self, response: CandidateResponse) -> Result<(), AdapterError> {
        if !self.state.host_attached { return Err(AdapterError::HostRequired); }
        let requests = self.requests()?;
        if !requests.iter().any(|request| request.candidate_id == response.candidate_id) {
            return Err(AdapterError::UnknownCandidate);
        }
        if self.state.responses.contains_key(&response.candidate_id) { return Err(AdapterError::DuplicateCandidate); }
        if self.state.responses.values().any(|existing| existing.response_hash == response.response_hash) {
            return Err(AdapterError::DuplicateResponse);
        }
        let expected_hash = canonical_hash(&response.content).map_err(|e| AdapterError::Encoding(e.to_string()))?;
        if expected_hash != response.response_hash { return Err(AdapterError::Encoding("response hash mismatch".into())); }
        self.state.responses.insert(response.candidate_id.clone(), response);
        self.persist()
    }

    pub fn responses(&self) -> impl Iterator<Item = &CandidateResponse> { self.state.responses.values() }

    pub fn advance(&mut self) -> Result<HostKernelStatus, AdapterError> {
        if !self.state.host_attached { return Err(AdapterError::HostRequired); }
        if self.state.responses.len() < self.state.minimum_candidates { return Ok(HostKernelStatus::Collecting); }
        self.state.kernel = transition(self.state.kernel.clone(), KernelEvent::SubmitEvidence {
            digest: canonical_hash(&self.state.responses).map_err(|e| AdapterError::Encoding(e.to_string()))?,
        }).map_err(AdapterError::Kernel)?;
        self.persist()?;
        Ok(HostKernelStatus::Ready)
    }

    fn persist(&self) -> Result<(), AdapterError> {
        let Some(path) = &self.path else { return Ok(()); };
        let temporary = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec(&self.state).map_err(|e| AdapterError::Encoding(e.to_string()))?;
        fs::write(&temporary, bytes).map_err(io_error)?;
        fs::rename(&temporary, path).map_err(io_error)
    }
}

fn io_error(error: std::io::Error) -> AdapterError { AdapterError::Io(error.to_string()) }

pub fn transition(mut snapshot: KernelSnapshot, event: KernelEvent) -> Result<KernelSnapshot, KernelError> {
    let next = match (&snapshot.state, &event) {
        (KernelState::New, KernelEvent::Acquire { lease_epoch: 1 }) => KernelState::Leased,
        (KernelState::Leased, KernelEvent::Begin { action_id }) if !action_id.trim().is_empty() => KernelState::Running,
        (KernelState::Running, KernelEvent::SubmitEvidence { digest }) if !digest.trim().is_empty() => KernelState::AwaitingEvidence,
        (KernelState::AwaitingEvidence, KernelEvent::Complete) => KernelState::Completed,
        (KernelState::Leased | KernelState::Running | KernelState::AwaitingEvidence, KernelEvent::Fail { reason }) if !reason.trim().is_empty() => KernelState::Failed,
        (KernelState::New | KernelState::Leased | KernelState::Running | KernelState::AwaitingEvidence, KernelEvent::Revoke) => KernelState::Revoked,
        (KernelState::New, KernelEvent::Acquire { .. }) => return Err(KernelError::NonMonotonicLease),
        _ => return Err(KernelError::InvalidTransition { state: snapshot.state, event: format!("{event:?}") }),
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
        KernelSnapshot { task_id: "task".into(), state: KernelState::New, step: 0, lease_epoch: 0, transcript_hash: "genesis".into() }
    }

    #[test]
    fn external_host_sequence_is_deterministic() {
        let events = [
            KernelEvent::Acquire { lease_epoch: 1 },
            KernelEvent::Begin { action_id: "a".into() },
            KernelEvent::SubmitEvidence { digest: "evidence".into() },
            KernelEvent::Complete,
        ];
        let left = events.iter().cloned().try_fold(initial(), transition).unwrap();
        let right = events.iter().cloned().try_fold(initial(), transition).unwrap();
        assert_eq!(left, right);
        assert_eq!(left.state, KernelState::Completed);
    }

    #[test]
    fn managed_fallback_and_illegal_order_are_impossible() {
        assert!(transition(initial(), KernelEvent::Complete).is_err());
        assert!(transition(initial(), KernelEvent::Acquire { lease_epoch: 2 }).is_err());
    }

    fn contract() -> AcceptanceContract {
        AcceptanceContract { task: "build it".into(), obligations: vec![Obligation {
            id: "builds".into(), statement: "it builds".into(), proof: Proof::BehaviorEvidence { description: "host evidence".into() },
        }], forbidden_regressions: vec![] }
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
        assert_eq!(adapter.record_response(CandidateResponse { candidate_id: requests[0].candidate_id.clone(), response_hash: canonical_hash(&"left").unwrap(), content: "left".into() }), Err(AdapterError::HostRequired));
        adapter.attach_host(1).unwrap();
        adapter.record_response(CandidateResponse { candidate_id: requests[0].candidate_id.clone(), response_hash: canonical_hash(&"left").unwrap(), content: "left".into() }).unwrap();
        assert_eq!(adapter.record_response(CandidateResponse { candidate_id: requests[0].candidate_id.clone(), response_hash: canonical_hash(&"other").unwrap(), content: "other".into() }), Err(AdapterError::DuplicateCandidate));
        assert_eq!(adapter.record_response(CandidateResponse { candidate_id: requests[1].candidate_id.clone(), response_hash: canonical_hash(&"left").unwrap(), content: "left".into() }), Err(AdapterError::DuplicateResponse));
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
            adapter.record_response(CandidateResponse { candidate_id: request.candidate_id.clone(), response_hash: canonical_hash(&content).unwrap(), content }).unwrap();
        }
        assert_eq!(adapter.advance().unwrap(), HostKernelStatus::Ready);
        let expected = adapter.kernel().clone();
        let recovered = ExternalHostAdapter::open(&path).unwrap();
        assert_eq!(recovered.kernel(), &expected);
        assert_eq!(recovered.status(), HostKernelStatus::Ready);
        assert_eq!(recovered.requests().unwrap(), requests);
        assert_eq!(recovered.responses().count(), 2);
    }
}