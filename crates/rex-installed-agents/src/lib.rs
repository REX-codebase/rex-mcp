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
use std::process::{Command, Stdio};
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
    InstalledAgentSummary{id:s.id,name:s.name,executable:s.executable,state:if s.id==InstalledAgentId::GoogleAntigravity{DetectionState::SupportNeedsReview}else if found.is_none(){DetectionState::Missing}else if version.is_some(){DetectionState::InstalledAuthUnknown}else{DetectionState::UnsupportedVersion},version,automation:s.automation,auth_boundary:s.auth_boundary,docs_url:s.docs_url,approval_boundary:"REX owns approval. The child never receives a skip-permissions flag and runs against an isolated staging copy.",compatibility:CompatibilityContract{verified_on:"2026-09-19",documented_interface:s.automation,entitlement_note:s.entitlement_note,fail_closed:true}}
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
    if id == InstalledAgentId::GoogleAntigravity {
        return Err(InstalledAgentError::Invalid("Google Antigravity support needs review: claimed documentation authority is unverified and no known-genuine installed binary was available for direct inspection".into()));
    }
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
    let staging = stage_workspace(workspace)?;
    let (args, input) = safe_args(id, prompt);
    let mut child = Command::new(exe)
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
    if let Some(line) = input {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| InstalledAgentError::Child("child stdin unavailable".into()))?;
        writeln!(stdin, "{line}").map_err(|e| InstalledAgentError::Io(e.to_string()))?;
    }
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| InstalledAgentError::Child("child stdout unavailable".into()))?;
    let mut events = Vec::new();
    for line in BufReader::new(stdout).lines() {
        let line = line.map_err(|e| InstalledAgentError::Io(e.to_string()))?;
        if line.trim().is_empty() {
            continue;
        };
        let payload = serde_json::from_str(&line).unwrap_or_else(|_| Value::String(line));
        let event = payload
            .get("event")
            .or_else(|| payload.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("output")
            .to_string();
        events.push(AgentEvent { event, payload });
    }
    let output = child
        .wait_with_output()
        .map_err(|e| InstalledAgentError::Child(e.to_string()))?;
    Ok(InstalledAgentRun {
        backend: id,
        status: if output.status.success() {
            "completed"
        } else {
            "failed"
        }
        .into(),
        exit_code: output.status.code(),
        events,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        staging_workspace: staging.to_string_lossy().into_owned(),
    })
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
    #[test]
    fn every_backend_has_a_documented_https_interface() {
        for s in SPECS {
            assert!(s.docs_url.starts_with("https://"));
            assert!(!s.automation.is_empty());
        }
    }
    #[test]
    fn safe_commands_never_skip_permissions() {
        for s in SPECS {
            let (a, _) = safe_args(s.id, "inspect");
            let text = a
                .iter()
                .map(|x| x.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ");
            assert!(!text.contains("dangerously"));
            assert!(!text.contains("bypass"));
        }
    }
    #[test]
    fn codex_is_read_only() {
        let (a, _) = safe_args(InstalledAgentId::Codex, "x");
        assert!(a
            .windows(2)
            .any(|v| v[0] == "--sandbox" && v[1] == "read-only"));
    }
    #[test]
    fn cursor_is_sandboxed_and_claude_uses_plan_mode() {
        let (cursor, _) = safe_args(InstalledAgentId::Cursor, "x");
        assert!(cursor
            .windows(2)
            .any(|v| v[0] == "--sandbox" && v[1] == "enabled"));
        let (claude, _) = safe_args(InstalledAgentId::ClaudeCode, "x");
        assert!(claude.iter().any(|v| v == "plan"));
    }
    #[test]
    fn streaming_backends_receive_json_stdin() {
        for id in [InstalledAgentId::Pi] {
            let (_, i) = safe_args(id, "hello");
            assert!(serde_json::from_str::<Value>(&i.unwrap()).is_ok());
        }
    }
    #[test]
    fn antigravity_fails_closed() {
        let err = run(
            InstalledAgentId::GoogleAntigravity,
            "hello",
            &std::env::temp_dir(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("support needs review"));
        let summary = discover()
            .into_iter()
            .find(|x| x.id == InstalledAgentId::GoogleAntigravity)
            .unwrap();
        assert_eq!(summary.state, DetectionState::SupportNeedsReview);
    }
}
