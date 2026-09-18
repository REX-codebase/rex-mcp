use serde::Serialize;
use std::fmt;

/// Truthful failure taxonomy for provider connectivity. Each variant maps to
/// a distinct UI state. Display strings must never carry secret material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum ProviderError {
    /// No credential stored for this provider.
    NotConfigured,
    /// Provider rejected the credential (HTTP 401/403).
    AuthFailed,
    /// Provider throttled the request (HTTP 429).
    RateLimited,
    /// DNS, TLS, timeout, or connection failure before an HTTP status.
    Network(String),
    /// The provider/endpoint does not implement model listing.
    Unsupported(String),
    /// The provider answered but listed zero usable models.
    EmptyCatalog,
    /// Unexpected HTTP status or unparseable body.
    InvalidResponse(String),
    /// Credential store failed.
    Store(String),
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProviderError::NotConfigured => write!(f, "no API key stored for this provider"),
            ProviderError::AuthFailed => write!(f, "provider rejected the API key"),
            ProviderError::RateLimited => write!(f, "provider rate-limited the request"),
            ProviderError::Network(d) => write!(f, "network error: {d}"),
            ProviderError::Unsupported(d) => write!(f, "model listing unsupported: {d}"),
            ProviderError::EmptyCatalog => write!(f, "provider returned no usable models"),
            ProviderError::InvalidResponse(d) => write!(f, "unexpected provider response: {d}"),
            ProviderError::Store(d) => write!(f, "credential store error: {d}"),
        }
    }
}

impl std::error::Error for ProviderError {}
