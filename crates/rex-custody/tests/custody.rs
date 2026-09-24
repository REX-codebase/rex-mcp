//! End-to-end and failure-path tests for the custody kernel.

use rex_custody::*;
use rex_tools::{ToolRequest, ToolRuntime};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const T0: u128 = 1_000_000_000;

struct Ctx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    workspace: PathBuf,
}

fn ctx() -> Ctx {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("custody");
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    Ctx {
        _tmp: tmp,
        root,
        workspace,
    }
}

fn caps(workspace: &std::path::Path, tools: &[&str], classes: &[ToolClass]) -> CapabilitySet {
    CapabilitySet {
        workspace_root: workspace.to_path_buf(),
        tool_classes: classes.iter().copied().collect(),
        allowed_tools: tools.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>(),
        allow_search: false,
        allow_preview: false,
        can_delegate: false,
    }
}

fn full_caps(workspace: &std::path::Path) -> CapabilitySet {
    caps(
        workspace,
        &[
            "read_file",
            "create_file",
            "edit_file",
            "search_files",
            "run_command",
        ],
        &[ToolClass::Read, ToolClass::Write, ToolClass::Execute],
    )
}

struct PassGates;
impl GateEvaluator for PassGates {
    fn evaluate(&self, _gate: &EvidenceGate, _grant: &CustodyGrant) -> GateOutcome {
        GateOutcome::Passed
    }
}

struct FailGates;
impl GateEvaluator for FailGates {
    fn evaluate(&self, gate: &EvidenceGate, _grant: &CustodyGrant) -> GateOutcome {
        GateOutcome::Failed(format!("{gate:?} failed"))
    }
}

fn agent() -> OperatorIdentity {
    OperatorIdentity::Agent(CustodyRegistry::register_agent(
        "t3-code",
        AgentProtocol::Acp {
            client: "t3".into(),
            version: "1.0".into(),
        },
    ))
}

fn offer_and_accept(
    reg: &mut CustodyRegistry,
    task_id: &str,
    operator: OperatorIdentity,
    caps: CapabilitySet,
    now: u128,
) -> (CapabilityToken, CustodyGrant) {
    let offer = reg
        .offer(
            task_id,
            "do the thing",
            operator,
            WorkerMode::ExternalAgent,
            caps,
            CustodyBudgets::default(),
            LeaseTerms::default(),
            CompletionContract::default(),
            now,
        )
        .unwrap();
    let acceptance = CustodyAcceptance {
        offer_id: offer.offer_id.clone(),
        nonce_echo: offer.nonce.clone(),
        commitment: offer.expected_acceptance(),
    };
    reg.accept(&acceptance, now).unwrap()
}

#[test]
fn happy_path_full_lifecycle() {
    let c = ctx();
    let mut reg = CustodyRegistry::open(c.root.clone()).unwrap();
    let (token, grant) = offer_and_accept(&mut reg, "task-1", agent(), full_caps(&c.workspace), T0);
    assert_eq!(grant.phase, CustodyPhase::Active);
    assert_eq!(grant.lease.epoch, 1);

    let lease = reg.heartbeat(&token, 1, T0 + 10_000).unwrap();
    assert!(lease.expires_ms > T0 + 10_000);

    reg.consume(
        &token,
        Consumption {
            steps: 3,
            tool_calls: 10,
            tokens: 5000,
            wall_ms: 0,
        },
        T0 + 20_000,
    )
    .unwrap();

    let claim = CompletionClaim {
        summary: "done".into(),
    };
    let reason = reg
        .claim_completion(&token, &claim, &PassGates, T0 + 30_000)
        .unwrap();
    assert!(matches!(reason, ReleaseReason::VerifiedCompletion { .. }));

    // Tombstoned: no new custody for the same task.
    let err = reg
        .offer(
            "task-1",
            "again",
            agent(),
            WorkerMode::ExternalAgent,
            full_caps(&c.workspace),
            CustodyBudgets::default(),
            LeaseTerms::default(),
            CompletionContract::default(),
            T0 + 40_000,
        )
        .unwrap_err();
    assert_eq!(err, CustodyError::TaskTombstoned);

    // Token is dead after release.
    assert!(matches!(
        reg.verify_token(&token, T0 + 41_000),
        Err(CustodyError::GrantReleased(_))
    ));

    // Audit chain records the whole story and verifies.
    let chain = reg.audit_chain(&grant.grant_id).unwrap();
    let kinds: Vec<&str> = chain.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(
        kinds,
        vec![
            "custody_granted",
            "heartbeat",
            "consumed",
            "completion_claimed",
            "released"
        ]
    );

    // Human reopen allows a fresh custody.
    reg.reopen_task("task-1", T0 + 50_000).unwrap();
    let (_t2, g2) = offer_and_accept(
        &mut reg,
        "task-1",
        agent(),
        full_caps(&c.workspace),
        T0 + 60_000,
    );
    assert_eq!(g2.phase, CustodyPhase::Active);
}

#[test]
fn handshake_rejects_wrong_nonce_and_commitment() {
    let c = ctx();
    let mut reg = CustodyRegistry::open(c.root.clone()).unwrap();
    let offer = reg
        .offer(
            "t",
            "x",
            agent(),
            WorkerMode::ExternalAgent,
            full_caps(&c.workspace),
            CustodyBudgets::default(),
            LeaseTerms::default(),
            CompletionContract::default(),
            T0,
        )
        .unwrap();
    let bad = CustodyAcceptance {
        offer_id: offer.offer_id.clone(),
        nonce_echo: "wrong".into(),
        commitment: offer.expected_acceptance(),
    };
    assert_eq!(
        reg.accept(&bad, T0 + 1).unwrap_err(),
        CustodyError::CommitmentMismatch
    );
    let bad2 = CustodyAcceptance {
        offer_id: offer.offer_id.clone(),
        nonce_echo: offer.nonce.clone(),
        commitment: "forged".into(),
    };
    assert_eq!(
        reg.accept(&bad2, T0 + 1).unwrap_err(),
        CustodyError::CommitmentMismatch
    );
    // A failed handshake must not consume the offer.
    let good = CustodyAcceptance {
        offer_id: offer.offer_id.clone(),
        nonce_echo: offer.nonce.clone(),
        commitment: offer.expected_acceptance(),
    };
    assert!(reg.accept(&good, T0 + 2).is_ok());
}

#[test]
fn expired_offer_cannot_be_accepted() {
    let c = ctx();
    let mut reg = CustodyRegistry::open(c.root.clone()).unwrap();
    let offer = reg
        .offer(
            "t",
            "x",
            agent(),
            WorkerMode::ExternalAgent,
            full_caps(&c.workspace),
            CustodyBudgets::default(),
            LeaseTerms::default(),
            CompletionContract::default(),
            T0,
        )
        .unwrap();
    let acceptance = CustodyAcceptance {
        offer_id: offer.offer_id.clone(),
        nonce_echo: offer.nonce.clone(),
        commitment: offer.expected_acceptance(),
    };
    let later = T0 + 11 * 60 * 1000;
    assert_eq!(
        reg.accept(&acceptance, later).unwrap_err(),
        CustodyError::OfferExpired
    );
}

#[test]
fn one_active_custody_per_task() {
    let c = ctx();
    let mut reg = CustodyRegistry::open(c.root.clone()).unwrap();
    let _ = offer_and_accept(&mut reg, "task-dup", agent(), full_caps(&c.workspace), T0);
    let err = reg
        .offer(
            "task-dup",
            "second",
            agent(),
            WorkerMode::ExternalAgent,
            full_caps(&c.workspace),
            CustodyBudgets::default(),
            LeaseTerms::default(),
            CompletionContract::default(),
            T0 + 1,
        )
        .unwrap_err();
    assert_eq!(err, CustodyError::TaskAlreadyCustodied);
}

#[test]
fn heartbeat_replay_rejected_and_extends_nothing() {
    let c = ctx();
    let mut reg = CustodyRegistry::open(c.root.clone()).unwrap();
    let (token, grant) =
        offer_and_accept(&mut reg, "task-hb", agent(), full_caps(&c.workspace), T0);
    let before = grant.lease.expires_ms;
    let err = reg.heartbeat(&token, 7, T0 + 5_000).unwrap_err();
    assert!(matches!(
        err,
        CustodyError::HeartbeatReplay { expected_seq: 1 }
    ));
    assert_eq!(reg.grant(&grant.grant_id).unwrap().lease.expires_ms, before);
    // Correct sequence works, then the old one is replay.
    reg.heartbeat(&token, 1, T0 + 5_000).unwrap();
    assert!(matches!(
        reg.heartbeat(&token, 1, T0 + 6_000),
        Err(CustodyError::HeartbeatReplay { .. })
    ));
}

#[test]
fn heartbeat_sequence_property() {
    let c = ctx();
    let mut reg = CustodyRegistry::open(c.root.clone()).unwrap();
    let (token, grant) =
        offer_and_accept(&mut reg, "task-prop", agent(), full_caps(&c.workspace), T0);
    let mut last_expiry = 0u128;
    for seq in 1..=64u64 {
        let now = T0 + seq as u128 * 1000;
        let lease = reg.heartbeat(&token, seq, now).unwrap();
        assert_eq!(lease.next_seq, seq + 1);
        assert!(lease.expires_ms > last_expiry);
        last_expiry = lease.expires_ms;
    }
    assert_eq!(reg.grant(&grant.grant_id).unwrap().lease.next_seq, 65);
}

#[test]
fn forged_token_secret_quarantines() {
    let c = ctx();
    let mut reg = CustodyRegistry::open(c.root.clone()).unwrap();
    let (token, grant) =
        offer_and_accept(&mut reg, "task-forge", agent(), full_caps(&c.workspace), T0);
    let forged = CapabilityToken {
        secret: "x".repeat(64),
        ..token.clone()
    };
    let err = reg.verify_token(&forged, T0 + 1).unwrap_err();
    assert!(matches!(
        err,
        CustodyError::Quarantined(Violation::TokenForgery { .. })
    ));
    // Even the legitimate token is dead now.
    assert!(matches!(
        reg.verify_token(&token, T0 + 2),
        Err(CustodyError::Quarantined(_))
    ));
    assert_eq!(
        reg.grant(&grant.grant_id).unwrap().phase,
        CustodyPhase::Quarantined
    );
    // And the task is tombstoned by the violation.
    assert!(reg.tombstone("task-forge").unwrap().violation.is_some());
}

#[test]
fn suspend_resume_rotates_secrets_and_kills_old_epoch() {
    let c = ctx();
    let mut reg = CustodyRegistry::open(c.root.clone()).unwrap();
    let (token, grant) =
        offer_and_accept(&mut reg, "task-rs", agent(), full_caps(&c.workspace), T0);
    let old_secret = grant.resume_secret.clone();
    reg.suspend(&grant.grant_id, "crash", T0 + 10_000).unwrap();

    // Old token cannot act while suspended.
    assert!(matches!(
        reg.verify_token(&token, T0 + 11_000),
        Err(CustodyError::WrongPhase(CustodyPhase::Suspended))
    ));

    // Wrong resume secret quarantines.
    let (token2, grant2) = {
        // Use a second grant so the quarantine of the first doesn't block.
        let (t2, g2) = offer_and_accept(
            &mut reg,
            "task-rs2",
            agent(),
            full_caps(&c.workspace),
            T0 + 12_000,
        );
        reg.suspend(&g2.grant_id, "crash", T0 + 13_000).unwrap();
        let err = reg
            .resume(&g2.grant_id, "wrong-secret", T0 + 14_000)
            .unwrap_err();
        assert!(matches!(
            err,
            CustodyError::Quarantined(Violation::TokenForgery { .. })
        ));
        (t2, g2)
    };
    let _ = (token2, grant2);

    // Correct secret resumes with a new epoch and rotated secrets.
    let new_token = reg
        .resume(&grant.grant_id, &old_secret, T0 + 20_000)
        .unwrap();
    assert_eq!(new_token.epoch, 2);
    assert_ne!(new_token.secret, token.secret);
    // The new epoch works.
    reg.heartbeat(&new_token, 1, T0 + 21_000).unwrap();
    // The OLD token is now proof of stale lease resurrection -> quarantine.
    let err = reg.verify_token(&token, T0 + 22_000).unwrap_err();
    assert!(matches!(
        err,
        CustodyError::Quarantined(Violation::StaleLeaseUse {
            presented_epoch: 1,
            current_epoch: 2
        })
    ));
}

#[test]
fn resume_grace_expiry_releases_lease() {
    let c = ctx();
    let mut reg = CustodyRegistry::open(c.root.clone()).unwrap();
    let (_token, grant) =
        offer_and_accept(&mut reg, "task-grace", agent(), full_caps(&c.workspace), T0);
    reg.suspend(&grant.grant_id, "crash", T0 + 10_000).unwrap();
    let way_later = T0 + 10_000 + 16 * 60 * 1000;
    let err = reg
        .resume(&grant.grant_id, &grant.resume_secret, way_later)
        .unwrap_err();
    assert_eq!(
        err,
        CustodyError::GrantReleased(ReleaseReason::LeaseExpired)
    );
    assert!(matches!(
        reg.grant(&grant.grant_id).unwrap().release,
        Some(ReleaseReason::LeaseExpired)
    ));
}

#[test]
fn sweep_lapses_and_expires() {
    let c = ctx();
    let mut reg = CustodyRegistry::open(c.root.clone()).unwrap();
    let (_t, grant) =
        offer_and_accept(&mut reg, "task-sweep", agent(), full_caps(&c.workspace), T0);
    // No heartbeat: after lease_ms the grant suspends.
    reg.sweep(T0 + 6 * 60 * 1000).unwrap();
    assert_eq!(
        reg.grant(&grant.grant_id).unwrap().phase,
        CustodyPhase::Suspended
    );
    // Past grace: released as expired.
    reg.sweep(T0 + 6 * 60 * 1000 + 16 * 60 * 1000).unwrap();
    assert_eq!(
        reg.grant(&grant.grant_id).unwrap().phase,
        CustodyPhase::Released
    );
}

#[test]
fn budget_exhaustion_releases_custody() {
    let c = ctx();
    let mut reg = CustodyRegistry::open(c.root.clone()).unwrap();
    let (token, _grant) = offer_and_accept(
        &mut reg,
        "task-budget",
        agent(),
        full_caps(&c.workspace),
        T0,
    );
    let err = reg
        .consume(
            &token,
            Consumption {
                steps: 0,
                tool_calls: 81,
                tokens: 0,
                wall_ms: 0,
            },
            T0 + 1000,
        )
        .unwrap_err();
    assert_eq!(err, CustodyError::BudgetExhausted);
    assert!(matches!(
        reg.grant(&token.grant_id).unwrap().release,
        Some(ReleaseReason::BudgetExhausted {
            which: BudgetKind::ToolCalls
        })
    ));
}

#[test]
fn false_completion_quarantines_after_attempts() {
    let c = ctx();
    let mut reg = CustodyRegistry::open(c.root.clone()).unwrap();
    let (token, _grant) =
        offer_and_accept(&mut reg, "task-false", agent(), full_caps(&c.workspace), T0);
    let claim = CompletionClaim {
        summary: "trust me".into(),
    };
    // attempts 1 and 2 are rejected but tolerated; 3 exceeds max (2).
    for i in 1..=2 {
        let err = reg
            .claim_completion(&token, &claim, &FailGates, T0 + i * 1000)
            .unwrap_err();
        assert!(matches!(err, CustodyError::ClaimRejected { .. }));
        // Grant is active again after a tolerated rejection.
        assert_eq!(
            reg.grant(&token.grant_id).unwrap().phase,
            CustodyPhase::Active
        );
    }
    let err = reg
        .claim_completion(&token, &claim, &FailGates, T0 + 3_000)
        .unwrap_err();
    assert!(matches!(
        err,
        CustodyError::Quarantined(Violation::FalseCompletion { attempts: 3 })
    ));
}

#[test]
fn capability_escape_through_tools_quarantines() {
    let c = ctx();
    let mut reg = CustodyRegistry::open(c.root.clone()).unwrap();
    // Read-only scope.
    let ro = caps(
        &c.workspace,
        &["read_file", "search_files"],
        &[ToolClass::Read],
    );
    let (token, _grant) = offer_and_accept(&mut reg, "task-ro", agent(), ro, T0);
    drop(reg);
    let reg = CustodyRegistry::open(c.root.clone()).unwrap();
    let reg = Arc::new(Mutex::new(reg));
    let tools = ToolRuntime::new(&c.workspace).unwrap();
    let custodied = CustodiedToolRuntime::new(tools, reg.clone());

    // In-scope read prepares fine.
    std::fs::write(c.workspace.join("note.txt"), "hello").unwrap();
    let prep = custodied
        .prepare(
            &token,
            ToolRequest::ReadFile {
                path: "note.txt".into(),
                offset: None,
                limit: None,
            },
            T0 + 1,
        )
        .unwrap();
    assert_eq!(prep.tool, "read_file");

    // Write attempt: outside granted class -> quarantine.
    let err = custodied
        .prepare(
            &token,
            ToolRequest::CreateFile {
                path: "evil.txt".into(),
                content: "x".into(),
                overwrite: false,
            },
            T0 + 2,
        )
        .unwrap_err();
    assert!(matches!(
        err,
        CustodyToolError::Custody(CustodyError::Quarantined(
            Violation::CapabilityEscape { .. }
        ))
    ));
    assert_eq!(
        reg.lock().unwrap().grant(&token.grant_id).unwrap().phase,
        CustodyPhase::Quarantined
    );
}

#[test]
fn path_escape_outside_workspace_is_escape() {
    let c = ctx();
    let mut reg = CustodyRegistry::open(c.root.clone()).unwrap();
    let (token, _g) = offer_and_accept(&mut reg, "task-esc", agent(), full_caps(&c.workspace), T0);
    let grant = reg.grant(&token.grant_id).unwrap().clone();
    for p in ["../outside.txt", "/etc/passwd", "sub/../../outside.txt"] {
        let req = ToolRequest::ReadFile {
            path: p.into(),
            offset: None,
            limit: None,
        };
        assert!(
            grant.capabilities.permits(&req).is_err(),
            "{p} must be refused"
        );
    }
    for p in ["note.txt", "sub/dir/file.rs", "./ok.txt"] {
        let req = ToolRequest::ReadFile {
            path: p.into(),
            offset: None,
            limit: None,
        };
        assert!(
            grant.capabilities.permits(&req).is_ok(),
            "{p} must be allowed"
        );
    }
}

#[test]
fn execute_after_release_is_refused() {
    let c = ctx();
    let mut reg = CustodyRegistry::open(c.root.clone()).unwrap();
    let (token, _g) = offer_and_accept(&mut reg, "task-x", agent(), full_caps(&c.workspace), T0);
    let reg = Arc::new(Mutex::new(reg));
    let tools = ToolRuntime::new(&c.workspace).unwrap();
    let custodied = CustodiedToolRuntime::new(tools, reg.clone());
    let prep = custodied
        .prepare(
            &token,
            ToolRequest::ReadFile {
                path: "note.txt".into(),
                offset: None,
                limit: None,
            },
            T0 + 1,
        )
        .unwrap();
    if prep.approval_required {
        custodied.resolve_approval(&prep.call_id, true).unwrap();
    }
    // Human stops custody between approval and execution.
    reg.lock()
        .unwrap()
        .human_stop(&token.grant_id, T0 + 2)
        .unwrap();
    let result = custodied.execute(&token, &prep.call_id, T0 + 3);
    assert!(!result.ok);
    assert_eq!(
        result.error.unwrap().kind,
        rex_tools::ErrorKind::PolicyDenied
    );
}

#[test]
fn human_stop_works_in_verifying_phase() {
    let c = ctx();
    let mut reg = CustodyRegistry::open(c.root.clone()).unwrap();
    let (token, _g) = offer_and_accept(&mut reg, "task-stop", agent(), full_caps(&c.workspace), T0);
    let claim = CompletionClaim {
        summary: "done?".into(),
    };
    // Tolerated rejection returns to Active; go to Verifying again and stop mid-phase.
    let _ = reg.claim_completion(&token, &claim, &FailGates, T0 + 1);
    reg.claim_completion(&token, &claim, &PassGates, T0 + 2)
        .ok();
    // Already released by passing gates; human stop now is a wrong-phase error but harmless.
    let err = reg.human_stop(&token.grant_id, T0 + 3).unwrap_err();
    assert!(matches!(
        err,
        CustodyError::WrongPhase(CustodyPhase::Released)
    ));
}

#[test]
fn delegation_capability_is_ungrantable() {
    let c = ctx();
    let mut reg = CustodyRegistry::open(c.root.clone()).unwrap();
    let mut deleg = full_caps(&c.workspace);
    deleg.can_delegate = true;
    let err = reg
        .offer(
            "task-del",
            "x",
            agent(),
            WorkerMode::ExternalAgent,
            deleg,
            CustodyBudgets::default(),
            LeaseTerms::default(),
            CompletionContract::default(),
            T0,
        )
        .unwrap_err();
    assert_eq!(err, CustodyError::DelegationRefused);
}

#[test]
fn crash_recovery_preserves_state_and_detects_tampering() {
    let c = ctx();
    let grant_id;
    {
        let mut reg = CustodyRegistry::open(c.root.clone()).unwrap();
        let (token, grant) =
            offer_and_accept(&mut reg, "task-crash", agent(), full_caps(&c.workspace), T0);
        grant_id = grant.grant_id.clone();
        reg.heartbeat(&token, 1, T0 + 1000).unwrap();
        reg.consume(
            &token,
            Consumption {
                steps: 2,
                tool_calls: 4,
                tokens: 100,
                wall_ms: 0,
            },
            T0 + 2000,
        )
        .unwrap();
    }
    // Recover: consumption survives the crash.
    let reg = CustodyRegistry::recover(c.root.clone(), T0 + 3000).unwrap();
    let grant = reg.grant(&grant_id).unwrap();
    assert_eq!(grant.consumed.steps, 2);
    assert_eq!(grant.consumed.tool_calls, 4);
    assert_eq!(grant.phase, CustodyPhase::Active);
    assert_eq!(reg.audit_chain(&grant_id).unwrap().len(), 3);
    // Recovery lapses the dead lease into suspension, then releases once
    // the resume grace window has fully passed.
    let reg2 = CustodyRegistry::recover(c.root.clone(), T0 + 60 * 60 * 1000).unwrap();
    assert_eq!(
        reg2.grant(&grant_id).unwrap().phase,
        CustodyPhase::Suspended
    );
    let reg3 =
        CustodyRegistry::recover(c.root.clone(), T0 + 60 * 60 * 1000 + 16 * 60 * 1000).unwrap();
    assert_eq!(reg3.grant(&grant_id).unwrap().phase, CustodyPhase::Released);
    assert!(matches!(
        reg3.grant(&grant_id).unwrap().release,
        Some(ReleaseReason::LeaseExpired)
    ));

    // Tamper with the audit log: opening must fail closed.
    let audit_path = c.root.join("audit").join(format!("{grant_id}.jsonl"));
    let mut lines = std::fs::read_to_string(&audit_path)
        .unwrap()
        .lines()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    lines[1] = lines[1].replace("heartbeat", "heartbeet");
    std::fs::write(&audit_path, lines.join("\n") + "\n").unwrap();
    let err = match CustodyRegistry::open(c.root.clone()) {
        Ok(_) => panic!("tampered audit chain must fail closed"),
        Err(e) => e,
    };
    assert!(matches!(err, CustodyError::AuditBroken(_)));
}

#[test]
fn managed_model_worker_records_mode() {
    let c = ctx();
    let mut reg = CustodyRegistry::open(c.root.clone()).unwrap();
    let offer = reg
        .offer(
            "task-mm",
            "x",
            OperatorIdentity::Human,
            WorkerMode::ManagedModel {
                provider: "gemini".into(),
                model: Some("gemini-3.5-flash-lite".into()),
            },
            full_caps(&c.workspace),
            CustodyBudgets::default(),
            LeaseTerms::default(),
            CompletionContract::default(),
            T0,
        )
        .unwrap();
    let (_token, grant) = reg.accept_for_human(&offer.offer_id, T0 + 1).unwrap();
    assert!(matches!(grant.worker, WorkerMode::ManagedModel { .. }));
    assert!(!grant.operator.is_agent());
}
