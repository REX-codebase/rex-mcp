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

pub mod catalog;
pub mod error;
pub mod http;
pub mod providers;
pub mod secrets;
pub mod service;

pub use catalog::{CatalogSource, ModelCatalog, ModelInfo};
pub use error::ProviderError;
pub use http::{Transport, UreqTransport};
pub use providers::{ModelDiscovery, ProviderProtocol, ProviderSpec, registry, find_spec};
pub use secrets::{FileSecretStore, MemorySecretStore, SecretStore};
pub use service::{ProviderService, ProviderSummary};
