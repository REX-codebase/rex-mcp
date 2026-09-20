//! Who operates a task, and which worker executes it.
//!
//! REX distinguishes the *operator* (the principal accountable for the task:
//! a human at the keyboard or an external agent) from the *worker* (what
//! actually executes: a REX-managed model loop, or the external agent itself
//! entering through a protocol boundary). Both are fixed at offer time and
//! can never be widened by the worker.

use serde::{Deserialize, Serialize};

/// The protocol boundary an external agent enters through. Unknown or
/// undocumented protocols fail closed, matching the provider policy: REX
/// never guesses a private control channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentProtocol {
    /// Agent Client Protocol: the external agent is the ACP client, REX is
    /// the ACP server. Authentication is the client's own; REX never accepts
    /// a consumer subscription OAuth token here.
    Acp { client: String, version: String },
    /// A locally installed vendor CLI driven through its documented
    /// automation interface (the rex-installed-agents route).
    InstalledCli { backend: String },
    /// A host agent connected to the local REX MCP server over stdio
    /// (Claude Code, Antigravity, any MCP client). Authentication is the
    /// local process boundary; REX never sees the host's account or
    /// credentials.
    Mcp { client: String, version: String },
}

/// An external agent asking to operate a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentIdentity {
    /// Stable self-declared name, recorded for audit only. REX does not
    /// trust this string for any security decision.
    pub name: String,
    pub protocol: AgentProtocol,
    /// Random per-registration id minted by the registry, so two agents
    /// claiming the same name never share an identity.
    pub instance_id: String,
}

/// The principal accountable for one task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OperatorIdentity {
    Human,
    Agent(AgentIdentity),
}

impl OperatorIdentity {
    pub fn is_agent(&self) -> bool {
        matches!(self, OperatorIdentity::Agent(_))
    }

    pub fn label(&self) -> String {
        match self {
            OperatorIdentity::Human => "human".to_string(),
            OperatorIdentity::Agent(a) => format!("agent:{}", a.name),
        }
    }
}

/// Which worker executes the task under custody.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkerMode {
    /// REX runs its own autonomous loop on a selected provider/model while
    /// the operator supervises. The model never sees custody internals.
    ManagedModel {
        provider: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    /// The external agent itself is the worker, entering through the
    /// declared protocol boundary and confined to the granted capability
    /// set.
    ExternalAgent,
}
