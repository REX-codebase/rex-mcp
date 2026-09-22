# REX MCP server: host setup, contracts, and operations

REX is a local custody daemon. A subscribed host agent (Claude Code,
Antigravity, any MCP client) stays the worker: it thinks, reads, writes,
and runs commands. REX freezes intent, confines every action to one
workspace, enforces leases and budgets, and decides completion from
evidence. REX holds no provider credentials and makes no model calls.

This guide covers Standard custody: the host drives its own work inside
one frozen plan. Full Ultra mode adds the orchestrated pipeline on top
(taste contract, candidate branches, deterministic gates, verified
promotion, proof bundles) - see docs/rex-mcp-ultra.md.

## Install

The release install surface is one command, matching what stdio MCP hosts
already accept:

```sh
npx --yes @rex-codebase/rex-mcp@latest
```

The npm package contains the native binary. It does not require Rust, clone the
repository, install a daemon, or send telemetry. It supports Linux x64/arm64,
macOS x64/arm64 and Windows x64. REX runs only while a host keeps its stdio
session open.

The package is staged but intentionally unpublished while this repository is
private. Maintainers can still build from source with
`scripts/rex-mcp-install.sh` (requires Rust and git). The private packaging
workflow builds every native target and emits one unpublished `.tgz`; publishing
that reviewed tarball later is a single `npm publish <file> --access public`.

## Claude Code

```sh
claude mcp add rex --scope user \
  --env REX_STATE_DIR=$HOME/.rex/harness \
  --env REX_WORKSPACE=/path/to/project \
  --env REX_APPROVE_TASK_MUTATIONS=1 \
  -- npx --yes @rex-codebase/rex-mcp@latest
```

Set `REX_APPROVE_TASK_MUTATIONS=1` only when you accept that this host may
write files and run allowlisted commands inside `REX_WORKSPACE`. With it
unset, writes and commands return `approval_required`; reads, search,
status, events, execute, next, submit, result and cancel still work.

## Antigravity

Add to the MCP servers config (`~/.antigravity/mcp_config.json`):

```json
{
  "mcpServers": {
    "rex": {
      "command": "npx",
      "args": ["--yes", "@rex-codebase/rex-mcp@latest"],
      "env": {
        "REX_STATE_DIR": "/home/USER/.rex/harness",
        "REX_WORKSPACE": "/path/to/project",
        "REX_APPROVE_TASK_MUTATIONS": "1"
      }
    }
  }
}
```

## The caller-driven loop

1. `rex_execute` with an idempotency `request_id`, the task, and an
   optional `plan` (frozen at creation; the freeze hash enters the audit
   chain). Returns the task id, the first action and the lease.
2. For the open action: `rex_read` / `rex_edit` / `rex_search` /
   `rex_run` / `rex_test`, each with `task_id` + `lease_epoch`.
   `rex_next` heartbeats the lease and returns the open action.
3. `rex_submit` the action with narrative and evidence. REX verifies and
   issues the next action, or completes the task through custody gates.
4. `rex_status`, `rex_events`, `rex_result` inspect; `rex_cancel` ends.

Continuation is cooperative: REX cannot force a host to keep working, and
a host cannot force REX to accept a claim. Either side stops cleanly.

## Security contract

- Every call needs a live lease; stale epochs return `stale_lease`.
- Every file path and command is confined to the task workspace. A path
  escape quarantines the custody grant: all further calls fail closed.
- Mutations (edit, run, test) additionally require the trusted launcher
  flag. MCP arguments can never grant it.
- REX never asks for, stores, or proxies account credentials, OAuth
  tokens, or subscription sessions. `initialize` carries no auth.
- Completion is evidence-gated (no pending approvals, within-scope
  changes, all plan steps accepted). Failed claims are audited.

## Error and versioning contract

- REX protocol version: `PROTOCOL_VERSION` in rex-protocol (now `1.0`).
  Minor versions add optional fields only; major bumps break. Stored
  tasks with a different major version refuse to load.
- MCP protocol version: `2025-11-25`; a mismatched `initialize` is
  rejected with `version_mismatch`.
- Error codes are stable snake_case (`task_not_found`, `stale_lease`,
  `scope_denied`, `approval_required`, `budget_exceeded`, `gate_failed`,
  `no_result`, ...). Domain failures travel in JSON-RPC error `data.code`
  so MCP transport never erases them.
- Idempotency: repeating `rex_execute` with the same `request_id` and
  payload resumes; same key with a different payload is
  `idempotency_conflict`.

## State layout

```
$REX_STATE_DIR/
  custody/            custody registry, grants, audit chains
  tasks/<task_id>/    task.json (durable record), events.jsonl
```

Both directories are append-safe and crash-recoverable: on open, custody
recovery suspends lapsed leases and the daemon reloads every task record.

## Update

```sh
npx --yes @rex-codebase/rex-mcp@latest     # hosts resolve the released package
```

Hosts pick up the new binary on their next MCP session start. Durable
tasks in `$REX_STATE_DIR` survive; a stored task with an incompatible
major protocol version refuses to load instead of being mangled.

## Uninstall

```sh
scripts/rex-mcp-uninstall.sh            # removes the binary
claude mcp remove rex                   # Claude Code config
# Antigravity: delete the "rex" entry from ~/.antigravity/mcp_config.json
rm -rf ~/.rex/harness                   # optional: durable tasks, custody, audits
```

Removing state is irreversible: events.jsonl and the custody audit chain
are the only proof of what happened. Archive them first if they matter.

## Release gates (current status)

- Unit + integration tests: green (`cargo test` across the workspace).
- End-to-end stdio lifecycle: green (crates/rex-mcp/tests/stdio.rs).
- Clean-machine proof: green (fresh clone, `rex-mcp-install.sh` with an
  empty target dir, full lifecycle against the installed binary).
- Not yet done: Tauri Human/Agent picker UI, SBOM/dependency scan,
  fuzzing, secret scan, malicious-workspace corpus, external review.
  Do not claim release readiness before those land.
