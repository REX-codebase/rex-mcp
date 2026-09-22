//! Adversarial regression suite: every exploit from the independent
//! audit of main d5b913b, reproduced against the rebuilt daemon over its
//! public surface. Each test is one attack; every one must fail closed.
//!
//! The headline fixture is the TON 618 false pass: placeholder 64-hex
//! hashes plus `critic_clean: true` once completed a visual kernel. It
//! must never qualify again.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use rex_daemon::{DaemonPolicy, HarnessDaemon};
use rex_protocol::*;
use tempfile::tempdir;

fn execute_ultra(daemon: &HarnessDaemon, id: &str, task: &str) -> ExecuteResponse {
    daemon
        .execute(ExecuteRequest {
            request_id: id.into(),
            task: task.into(),
            task_id: None,
            resume_handle: None,
            follow_up: None,
            host: HostKind::ClaudeCode,
            operator_is_agent: true,
            ultra: true,
            budgets: None,
            proof: None,
            plan: Some(vec![PlanStep {
                instructions: "write hello".into(),
                acceptance: Some("file exists".into()),
            }]),
        })
        .unwrap()
}

fn cap_of(r: &ExecuteResponse) -> String {
    r.task_capability.clone().expect("capability issued")
}

const VISUAL_TASK: &str = "build a website landing page with animation";

const VALID_DRAFT: &str = r#"{"obligations":[{"id":"ob-1","statement":"hello.txt exists and says hello","proof":{"kind":"file_contains","path":"hello.txt","needle":"hello"}}],"forbidden_regressions":[]}"#;

fn open_ultra(daemon: &HarnessDaemon, ex: &ExecuteResponse) -> UltraViewResponse {
    daemon
        .ultra_open(UltraOpenRequest {
            task_id: ex.task_id.clone(),
            capability: cap_of(ex),
            lease_epoch: ex.lease.epoch,
            contract_draft: Some(VALID_DRAFT.into()),
        })
        .unwrap()
}

fn submit(
    daemon: &HarnessDaemon,
    ex: &ExecuteResponse,
    kind: UltraSubmissionKind,
    request_id: &str,
    candidate_id: &str,
    content: &str,
) -> Result<UltraViewResponse, rex_protocol::ProtocolError> {
    daemon.ultra_submit(UltraSubmitRequest {
        task_id: ex.task_id.clone(),
        capability: cap_of(ex),
        lease_epoch: ex.lease.epoch,
        kind,
        request_id: request_id.into(),
        candidate_id: candidate_id.into(),
        response_hash: rex_protocol::schema::canonical_hash(&content).unwrap(),
        content: content.into(),
    })
}

/// Three sealed candidates in place; returns the awaiting-evidence view.
fn seeded_candidates(daemon: &HarnessDaemon, ex: &ExecuteResponse) -> UltraViewResponse {
    let open = open_ultra(daemon, ex);
    let mut view = open.clone();
    for (i, c) in open.candidate_requests.iter().enumerate() {
        let content = format!(
            "{{\"files\":[{{\"path\":\"hello.txt\",\"content\":\"hello {i}\"}}]}}"
        );
        view = submit(
            daemon,
            ex,
            UltraSubmissionKind::Candidate,
            &c.candidate_id,
            &c.candidate_id,
            &content,
        )
        .unwrap();
    }
    view
}

fn visual_report(thesis: &str, desktop: &str, phone: &str, replay: &str) -> String {
    serde_json::json!({
        "thesis_id": thesis,
        "screenshots": [
            {"viewport": "desktop", "artifact_hash": desktop},
            {"viewport": "phone", "artifact_hash": phone},
        ],
        "interaction_replay_hash": replay,
        "forbidden_patterns_hit": [],
        "critic_clean": true,
    })
    .to_string()
}

fn make_png(width: u32, height: u32, f: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
    let mut rgb = Vec::with_capacity((width * height * 3) as usize);
    for y in 0..height {
        for x in 0..width {
            rgb.extend_from_slice(&f(x, y));
        }
    }
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&rgb).unwrap();
    }
    out
}

/// TON 618, the audit's headline false pass: placeholder 64-hex hashes
/// ("a"*64, "b"*64, "c"*64) plus a host-declared `critic_clean: true`
/// once completed a visual kernel. Reproduced verbatim: every visual
/// report cites hashes no artifact exists for. The kernel must never
/// complete, the task must never complete, and promotion must fail.
#[test]
fn ton_618_placeholder_hashes_never_complete_a_visual_kernel() {
    let d = tempdir().unwrap();
    let w = d.path().join("ws");
    let daemon = HarnessDaemon::open(d.path().join("state"), DaemonPolicy::conservative(&w)).unwrap();
    let ex = execute_ultra(&daemon, "r-ton618", VISUAL_TASK);
    let view = seeded_candidates(&daemon, &ex);
    assert_eq!(view.kernel_state, "awaiting_evidence");
    assert_eq!(view.evidence_requests.len(), 3);

    let mut rejected = 0;
    let mut last_state = String::new();
    for request in &view.evidence_requests {
        let report = visual_report(
            &format!("thesis-{}", request.candidate_id),
            &"a".repeat(64),
            &"b".repeat(64),
            &"c".repeat(64),
        );
        match submit(
            &daemon,
            &ex,
            UltraSubmissionKind::Visual,
            &request.request_id,
            &request.candidate_id,
            &report,
        ) {
            Err(_) => rejected += 1,
            Ok(v) => {
                last_state = v.kernel_state.clone();
                assert_ne!(
                    v.kernel_state, "completed",
                    "placeholder hashes must never qualify a visual candidate"
                );
            }
        }
    }
    let status = daemon
        .status(TaskRefRequest {
            task_id: ex.task_id.clone(),
        })
        .unwrap();
    // Fail-closed either way: reports were refused outright, or the
    // kernel failed and the joined state machine failed the task. Never
    // completed, never promotable.
    assert!(
        rejected == 3 || matches!(status.state, TaskState::Failed),
        "rejected={rejected} last_kernel={last_state} task={:?}",
        status.state
    );
    assert!(!matches!(status.state, TaskState::Completed));
    assert!(daemon
        .ultra_promote(UltraPromoteRequest {
            task_id: ex.task_id.clone(),
            capability: cap_of(&ex),
            lease_epoch: ex.lease.epoch,
        })
        .is_err());
}

/// Finding 1: host-supplied adversary and verifier verdicts are not an
/// input the rebuilt path accepts at all.
#[test]
fn host_supplied_adversary_and_verifier_verdicts_are_rejected() {
    let d = tempdir().unwrap();
    let w = d.path().join("ws");
    let daemon = HarnessDaemon::open(d.path().join("state"), DaemonPolicy::conservative(&w)).unwrap();
    let ex = execute_ultra(&daemon, "r-selfattest", VISUAL_TASK);
    let view = seeded_candidates(&daemon, &ex);
    let request = &view.evidence_requests[0];
    for kind in [UltraSubmissionKind::Adversary, UltraSubmissionKind::Verifier] {
        let err = submit(
            &daemon,
            &ex,
            kind,
            &request.request_id,
            &request.candidate_id,
            "{\"defects\":[],\"status\":\"proven\"}",
        )
        .unwrap_err();
        assert!(
            format!("{err:?}").contains("hosts cannot submit verdicts"),
            "host verdicts are refused: {err:?}"
        );
    }
}

/// Finding 2: an Ultra task cannot complete through ordinary rex_submit.
#[test]
fn ordinary_submit_cannot_complete_an_ultra_task() {
    let d = tempdir().unwrap();
    let w = d.path().join("ws");
    std::fs::create_dir_all(&w).unwrap();
    let daemon = HarnessDaemon::open(d.path().join("state"), DaemonPolicy::conservative(&w)).unwrap();
    let ex = execute_ultra(&daemon, "r-submitbypass", "make hello");
    let next = daemon
        .next(NextRequest {
            task_id: ex.task_id.clone(),
            capability: cap_of(&ex),
            lease_epoch: ex.lease.epoch,
        })
        .unwrap();
    let action = next.next.expect("an action is open");
    let err = daemon
        .submit(SubmitRequest {
            task_id: ex.task_id.clone(),
            capability: cap_of(&ex),
            lease_epoch: ex.lease.epoch,
            action_id: action.action_id,
            narrative: "done, trust me".into(),
            evidence: Default::default(),
        })
        .unwrap_err();
    assert!(format!("{err:?}").contains("ultra"), "{err:?}");
    let status = daemon
        .status(TaskRefRequest {
            task_id: ex.task_id.clone(),
        })
        .unwrap();
    assert!(!matches!(status.state, TaskState::Completed));
}

/// Finding 3: a task id plus the epoch from the unauthenticated status
/// endpoint authorizes nothing without the per-task capability.
#[test]
fn task_id_and_epoch_without_capability_authorize_nothing() {
    let d = tempdir().unwrap();
    let w = d.path().join("ws");
    std::fs::create_dir_all(&w).unwrap();
    let daemon = HarnessDaemon::open(d.path().join("state"), DaemonPolicy::conservative(&w)).unwrap();
    let ex = execute_ultra(&daemon, "r-hijack", "make hello");
    let status = daemon
        .status(TaskRefRequest {
            task_id: ex.task_id.clone(),
        })
        .unwrap();
    let epoch = status.lease.epoch;
    let forged = "forged-capability".to_string();
    assert!(daemon
        .read(ReadRequest {
            task_id: ex.task_id.clone(),
            capability: forged.clone(),
            lease_epoch: epoch,
            path: "hello.txt".into(),
            byte_range: None,
        })
        .is_err());
    assert!(daemon
        .edit(EditRequest {
            task_id: ex.task_id.clone(),
            capability: forged.clone(),
            lease_epoch: epoch,
            path: "hello.txt".into(),
            expected: None,
            replacement: "pwned".into(),
            create: true,
        })
        .is_err());
    assert!(daemon
        .run(RunRequest {
            task_id: ex.task_id.clone(),
            capability: forged.clone(),
            lease_epoch: epoch,
            argv: vec!["echo".into(), "hi".into()],
            timeout_ms: None,
        })
        .is_err());
    assert!(daemon
        .cancel(CancelRequest {
            task_id: ex.task_id.clone(),
            capability: forged.clone(),
            reason: Some("attacker cancels".into()),
        })
        .is_err());
    assert!(daemon
        .ultra_open(UltraOpenRequest {
            task_id: ex.task_id.clone(),
            capability: forged.clone(),
            lease_epoch: epoch,
            contract_draft: Some(VALID_DRAFT.into()),
        })
        .is_err());
    assert!(daemon
        .ultra_promote(UltraPromoteRequest {
            task_id: ex.task_id.clone(),
            capability: forged,
            lease_epoch: epoch,
        })
        .is_err());
    let status = daemon
        .status(TaskRefRequest {
            task_id: ex.task_id.clone(),
        })
        .unwrap();
    assert!(matches!(status.state, TaskState::Active));
}

/// Findings 2 and 12: promotion cannot run before the kernel completes,
/// and a completed task cannot be promoted again.
#[test]
fn promotion_requires_a_completed_kernel_on_a_live_task() {
    let d = tempdir().unwrap();
    let w = d.path().join("ws");
    std::fs::create_dir_all(&w).unwrap();
    let daemon = HarnessDaemon::open(d.path().join("state"), DaemonPolicy::conservative(&w)).unwrap();
    let ex = execute_ultra(&daemon, "r-promotegate", "make hello");
    // Before any candidate: the kernel is not complete.
    assert!(daemon
        .ultra_promote(UltraPromoteRequest {
            task_id: ex.task_id.clone(),
            capability: cap_of(&ex),
            lease_epoch: ex.lease.epoch,
        })
        .is_err());
    let view = seeded_candidates(&daemon, &ex);
    assert_eq!(view.kernel_state, "completed");
    let first = daemon
        .ultra_promote(UltraPromoteRequest {
            task_id: ex.task_id.clone(),
            capability: cap_of(&ex),
            lease_epoch: ex.lease.epoch,
        })
        .unwrap();
    assert_eq!(first.state, "committed");
    // The joined transition completed the task; a replayed promote fails.
    assert!(daemon
        .ultra_promote(UltraPromoteRequest {
            task_id: ex.task_id.clone(),
            capability: cap_of(&ex),
            lease_epoch: ex.lease.epoch,
        })
        .is_err());
}

/// Findings 1 and 14: visual evidence must resolve to real artifact
/// bytes the daemon can decode. A nonexistent digest and a digest of
/// non-image bytes both fail; neither can qualify a candidate.
#[test]
fn visual_reports_must_resolve_to_real_decodable_bytes() {
    let d = tempdir().unwrap();
    let w = d.path().join("ws");
    let daemon = HarnessDaemon::open(d.path().join("state"), DaemonPolicy::conservative(&w)).unwrap();
    let ex = execute_ultra(&daemon, "r-visualdrift", VISUAL_TASK);
    let view = seeded_candidates(&daemon, &ex);
    let put = |bytes: &[u8]| {
        daemon
            .artifact_put(ArtifactPutRequest {
                task_id: ex.task_id.clone(),
                capability: cap_of(&ex),
                lease_epoch: ex.lease.epoch,
                kind: "screenshot".into(),
                bytes_base64: B64.encode(bytes),
                candidate_id: None,
                round: None,
            })
            .unwrap()
            .sha256
    };
    let real = put(&make_png(64, 64, |x, y| {
        [(x % 16) as u8 * 16, (y % 16) as u8 * 16, 3]
    }));
    let garbage = put(b"these are not pixels");
    let nonexistent = "f".repeat(64);
    let cases = [
        visual_report("t", &nonexistent, &real, &real),
        visual_report("t", &real, &garbage, &real),
        visual_report("t", &garbage, &garbage, &garbage),
    ];
    for (i, report) in cases.iter().enumerate() {
        let request = &view.evidence_requests[i];
        match submit(
            &daemon,
            &ex,
            UltraSubmissionKind::Visual,
            &request.request_id,
            &request.candidate_id,
            report,
        ) {
            Err(_) => {}
            Ok(v) => assert_ne!(v.kernel_state, "completed"),
        }
    }
    assert!(daemon
        .ultra_promote(UltraPromoteRequest {
            task_id: ex.task_id.clone(),
            capability: cap_of(&ex),
            lease_epoch: ex.lease.epoch,
        })
        .is_err());
}

/// Finding 14: artifact digests are content-addressed and bound - the
/// same bytes cannot be re-registered under a second candidate.
#[test]
fn artifacts_cannot_be_rebound_across_candidates() {
    let d = tempdir().unwrap();
    let w = d.path().join("ws");
    let daemon = HarnessDaemon::open(d.path().join("state"), DaemonPolicy::conservative(&w)).unwrap();
    let ex = execute_ultra(&daemon, "r-artifactbind", VISUAL_TASK);
    let open = open_ultra(&daemon, &ex);
    let bytes = make_png(32, 32, |x, y| [x as u8, y as u8, 9]);
    let put_for = |candidate: &str| {
        daemon.artifact_put(ArtifactPutRequest {
            task_id: ex.task_id.clone(),
            capability: cap_of(&ex),
            lease_epoch: ex.lease.epoch,
            kind: "screenshot".into(),
            bytes_base64: B64.encode(&bytes),
            candidate_id: Some(candidate.into()),
            round: None,
        })
    };
    let first = &open.candidate_requests[0].candidate_id;
    let second = &open.candidate_requests[1].candidate_id;
    put_for(first).unwrap();
    assert!(
        put_for(second).is_err(),
        "one digest cannot be reused across candidates"
    );
}
