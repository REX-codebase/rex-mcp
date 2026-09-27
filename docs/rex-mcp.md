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
and Hermes examples. OpenCode CLI v1.17.4 reported a connection to a built REX binary in an isolated no-inference smoke test. Codex CLI v0.157.0 discovered the tools and resource templates through app-server and read the workflow quickstart. Neither check used a model-driven agent to call tools, and Claude Code and Hermes remain untested as installed hosts. Antigravity is not an installed backend in this project; do not present it as tested.

Set `REX_APPROVE_TASK_MUTATIONS=1` only when you accept that the host may
write files and run allowlisted commands inside `REX_WORKSPACE`. With it
unset, writes and commands return `approval_required`; reads, search,
status, events, execute, next, submit, result and cancel still work.

## First `rex_execute` call

Choose `host` for the client actually making the MCP call. Accepted values are
`human`, `claude_code`, `codex`, `open_code`, `hermes`, `antigravity`, and
`generic_agent`. `generic_agent` is for another agent client, not a default
for every setup; a host's account or model is not inferred from this field.
Set `operator_is_agent` to `true` when an agent operates the tools and to
`false` for a human operator. It is a declaration, not an approval switch.
Both fields are required; neither has an implicit default.

For example, an agent running in Codex might provide these *arguments* to the
`rex_execute` MCP tool (replace the task and use a fresh idempotency key):

```json
{
  "request_id": "first-task-001",
  "task": "Inspect the project and report what needs work",
  "host": "codex",
  "operator_is_agent": true
}
```

This creates a durable task, even when file writes are disabled. Keep its
returned task id, task capability, lease epoch and host resume handle. To allow file edits
and allowlisted commands, a *trusted launcher* must separately set
`REX_APPROVE_TASK_MUTATIONS=1` for the intended workspace before starting
the MCP server. It is not a `rex_execute` argument, and leaving it unset
does not make task creation read-only. Do not set it just to try the first
call; inspect the workspace and permissions first.

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

MCP hosts can also read the `rex://workflow/quickstart` resource and the
`rex://task/{task_id}/status`, `events/{after_seq}`, and `result` resource
templates. The `rex_task_workflow` prompt guides a new task; the
`rex_task_inspect` prompt takes an existing task ID and asks for read-only
status, events and an available result. These prompts are suggestions to the
host, not execution or permission grants. The host remains responsible for
choosing calls and honoring its user's approval.

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

### Standard visual-action evidence gate

For Standard tasks whose frozen task or current action names a recognized visual/UI/webpage/web app/mobile app/interface-design/landing-page/dashboard work item, `rex_submit` does not accept a source receipt alone. Each action must cite six task-bound PNG artifacts captured **by `rex_preview_capture` in the same REX task and current action**, using matching `kind` and evidence-map keys: `render.desktop.first` (at least 760 CSS pixels wide), `render.mobile390.first` (390 pixels wide), and `render.state.start`, `.mid`, `.end`, `.reverse`. Put the `sha256` returned by each capture in the matching `rex_submit` evidence field. `rex_preview_capture` registers the PNG as an immutable artifact and binds the local project source tree; uploading host-supplied PNGs with `rex_artifact_put` does not satisfy this visual gate. REX re-reads the immutable artifacts, verifies their digests, decodes PNG pixels, checks basic dimensions and confirms the cited set came from one local source snapshot. A rejected action remains open with a repair message; it does not advance or complete. The host must inspect real viewport captures and report what the states show, not invent transition evidence to fill six slots. The local preview does not reproduce an external hosted frame or certify settled timing, visual quality or meaningful state differences. The task text is frozen; a brief that omits the recognized visual/UI terms can evade this classification, so this is not a universal proof of design quality.

### Local MCP preview and mandatory critique challenge

`rex_preview_start` takes a task capability, lease epoch and workspace-relative project directory, detects static HTML or a supported app framework, and starts a bounded loopback preview. `rex_preview_action` drives a **local headless Chrome** renderer through pointer, keyboard, text, scroll, route, viewport and reduced-motion media actions; `rex_preview_capture` returns a PNG in base64 plus bounded DOM/AX observations, and registers that PNG as a task-bound immutable artifact. `rex_preview_stop` tears down the session. Each `scroll` action accepts finite `delta_x` and `delta_y` values from -10000 through 10000; split longer movement across smaller actions. The host does not need to open the user's browser. This is not a browserless renderer: Chrome is still the local rendering engine. Sessions live only in the current MCP process; after a process restart, start a new preview. REX rejects a capture whose current page URL has left the reserved preview origin, including navigation caused by a page control. This is a capture guard, not a guarantee that a previewed page cannot attempt a remote request. The preview does not reproduce an external File host frame by itself, and network blocking is best-effort at this stage. Do not put untrusted remote applications or saved credentials into the local preview.

For Standard custody actions, `rex_critique_prompt` issues the action-bound adversarial review challenge before any preview action or capture. The host inspects, then `rex_critique_record` logs concrete findings tied to that action. `rex_test` and `rex_submit` reject calls until that record exists; each new action needs its own prompt and critique. REX can force the step and retain its text, not guarantee the host's judgment is severe, accurate or independent. For UI/visual tasks, generic `rex_run` also requires the recorded critique before dispatch, closing the command-as-check bypass. Edit and read remain available for construction; the preview still cannot judge whether the critique is honest. Ultra promotion retains its separate gate. The newly added preview captures are first-party within the local MCP session, whereas `rex_artifact_put` remains host-supplied; do not conflate their provenance. For Standard visual acceptance, cite six captures from `rex_preview_capture` for the current action. REX hashes the local project tree around each capture and rejects a cited set with mixed source snapshots or unbound host-supplied PNGs. This is a local source-coherence check, not proof that the rendered DOM matches the source, that a hosted File uses it, or that a dynamic backend state was identical across captures. Static HTML previews hash served `dist`/`build` assets as well as source files and refuse requests for unhashed `node_modules`, `.next`, `target` and `.git` paths; framework dev servers still exclude generated `dist`/`build` output and dependencies, so their dynamically served output is not fully attested by this check. A byte-identical PNG may be recaptured under a *later action* of the same task when the source snapshot is unchanged. Each action still needs its own critique and six fresh `rex_preview_capture` calls; citing the previous action's digests without those calls fails. A byte-identical PNG from a later **source revision** cannot be rebound within the same task: the digest has no per-capture identity. REX returns an `idempotency_conflict` with a recovery hint. Do not make cosmetic changes solely to force new pixels; finish or stop the old task and start a fresh task for the revised source, then recapture all cited states. This preserves fail-closed source coherence rather than claiming that identical pixels authenticate a new source. Accepted Standard visual submissions expose informational `visual_capture_coverage` in `rex_submit` and the latest accepted action's summary in `rex_status`: six cited slots, unique PNG digests, and duplicate slots. Duplicate bytes do not reject a valid static/reduced-motion state, and distinct bytes do not prove a meaningful transition, settled motion, or host-frame quality. Hosts must still state which transitions were exercised or untested. Re-capture after any edit; inspect the pixels and host frame independently.
