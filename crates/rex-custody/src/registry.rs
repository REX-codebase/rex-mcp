//! The custody registry: the institution that offers, verifies, fences and
//! releases custody.
//!
//! Authority layout (confused-deputy rules):
//! - Only the host shell constructs a registry and calls `offer_*`,
//!   `human_stop`, `reopen_task` and `confirm_human_gate`. None of these is
//!   reachable through the tool surface, so no worker (model or external
//!   agent) can widen itself.
//! - Workers hold only a `CapabilityToken`. It authorizes heartbeats, tool
//!   calls within scope, one completion claim at a time, and declaring
//!   failure or cancellation. Nothing else.
//! - Human approval for risky tool calls still lives in rex-tools; custody
//!   never auto-approves.
//!
//! Duplicate execution is blocked structurally: one live or terminal
//! custody per task id until a human reopens the tombstone.

use crate::audit::{read_chain, AuditEvent, AuditLog};
use crate::budget::Consumption;
use crate::budget::CustodyBudgets;
use crate::capability::CapabilitySet;
use crate::capability::{random_hex, CapabilityToken};
use crate::contract::CompletionContract;
use crate::contract::{CompletionClaim, GateEvaluator, GateOutcome};
use crate::identity::{AgentIdentity, OperatorIdentity, WorkerMode};
use crate::lease::{Lease, LeaseTerms};
use crate::offer::{CustodyAcceptance, CustodyOffer};
use crate::state::{CustodyGrant, CustodyPhase, ReleaseReason, Violation};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tombstone {
    pub task_id: String,
    pub grant_id: String,
    pub ended_ms: u128,
    pub release: Option<ReleaseReason>,
    pub violation: Option<Violation>,
}

/// Durable index: which tasks are taken, which are tombstoned.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Index {
    /// task_id -> grant_id for non-terminal custody
    active: HashMap<String, String>,
    tombstones: HashMap<String, Tombstone>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustodyError {
    UnknownOffer,
    OfferExpired,
    TaskAlreadyCustodied,
    TaskTombstoned,
    CommitmentMismatch,
    UnknownGrant,
    UnknownToken,
    WrongPhase(CustodyPhase),
    LeaseStale,
    HeartbeatReplay { expected_seq: u64 },
    ResumeSecretMismatch,
    BudgetExhausted,
    GrantReleased(ReleaseReason),
    Quarantined(Violation),
    AuditBroken(String),
    ClaimRejected { failures: Vec<String> },
    DelegationRefused,
    Io(String),
}

impl std::fmt::Display for CustodyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for CustodyError {}

pub struct CustodyRegistry {
    root: PathBuf,
    index: Index,
    grants: HashMap<String, CustodyGrant>,
    audits: HashMap<String, AuditLog>,
    offers: HashMap<String, CustodyOffer>,
}

const DEFAULT_OFFER_TTL_MS: u64 = 10 * 60 * 1000;

impl CustodyRegistry {
    pub fn open(root: PathBuf) -> Result<Self, CustodyError> {
        fs::create_dir_all(root.join("grants")).map_err(ioe)?;
        fs::create_dir_all(root.join("audit")).map_err(ioe)?;
        let index: Index = match fs::read(root.join("index.json")) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|e| CustodyError::Io(e.to_string()))?
            }
            Err(_) => Index::default(),
        };
        let mut grants = HashMap::new();
        let mut audits = HashMap::new();
        let mut grant_ids: Vec<String> = index.active.values().cloned().collect();
        grant_ids.extend(index.tombstones.values().map(|t| t.grant_id.clone()));
        grant_ids.sort();
        grant_ids.dedup();
        for grant_id in grant_ids {
            let bytes =
                fs::read(root.join("grants").join(format!("{grant_id}.json"))).map_err(ioe)?;
            let grant: CustodyGrant =
                serde_json::from_slice(&bytes).map_err(|e| CustodyError::Io(e.to_string()))?;
            let log = AuditLog::open(root.join("audit").join(format!("{grant_id}.jsonl")))
                .map_err(|e| CustodyError::AuditBroken(e.to_string()))?;
            audits.insert(grant_id.clone(), log);
            grants.insert(grant_id, grant);
        }
        Ok(Self {
            root,
            index,
            grants,
            audits,
            offers: HashMap::new(),
        })
    }

    fn persist_grant(&self, grant: &CustodyGrant) -> Result<(), CustodyError> {
        let tmp = self
            .root
            .join("grants")
            .join(format!("{}.json.tmp", grant.grant_id));
        let fin = self
            .root
            .join("grants")
            .join(format!("{}.json", grant.grant_id));
        fs::write(
            &tmp,
            serde_json::to_vec_pretty(grant).expect("grant serializes"),
        )
        .map_err(ioe)?;
        fs::rename(&tmp, &fin).map_err(ioe)
    }

    fn persist_index(&self) -> Result<(), CustodyError> {
        let tmp = self.root.join("index.json.tmp");
        let fin = self.root.join("index.json");
        fs::write(
            &tmp,
            serde_json::to_vec_pretty(&self.index).expect("index serializes"),
        )
        .map_err(ioe)?;
        fs::rename(&tmp, &fin).map_err(ioe)
    }

    fn audit(
        &mut self,
        grant_id: &str,
        now_ms: u128,
        kind: &str,
        detail: serde_json::Value,
    ) -> Result<(), CustodyError> {
        let log = self
            .audits
            .get_mut(grant_id)
            .ok_or_else(|| CustodyError::UnknownGrant)?;
        log.append(now_ms, kind, detail).map_err(ioe)?;
        Ok(())
    }

    // ----- offering -----------------------------------------------------

    /// Host-only: mint an offer for a task. Refused when the task is
    /// already custodied or tombstoned (duplicate execution block).
    pub fn offer(
        &mut self,
        task_id: &str,
        task_summary: &str,
        operator: OperatorIdentity,
        worker: WorkerMode,
        capabilities: CapabilitySet,
        budgets: CustodyBudgets,
        lease_terms: LeaseTerms,
        contract: CompletionContract,
        now_ms: u128,
    ) -> Result<CustodyOffer, CustodyError> {
        if self.index.active.contains_key(task_id) {
            return Err(CustodyError::TaskAlreadyCustodied);
        }
        if self.index.tombstones.contains_key(task_id) {
            return Err(CustodyError::TaskTombstoned);
        }
        if capabilities.can_delegate {
            // Structural rule: no custody grant may carry delegation rights.
            return Err(CustodyError::DelegationRefused);
        }
        let offer = CustodyOffer {
            offer_id: format!("offer-{}", random_hex(8)),
            task_id: task_id.to_string(),
            task_summary: task_summary.chars().take(500).collect(),
            operator,
            worker,
            capabilities,
            budgets,
            lease_terms,
            contract,
            nonce: random_hex(16),
            created_ms: now_ms,
            expires_ms: now_ms + DEFAULT_OFFER_TTL_MS as u128,
        };
        self.offers.insert(offer.offer_id.clone(), offer.clone());
        Ok(offer)
    }

    // ----- handshake ----------------------------------------------------

    /// Verify the commitment/acceptance handshake and activate custody.
    pub fn accept(
        &mut self,
        acceptance: &CustodyAcceptance,
        now_ms: u128,
    ) -> Result<(CapabilityToken, CustodyGrant), CustodyError> {
        let offer = self
            .offers
            .get(&acceptance.offer_id)
            .cloned()
            .ok_or(CustodyError::UnknownOffer)?;
        if now_ms >= offer.expires_ms {
            self.offers.remove(&acceptance.offer_id);
            return Err(CustodyError::OfferExpired);
        }
        if self.index.active.contains_key(&offer.task_id) {
            return Err(CustodyError::TaskAlreadyCustodied);
        }
        if acceptance.nonce_echo != offer.nonce
            || acceptance.commitment != offer.expected_acceptance()
        {
            return Err(CustodyError::CommitmentMismatch);
        }
        self.offers.remove(&acceptance.offer_id);
        let grant_id = format!("custody-{}", random_hex(8));
        let lease = Lease {
            lease_id: format!("lease-{}", random_hex(8)),
            epoch: 1,
            issued_ms: now_ms,
            expires_ms: now_ms + offer.lease_terms.lease_ms as u128,
            heartbeat_interval_ms: offer.lease_terms.heartbeat_interval_ms,
            next_seq: 1,
        };
        let grant = CustodyGrant {
            grant_id: grant_id.clone(),
            offer_id: offer.offer_id.clone(),
            task_id: offer.task_id.clone(),
            task_summary: offer.task_summary.clone(),
            operator: offer.operator.clone(),
            worker: offer.worker.clone(),
            capabilities: offer.capabilities.clone(),
            budgets: offer.budgets,
            consumed: Consumption::default(),
            lease_terms: offer.lease_terms,
            lease,
            contract: offer.contract.clone(),
            phase: CustodyPhase::Active,
            claim_attempts: 0,
            resume_secret: random_hex(16),
            token_secret: random_hex(32),
            created_ms: now_ms,
            updated_ms: now_ms,
            suspended_at_ms: None,
            release: None,
            violation: None,
        };
        self.persist_grant(&grant)?;
        self.audits.insert(
            grant_id.clone(),
            AuditLog::open(self.root.join("audit").join(format!("{grant_id}.jsonl")))
                .map_err(|e| CustodyError::AuditBroken(e.to_string()))?,
        );
        self.audit(
            &grant_id,
            now_ms,
            "custody_granted",
            json!({
                "task_id": grant.task_id,
                "operator": grant.operator.label(),
                "worker": grant.worker,
                "scope_hash": grant.capabilities.scope_hash(),
                "offer_hash": offer.offer_hash(),
                "epoch": 1,
            }),
        )?;
        self.grants.insert(grant_id.clone(), grant.clone());
        self.index
            .active
            .insert(offer.task_id.clone(), grant_id.clone());
        self.persist_index()?;
        let token = CapabilityToken {
            grant_id: grant_id.clone(),
            epoch: 1,
            secret: grant.token_secret.clone(),
            scope_hash: grant.capabilities.scope_hash(),
        };
        Ok((token, grant))
    }

    /// Human operators commit by the same UI action that starts the task;
    /// the host performs the handshake on their behalf.
    pub fn accept_for_human(
        &mut self,
        offer_id: &str,
        now_ms: u128,
    ) -> Result<(CapabilityToken, CustodyGrant), CustodyError> {
        let offer = self
            .offers
            .get(offer_id)
            .cloned()
            .ok_or(CustodyError::UnknownOffer)?;
        if offer.operator.is_agent() {
            return Err(CustodyError::CommitmentMismatch);
        }
        let acceptance = CustodyAcceptance {
            offer_id: offer.offer_id.clone(),
            nonce_echo: offer.nonce.clone(),
            commitment: offer.expected_acceptance(),
        };
        self.accept(&acceptance, now_ms)
    }

    // ----- token verification ------------------------------------------

    fn grant_for_token(
        &mut self,
        token: &CapabilityToken,
        now_ms: u128,
    ) -> Result<(), CustodyError> {
        let grant = self
            .grants
            .get(&token.grant_id)
            .cloned()
            .ok_or(CustodyError::UnknownGrant)?;
        if grant.phase == CustodyPhase::Quarantined {
            return Err(CustodyError::Quarantined(
                grant.violation.clone().unwrap_or(Violation::TokenForgery {
                    grant_id: token.grant_id.clone(),
                }),
            ));
        }
        if let Some(rel) = &grant.release {
            return Err(CustodyError::GrantReleased(rel.clone()));
        }
        if token.epoch != grant.lease.epoch {
            // Checked before the secret: resume rotates both, and an honest
            // stale process (old epoch, old secret) must read as stale lease
            // resurrection, not as an active forgery attempt.
            let v = Violation::StaleLeaseUse {
                presented_epoch: token.epoch,
                current_epoch: grant.lease.epoch,
            };
            self.violate(&token.grant_id, v.clone(), now_ms)?;
            return Err(CustodyError::Quarantined(v));
        }
        if token.secret != grant.token_secret {
            let v = Violation::TokenForgery {
                grant_id: token.grant_id.clone(),
            };
            self.violate(&token.grant_id, v.clone(), now_ms)?;
            return Err(CustodyError::Quarantined(v));
        }
        if token.scope_hash != grant.capabilities.scope_hash() {
            return Err(CustodyError::CommitmentMismatch);
        }
        if grant.phase != CustodyPhase::Active {
            return Err(CustodyError::WrongPhase(grant.phase));
        }
        Ok(())
    }

    /// Public read-only check used by the tool wrapper.
    pub fn verify_token(
        &mut self,
        token: &CapabilityToken,
        now_ms: u128,
    ) -> Result<CustodyGrant, CustodyError> {
        self.grant_for_token(token, now_ms)?;
        Ok(self.grants.get(&token.grant_id).expect("checked").clone())
    }

    // ----- heartbeats ---------------------------------------------------

    pub fn heartbeat(
        &mut self,
        token: &CapabilityToken,
        seq: u64,
        now_ms: u128,
    ) -> Result<Lease, CustodyError> {
        self.grant_for_token(token, now_ms)?;
        let grant = self.grants.get_mut(&token.grant_id).expect("checked");
        if seq != grant.lease.next_seq {
            return Err(CustodyError::HeartbeatReplay {
                expected_seq: grant.lease.next_seq,
            });
        }
        grant.lease.next_seq += 1;
        grant.lease.expires_ms = now_ms + grant.lease_terms.lease_ms as u128;
        grant.updated_ms = now_ms;
        let lease = grant.lease.clone();
        let grant_id = grant.grant_id.clone();
        self.persist_grant(self.grants.get(&grant_id).expect("checked"))?;
        self.audit(
            &grant_id,
            now_ms,
            "heartbeat",
            json!({ "seq": seq, "epoch": lease.epoch }),
        )?;
        Ok(lease)
    }

    // ----- consumption / budgets ---------------------------------------

    pub fn consume(
        &mut self,
        token: &CapabilityToken,
        delta: Consumption,
        now_ms: u128,
    ) -> Result<(), CustodyError> {
        self.grant_for_token(token, now_ms)?;
        let grant = self.grants.get_mut(&token.grant_id).expect("checked");
        grant.consumed.steps += delta.steps;
        grant.consumed.tool_calls += delta.tool_calls;
        grant.consumed.tokens += delta.tokens;
        grant.consumed.wall_ms = now_ms.saturating_sub(grant.created_ms) as u64;
        grant.updated_ms = now_ms;
        let exceeded = grant.budgets.first_exceeded(&grant.consumed);
        let grant_id = grant.grant_id.clone();
        let consumed = grant.consumed;
        self.persist_grant(self.grants.get(&grant_id).expect("checked"))?;
        self.audit(
            &grant_id,
            now_ms,
            "consumed",
            json!({ "delta": delta, "total": consumed }),
        )?;
        if let Some(which) = exceeded {
            self.release(&grant_id, ReleaseReason::BudgetExhausted { which }, now_ms)?;
            return Err(CustodyError::BudgetExhausted);
        }
        Ok(())
    }

    // ----- suspend / resume --------------------------------------------

    /// Interruption: crash detected, operator paused, or lease lapsed.
    pub fn suspend(
        &mut self,
        grant_id: &str,
        reason: &str,
        now_ms: u128,
    ) -> Result<(), CustodyError> {
        let grant = self
            .grants
            .get_mut(grant_id)
            .ok_or(CustodyError::UnknownGrant)?;
        if grant.phase != CustodyPhase::Active {
            return Err(CustodyError::WrongPhase(grant.phase));
        }
        grant.phase = CustodyPhase::Suspended;
        grant.suspended_at_ms = Some(now_ms);
        grant.updated_ms = now_ms;
        self.persist_grant(self.grants.get(grant_id).expect("checked"))?;
        self.audit(
            grant_id,
            now_ms,
            "suspended",
            json!({ "reason": reason, "epoch": self.grants[grant_id].lease.epoch }),
        )?;
        Ok(())
    }

    /// Resume a suspended grant. Requires the per-epoch resume secret;
    /// bumps the epoch and rotates every secret, killing all outstanding
    /// tokens (stale lease resurrection becomes detectable, not just
    /// unlikely).
    pub fn resume(
        &mut self,
        grant_id: &str,
        resume_secret: &str,
        now_ms: u128,
    ) -> Result<CapabilityToken, CustodyError> {
        let grant = self
            .grants
            .get(grant_id)
            .cloned()
            .ok_or(CustodyError::UnknownGrant)?;
        if grant.phase != CustodyPhase::Suspended {
            return Err(CustodyError::WrongPhase(grant.phase));
        }
        let suspended_at = grant.suspended_at_ms.unwrap_or(grant.updated_ms);
        if now_ms > suspended_at + grant.lease_terms.resume_grace_ms as u128 {
            self.release(grant_id, ReleaseReason::LeaseExpired, now_ms)?;
            return Err(CustodyError::GrantReleased(ReleaseReason::LeaseExpired));
        }
        if resume_secret != grant.resume_secret {
            // Wrong secret on a suspended grant is a forgery attempt.
            let v = Violation::TokenForgery {
                grant_id: grant_id.to_string(),
            };
            self.violate(grant_id, v.clone(), now_ms)?;
            return Err(CustodyError::Quarantined(v));
        }
        let grant = self.grants.get_mut(grant_id).expect("checked");
        grant.lease.epoch += 1;
        grant.lease.lease_id = format!("lease-{}", random_hex(8));
        grant.lease.issued_ms = now_ms;
        grant.lease.expires_ms = now_ms + grant.lease_terms.lease_ms as u128;
        grant.lease.next_seq = 1;
        grant.resume_secret = random_hex(16);
        grant.token_secret = random_hex(32);
        grant.phase = CustodyPhase::Active;
        grant.suspended_at_ms = None;
        grant.updated_ms = now_ms;
        let epoch = grant.lease.epoch;
        let token = CapabilityToken {
            grant_id: grant.grant_id.clone(),
            epoch,
            secret: grant.token_secret.clone(),
            scope_hash: grant.capabilities.scope_hash(),
        };
        self.persist_grant(self.grants.get(grant_id).expect("checked"))?;
        self.audit(grant_id, now_ms, "resumed", json!({ "epoch": epoch }))?;
        Ok(token)
    }

    // ----- completion ---------------------------------------------------

    /// The operator claims completion. Gates decide, not the claim.
    pub fn claim_completion(
        &mut self,
        token: &CapabilityToken,
        claim: &CompletionClaim,
        evaluator: &dyn GateEvaluator,
        now_ms: u128,
    ) -> Result<ReleaseReason, CustodyError> {
        self.grant_for_token(token, now_ms)?;
        {
            let grant = self.grants.get_mut(&token.grant_id).expect("checked");
            grant.phase = CustodyPhase::Verifying;
            grant.updated_ms = now_ms;
        }
        self.persist_grant(self.grants.get(&token.grant_id).expect("checked"))?;
        self.audit(
            &token.grant_id,
            now_ms,
            "completion_claimed",
            json!({ "summary": claim.summary }),
        )?;

        let grant = self.grants.get(&token.grant_id).expect("checked").clone();
        let mut failures = Vec::new();
        for gate in &grant.contract.gates {
            if let GateOutcome::Failed(why) = evaluator.evaluate(gate, &grant) {
                failures.push(format!("{gate:?}: {why}"));
            }
        }
        if failures.is_empty() {
            let reason = ReleaseReason::VerifiedCompletion {
                summary: claim.summary.clone(),
            };
            self.release(&token.grant_id, reason.clone(), now_ms)?;
            Ok(reason)
        } else {
            let grant = self.grants.get_mut(&token.grant_id).expect("checked");
            grant.claim_attempts += 1;
            let attempts = grant.claim_attempts;
            let max = grant.contract.max_claim_attempts;
            let grant_id = grant.grant_id.clone();
            self.audit(
                &grant_id,
                now_ms,
                "completion_rejected",
                json!({ "failures": failures, "attempt": attempts }),
            )?;
            if attempts > max {
                let v = Violation::FalseCompletion { attempts };
                self.violate(&grant_id, v.clone(), now_ms)?;
                return Err(CustodyError::Quarantined(v));
            }
            let grant = self.grants.get_mut(&grant_id).expect("checked");
            grant.phase = CustodyPhase::Active;
            grant.updated_ms = now_ms;
            self.persist_grant(self.grants.get(&grant_id).expect("checked"))?;
            Err(CustodyError::ClaimRejected { failures })
        }
    }

    pub fn declare_failure(
        &mut self,
        token: &CapabilityToken,
        summary: &str,
        now_ms: u128,
    ) -> Result<ReleaseReason, CustodyError> {
        self.grant_for_token(token, now_ms)?;
        let reason = ReleaseReason::ExplicitFailure {
            summary: summary.chars().take(1000).collect(),
        };
        self.release(&token.grant_id, reason.clone(), now_ms)?;
        Ok(reason)
    }

    pub fn operator_cancel(
        &mut self,
        token: &CapabilityToken,
        now_ms: u128,
    ) -> Result<ReleaseReason, CustodyError> {
        self.grant_for_token(token, now_ms)?;
        self.release(&token.grant_id, ReleaseReason::OperatorCancelled, now_ms)?;
        Ok(ReleaseReason::OperatorCancelled)
    }

    /// The human stop button. Works in every non-terminal phase, including
    /// Verifying and Suspended. Never gated on operator state.
    pub fn human_stop(
        &mut self,
        grant_id: &str,
        now_ms: u128,
    ) -> Result<ReleaseReason, CustodyError> {
        let grant = self
            .grants
            .get(grant_id)
            .ok_or(CustodyError::UnknownGrant)?;
        if grant.phase.is_terminal() {
            return Err(CustodyError::WrongPhase(grant.phase));
        }
        // An offer-phase grant has no grant row yet; offers are separate.
        self.release(grant_id, ReleaseReason::HumanStop, now_ms)?;
        Ok(ReleaseReason::HumanStop)
    }

    // ----- release / quarantine ----------------------------------------

    fn release(
        &mut self,
        grant_id: &str,
        reason: ReleaseReason,
        now_ms: u128,
    ) -> Result<(), CustodyError> {
        let grant = self
            .grants
            .get_mut(grant_id)
            .ok_or(CustodyError::UnknownGrant)?;
        if grant.phase.is_terminal() {
            return Err(CustodyError::WrongPhase(grant.phase));
        }
        grant.phase = CustodyPhase::Released;
        grant.release = Some(reason.clone());
        grant.updated_ms = now_ms;
        let task_id = grant.task_id.clone();
        let tombstone = Tombstone {
            task_id: task_id.clone(),
            grant_id: grant_id.to_string(),
            ended_ms: now_ms,
            release: Some(reason.clone()),
            violation: None,
        };
        self.persist_grant(self.grants.get(grant_id).expect("checked"))?;
        self.audit(grant_id, now_ms, "released", json!({ "reason": reason }))?;
        self.index.active.remove(&task_id);
        self.index.tombstones.insert(task_id, tombstone);
        self.persist_index()?;
        Ok(())
    }

    pub fn violate(
        &mut self,
        grant_id: &str,
        violation: Violation,
        now_ms: u128,
    ) -> Result<(), CustodyError> {
        let grant = match self.grants.get_mut(grant_id) {
            Some(g) => g,
            None => return Err(CustodyError::UnknownGrant),
        };
        if grant.phase.is_terminal() {
            return Ok(());
        }
        grant.phase = CustodyPhase::Quarantined;
        grant.violation = Some(violation.clone());
        grant.updated_ms = now_ms;
        let task_id = grant.task_id.clone();
        let tombstone = Tombstone {
            task_id: task_id.clone(),
            grant_id: grant_id.to_string(),
            ended_ms: now_ms,
            release: None,
            violation: Some(violation.clone()),
        };
        self.persist_grant(self.grants.get(grant_id).expect("checked"))?;
        self.audit(
            grant_id,
            now_ms,
            "quarantined",
            json!({ "violation": violation }),
        )?;
        self.index.active.remove(&task_id);
        self.index.tombstones.insert(task_id, tombstone);
        self.persist_index()?;
        Ok(())
    }

    /// Host-side budget release: the supervised runtime hit a budget wall
    /// it was given by this grant. Needs no token - the host is the
    /// authority that enforced the budget in the first place.
    pub fn release_budget_exhausted(
        &mut self,
        grant_id: &str,
        which: crate::state::BudgetKind,
        now_ms: u128,
    ) -> Result<(), CustodyError> {
        self.release(grant_id, ReleaseReason::BudgetExhausted { which }, now_ms)
    }

    /// Human-only: clear a task's tombstone so it can be offered again.
    pub fn reopen_task(&mut self, task_id: &str, now_ms: u128) -> Result<(), CustodyError> {
        if self.index.active.contains_key(task_id) {
            return Err(CustodyError::TaskAlreadyCustodied);
        }
        let tomb = self
            .index
            .tombstones
            .remove(task_id)
            .ok_or(CustodyError::UnknownGrant)?;
        self.persist_index()?;
        self.audit(
            &tomb.grant_id,
            now_ms,
            "task_reopened",
            json!({ "task_id": task_id }),
        )?;
        Ok(())
    }

    // ----- sweeping -----------------------------------------------------

    /// Periodic reconciliation: lapse live leases into suspension, release
    /// suspensions past grace, expire stale offers. Also called on load
    /// paths via `recover`.
    pub fn sweep(&mut self, now_ms: u128) -> Result<(), CustodyError> {
        let ids: Vec<String> = self.grants.keys().cloned().collect();
        for id in ids {
            let (phase, lease_dead, past_grace) = {
                let g = match self.grants.get(&id) {
                    Some(g) => g,
                    None => continue,
                };
                let past_grace = g
                    .suspended_at_ms
                    .map(|s| now_ms > s + g.lease_terms.resume_grace_ms as u128)
                    .unwrap_or(false);
                (g.phase, !g.lease.is_live(now_ms), past_grace)
            };
            match phase {
                CustodyPhase::Active if lease_dead => {
                    self.suspend(&id, "lease lapsed", now_ms)?;
                }
                CustodyPhase::Suspended if past_grace => {
                    self.release(&id, ReleaseReason::LeaseExpired, now_ms)?;
                }
                _ => {}
            }
        }
        let expired: Vec<String> = self
            .offers
            .iter()
            .filter(|(_, o)| now_ms >= o.expires_ms)
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            self.offers.remove(&id);
        }
        Ok(())
    }

    /// Crash recovery: reload and reconcile against the clock.
    pub fn recover(root: PathBuf, now_ms: u128) -> Result<Self, CustodyError> {
        let mut reg = Self::open(root)?;
        reg.sweep(now_ms)?;
        Ok(reg)
    }

    // ----- views ----------------------------------------------------------

    pub fn grant(&self, grant_id: &str) -> Option<&CustodyGrant> {
        self.grants.get(grant_id)
    }

    pub fn grant_for_task(&self, task_id: &str) -> Option<&CustodyGrant> {
        self.index
            .active
            .get(task_id)
            .and_then(|id| self.grants.get(id))
    }

    pub fn tombstone(&self, task_id: &str) -> Option<&Tombstone> {
        self.index.tombstones.get(task_id)
    }

    pub fn audit_chain(&self, grant_id: &str) -> Result<Vec<AuditEvent>, CustodyError> {
        read_chain(&self.root.join("audit").join(format!("{grant_id}.jsonl")))
            .map_err(|e| CustodyError::AuditBroken(e.to_string()))
    }

    /// Register a named external agent instance (id minting lives here so
    /// name collisions never share identity).
    pub fn register_agent(name: &str, protocol: crate::identity::AgentProtocol) -> AgentIdentity {
        AgentIdentity {
            name: name.chars().take(120).collect(),
            protocol,
            instance_id: format!("aginst-{}", random_hex(8)),
        }
    }
}

fn ioe(e: std::io::Error) -> CustodyError {
    CustodyError::Io(e.to_string())
}
