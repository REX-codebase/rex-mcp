# Installed agent compatibility contract

Verified 2026-09-19 against the linked vendor documentation. REX treats these as installed **agents**, not raw model providers. The vendor executable owns authentication and entitlement. REX never reads tokens, cookies, keychain entries, or auth files.

| Backend | Supported interface | Current auth/subscription boundary | REX containment | Source |
|---|---|---|---|---|
| Cursor | ACP over stdio; `agent -p` stream JSON fallback | Cursor CLI browser login and the user's Cursor plan | sandbox enabled in an isolated staging copy | https://cursor.com/docs/cli/acp and https://cursor.com/docs/cli/headless |
| Codex | `codex exec --json` | Codex CLI's saved ChatGPT/API login; OpenAI documents ChatGPT sign-in and recommends API keys for ordinary automation | read-only sandbox in an isolated staging copy | https://developers.openai.com/codex/auth and https://developers.openai.com/codex/non-interactive-mode |
| Claude Code | `claude -p --output-format stream-json` | Claude Code's accepted Pro/Max/API login. REX does not use `--bare`, which is API/cloud credential-only | plan permission mode in an isolated staging copy | https://docs.anthropic.com/en/docs/claude-code/iam and https://docs.anthropic.com/en/docs/claude-code/headless |
| Pi | JSONL RPC over stdin/stdout | Pi owns provider login. Entitlement varies by provider and can involve separate billed usage | isolated staging copy; no auto-approval flags | https://pi.dev/docs/latest/rpc |
| Google Antigravity | **Disabled: support needs review** | Claimed documentation authority is unverified; no known-genuine installed binary was available for direct inspection | fail closed; no process launch | Unverified pages intentionally not used as authority |

## Fail closed

Support must be reviewed when the compatibility date or documented command contract is stale. Unknown versions and removed/changed automation interfaces are not treated as usable. REX never falls back to token scraping, hidden endpoints, copied OAuth clients, private protocols, `dangerously-skip-permissions`, or equivalent bypass modes. Provider rate limits and abuse controls remain the provider's authority.
