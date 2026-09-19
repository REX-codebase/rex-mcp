//! Installed agent backends for REX Harness.
//!
//! These adapters never impersonate a provider and never read another app's
//! tokens, cookies, keychain, or auth files. They execute the vendor's own
//! installed CLI through its documented automation interface. Each child runs
//! with the safest documented permission mode: REX remains the approval
//! boundary, while the child can inspect and reason but cannot silently take
//! privileged actions in the user's real workspace.
//!
//! Route policy (reviewed 2026-09-19): OpenAI Codex CLI is the only retained
//! installed-agent backend. OpenAI's own documentation covers `codex exec`
//! non-interactive automation with the CLI's own ChatGPT or API-key sign-in,
//! and the OpenAI Terms of Use contain no clause barring third-party software
//! from driving the official client. The Google Antigravity, Claude Code,
//! Cursor and Pi adapters were removed the same day: their providers' current
//! terms either expressly prohibit third-party harness use of a consumer
//! subscription (Google Antigravity Additional Terms section 6; Anthropic's
//! third-party prohibition) or publish no third-party harness entitlement for
//! the subscription the CLI would spend (Cursor, Pi). See
//! `docs/subscription-policy.md` and `crates/rex-installed-agents/COMPATIBILITY.md`.

pub mod run_service;
pub use run_service::{DiffEntry, DiffKind, DiffSummary, PromotionState, RunManager, RunSnapshot, RunStatus};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstalledAgentId {
    Codex,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectionState {
    Ready,
    InstalledAuthUnknown,
    Missing,
    UnsupportedVersion,
    SupportNeedsReview,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompatibilityContract {
    pub verified_on: &'static str,
    pub documented_interface: &'static str,
    pub entitlement_note: &'static str,
    pub fail_closed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledAgentSummary {
    pub id: InstalledAgentId,
    pub name: &'static str,
    pub executable: &'static str,
    pub state: DetectionState,
    pub version: Option<String>,
    pub automation: &'static str,
    pub auth_boundary: &'static str,
    pub docs_url: &'static str,
    pub approval_boundary: &'static str,
    pub compatibility: CompatibilityContract,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEvent {
    pub event: String,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledAgentRun {
    pub backend: InstalledAgentId,
    pub status: String,
    pub exit_code: Option<i32>,
    pub events: Vec<AgentEvent>,
    pub stderr: String,
    pub staging_workspace: String,
}

/// Operator-selected generation controls. `None` always means the vendor
/// CLI's own default - REX never invents a model name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunOptions {
    pub model: Option<String>,
    pub effort: Option<String>,
}

#[derive(Debug)]
pub enum InstalledAgentError {
    Missing(String),
    Io(String),
    Invalid(String),
    Child(String),
}
impl std::fmt::Display for InstalledAgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing(s) | Self::Io(s) | Self::Invalid(s) | Self::Child(s) => f.write_str(s),
        }
    }
}
impl std::error::Error for InstalledAgentError {}
impl serde::Serialize for InstalledAgentError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

type Args = Vec<OsString>;

#[derive(Clone, Copy)]
struct Spec {
    id: InstalledAgentId,
    name: &'static str,
    executable: &'static str,
    automation: &'static str,
    auth_boundary: &'static str,
    docs_url: &'static str,
    entitlement_note: &'static str,
}

/// Date the Codex route's interface and terms basis was last re-verified
/// against first-party OpenAI sources. `docs/codex-compatibility.md` carries
/// the full reviewable contract; bump both together.
pub const CODEX_CONTRACT_VERIFIED_ON: &str = "2026-09-19";

const SPECS: [Spec;1] = [
    Spec{id:InstalledAgentId::Codex,name:"Codex CLI",executable:"codex",automation:"codex exec JSONL",auth_boundary:"Codex CLI owns ChatGPT/API authentication",docs_url:"https://developers.openai.com/codex/non-interactive-mode",entitlement_note:"OpenAI documents codex exec non-interactive automation with the CLI's own ChatGPT or API-key sign-in; auth never passes through REX. API keys remain OpenAI's recommended automation route"},
];

pub(crate) fn spec(id: InstalledAgentId) -> Spec {
    *SPECS
        .iter()
        .find(|s| s.id == id)
        .expect("registered backend")
}

pub fn discover() -> Vec<InstalledAgentSummary> {
    SPECS.iter().map(|s| detect(*s)).collect()
}

/// Offline contract probe for the retained Codex route: the documented
/// non-interactive flags must still exist in the installed CLI. This runs the
/// binary's own help text only - no network, no auth, no model call. If the
/// flags changed, REX fails closed until the adapter is re-reviewed.
fn codex_exec_interface_matches(exe: &Path) -> bool {
    Command::new(exe)
        .args(["exec", "--help"])
        .stdin(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            let text = String::from_utf8_lossy(&o.stdout);
            ["--json", "--sandbox", "--skip-git-repo-check"]
                .iter()
                .all(|flag| text.contains(flag))
        })
        .unwrap_or(false)
}

fn detect(s: Spec) -> InstalledAgentSummary {
    let found = find_on_path(s.executable);
    let version = found
        .as_ref()
        .and_then(|p| {
            Command::new(p)
                .arg("--version")
                .stdin(Stdio::null())
                .output()
                .ok()
        })
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        });
    let state = if found.is_none() {
        DetectionState::Missing
    } else if version.is_none() {
        DetectionState::UnsupportedVersion
    } else if !codex_exec_interface_matches(found.as_ref().expect("checked above")) {
        DetectionState::SupportNeedsReview
    } else {
        DetectionState::InstalledAuthUnknown
    };
    InstalledAgentSummary{id:s.id,name:s.name,executable:s.executable,state,version,automation:s.automation,auth_boundary:s.auth_boundary,docs_url:s.docs_url,approval_boundary:"REX owns approval. The child never receives a skip-permissions flag and runs against an isolated staging copy.",compatibility:CompatibilityContract{verified_on:CODEX_CONTRACT_VERIFIED_ON,documented_interface:s.automation,entitlement_note:s.entitlement_note,fail_closed:true}}
}

pub(crate) fn find_on_path(exe: &str) -> Option<PathBuf> {
    let candidate = Path::new(exe);
    if candidate.components().count() > 1 && candidate.is_file() {
        return Some(candidate.to_path_buf());
    }
    env::var_os("PATH").and_then(|v| {
        env::split_paths(&v)
            .map(|d| d.join(exe))
            .find(|p| p.is_file())
    })
}

fn safe_args(id: InstalledAgentId, prompt: &str) -> (Args, Option<String>) {
    safe_args_with_options(id, prompt, &RunOptions::default())
        .expect("default options are valid for every backend")
}

/// Build the documented sandboxed command for a backend, threading explicit
/// generation controls only through interfaces verified to accept them.
/// Model and effort values travel as separate argv entries - never through a
/// shell - and a backend whose control surface is not yet verified fails
/// closed instead of silently dropping or mangling the selection. No
/// retained backend currently has a verified model/effort control surface.
pub fn safe_args_with_options(
    id: InstalledAgentId,
    prompt: &str,
    options: &RunOptions,
) -> Result<(Args, Option<String>), InstalledAgentError> {
    if options.model.is_some() || options.effort.is_some() {
        return Err(InstalledAgentError::Invalid(format!(
            "model/effort selection is not yet verified for {}; run it with its own defaults",
            spec(id).name
        )));
    }
    Ok(safe_args_verified(id, prompt, options))
}

fn safe_args_verified(id: InstalledAgentId, prompt: &str, _options: &RunOptions) -> (Args, Option<String>) {
    match id {
        InstalledAgentId::Codex => (
            vec![
                "exec".into(),
                "--json".into(),
                "--sandbox".into(),
                "read-only".into(),
                "--skip-git-repo-check".into(),
                prompt.into(),
            ],
            None,
        ),
    }
}

/// Fail closed before spawning: the installed executable must still match
/// the reviewed interface contract. Detection alone is display-only; this is
/// the run-time gate.
fn enforce_contract(exe: &Path, s: Spec) -> Result<(), InstalledAgentError> {
    if codex_exec_interface_matches(exe) {
        Ok(())
    } else {
        Err(InstalledAgentError::Invalid(format!(
            "{} no longer matches the reviewed {} interface contract; support needs review before further runs",
            s.name, CODEX_CONTRACT_VERIFIED_ON
        )))
    }
}

pub fn run(
    id: InstalledAgentId,
    prompt: &str,
    workspace: &Path,
) -> Result<InstalledAgentRun, InstalledAgentError> {
    if prompt.trim().is_empty() {
        return Err(InstalledAgentError::Invalid("task is empty".into()));
    }
    if !workspace.is_dir() {
        return Err(InstalledAgentError::Invalid(
            "workspace is not a directory".into(),
        ));
    }
    let s = spec(id);
    let exe = find_on_path(s.executable)
        .ok_or_else(|| InstalledAgentError::Missing(format!("{} is not installed", s.name)))?;
    enforce_contract(&exe, s)?;
    run_with_executable(id, prompt, workspace, &exe)
}

fn run_with_executable(
    id: InstalledAgentId,
    prompt: &str,
    workspace: &Path,
    executable: &Path,
) -> Result<InstalledAgentRun, InstalledAgentError> {
    let staging = stage_workspace(workspace)?;
    let (args, input) = safe_args(id, prompt);
    let child = Command::new(executable)
        .args(args)
        .current_dir(&staging)
        .env("REX_INSTALLED_AGENT", "1")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| InstalledAgentError::Child(e.to_string()))?;
    collect_child(id, child, input, staging)
}

struct ChildGuard(Option<Child>);
impl ChildGuard {
    fn child(&mut self) -> &mut Child {
        self.0.as_mut().expect("child present")
    }
    fn take(&mut self) -> Child {
        self.0.take().expect("child present")
    }
}
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Per-line stream parsing shared by the buffered and live lifecycles.
pub mod collect_events {
    use super::{AgentEvent, InstalledAgentError, InstalledAgentId};
    use serde_json::Value;

    pub fn parse_line(_id: InstalledAgentId, line: &str) -> Result<AgentEvent, InstalledAgentError> {
        let payload: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => Value::String(line.to_string()),
        };
        let event = payload
            .get("type")
            .or_else(|| payload.get("event"))
            .and_then(Value::as_str)
            .unwrap_or("output")
            .to_string();
        Ok(AgentEvent { event, payload })
    }
}

fn collect_child(
    id: InstalledAgentId,
    child: Child,
    input: Option<String>,
    staging: PathBuf,
) -> Result<InstalledAgentRun, InstalledAgentError> {
    let mut child = ChildGuard(Some(child));
    if let Some(line) = input {
        let mut stdin = child
            .child()
            .stdin
            .take()
            .ok_or_else(|| InstalledAgentError::Child("child stdin unavailable".into()))?;
        // A child may legitimately close stdin before consuming the turn
        // (fast failure, protocol exit); a broken pipe there is not a REX
        // error - the child's own stream and exit decide the verdict.
        match writeln!(stdin, "{line}") {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
            Err(e) => return Err(InstalledAgentError::Io(e.to_string())),
        }
        // Closing stdin is the documented graceful end-of-session signal.
        drop(stdin);
    }
    let stdout = child
        .child()
        .stdout
        .take()
        .ok_or_else(|| InstalledAgentError::Child("child stdout unavailable".into()))?;
    let stderr = child
        .child()
        .stderr
        .take()
        .ok_or_else(|| InstalledAgentError::Child("child stderr unavailable".into()))?;
    // Drain stderr concurrently so a diagnostic-heavy child cannot deadlock on a full pipe.
    let stderr_thread = thread::spawn(move || {
        let mut out = String::new();
        let mut reader = BufReader::new(stderr);
        std::io::Read::read_to_string(&mut reader, &mut out).map(|_| out)
    });
    let mut events = Vec::new();
    for line in BufReader::new(stdout).lines() {
        let line = line.map_err(|e| InstalledAgentError::Io(e.to_string()))?;
        if line.trim().is_empty() {
            continue;
        }
        events.push(collect_events::parse_line(id, &line)?);
    }
    let output = child
        .take()
        .wait_with_output()
        .map_err(|e| InstalledAgentError::Child(e.to_string()))?;
    let stderr = stderr_thread
        .join()
        .map_err(|_| InstalledAgentError::Child("stderr reader panicked".into()))?
        .map_err(|e| InstalledAgentError::Io(e.to_string()))?;
    let status = validate_codex_stream(&events, output.status.code())?;
    Ok(InstalledAgentRun {
        backend: id,
        status,
        exit_code: output.status.code(),
        events,
        stderr,
        staging_workspace: staging.to_string_lossy().into_owned(),
    })
}

/// Completion gate for the documented `codex exec --json` stream, reviewed
/// 2026-09-19 against https://developers.openai.com/codex/non-interactive-mode.
/// The stream must open with `thread.started`, stay inside the documented
/// event vocabulary (`thread.started`, `turn.started`, `turn.completed`,
/// `turn.failed`, `error`, `item.*`), and end with exactly one terminal
/// `turn.completed`; `turn.failed`, an `error` event, an unknown event type,
/// or a non-zero exit each fail the run closed. Exit code alone never counts
/// as success.
pub(crate) fn validate_codex_stream(
    events: &[AgentEvent],
    exit_code: Option<i32>,
) -> Result<String, InstalledAgentError> {
    if events.is_empty() {
        return Err(InstalledAgentError::Invalid(
            "Codex emitted no events".into(),
        ));
    }
    if events[0].event != "thread.started" {
        return Err(InstalledAgentError::Invalid(
            "Codex stream must begin with thread.started".into(),
        ));
    }
    let mut thread_starts = 0;
    let mut terminal_count = 0;
    let mut terminal_failure: Option<String> = None;
    for (index, event) in events.iter().enumerate() {
        let kind = event.event.as_str();
        let terminal = matches!(kind, "turn.completed" | "turn.failed" | "error");
        let known = matches!(kind, "thread.started" | "turn.started")
            || terminal
            || kind.starts_with("item.");
        if !known {
            return Err(InstalledAgentError::Invalid(format!(
                "unknown Codex output event: {kind}"
            )));
        }
        if kind == "thread.started" {
            thread_starts += 1;
            if index != 0 {
                return Err(InstalledAgentError::Invalid(
                    "thread.started appeared after stream start".into(),
                ));
            }
        }
        if terminal {
            terminal_count += 1;
            if index + 1 != events.len() {
                return Err(InstalledAgentError::Invalid(
                    "terminal event was not the last event".into(),
                ));
            }
            if kind != "turn.completed" {
                let detail = event
                    .payload
                    .get("message")
                    .or_else(|| event.payload.get("error"))
                    .and_then(Value::as_str)
                    .unwrap_or(kind)
                    .to_string();
                terminal_failure = Some(format!("Codex terminal event was {kind}: {detail}"));
            }
        }
    }
    if thread_starts != 1 || terminal_count != 1 {
        return Err(InstalledAgentError::Invalid(
            "Codex stream requires exactly one thread.started and one terminal event".into(),
        ));
    }
    if let Some(failure) = terminal_failure {
        return Err(InstalledAgentError::Child(failure));
    }
    if exit_code != Some(0) {
        return Err(InstalledAgentError::Child(format!(
            "Codex exited with {exit_code:?} after its result"
        )));
    }
    Ok("completed".into())
}

pub(crate) fn stage_workspace(source: &Path) -> Result<PathBuf, InstalledAgentError> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| InstalledAgentError::Io(e.to_string()))?
        .as_nanos();
    let dest = env::temp_dir().join(format!("rex-agent-staging-{stamp}"));
    fs::create_dir_all(&dest).map_err(|e| InstalledAgentError::Io(e.to_string()))?;
    copy_tree(source, &dest)?;
    Ok(dest)
}
fn copy_tree(src: &Path, dst: &Path) -> Result<(), InstalledAgentError> {
    for item in fs::read_dir(src).map_err(|e| InstalledAgentError::Io(e.to_string()))? {
        let item = item.map_err(|e| InstalledAgentError::Io(e.to_string()))?;
        let name = item.file_name();
        if matches!(
            name.to_str(),
            Some(".git" | "target" | "node_modules" | ".rex")
        ) {
            continue;
        };
        let from = item.path();
        let to = dst.join(name);
        let ty = item
            .file_type()
            .map_err(|e| InstalledAgentError::Io(e.to_string()))?;
        if ty.is_dir() {
            fs::create_dir_all(&to).map_err(|e| InstalledAgentError::Io(e.to_string()))?;
            copy_tree(&from, &to)?
        } else if ty.is_file() {
            fs::copy(&from, &to).map_err(|e| InstalledAgentError::Io(e.to_string()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: &str, payload: Value) -> AgentEvent {
        AgentEvent {
            event: kind.into(),
            payload,
        }
    }
    fn valid_events() -> Vec<AgentEvent> {
        vec![
            event(
                "thread.started",
                serde_json::json!({"type":"thread.started","thread_id":"0199a213-81c0-7800-8aa1-bbab2a035a53"}),
            ),
            event("turn.started", serde_json::json!({"type":"turn.started"})),
            event(
                "item.completed",
                serde_json::json!({"type":"item.completed","item":{"id":"item_3","type":"agent_message","text":"done"}}),
            ),
            event(
                "turn.completed",
                serde_json::json!({"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":2}}),
            ),
        ]
    }

    #[test]
    fn every_backend_has_a_documented_https_interface() {
        for s in SPECS {
            assert!(s.docs_url.starts_with("https://"));
            assert!(!s.automation.is_empty());
        }
    }
    #[test]
    fn only_codex_is_retained() {
        assert_eq!(SPECS.len(), 1);
        assert_eq!(SPECS[0].id, InstalledAgentId::Codex);
        assert_eq!(SPECS[0].executable, "codex");
    }
    #[test]
    fn contract_has_a_review_date() {
        assert_eq!(CODEX_CONTRACT_VERIFIED_ON, "2026-09-19");
    }
    #[test]
    fn codex_command_is_the_documented_sandboxed_contract() {
        let (args, input) = safe_args(InstalledAgentId::Codex, "hello");
        let args = args.iter().map(|x| x.to_string_lossy()).collect::<Vec<_>>();
        assert_eq!(
            args,
            [
                "exec",
                "--json",
                "--sandbox",
                "read-only",
                "--skip-git-repo-check",
                "hello"
            ]
        );
        assert!(input.is_none());
    }
    #[test]
    fn safe_commands_never_skip_permissions_or_add_allow_rules() {
        for s in SPECS {
            let (a, _) = safe_args(s.id, "inspect");
            let text = a
                .iter()
                .map(|x| x.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ");
            assert!(!text.contains("dangerously"));
            assert!(!text.contains("bypass"));
            assert!(!text.contains("permissions.allow"));
            assert!(!text.contains("full-auto"));
        }
    }
    #[test]
    fn codex_stays_contained_in_read_only_sandbox() {
        let (codex, _) = safe_args(InstalledAgentId::Codex, "x");
        assert!(codex
            .windows(2)
            .any(|v| v[0] == "--sandbox" && v[1] == "read-only"));
    }
    #[test]
    fn model_and_effort_fail_closed() {
        for options in [
            RunOptions {
                model: Some("gpt-5-codex".into()),
                effort: None,
            },
            RunOptions {
                model: None,
                effort: Some("high".into()),
            },
        ] {
            assert!(safe_args_with_options(InstalledAgentId::Codex, "x", &options).is_err());
        }
    }
    #[test]
    fn accepts_documented_stream_with_one_terminal_completion() {
        assert_eq!(
            validate_codex_stream(&valid_events(), Some(0)).unwrap(),
            "completed"
        );
    }
    #[test]
    fn failed_turn_is_not_completion() {
        let mut events = valid_events();
        *events.last_mut().unwrap() = event(
            "turn.failed",
            serde_json::json!({"type":"turn.failed","message":"model error"}),
        );
        assert!(validate_codex_stream(&events, Some(1)).is_err());
    }
    #[test]
    fn error_event_is_not_completion() {
        let mut events = valid_events();
        *events.last_mut().unwrap() =
            event("error", serde_json::json!({"type":"error","message":"boom"}));
        assert!(validate_codex_stream(&events, Some(0)).is_err());
    }
    #[test]
    fn missing_or_misplaced_thread_start_fails_closed() {
        let mut e = valid_events();
        e.swap(0, 1);
        assert!(validate_codex_stream(&e, Some(0)).is_err());
        assert!(validate_codex_stream(&valid_events()[1..], Some(0)).is_err());
    }
    #[test]
    fn event_after_terminal_and_duplicate_terminal_fail_closed() {
        let mut e = valid_events();
        e.push(event("turn.started", serde_json::json!({"type":"turn.started"})));
        assert!(validate_codex_stream(&e, Some(0)).is_err());
        let mut e = valid_events();
        e.push(e.last().unwrap().clone());
        assert!(validate_codex_stream(&e, Some(0)).is_err());
    }
    #[test]
    fn unknown_event_type_fails_closed() {
        let mut e = valid_events();
        e.insert(
            1,
            event("future.event", serde_json::json!({"type":"future.event"})),
        );
        assert!(validate_codex_stream(&e, Some(0)).is_err());
    }
    #[test]
    fn non_zero_exit_is_not_completion() {
        assert!(validate_codex_stream(&valid_events(), Some(3)).is_err());
        assert!(validate_codex_stream(&valid_events(), None).is_err());
    }
    #[test]
    fn empty_stream_is_not_completion() {
        assert!(validate_codex_stream(&[], Some(0)).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn parser_failure_terminates_and_reaps_child() {
        use std::os::unix::fs::PermissionsExt;
        let root = env::temp_dir().join(format!("rex-codex-fixture-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let fake = root.join("codex-fixture");
        fs::write(
            &fake,
            "#!/bin/sh\nprintf '%s\\n' 'this is not json at all {'\n",
        )
        .unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o700)).unwrap();
        let started = std::time::Instant::now();
        let err = run_with_executable(InstalledAgentId::Codex, "x", &root, &fake).unwrap_err();
        assert!(err.to_string().contains("thread.started"));
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        let _ = fs::remove_dir_all(&root);
    }
    #[cfg(unix)]
    #[test]
    fn child_guard_kills_and_reaps_on_early_return() {
        let child = Command::new("sh").args(["-c", "sleep 30"]).spawn().unwrap();
        let pid = child.id();
        {
            let _guard = ChildGuard(Some(child));
        }
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    }
    #[test]
    fn copy_tree_ignores_links_and_build_or_secret_state() {
        let root = env::temp_dir().join(format!("rex-stage-fixture-{}", std::process::id()));
        let source = root.join("src");
        let dest = root.join("dst");
        fs::create_dir_all(source.join(".git")).unwrap();
        fs::create_dir_all(&dest).unwrap();
        fs::write(source.join("ok.txt"), "ok").unwrap();
        fs::write(source.join(".git/secret"), "no").unwrap();
        copy_tree(&source, &dest).unwrap();
        assert!(dest.join("ok.txt").exists());
        assert!(!dest.join(".git").exists());
        let _ = fs::remove_dir_all(root);
    }
}
