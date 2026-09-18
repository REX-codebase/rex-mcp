use serde::Serialize;

/// Wire protocol spoken by a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderProtocol {
    Gemini,
    Anthropic,
    OpenAiCompatible,
}

/// How the backend can learn the provider's model list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelDiscovery {
    /// Provider-native listing endpoint.
    Native,
    /// OpenAI-compatible `GET /models`.
    OpenAiModels,
    /// No listing API; the user must type a model ID.
    Manual,
}

/// Static description of a supported provider. Mirrors the frontend presets
/// in `src/data/providers.ts`; the backend is the source of truth for what
/// actually connects.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub protocol: ProviderProtocol,
    pub default_base_url: Option<&'static str>,
    pub discovery: ModelDiscovery,
}

pub fn registry() -> Vec<ProviderSpec> {
    vec![
        ProviderSpec {
            id: "gemini",
            name: "Google Gemini",
            protocol: ProviderProtocol::Gemini,
            default_base_url: Some("https://generativelanguage.googleapis.com"),
            discovery: ModelDiscovery::Native,
        },
        ProviderSpec {
            id: "anthropic",
            name: "Anthropic",
            protocol: ProviderProtocol::Anthropic,
            default_base_url: Some("https://api.anthropic.com"),
            discovery: ModelDiscovery::Native,
        },
        ProviderSpec {
            id: "deepseek",
            name: "DeepSeek",
            protocol: ProviderProtocol::OpenAiCompatible,
            default_base_url: Some("https://api.deepseek.com"),
            discovery: ModelDiscovery::OpenAiModels,
        },
        ProviderSpec {
            id: "kimi",
            name: "Kimi",
            protocol: ProviderProtocol::OpenAiCompatible,
            default_base_url: Some("https://api.moonshot.ai/v1"),
            discovery: ModelDiscovery::OpenAiModels,
        },
        ProviderSpec {
            id: "glm",
            name: "GLM",
            protocol: ProviderProtocol::OpenAiCompatible,
            default_base_url: Some("https://open.bigmodel.cn/api/paas/v4"),
            discovery: ModelDiscovery::OpenAiModels,
        },
        ProviderSpec {
            id: "local",
            name: "Local server",
            protocol: ProviderProtocol::OpenAiCompatible,
            default_base_url: Some("http://127.0.0.1:11434/v1"),
            discovery: ModelDiscovery::OpenAiModels,
        },
        ProviderSpec {
            id: "custom",
            name: "Custom endpoint",
            protocol: ProviderProtocol::OpenAiCompatible,
            default_base_url: None,
            discovery: ModelDiscovery::Manual,
        },
    ]
}

pub fn find_spec(id: &str) -> Option<ProviderSpec> {
    registry().into_iter().find(|p| p.id == id)
}
