# Search providers

REX Harness uses one normalized `SearchResponse` contract for every search source. `REX-search` is always present and is the default. It performs bounded, robots-aware retrieval over operator-supplied public seeds and does not need an account.

Users may opt into hosted search from Settings:

| Provider | Official endpoint | Authentication | Notes |
| --- | --- | --- | --- |
| Exa | `POST https://api.exa.ai/search` | `x-api-key` | REX requests text and highlights, then preserves URL, title, content, score and the hosted-source provenance. |
| TinyFish | `GET https://api.search.tinyfish.ai?query=...` | `X-API-Key` | REX uses TinyFish's direct official endpoint, not an assumed Monid route. |

Official contracts verified 2026-09-19:
- Exa Search API: https://exa.ai/docs/reference/search
- TinyFish Search API reference: https://docs.tinyfish.ai/search-api/reference
- TinyFish authentication: https://docs.tinyfish.ai/authentication

## Credential boundary

External keys use the existing Rust `SecretStore` under namespaced IDs (`search:exa`, `search:tinyfish`). The frontend sends a key once to a Tauri command. Summaries expose only `has_key`; keys are never serialized back, logged, stored in browser state beyond the transient password input, or written into repo files.

Disconnecting the active external source clears its key and atomically returns the active source to `REX-search`. Selecting an external source without a stored key fails as `not_configured`.

## Errors and fallback

HTTP 401/403 remains `auth_failed`, 429 remains `rate_limited`, transport failures remain `network`, and malformed provider responses remain `invalid_response`. REX does not silently mix hosted and local results or hide a provider outage. The only automatic fallback is on explicit disconnect of the active provider, when it returns to the safe built-in default.

The normalized response makes the source visible in `coverage.model` (`hosted:exa` or `hosted:tinyfish`) and in each evidence item's `discovered_from` field. Hosted results carry the provider's own crawl and ranking policy, which the response disclaimer states.
