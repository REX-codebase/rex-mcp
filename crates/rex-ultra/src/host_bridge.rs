//! Durable store bridge over Ultra's external-host kernel.
//!
//! One persisted `ExternalHostAdapter` per task, replayable across restarts.
//! Callers stay caller-driven: the host asks for the open requests, submits
//! candidate responses and later adversary/verifier evidence, and the kernel
//! alone decides when the evidence gates a completion or a failure. No
//! provider, no managed inference, no silent fallback - a missing host stays
//! `HostRequired`.

use crate::contract::{AcceptanceContract, Obligation, Proof};
use crate::external_kernel::{
    AdapterError, CandidateResponse, ExternalHostAdapter, HostCandidateRequest, HostEvidenceRequest,
    HostKernelStatus, KernelState,
};
use rex_protocol::PlanStep;
use std::fs;
use std::path::PathBuf;

/// External Ultra never competes fewer theses than the taste floor: a
/// two-candidate race cannot satisfy MIN_DISTINCT_THESES, so the floor is
/// the same constant.
pub const DEFAULT_MINIMUM_CANDIDATES: usize = crate::taste::MIN_DISTINCT_THESES;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeError {
    InvalidTaskId,
    Adapter(AdapterError),
    Promotion(crate::promotion::PromotionError),
    Io(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UltraHostView {
    pub task_id: String,
    pub status: HostKernelStatus,
    pub kernel_state: KernelState,
    pub candidate_requests: Vec<HostCandidateRequest>,
    pub evidence_requests: Vec<HostEvidenceRequest>,
}

/// Deterministic contract for the external kernel: one behavior obligation
/// per frozen plan step, so candidate and verifier requests bind to the same
/// plan the daemon froze at task creation.
pub fn contract_from_plan(task: &str, plan: &[PlanStep]) -> AcceptanceContract {
    let obligations = plan
        .iter()
        .enumerate()
        .map(|(index, step)| Obligation {
            id: format!("step-{}", index + 1),
            statement: step
                .acceptance
                .clone()
                .unwrap_or_else(|| step.instructions.clone()),
            proof: Proof::BehaviorEvidence {
                description: step.instructions.clone(),
            },
        })
        .collect();
    AcceptanceContract {
        task: task.to_string(),
        work_kind: crate::contract::classify_work_kind(task),
        obligations,
        forbidden_regressions: Vec::new(),
    }
}

pub struct UltraHostBridge {
    dir: PathBuf,
}

impl UltraHostBridge {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, BridgeError> {
        let dir = root.into().join("ultra");
        fs::create_dir_all(&dir).map_err(|e| BridgeError::Io(e.to_string()))?;
        Ok(Self { dir })
    }

    /// True when a frozen kernel already exists for this task.
    pub fn exists(&self, task_id: &str) -> bool {
        self.path(task_id).map(|p| p.exists()).unwrap_or(false)
    }

    /// The contract frozen at ultra_open for this task.
    pub fn frozen_contract(&self, task_id: &str) -> Result<AcceptanceContract, BridgeError> {
        let path = self.path(task_id)?;
        let adapter = ExternalHostAdapter::open(&path).map_err(BridgeError::Adapter)?;
        Ok(adapter.contract().clone())
    }

    fn path(&self, task_id: &str) -> Result<PathBuf, BridgeError> {
        if task_id.is_empty()
            || !task_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err(BridgeError::InvalidTaskId);
        }
        Ok(self.dir.join(format!("{task_id}.json")))
    }

    fn load_or_create(
        &self,
        task_id: &str,
        contract: &AcceptanceContract,
        minimum_candidates: usize,
    ) -> Result<ExternalHostAdapter, BridgeError> {
        let path = self.path(task_id)?;
        if path.exists() {
            ExternalHostAdapter::open(&path).map_err(BridgeError::Adapter)
        } else {
            let mut adapter = ExternalHostAdapter::new(contract.clone(), minimum_candidates)
                .map_err(BridgeError::Adapter)?;
            adapter.save_as(&path).map_err(BridgeError::Adapter)?;
            Ok(adapter)
        }
    }

    /// Open requests for the host. First call attaches the host at the given
    /// lease epoch; afterwards the view is derived purely from kernel state.
    pub fn open_requests(
        &self,
        task_id: &str,
        contract: &AcceptanceContract,
        minimum_candidates: usize,
        lease_epoch: u64,
    ) -> Result<UltraHostView, BridgeError> {
        let mut adapter = self.load_or_create(task_id, contract, minimum_candidates)?;
        adapter
            .attach_host(lease_epoch)
            .map_err(BridgeError::Adapter)?;
        self.view(task_id, &mut adapter)
    }

    pub fn record_response(
        &self,
        task_id: &str,
        contract: &AcceptanceContract,
        response: CandidateResponse,
    ) -> Result<UltraHostView, BridgeError> {
        let mut adapter = self.load_or_create(task_id, contract, DEFAULT_MINIMUM_CANDIDATES)?;
        adapter
            .record_response(response)
            .map_err(BridgeError::Adapter)?;
        adapter.advance().map_err(BridgeError::Adapter)?;
        self.view(task_id, &mut adapter)
    }

    /// Record the daemon's own adversary scan for one candidate, then
    /// re-run finalization. Only the daemon calls this: hosts never set
    /// adversary verdicts.
    pub fn record_daemon_adversary(
        &self,
        task_id: &str,
        candidate_id: &str,
        report: crate::daemon_adversary::DaemonAdversaryReport,
        receipts_hash: String,
    ) -> Result<UltraHostView, BridgeError> {
        let mut adapter = self.load_existing(task_id)?.ok_or(BridgeError::Adapter(
            AdapterError::EvidenceStageNotOpen,
        ))?;
        adapter
            .record_daemon_adversary(candidate_id, report, receipts_hash)
            .map_err(BridgeError::Adapter)?;
        adapter.finalize().map_err(BridgeError::Adapter)?;
        self.view(task_id, &mut adapter)
    }

    /// Record the daemon's own verifier execution for one candidate, then
    /// re-run finalization. Only the daemon calls this: hosts never set
    /// verifier outcomes.
    pub fn record_daemon_verifier(
        &self,
        task_id: &str,
        candidate_id: &str,
        outcomes: std::collections::BTreeMap<String, bool>,
        receipts_hash: String,
    ) -> Result<UltraHostView, BridgeError> {
        let mut adapter = self.load_existing(task_id)?.ok_or(BridgeError::Adapter(
            AdapterError::EvidenceStageNotOpen,
        ))?;
        adapter
            .record_daemon_verifier(candidate_id, outcomes, receipts_hash)
            .map_err(BridgeError::Adapter)?;
        adapter.finalize().map_err(BridgeError::Adapter)?;
        self.view(task_id, &mut adapter)
    }

    /// The current durable view; never creates or mutates state.
    pub fn current_view(&self, task_id: &str) -> Result<UltraHostView, BridgeError> {
        let mut adapter = self.load_existing(task_id)?.ok_or(BridgeError::Adapter(
            AdapterError::EvidenceStageNotOpen,
        ))?;
        self.view(task_id, &mut adapter)
    }

    /// Read-only adapter access for proof assembly; never creates state.
    pub fn load_existing(&self, task_id: &str) -> Result<Option<ExternalHostAdapter>, BridgeError> {
        let path = self.path(task_id)?;
        if path.exists() {
            ExternalHostAdapter::open(&path)
                .map(Some)
                .map_err(BridgeError::Adapter)
        } else {
            Ok(None)
        }
    }

    /// Promote the qualified candidate's sealed bundle into the destination
    /// with the full section-J sequence and verified rollback.
    pub fn promote(
        &self,
        task_id: &str,
        contract: &AcceptanceContract,
        destination: &std::path::Path,
    ) -> Result<crate::promotion::PromotionReceipt, BridgeError> {
        let adapter = self.load_or_create(task_id, contract, DEFAULT_MINIMUM_CANDIDATES)?;
        let store =
            crate::promotion::PromotionStore::open(&self.dir).map_err(BridgeError::Promotion)?;
        store
            .promote(task_id, &adapter, contract, destination)
            .map_err(BridgeError::Promotion)
    }

    pub fn record_visual(
        &self,
        task_id: &str,
        contract: &AcceptanceContract,
        evidence: crate::external_kernel::VisualEvidence,
    ) -> Result<UltraHostView, BridgeError> {
        let mut adapter = self.load_or_create(task_id, contract, DEFAULT_MINIMUM_CANDIDATES)?;
        adapter
            .record_visual(evidence)
            .map_err(BridgeError::Adapter)?;
        adapter.finalize().map_err(BridgeError::Adapter)?;
        self.view(task_id, &mut adapter)
    }

    fn view(
        &self,
        task_id: &str,
        adapter: &mut ExternalHostAdapter,
    ) -> Result<UltraHostView, BridgeError> {
        Ok(UltraHostView {
            task_id: task_id.to_string(),
            status: adapter.status(),
            kernel_state: adapter.kernel().state,
            candidate_requests: adapter.requests().map_err(BridgeError::Adapter)?,
            evidence_requests: adapter.evidence_requests().map_err(BridgeError::Adapter)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rex_protocol::schema::canonical_hash;
    use tempfile::tempdir;

    fn plan() -> Vec<PlanStep> {
        vec![
            PlanStep {
                instructions: "build the thing".into(),
                acceptance: Some("it builds".into()),
            },
            PlanStep {
                instructions: "prove the thing".into(),
                acceptance: None,
            },
        ]
    }

    /// The daemon's own adversary scan over the plan-derived contract.
    fn daemon_adversary(
        bridge: &UltraHostBridge,
        task_id: &str,
        candidate_id: &str,
        clean: bool,
    ) -> UltraHostView {
        let mut report = crate::daemon_adversary::DaemonAdversaryReport {
            defects: Vec::new(),
            scanned_files: 1,
            scanned_bytes: 1,
            tree_hash: "tree".into(),
            truncated: false,
        };
        if !clean {
            report.defects.push(crate::adversary::Defect {
                title: "placeholder content".into(),
                detail: "index.html contains \"lorem ipsum\"".into(),
            });
        }
        bridge
            .record_daemon_adversary(task_id, candidate_id, report, canonical_hash(&clean).unwrap())
            .unwrap()
    }

    /// The daemon's own verifier run over the plan-derived contract.
    fn daemon_verify(bridge: &UltraHostBridge, task_id: &str, candidate_id: &str, proven: bool) -> UltraHostView {
        let outcomes = std::collections::BTreeMap::from([
            ("step-1".to_string(), proven),
            ("step-2".to_string(), proven),
        ]);
        bridge
            .record_daemon_verifier(task_id, candidate_id, outcomes, canonical_hash(&proven).unwrap())
            .unwrap()
    }

    fn answer_requests(
        bridge: &UltraHostBridge,
        task_id: &str,
        contract: &AcceptanceContract,
        view: &UltraHostView,
    ) -> UltraHostView {
        let mut current = view.clone();
        for request in &view.candidate_requests {
            let content = format!("candidate for {}", request.candidate_id);
            current = bridge
                .record_response(
                    task_id,
                    contract,
                    CandidateResponse {
                        candidate_id: request.candidate_id.clone(),
                        response_hash: canonical_hash(&content).unwrap(),
                        content,
                    },
                )
                .unwrap();
        }
        current
    }

    #[test]
    fn plan_becomes_a_deterministic_contract() {
        let contract = contract_from_plan("do it", &plan());
        assert_eq!(contract.obligations.len(), 2);
        assert_eq!(contract.obligations[0].id, "step-1");
        assert_eq!(contract.obligations[0].statement, "it builds");
        assert_eq!(contract.obligations[1].statement, "prove the thing");
        assert_eq!(contract_from_plan("do it", &plan()), contract);
    }

    #[test]
    fn full_external_host_lifecycle_across_restarts() {
        let directory = tempdir().unwrap();
        let contract = contract_from_plan("do it", &plan());

        let bridge = UltraHostBridge::open(directory.path()).unwrap();
        let view = bridge.open_requests("task-abc", &contract, 2, 1).unwrap();
        assert_eq!(view.status, HostKernelStatus::Collecting);
        assert_eq!(view.candidate_requests.len(), 2);
        assert!(view.evidence_requests.is_empty());

        // A fresh bridge (daemon restart) sees the same persisted kernel.
        let bridge = UltraHostBridge::open(directory.path()).unwrap();
        let view = bridge.open_requests("task-abc", &contract, 2, 1).unwrap();
        assert_eq!(view.candidate_requests.len(), 2);

        let view = answer_requests(&bridge, "task-abc", &contract, &view);
        assert_eq!(view.status, HostKernelStatus::Ready);
        assert_eq!(view.kernel_state, KernelState::AwaitingEvidence);
        // General contracts issue no host evidence requests: the daemon
        // executes the verifier and the adversary itself.
        assert!(view.evidence_requests.is_empty());

        // The daemon's clean adversary scan alone does not qualify.
        let candidate = view.candidate_requests[0].candidate_id.clone();
        daemon_adversary(&bridge, "task-abc", &candidate, true);
        // The daemon executes the contract proofs itself; its clean verdict
        // qualifies the candidate and completes the kernel.
        let after = daemon_verify(&bridge, "task-abc", &candidate, true);
        assert_eq!(after.kernel_state, KernelState::Completed);

        let bridge = UltraHostBridge::open(directory.path()).unwrap();
        let final_view = bridge.open_requests("task-abc", &contract, 2, 1).unwrap();
        assert_eq!(final_view.kernel_state, KernelState::Completed);
        assert!(final_view.evidence_requests.is_empty());
    }

    #[test]
    fn unqualified_candidates_fail_closed_across_restart() {
        let directory = tempdir().unwrap();
        let contract = contract_from_plan("do it", &plan());
        let bridge = UltraHostBridge::open(directory.path()).unwrap();
        let view = bridge.open_requests("task-def", &contract, 2, 1).unwrap();
        let view = answer_requests(&bridge, "task-def", &contract, &view);
        // The daemon's adversary scan finds standing defects in every
        // candidate.
        for request in &view.candidate_requests {
            daemon_adversary(&bridge, "task-def", &request.candidate_id, false);
        }
        // The daemon's own verifier run finds every candidate wanting, so
        // the kernel fails closed.
        for request in &view.candidate_requests {
            daemon_verify(&bridge, "task-def", &request.candidate_id, false);
        }
        let bridge = UltraHostBridge::open(directory.path()).unwrap();
        let final_view = bridge.open_requests("task-def", &contract, 2, 1).unwrap();
        assert_eq!(final_view.kernel_state, KernelState::Failed);
    }

    #[test]
    fn task_ids_cannot_escape_the_store() {
        let directory = tempdir().unwrap();
        let bridge = UltraHostBridge::open(directory.path()).unwrap();
        let contract = contract_from_plan("do it", &plan());
        assert_eq!(
            bridge
                .open_requests("../escape", &contract, 2, 1)
                .unwrap_err(),
            BridgeError::InvalidTaskId
        );
        assert_eq!(
            bridge.open_requests("", &contract, 2, 1).unwrap_err(),
            BridgeError::InvalidTaskId
        );
    }
    #[test]
    fn contract_from_plan_classifies_visual_work() {
        let visual = contract_from_plan("build a website landing page", &[]);
        assert_eq!(visual.work_kind, crate::contract::WorkKind::Visual);
        let general = contract_from_plan("tune the batch scheduler", &[]);
        assert_eq!(general.work_kind, crate::contract::WorkKind::General);
    }
}
