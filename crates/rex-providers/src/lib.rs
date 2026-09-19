//! REX Harness provider core.
//!
//! First real backend slice: provider connectivity. Owns everything that
//! touches a provider API or a credential so the frontend never holds a key:
//! the frontend sees catalog data, status, and `has_key` booleans only.
//!
//! Security rules enforced here:
//! - API keys enter through `ProviderService::set_key`, land in a
//!   `SecretStore`, and are never returned, logged, or serialized.
//! - Error messages describing failures never include the key material.
//! - Every state the UI can render is truthful: unsupported, auth failure,
//!   network failure, rate limit, empty catalog, and stale cache are distinct.

pub mod agent_loop;
pub mod catalog;
pub mod conversation;
pub mod error;
pub mod http;
pub mod policy;
pub mod providers;
pub mod search;
pub mod secrets;
pub mod service;

pub mod live;

pub use agent_loop::{AgentLoop, LoopEvent, DEFAULT_MAX_STEPS, HARD_MAX_STEPS};
pub use live::{LiveRunService, RunSnapshot, RunStatus};
pub use catalog::{CatalogSource, ModelCatalog, ModelInfo};
pub use conversation::{
    decode_turn, encode_tool_outcomes, NormalizedToolCall, NormalizedTurn, ToolOutcome,
};
pub use error::ProviderError;
pub use http::{Transport, UreqTransport};
pub use policy::{access_policies, AccessKind, AccessPolicy, PolicyStatus, VERIFIED_ON};
pub use providers::{find_spec, registry, ModelDiscovery, ProviderProtocol, ProviderSpec};
pub use search::{SearchProvider, SearchProviderSummary, SearchRouter};
pub use secrets::{FileSecretStore, MemorySecretStore, SecretStore};
pub use service::{ProviderService, ProviderSummary};
