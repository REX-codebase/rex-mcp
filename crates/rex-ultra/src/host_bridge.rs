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

    /// Persist the compiled skill plan bound at ultra_open. The plan is
    /// frozen once; later reads load it, never recompute it, so the gates
    /// that apply at promotion are exactly the gates bound at open
    /// (audit findings 9 and 10).
    pub fn freeze_skill_plan(
        &self,
        task_id: &str,
        plan: &crate::skills::CompiledSkillPlan,
    ) -> Result<(), BridgeError> {
        let path = self.skill_plan_path(task_id)?;
        if path.exists() {
            let existing = self.frozen_skill_plan(task_id)?;
            if existing.as_ref() != Some(plan) {
                return Err(BridgeError::Io(format!(
                    "a different skill plan is already frozen for task {task_id}"
                )));
            }
            return Ok(());
        }
        let bytes = serde_json::to_vec(plan).map_err(|e| BridgeError::Io(e.to_string()))?;
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, bytes).map_err(|e| BridgeError::Io(e.to_string()))?;
        fs::rename(&temporary, &path).map_err(|e| BridgeError::Io(e.to_string()))
    }

    pub fn frozen_skill_plan(
        &self,
        task_id: &str,
    ) -> Result<Option<crate::skills::CompiledSkillPlan>, BridgeError> {
        let path = self.skill_plan_path(task_id)?;
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(&path).map_err(|e| BridgeError::Io(e.to_string()))?;
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| BridgeError::Io(format!("frozen skill plan is unreadable: {e}")))
    }

    fn skill_plan_path(&self, task_id: &str) -> Result<PathBuf, BridgeError> {
        if task_id.is_empty()
            || !task_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err(BridgeError::InvalidTaskId);
        }
        Ok(self.dir.join(format!("skill-plan-{task_id}.json")))
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

    /// Record the daemon's own visual verification for one candidate,
    /// then re-run finalization. Only the daemon calls this.
    pub fn record_daemon_visual(
        &self,
        task_id: &str,
        candidate_id: &str,
        record: crate::external_kernel::DaemonVisualRecord,
    ) -> Result<UltraHostView, BridgeError> {
        let mut adapter = self.load_existing(task_id)?.ok_or(BridgeError::Adapter(
            AdapterError::EvidenceStageNotOpen,
        ))?;
        adapter
            .record_daemon_visual(candidate_id, record)
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
        skill_gates: &[crate::skills::CompiledGate],
    ) -> Result<crate::promotion::PromotionReceipt, BridgeError> {
        let adapter = self.load_or_create(task_id, contract, DEFAULT_MINIMUM_CANDIDATES)?;
        let store =
            crate::promotion::PromotionStore::open(&self.dir).map_err(BridgeError::Promotion)?;
        store
            .promote(task_id, &adapter, contract, destination, skill_gates)
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

    /// Executable test contract: the flattening contract_from_plan helper
    /// was removed with audit finding 7; tests use daemon-executable proofs.
    fn test_contract() -> AcceptanceContract {
        AcceptanceContract {
            task: "do it".into(),
            work_kind: crate::contract::WorkKind::General,
            obligations: vec![
                crate::contract::Obligation {
                    id: "step-1".into(),
                    statement: "it builds".into(),
                    proof: crate::contract::Proof::FileContains {
                        path: "result.txt".into(),
                        needle: "built".into(),
                    },
                },
                crate::contract::Obligation {
                    id: "step-2".into(),
                    statement: "prove the thing".into(),
                    proof: crate::contract::Proof::FileExists {
                        path: "result.txt".into(),
                    },
                },
            ],
            forbidden_regressions: vec![],
        }
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
    fn frozen_skill_plan_roundtrips_and_conflicts() {
        let directory = tempdir().unwrap();
        let bridge = UltraHostBridge::open(directory.path()).unwrap();
        let facts = crate::skills::RepositoryFacts::default();
        let plan =
            crate::skills::compile_plan(&facts, &crate::skill_packs::first_class_registry())
                .unwrap();
        assert!(bridge.frozen_skill_plan("task-plan").unwrap().is_none());
        bridge.freeze_skill_plan("task-plan", &plan).unwrap();
        assert_eq!(
            bridge.frozen_skill_plan("task-plan").unwrap(),
            Some(plan.clone())
        );
        // Idempotent re-freeze of the same plan; a different one conflicts.
        bridge.freeze_skill_plan("task-plan", &plan).unwrap();
        let mut other = plan.clone();
        other.compiler_version = "different".into();
        assert!(bridge.freeze_skill_plan("task-plan", &other).is_err());
    }

    #[test]
    fn full_external_host_lifecycle_across_restarts() {
        let directory = tempdir().unwrap();
        let contract = test_contract();

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
        let after = daemon_verify(&bridge, "task-abc", &candidate, true);
        // The rest of the field is not evidenced yet: never first-clean.
        assert_eq!(after.kernel_state, KernelState::AwaitingEvidence);
        // Once the whole field is gated, the qualifying candidate wins.
        let other = view.candidate_requests[1].candidate_id.clone();
        daemon_adversary(&bridge, "task-abc", &other, false);
        let after = daemon_verify(&bridge, "task-abc", &other, true);
        assert_eq!(after.kernel_state, KernelState::Completed);

        let bridge = UltraHostBridge::open(directory.path()).unwrap();
        let final_view = bridge.open_requests("task-abc", &contract, 2, 1).unwrap();
        assert_eq!(final_view.kernel_state, KernelState::Completed);
        assert!(final_view.evidence_requests.is_empty());
    }

    #[test]
    fn unqualified_candidates_fail_closed_across_restart() {
        let directory = tempdir().unwrap();
        let contract = test_contract();
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
        let contract = test_contract();
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
}
