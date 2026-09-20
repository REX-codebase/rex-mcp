//! Least-authority capability sets and unforgeable execution tokens.
//!
//! A capability set is fixed inside the offer; the handshake binds the
//! operator to its exact hash. At execution time every tool call must carry
//! a `CapabilityToken` the registry minted. The token is checked against the
//! *grant's* scope hash, so a mid-grant capability change (which the model
//! or agent can never perform) would invalidate every outstanding token.

use rex_tools::ToolRequest;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

/// Broad tool classes, mirroring rex-tools' risk classes minus `Denied`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolClass {
    Read,
    Write,
    Execute,
}

/// Why a request is outside the granted scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CapabilityDenial {
    ToolNotGranted { tool: String },
    ClassNotGranted { tool: String },
    PathOutsideScope { path: String },
    DelegationRefused,
}

impl std::fmt::Display for CapabilityDenial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ToolNotGranted { tool } => write!(f, "tool {tool} not in grant"),
            Self::ClassNotGranted { tool } => write!(f, "tool class for {tool} not in grant"),
            Self::PathOutsideScope { path } => write!(f, "path {path} outside granted workspace"),
            Self::DelegationRefused => write!(f, "custodied operators cannot create custody"),
        }
    }
}

/// The exact authority one custody grant confers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitySet {
    /// The single canonical workspace root every file and command is
    /// confined to. Independent of the tool runtime's own root check:
    /// custody enforces equality with the *granted* root.
    pub workspace_root: PathBuf,
    pub tool_classes: BTreeSet<ToolClass>,
    /// Tool names from rex-tools (`read_file`, `create_file`, `edit_file`,
    /// `search_files`, `run_command`). Empty means no tools at all.
    pub allowed_tools: BTreeSet<String>,
    pub allow_search: bool,
    pub allow_preview: bool,
    /// Always false today. A custodied operator can never mint further
    /// custody: that is the structural block on recursive agent loops.
    pub can_delegate: bool,
}

impl CapabilitySet {
    /// Canonical content hash bound into offers, tokens and the audit log.
    pub fn scope_hash(&self) -> String {
        let canon = serde_json::to_string(self).expect("capability set serializes");
        hex_sha256(canon.as_bytes())
    }

    fn tool_class(tool: &str) -> ToolClass {
        match tool {
            "read_file" | "search_files" => ToolClass::Read,
            "create_file" | "edit_file" => ToolClass::Write,
            _ => ToolClass::Execute,
        }
    }

    /// Normalize a request path lexically and confine it to the granted
    /// root. Symlink and device checks stay with rex-tools; this layer only
    /// proves the *target name* stays inside the granted workspace.
    fn confine(&self, raw: &str) -> Result<PathBuf, CapabilityDenial> {
        let raw_path = Path::new(raw);
        let joined = if raw_path.is_absolute() {
            raw_path.to_path_buf()
        } else {
            self.workspace_root.join(raw_path)
        };
        let mut norm = PathBuf::new();
        for comp in joined.components() {
            match comp {
                Component::CurDir => {}
                Component::ParentDir => {
                    if !norm.pop() {
                        return Err(CapabilityDenial::PathOutsideScope { path: raw.into() });
                    }
                }
                other => norm.push(other.as_os_str()),
            }
        }
        if !norm.starts_with(&self.workspace_root) {
            return Err(CapabilityDenial::PathOutsideScope { path: raw.into() });
        }
        Ok(norm)
    }

    /// Full scope decision for one normalized tool request.
    pub fn permits(&self, request: &ToolRequest) -> Result<(), CapabilityDenial> {
        let (tool, paths): (&str, Vec<&str>) = match request {
            ToolRequest::ReadFile { path } => ("read_file", vec![path]),
            ToolRequest::CreateFile { path, .. } => ("create_file", vec![path]),
            ToolRequest::EditFile { path, .. } => ("edit_file", vec![path]),
            ToolRequest::SearchFiles { path, .. } => {
                ("search_files", path.as_deref().into_iter().collect())
            }
            ToolRequest::RunCommand { cwd, .. } => {
                ("run_command", cwd.as_deref().into_iter().collect())
            }
        };
        if !self.allowed_tools.contains(tool) {
            return Err(CapabilityDenial::ToolNotGranted { tool: tool.into() });
        }
        if !self.tool_classes.contains(&Self::tool_class(tool)) {
            return Err(CapabilityDenial::ClassNotGranted { tool: tool.into() });
        }
        for p in paths {
            self.confine(p)?;
        }
        Ok(())
    }
}

/// Unforgeable proof that a live grant covers this exact scope at this exact
/// epoch. Minted only by the registry; the random 256-bit secret is never
/// derived from any operator-visible value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityToken {
    pub grant_id: String,
    pub epoch: u64,
    pub secret: String,
    pub scope_hash: String,
}

pub fn random_hex(bytes: usize) -> String {
    // /dev/urandom-backed; same entropy source class as rex-tools call ids.
    let mut buf = vec![0u8; bytes];
    use std::io::Read;
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .expect("system entropy");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn hex_sha256(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}
