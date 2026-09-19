//! Installed agent backends for REX Harness.
//!
//! These adapters never impersonate a provider and never read another app's
//! tokens, cookies, keychain, or auth files. They execute the vendor's own
//! installed CLI through its documented automation interface. Each child runs
//! with the safest documented permission mode: REX remains the approval
//! boundary, while the child can inspect and reason but cannot silently take
//! privileged actions in the user's real workspace.

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
    Cursor,
    Codex,
    ClaudeCode,
    Pi,
    GoogleAntigravity,
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

const SPECS: [Spec;5] = [
    Spec{id:InstalledAgentId::Cursor,name:"Cursor Agent",executable:"agent",automation:"ACP over stdio; headless stream-json fallback",auth_boundary:"Cursor CLI owns browser login and subscription access",docs_url:"https://cursor.com/docs/cli/acp",entitlement_note:"Uses Cursor CLI authentication; availability follows the user's Cursor plan and current CLI terms"},
    Spec{id:InstalledAgentId::Codex,name:"Codex CLI",executable:"codex",automation:"codex exec JSONL",auth_boundary:"Codex CLI owns ChatGPT/API authentication",docs_url:"https://developers.openai.com/codex/non-interactive-mode",entitlement_note:"OpenAI documents ChatGPT sign-in for Codex CLI; API keys remain the recommended ordinary automation route"},
    Spec{id:InstalledAgentId::ClaudeCode,name:"Claude Code",executable:"claude",automation:"claude -p stream-json",auth_boundary:"Claude Code owns Pro/Max/API authentication",docs_url:"https://docs.anthropic.com/en/docs/claude-code/headless",entitlement_note:"Uses Claude Code's own accepted Pro/Max or API login; never the bare/API-only path for subscription reuse"},
    Spec{id:InstalledAgentId::Pi,name:"Pi",executable:"pi",automation:"Pi JSONL RPC over stdin/stdout",auth_boundary:"Pi owns provider login; entitlement depends on its configured provider",docs_url:"https://pi.dev/docs/latest/rpc",entitlement_note:"RPC is supported; each configured provider's current subscription entitlement is evaluated by Pi and may require billed usage"},
    Spec{id:InstalledAgentId::GoogleAntigravity,name:"Google Antigravity",executable:"agy",automation:"agy stream-json input/output",auth_boundary:"Antigravity owns Google keyring or explicit Gemini API authentication",docs_url:"https://antigravity.google/docs/cli/headless/",entitlement_note:"Uses Antigravity's own secure-keyring Google session or explicit Gemini API-key mode"},
];

fn spec(id: InstalledAgentId) -> Spec {
    *SPECS
        .iter()
        .find(|s| s.id == id)
        .expect("registered backend")
}

pub fn discover() -> Vec<InstalledAgentSummary> {
    SPECS.iter().map(|s| detect(*s)).collect()
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
    } else if s.id == InstalledAgentId::GoogleAntigravity {
        match version.as_deref() {
            Some(value)
                if value
                    .split(|c: char| !(c.is_ascii_digit() || c == '.'))
                    .any(|part| part == "1.2.7") =>
            {
                DetectionState::InstalledAuthUnknown
            }
            _ => DetectionState::UnsupportedVersion,
        }
    } else if version.is_some() {
        DetectionState::InstalledAuthUnknown
    } else {
        DetectionState::UnsupportedVersion
    };
    InstalledAgentSummary{id:s.id,name:s.name,executable:s.executable,state,version,automation:s.automation,auth_boundary:s.auth_boundary,docs_url:s.docs_url,approval_boundary:"REX owns approval. The child never receives a skip-permissions flag and runs against an isolated staging copy.",compatibility:CompatibilityContract{verified_on:"2026-09-19",documented_interface:s.automation,entitlement_note:s.entitlement_note,fail_closed:true}}
}

fn find_on_path(exe: &str) -> Option<PathBuf> {
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
    match id {
        InstalledAgentId::Cursor => (
            vec![
                "-p".into(),
                prompt.into(),
                "--output-format".into(),
                "stream-json".into(),
                "--sandbox".into(),
                "enabled".into(),
            ],
            None,
        ),
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
        InstalledAgentId::ClaudeCode => (
            vec![
                "-p".into(),
                prompt.into(),
                "--output-format".into(),
                "stream-json".into(),
                "--permission-mode".into(),
                "plan".into(),
                "--verbose".into(),
            ],
            None,
        ),
        InstalledAgentId::Pi => (
            vec!["--mode".into(), "rpc".into()],
            Some(serde_json::json!({"type":"prompt","message":prompt}).to_string()),
        ),
        InstalledAgentId::GoogleAntigravity => (
            vec![
                "--input-format".into(),
                "stream-json".into(),
                "--output-format".into(),
                "stream-json".into(),
                "--sandbox".into(),
            ],
            Some(serde_json::json!({"event":"user","message":{"content":prompt}}).to_string()),
        ),
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
        writeln!(stdin, "{line}").map_err(|e| InstalledAgentError::Io(e.to_string()))?;
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
        let payload: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(error) if id == InstalledAgentId::GoogleAntigravity => {
                return Err(InstalledAgentError::Invalid(format!(
                    "malformed agent event: {error}"
                )))
            }
            Err(_) => Value::String(line),
        };
        let event = payload
            .get("event")
            .or_else(|| payload.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("output")
            .to_string();
        events.push(AgentEvent { event, payload });
    }
    let output = child
        .take()
        .wait_with_output()
        .map_err(|e| InstalledAgentError::Child(e.to_string()))?;
    let stderr = stderr_thread
        .join()
        .map_err(|_| InstalledAgentError::Child("stderr reader panicked".into()))?
        .map_err(|e| InstalledAgentError::Io(e.to_string()))?;
    let status = if id == InstalledAgentId::GoogleAntigravity {
        validate_antigravity_stream(&events, &stderr, output.status.code())?
    } else if output.status.success() {
        "completed".into()
    } else {
        "failed".into()
    };
    Ok(InstalledAgentRun {
        backend: id,
        status,
        exit_code: output.status.code(),
        events,
        stderr,
        staging_workspace: staging.to_string_lossy().into_owned(),
    })
}

fn validate_antigravity_stream(
    events: &[AgentEvent],
    stderr: &str,
    exit_code: Option<i32>,
) -> Result<String, InstalledAgentError> {
    if events.is_empty() {
        return Err(InstalledAgentError::Invalid(
            "Antigravity emitted no events".into(),
        ));
    }
    if events[0].event != "init" {
        return Err(InstalledAgentError::Invalid(
            "Antigravity stream must begin with init".into(),
        ));
    }
    let mut init_count = 0;
    let mut result_count = 0;
    let mut result_status = None;
    for (index, event) in events.iter().enumerate() {
        match event.event.as_str() {
            "init" => {
                init_count += 1;
                if index != 0 {
                    return Err(InstalledAgentError::Invalid(
                        "init appeared after stream start".into(),
                    ));
                }
                let init = event
                    .payload
                    .get("init")
                    .and_then(Value::as_object)
                    .ok_or_else(|| {
                        InstalledAgentError::Invalid("init payload is invalid".into())
                    })?;
                if !init.get("cwd").is_some_and(Value::is_string)
                    || !init.get("tools").is_some_and(Value::is_array)
                {
                    return Err(InstalledAgentError::Invalid(
                        "init omitted cwd or tools".into(),
                    ));
                }
                let mode = event
                    .payload
                    .pointer("/init/permission_mode")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        InstalledAgentError::Invalid("init omitted permission_mode".into())
                    })?;
                if mode == "always-proceed" {
                    return Err(InstalledAgentError::Invalid(
                        "unsafe Antigravity permission mode".into(),
                    ));
                }
            }
            "step_update" => {
                if result_count > 0 {
                    return Err(InstalledAgentError::Invalid(
                        "event appeared after terminal result".into(),
                    ));
                }
                let step = event
                    .payload
                    .get("step_update")
                    .and_then(Value::as_object)
                    .ok_or_else(|| {
                        InstalledAgentError::Invalid("step_update payload is invalid".into())
                    })?;
                if !step.get("state").is_some_and(Value::is_string)
                    || !step.get("step_type").is_some_and(Value::is_string)
                {
                    return Err(InstalledAgentError::Invalid(
                        "step_update omitted state or step_type".into(),
                    ));
                }
                if step.get("step_type").and_then(Value::as_str) == Some("tool") {
                    let info = step
                        .get("tool_info")
                        .and_then(Value::as_object)
                        .ok_or_else(|| {
                            InstalledAgentError::Invalid("tool step omitted tool_info".into())
                        })?;
                    if info.contains_key("error") {
                        return Err(InstalledAgentError::Child(
                            "Antigravity tool step reported an error".into(),
                        ));
                    }
                }
            }
            "result" => {
                result_count += 1;
                if index + 1 != events.len() {
                    return Err(InstalledAgentError::Invalid(
                        "result was not terminal".into(),
                    ));
                }
                result_status = event
                    .payload
                    .pointer("/result/status")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let result = event
                    .payload
                    .get("result")
                    .and_then(Value::as_object)
                    .ok_or_else(|| {
                        InstalledAgentError::Invalid("result payload is invalid".into())
                    })?;
                if !result.get("response").is_some_and(Value::is_string)
                    || !result.get("usage").is_some_and(Value::is_object)
                {
                    return Err(InstalledAgentError::Invalid(
                        "result omitted response or usage".into(),
                    ));
                }
            }
            other => {
                return Err(InstalledAgentError::Invalid(format!(
                    "unknown Antigravity output event: {other}"
                )))
            }
        }
    }
    if init_count != 1 || result_count != 1 {
        return Err(InstalledAgentError::Invalid(
            "Antigravity stream requires exactly one init and one result per turn".into(),
        ));
    }
    let status = result_status.unwrap_or_else(|| "INVALID".into());
    if status != "SUCCESS" {
        return Err(InstalledAgentError::Child(format!(
            "Antigravity terminal status was {status}"
        )));
    }
    let lower = stderr.to_ascii_lowercase();
    let denial = [
        "soft-denied",
        "soft denied",
        "permission denied",
        "requires approval",
        "not allowed by permissions",
        "denied by permission",
        "was denied",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    if denial {
        return Err(InstalledAgentError::Child(
            "Antigravity reported a permission denial; exit code is not accepted as completion"
                .into(),
        ));
    }
    if exit_code != Some(0) {
        return Err(InstalledAgentError::Child(format!(
            "Antigravity exited with {exit_code:?} after its result"
        )));
    }
    Ok("completed".into())
}

fn stage_workspace(source: &Path) -> Result<PathBuf, InstalledAgentError> {
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
                "init",
                serde_json::json!({"event":"init","init":{"cwd":"/tmp/stage","tools":["read_file"],"permission_mode":"request-review"}}),
            ),
            event(
                "step_update",
                serde_json::json!({"event":"step_update","step_update":{"state":"DONE","step_type":"tool","tool_name":"read_file","tool_info":{"name":"read_file","parameters":{"path":"README.md"},"output":"ok"},"usage":{"input_tokens":2,"output_tokens":1,"total_tokens":3}}}),
            ),
            event(
                "result",
                serde_json::json!({"event":"result","result":{"status":"SUCCESS","response":"done","usage":{"input_tokens":2,"output_tokens":1,"total_tokens":3}}}),
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
    fn antigravity_command_is_the_documented_sandboxed_stream_contract() {
        let (args, input) = safe_args(InstalledAgentId::GoogleAntigravity, "hello");
        let args = args.iter().map(|x| x.to_string_lossy()).collect::<Vec<_>>();
        assert_eq!(
            args,
            [
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--sandbox"
            ]
        );
        assert_eq!(
            serde_json::from_str::<Value>(&input.unwrap()).unwrap(),
            serde_json::json!({"event":"user","message":{"content":"hello"}})
        );
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
        }
    }
    #[test]
    fn codex_cursor_and_claude_stay_contained() {
        let (codex, _) = safe_args(InstalledAgentId::Codex, "x");
        assert!(codex
            .windows(2)
            .any(|v| v[0] == "--sandbox" && v[1] == "read-only"));
        let (cursor, _) = safe_args(InstalledAgentId::Cursor, "x");
        assert!(cursor
            .windows(2)
            .any(|v| v[0] == "--sandbox" && v[1] == "enabled"));
        let (claude, _) = safe_args(InstalledAgentId::ClaudeCode, "x");
        assert!(claude.iter().any(|v| v == "plan"));
    }
    #[test]
    fn accepts_tool_calls_usage_and_one_terminal_result() {
        assert_eq!(
            validate_antigravity_stream(&valid_events(), "", Some(0)).unwrap(),
            "completed"
        );
    }
    #[test]
    fn tool_error_is_not_completion() {
        let mut events = valid_events();
        events[1].payload["step_update"]["tool_info"]["error"] =
            serde_json::json!({"type":"permission","message":"denied"});
        assert!(validate_antigravity_stream(&events, "", Some(0)).is_err());
    }
    #[test]
    fn denial_with_zero_exit_is_not_completion() {
        let err = validate_antigravity_stream(
            &valid_events(),
            "tool soft-denied because it requires approval",
            Some(0),
        )
        .unwrap_err();
        assert!(err.to_string().contains("permission denial"));
    }
    #[test]
    fn rejects_malformed_event_ordering_and_duplicate_result() {
        let mut e = valid_events();
        e.swap(0, 1);
        assert!(validate_antigravity_stream(&e, "", Some(0)).is_err());
        let mut e = valid_events();
        e.push(e.last().unwrap().clone());
        assert!(validate_antigravity_stream(&e, "", Some(0)).is_err());
    }
    #[test]
    fn rejects_missing_result_error_result_and_unknown_schema() {
        let mut e = valid_events();
        e.pop();
        assert!(validate_antigravity_stream(&e, "", Some(0)).is_err());
        let mut e = valid_events();
        e.last_mut().unwrap().payload["result"]["status"] = Value::String("ERROR".into());
        assert!(validate_antigravity_stream(&e, "", Some(0)).is_err());
        let mut e = valid_events();
        e.insert(
            1,
            event("future_event", serde_json::json!({"event":"future_event"})),
        );
        assert!(validate_antigravity_stream(&e, "", Some(0)).is_err());
    }
    #[test]
    fn always_proceed_is_rejected() {
        let mut e = valid_events();
        e[0].payload["init"]["permission_mode"] = Value::String("always-proceed".into());
        assert!(validate_antigravity_stream(&e, "", Some(0)).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn parser_failure_terminates_and_reaps_child() {
        use std::os::unix::fs::PermissionsExt;
        let root = env::temp_dir().join(format!("rex-agy-fixture-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let fake = root.join("agy-fixture");
        fs::write(
            &fake,
            "#!/bin/sh\nprintf '%s\\n' '{\"event\":\"unknown\"}'\n",
        )
        .unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o700)).unwrap();
        let started = std::time::Instant::now();
        let err = run_with_executable(InstalledAgentId::GoogleAntigravity, "x", &root, &fake)
            .unwrap_err();
        assert!(err.to_string().contains("must begin with init"));
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
