# Installed agent compatibility contract

Verified 2026-09-19 against the linked vendor documentation. REX treats these as installed **agents**, not raw model providers. The vendor executable owns authentication and entitlement. REX never reads tokens, cookies, keychain entries, or auth files.

| Backend | Supported interface | Current auth/subscription boundary | REX containment | Source |
|---|---|---|---|---|
| Cursor | ACP over stdio; `agent -p` stream JSON fallback | Cursor CLI browser login and the user's Cursor plan | sandbox enabled in an isolated staging copy | https://cursor.com/docs/cli/acp and https://cursor.com/docs/cli/headless |
| Codex | `codex exec --json` | Codex CLI's saved ChatGPT/API login; OpenAI documents ChatGPT sign-in and recommends API keys for ordinary automation | read-only sandbox in an isolated staging copy | https://developers.openai.com/codex/auth and https://developers.openai.com/codex/non-interactive-mode |
| Claude Code | `claude -p --output-format stream-json` | Claude Code's accepted Pro/Max/API login. REX does not use `--bare`, which is API/cloud credential-only | plan permission mode in an isolated staging copy | https://docs.anthropic.com/en/docs/claude-code/iam and https://docs.anthropic.com/en/docs/claude-code/headless |
| Pi | JSONL RPC over stdin/stdout | Pi owns provider login. Entitlement varies by provider and can involve separate billed usage | isolated staging copy; no auto-approval flags | https://pi.dev/docs/latest/rpc |
| Google Antigravity | `agy --input-format stream-json --output-format stream-json --sandbox` | Antigravity CLI owns its cached Google or explicit Gemini API authentication; REX never reads it | sandbox enabled in an isolated staging copy; completion requires a valid terminal `result`, no denial diagnostics, and exit 0 | https://antigravity.google/docs/cli/headless/ and https://www.antigravity.google/docs/cli/reference/ |

## Fail closed

Support must be reviewed when the compatibility date or documented command contract is stale. Unknown versions and removed/changed automation interfaces are not treated as usable. REX never falls back to token scraping, hidden endpoints, copied OAuth clients, private protocols, `dangerously-skip-permissions`, or equivalent bypass modes. Provider rate limits and abuse controls remain the provider's authority.


## Google Antigravity verification boundary

Protocol support was added from Google Antigravity CLI v1.2.7 after the Linux x64 archive was matched to Google’s live manifest SHA-512 and the Google GitHub release SHA-256, then inspected without execution or authentication. This adapter is **protocol-tested, not live subscription-verified**. REX parses `init`, `step_update`, tool/usage data, and exactly one terminal `result`; malformed ordering, unknown output events, `always-proceed`, non-success terminal states, permission-denial diagnostics, and non-zero exits fail closed. It never calls private `agentapi` or internal endpoints.
