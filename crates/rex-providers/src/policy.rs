//! Access policy: which account types REX may connect for each provider, and
//! why. Agrim's rule, encoded: REX only offers a subscription hook when the
//! provider's own current terms and authentication documentation clearly
//! permit third-party harness use. Explicit bans, undocumented routes, and partner-gated routes are distinct states.
//!
//! Every verdict here is grounded in official provider sources, checked on
//! `VERIFIED_ON`. The human-readable matrix with quotes lives in
//! `docs/subscription-policy.md`. When a provider changes its terms, update
//! the entry, the doc, and the date together.

use serde::Serialize;

/// Day the sources behind every verdict were last checked (YYYY-MM-DD).
pub const VERIFIED_ON: &str = "2026-09-19";

/// How a provider account would be connected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AccessKind {
    /// Pay-as-you-go API key from the provider's developer platform.
    ApiKey,
    /// A consumer subscription whose provider officially issues an API key or
    /// endpoint for use inside third-party tools.
    SubscriptionKey,
    /// Signing in with a consumer subscription's login/OAuth credentials.
    SubscriptionOauth,
}

/// Whether REX offers the route, per official provider documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PolicyStatus {
    /// Official docs explicitly support this route for third-party tools.
    Supported,
    /// The subscription exists, but its terms limit it to the provider's own
    /// list of supported tools. REX is not on the list, so REX does not offer
    /// it; joining the list is the only legitimate unlock.
    ToolScoped,
    /// Official terms explicitly forbid the route.
    NotPermitted,
    /// Provider has not published a general contract for arbitrary harnesses.
    /// Disabled without claiming the provider bans users or that the route is illegal.
    Undocumented,
    /// Provider officially supports named third-party integrations but has not
    /// published general self-service client registration; REX needs approval.
    PartnerGated,
    /// Route is not offered for another documented reason.
    NotOffered,
}

/// One possible way to connect a provider account, with its verdict.
#[derive(Debug, Clone, Serialize)]
pub struct AccessPolicy {
    pub kind: AccessKind,
    pub status: PolicyStatus,
    /// User-facing name of the route, e.g. "Anthropic API key".
    pub label: &'static str,
    /// One sentence: what the verdict is and the official reason behind it.
    pub detail: &'static str,
    /// Official provider sources the verdict is grounded in.
    pub sources: &'static [&'static str],
    pub verified_on: &'static str,
}

const fn api_key(
    label: &'static str,
    detail: &'static str,
    sources: &'static [&'static str],
) -> AccessPolicy {
    AccessPolicy {
        kind: AccessKind::ApiKey,
        status: PolicyStatus::Supported,
        label,
        detail,
        sources,
        verified_on: VERIFIED_ON,
    }
}

const fn policy(
    kind: AccessKind,
    status: PolicyStatus,
    label: &'static str,
    detail: &'static str,
    sources: &'static [&'static str],
) -> AccessPolicy {
    AccessPolicy {
        kind,
        status,
        label,
        detail,
        sources,
        verified_on: VERIFIED_ON,
    }
}

/// The access routes evaluated for one registry provider. The frontend reads
/// these through `ProviderSummary::access`; anything not marked
/// `PolicyStatus::Supported` is not connectable in REX, only explained.
pub fn access_policies(provider: &str) -> Vec<AccessPolicy> {
    match provider {
        "gemini" => vec![
            api_key(
                "Gemini API key",
                "Google documents API keys from AI Studio as the programmatic path for your own applications, with a free tier.",
                &["https://ai.google.dev/gemini-api/docs/api-key", "https://ai.google.dev/gemini-api/docs/billing"],
            ),
            policy(
                AccessKind::SubscriptionOauth,
                PolicyStatus::NotOffered,
                "Google AI Pro / Ultra",
                "These consumer plans are documented for Google's own products only; REX found no official route that lets a third-party harness spend the subscription, so REX omits it.",
                &["https://ai.google.dev/gemini-api/docs/billing"],
            ),
        ],
        "anthropic" => vec![
            api_key(
                "Anthropic API key",
                "Anthropic directs developers building products or services to API key authentication through the Claude Console.",
                &["https://code.claude.com/docs/en/legal-and-compliance"],
            ),
            policy(
                AccessKind::SubscriptionOauth,
                PolicyStatus::NotPermitted,
                "Claude Pro / Max sign-in",
                "Anthropic states it does not permit third-party developers to offer Claude.ai login or to route requests through Free, Pro, or Max plan credentials, and reserves the right to enforce without notice.",
                &["https://code.claude.com/docs/en/legal-and-compliance"],
            ),
        ],
        "openai" => vec![
            api_key(
                "OpenAI API key",
                "OpenAI documents API keys for programmatic access, billed through the Platform account at standard API rates.",
                &["https://developers.openai.com/codex/auth"],
            ),
            policy(
                AccessKind::SubscriptionOauth,
                PolicyStatus::Undocumented,
                "ChatGPT sign-in",
                "Pi implements Codex OAuth and OpenAI names Pi among tools OSS maintainers may prefer, but OpenAI publishes no reusable client-registration or auth contract for arbitrary harnesses. REX disables this as undocumented, not forbidden. The official Codex CLI remains available separately as an installed agent, where the CLI itself owns ChatGPT or API-key sign-in.",
                &[
                    "https://developers.openai.com/codex/auth",
                    "https://developers.openai.com/community/codex-for-oss",
                    "https://github.com/openai/codex/issues/36886",
                    "https://github.com/earendil-works/pi/blob/36b60d2e8985899743c4cf5bd5f8929832a3f05d/packages/ai/src/auth/oauth/openai-codex.ts",
                ],
            ),
        ],
        "xai" => vec![
            api_key(
                "xAI API key",
                "xAI's API is compatible with the OpenAI and Anthropic SDKs and is billed per token through console.x.ai.",
                &["https://x.ai/api", "https://docs.x.ai/developers/pricing"],
            ),
            policy(
                AccessKind::SubscriptionOauth,
                PolicyStatus::PartnerGated,
                "X Premium / SuperGrok",
                "xAI officially enables subscription OAuth in named third-party harnesses, while publishing no general client-registration route. REX needs its own approved client and will not copy another app client ID.",
                &[
                    "https://x.ai/news/grok-hermes",
                    "https://x.ai/news/grok-openclaw",
                    "https://x.ai/news/grok-opencode",
                    "https://x.ai/news/grok-warp",
                    "https://github.com/earendil-works/pi/blob/36b60d2e8985899743c4cf5bd5f8929832a3f05d/packages/ai/src/auth/oauth/xai.ts",
                ],
            ),
        ],
        "deepseek" => vec![api_key(
            "DeepSeek API key",
            "DeepSeek's official docs describe top-up, balance-based API access for any client.",
            &["https://api-docs.deepseek.com/quick_start/pricing"],
        )],
        "kimi" => vec![api_key(
            "Moonshot API key",
            "Moonshot's platform documents API-key access to its OpenAI-compatible chat API.",
            &["https://platform.moonshot.ai/docs/api/chat"],
        )],
        "kimi-coding" => vec![policy(
            AccessKind::SubscriptionKey,
            PolicyStatus::Supported,
            "Kimi for Coding",
            "Kimi membership officially includes an API key for third-party development tools; REX connects to https://api.kimi.com/coding with that key. Available models depend on the plan tier, so the model ID is entered manually.",
            &["https://www.kimi.com/code/docs/en/", "https://www.kimi.com/code/docs/en/third-party-tools/claude-code.html"],
        )],
        "glm" => vec![
            api_key(
                "Zhipu GLM API key",
                "Z.AI's open platform sells pay-as-you-go GLM API access usable from any client.",
                &["https://docs.z.ai/guides/overview/pricing.md"],
            ),
            policy(
                AccessKind::SubscriptionKey,
                PolicyStatus::ToolScoped,
                "GLM Coding Plan",
                "Z.AI limits the plan to its officially supported tools (Claude Code, Cline, OpenCode, Kilo Code, and others on its list) and warns that benefits may be restricted on unsupported tools; REX is not on the list, so REX does not offer it.",
                &["https://docs.z.ai/devpack/overview"],
            ),
        ],
        "qwen" => vec![
            api_key(
                "Alibaba ModelStudio API key",
                "Qwen's own tooling documents the standard ModelStudio API key against DashScope's OpenAI-compatible endpoint.",
                &["https://qwenlm.github.io/qwen-code-docs/en/users/configuration/auth/"],
            ),
            policy(
                AccessKind::SubscriptionOauth,
                PolicyStatus::NotOffered,
                "Qwen OAuth free tier",
                "Qwen Code's docs state this free tier was discontinued on 2026-04-15; new requests are rejected.",
                &["https://qwenlm.github.io/qwen-code-docs/en/users/configuration/auth/"],
            ),
            policy(
                AccessKind::SubscriptionKey,
                PolicyStatus::ToolScoped,
                "Alibaba Cloud Coding Plan",
                "The plan is documented for Qwen Code with a dedicated endpoint and subscription key; Alibaba documents no route for arbitrary third-party harnesses, so REX omits it.",
                &["https://qwenlm.github.io/qwen-code-docs/en/users/configuration/auth/"],
            ),
        ],
        "local" => vec![api_key("Local server", "Your own server on your own machine; a key only if the server asks for one.", &[])],
        "custom" => vec![api_key(
            "Custom endpoint",
            "Any OpenAI-compatible endpoint you are authorized to use; authorization is the user's responsibility.",
            &[],
        )],
        _ => vec![],
    }
}
