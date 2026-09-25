# REX MCP server: host setup, contracts, and operations

REX is a local custody daemon. A connected host agent (Claude Code,
Codex, OpenCode, Hermes, or another MCP client) stays the worker: it thinks, reads, writes,
and runs commands. REX freezes intent, confines every action to one
workspace, enforces leases and budgets, and decides completion from
evidence. REX holds no provider credentials and makes no model calls.

This guide covers Standard custody: the host drives its own work inside
one frozen plan. Full Ultra mode adds the orchestrated pipeline on top
(taste contract, candidate branches, deterministic gates, verified
promotion, proof bundles) - see docs/rex-mcp-ultra.md.

## Install

The npm launcher is staged but not published. `npx @rex-codebase/rex-mcp@latest`
will not work until it is released. For this private checkout, install Rust and
run `bash scripts/rex-mcp-install.sh`; use the resulting absolute path to
`~/.rex/bin/rex-mcp` (or `REX_INSTALL_DIR`) in the host's stdio MCP settings.
Set `REX_STATE_DIR` and `REX_WORKSPACE` to absolute paths. Refer to the
[private host setup](../README.md) for current Claude Code, Codex, OpenCode
and Hermes examples. These are documented configurations, not verified
installed-host integration results. Antigravity is not an installed backend in
this project; do not present it as tested.

Set `REX_APPROVE_TASK_MUTATIONS=1` only when you accept that the host may
write files and run allowlisted commands inside `REX_WORKSPACE`. With it
unset, writes and commands return `approval_required`; reads, search,
status, events, execute, next, submit, result and cancel still work.

## The caller-driven loop

1. `rex_execute` with an idempotency `request_id`, the task, and an
   optional `plan` (frozen at creation; the freeze hash enters the audit
   chain). Returns the task id, per-task capability, first action and lease.
2. For the open action: `rex_read` / `rex_edit` / `rex_search` /
   `rex_run` / `rex_test`, each with `task_id`, `capability` and
   `lease_epoch`. `rex_next` heartbeats the lease and returns the open action.
3. `rex_submit` the action with the same scope fields, narrative and evidence.
   REX verifies and issues the next action, or completes through custody gates.
4. `rex_status`, `rex_events`, `rex_result` inspect by task id;
   `rex_cancel` requires the per-task capability and ends the task.

Continuation is cooperative: REX cannot force a host to keep working, and
a host cannot force REX to accept a claim. Either side stops cleanly.

## Security contract

- Work calls need a live lease and the per-task capability; stale epochs return
  `stale_lease`. Task id and lease epoch alone do not authorize work.
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
- MCP protocol versions: `2025-11-25` and `2025-06-18`. Initialize echoes a
  supported client version; for another offered version it returns the latest
  supported version so the client can decide whether to disconnect.
- Error codes are stable snake_case (`task_not_found`, `stale_lease`,
  `scope_denied`, `approval_required`, `budget_exceeded`, `gate_failed`,
  `no_result`, ...). Valid tool calls that fail in REX return MCP tool results
  with `isError: true`, a human-readable message, and structured error
  details including the code. Malformed arguments and unknown tool names
  remain JSON-RPC errors with `data.code`. Hosts must inspect `isError`;
  transport success alone does not mean the tool succeeded.
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

## Update and uninstall

Until npm is published, rebuild from the private checkout with
`bash scripts/rex-mcp-install.sh`. Restart the host MCP session to load the new
binary. Durable tasks in `REX_STATE_DIR` remain, and a stored task with an
incompatible major protocol version refuses to load.

Run `scripts/rex-mcp-uninstall.sh` to remove the installed binary, then remove
`rex` from the relevant host MCP settings. Removing `~/.rex/harness` (or a
custom state directory) is irreversible: it contains durable tasks, events and
audit records. Archive it first if needed.

## Release gates

CI tests the narrowed Rust workspace and the npm launcher on candidate and
main. A passing run is evidence for that commit, not proof of integration
with every installed host or publication readiness. Before any public release,
review packaging targets, supply-chain scans, host setup and compatibility,
then publish only with explicit approval.
