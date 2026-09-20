# REX MCP server: host setup, contracts, and operations

REX is a local custody daemon. A subscribed host agent (Claude Code,
Antigravity, any MCP client) stays the worker: it thinks, reads, writes,
and runs commands. REX freezes intent, confines every action to one
workspace, enforces leases and budgets, and decides completion from
evidence. REX holds no provider credentials and makes no model calls.

## Install

```sh
scripts/rex-mcp-install.sh            # builds and installs ~/.rex/bin/rex-mcp
```

Requirements: Rust stable (`rustup`), git. The installer builds
`cargo build --release -p rex-mcp` and copies the binary. No network
access, telemetry, or background service is installed; rex-mcp runs only
while a host keeps its stdio session open.

## Claude Code

```sh
claude mcp add rex --scope user \
  --env REX_STATE_DIR=$HOME/.rex/harness \
  --env REX_WORKSPACE=/path/to/project \
  --env REX_APPROVE_TASK_MUTATIONS=1 \
  -- ~/.rex/bin/rex-mcp
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
      "command": "/home/USER/.rex/bin/rex-mcp",
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
