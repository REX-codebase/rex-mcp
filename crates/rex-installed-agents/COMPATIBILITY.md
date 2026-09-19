# Installed agent compatibility and terms contract

**Verified 2026-09-19** against the linked first-party sources. Re-verify before
changing this file, `CODEX_CONTRACT_VERIFIED_ON` in `src/lib.rs`, or the Codex
adapter. REX treats Codex as an installed **agent**, not a raw model provider.
The vendor executable owns authentication and entitlement. REX never reads
tokens, cookies, keychain entries, or auth files, and never re-implements the
vendor's OAuth flow.

## Retained route: OpenAI Codex CLI

| | |
|---|---|
| Backend | Codex CLI (`codex`) |
| Supported interface | `codex exec --json --sandbox read-only --skip-git-repo-check "<prompt>"` |
| Stream contract | JSONL on stdout: opens with `thread.started`, then `turn.started` / `item.*`, exactly one terminal `turn.completed`; `turn.failed` or `error` is a failed run |
| Auth/subscription boundary | Codex CLI's own saved sign-in: Sign in with ChatGPT (subscription) or API key. OpenAI documents both for the CLI and documents `codex exec` reusing saved CLI authentication, including ChatGPT-managed auth for automation. API keys remain OpenAI's recommended automation route |
| REX containment | read-only sandbox in an isolated staging copy; promotion of changes only after operator review |
| Sources | https://developers.openai.com/codex/non-interactive-mode and https://developers.openai.com/codex/auth and https://openai.com/policies/terms-of-use/ |

Terms basis, reviewed 2026-09-19: OpenAI's own documentation covers driving
Codex from scripts, pipelines and other tools through `codex exec`
("Non-interactive mode"). The OpenAI Terms of Use prohibit reverse
engineering, automated extraction of data, circumventing rate limits or
protective measures, and competing-model development; unlike some other
providers, they contain no clause barring third-party software from launching
the official client. REX's use stays inside the documented surface: the
official binary, the user's own sign-in, the read-only sandbox, and no
parallel or disguised traffic. OpenAI publishes no general third-party client
registration for Codex; REX therefore does not implement Codex OAuth itself
and this contract does not cover doing so.

## Fail closed

- Detection probes the installed binary offline (`codex --version`, then
  `codex exec --help` must still list `--json`, `--sandbox` and
  `--skip-git-repo-check`). A missing binary is `missing`; an unreadable
  version is `unsupported_version`; a changed interface is
  `support_needs_review` and the run path refuses to spawn.
- Stream validation fails closed on an empty stream, a missing or misplaced
  `thread.started`, any event outside the documented vocabulary, anything
  after the terminal event, duplicate terminals, `turn.failed`, `error`, and
  any non-zero exit. Exit code alone never counts as success.
- Model/effort overrides fail closed: no control surface for them has been
  verified against the current CLI, so REX runs the CLI's own defaults.
- REX never falls back to token scraping, copied OAuth clients, hidden or
  private endpoints, `danger-full-access`, `--full-auto`,
  `dangerously-skip-permissions`, or equivalent bypass modes. Provider rate
  limits and abuse controls remain the provider's authority.

## Removed routes (2026-09-19)

These adapters were deleted, not disabled, because their providers' current
terms do not permit the route. Re-adding one requires a provider-published
third-party entitlement and a fresh entry in this contract:

- **Google Antigravity (`agy` CLI and ACP).** Antigravity Additional Terms
  section 6 prohibits "using the Service in connection with products not
  provided by us" and names third-party software accessing the service with
  Antigravity OAuth a breach that can suspend the account; Google's FAQ
  repeats it and recommends Vertex or AI Studio API keys instead. No ACP
  exception exists. Sources: https://antigravity.google/terms/ and
  https://antigravity.google/docs/faq/
- **Claude Code consumer login.** Anthropic states third-party developers
  may not offer claude.ai login or route requests through Free/Pro/Max plan
  credentials, including through the Agent SDK, without prior written
  approval. Source: https://docs.anthropic.com/en/docs/claude-code/sdk
- **Cursor Agent.** Cursor publishes CLI/ACP automation, but no third-party
  harness entitlement for the consumer Cursor subscription the CLI would
  spend; the route stays out until Cursor documents one.
  Source: https://cursor.com/docs/cli/acp
- **Pi.** Pi's harness pattern re-implements providers' OAuth with copied
  first-party client IDs; no provider has published a contract permitting
  REX to do that. Source: https://github.com/openai/codex/issues/36886
