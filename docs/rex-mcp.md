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

This creates a durable task, even when file writes are disabled. Persist the
full creation request (including `plan`, `proof`, `budgets`, `ultra`, `host`, and
`operator_is_agent` when supplied), plus the returned task id, task capability,
lease epoch and host resume handle. A later `rex_execute` with the same
`request_id` must repeat the *identical creation payload*, adding the current
`resume_handle`; omitting a frozen field such as `plan` is an
`idempotency_conflict`. Alternatively, resume by `task_id` with the same task
text and current `resume_handle`; that route checks the task text and handle.
Every successful resume rotates both the handle and task capability: store the
new ones before the next operation. A stale or lost handle is not a reason to
create a replacement task to bypass custody; inspect `rex_status` and events
and stop if you cannot recover the current handle. To allow file edits
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

For visual work, finish all UI edits before the first cited preview capture. Keep exported screenshots and videos outside the preview project, then capture all six slots from the final source revision. A source edit after a capture makes that set mixed; recapture the entire set, and if identical pixels were previously bound to an older revision in this task, start a fresh task instead of forcing cosmetic changes. The quickstart resource now states this before the capture sequence.

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

For Standard tasks whose frozen task or current action names a recognized visual/UI/webpage/web app/mobile app/interface-design/landing-page/dashboard work item, `rex_submit` does not accept a source receipt alone. Each action must cite six task-bound PNG artifacts captured **by `rex_preview_capture` in the same REX task and current action**, using matching `kind` and evidence-map keys: `render.desktop.first` (at least 760 CSS pixels wide), `render.mobile390.first` (390 pixels wide), and `render.state.start`, `.mid`, `.end`, `.reverse`. Put the `sha256` returned by each capture in the matching `rex_submit` evidence field. `rex_preview_capture` registers the PNG as an immutable artifact and binds the local project source tree; uploading host-supplied PNGs with `rex_artifact_put` does not satisfy this visual gate. REX re-reads the immutable artifacts, verifies their digests, decodes PNG pixels, checks basic dimensions and confirms the cited set came from one local source snapshot. A rejected action remains open with a repair message; it does not advance or complete. The host must inspect real viewport captures and report what the states show, not invent transition evidence to fill six slots. REX receipts certify tool calls, not exclusive source authorship: if the host edits project files outside rex_edit/rex_run after a receipt, an earlier receipt does not certify the final bytes. Do not re-register those bytes with a no-op edit. Reconcile the workspace with real REX-scoped edits or report the custody gap; the preview source hash alone does not detect who wrote a file. The local preview does not reproduce an external hosted frame or certify settled timing, visual quality or meaningful state differences. The task text is frozen; a brief that omits the recognized visual/UI terms can evade this classification, so this is not a universal proof of design quality.

### Frozen first-view result assertions (opt-in Standard visual tasks)

`rex_execute` may include `mobile_result_fields` and/or `desktop_result_fields` before any action: 1–24 fields, each `{name, kind: "text" | "control" | "list_count", alternatives: ["literal phrase", ...], match_mode?: "exact" | "contains", min_font_px?: 8..32, region?: "#moves", min_count?: 2..12}`. `exact` is the default; `contains` matches a case-insensitive whitespace-normalized substring inside one fully visible text node or control label. It does not allow the phrase to be assembled from separate nodes, and one node/control still cannot satisfy multiple fields. `list_count` requires `region` (a simple ID selector), `min_count`, and empty `alternatives`, and counts distinct fully visible `<li>` labels inside that region. It proves neither that those labels are actual moves nor that they form a usable workout. Optional `min_font_px` requires a node/control or list item to have at least that computed CSS font size; it is a narrow floor, not a readability verdict. The task creator must derive these assertions from the original brief; REX cannot authenticate that the caller is independent of the builder or infer omitted requirements from free text. The contract is frozen at task creation and stored durably; a resume by task ID cannot substitute a different contract. If the creator omits a surface contract, the existing visual capture gate still applies, but **there is no result-field enforcement**. Existing tasks and Ultra tasks are not silently upgraded.

On a contracted task, capture `render.mobile390.first` at exact 390×650 CSS pixels and/or `render.desktop.first` at exact 1280×800 CSS pixels, each at page top. The local preview computes which required field alternatives are visible in that first view. A `text` assertion matches a complete text node; a `control` assertion matches the complete visible label, text or value of an enabled button, link or form control, including `label[for]` and visible `aria-labelledby` references. A visually hidden styled input can borrow a fully visible associated label; an isolated hidden `aria-label` cannot count as visual evidence. Matching is case-insensitive with whitespace normalized. Rendered geometry must be fully within the viewport, have nontrivial size, pass basic computed visibility/ancestor checks and not be covered at its center. The observation is bound to the preview PNG digest, current action and source snapshot; uploaded images, narrative prose, another action's capture and a capture of the other surface cannot supply it. `rex_submit` rejects the action with named missing fields until a qualifying first-party capture on each contracted surface is cited. Use separate fields for separate movement names, quantity/duration, order values, delta, status and each decision control. One rendered text node/control cannot satisfy multiple independent fields. Literal alternatives are synonyms for **one** text/control field, not a minimum count; use `list_count` for the latter. For a usable result, specify its essential fields rather than a generic category title.

This is a narrow structural check. It does **not** establish that a movement is safe, a duration is a valid prescription, the light beam reaches the claimed page, an order correction is causally correct, a control works after clicking, or pixels remain readable under the actual hosted shell. DOM and geometry can be gamed, and a compliant string may be visually weak or semantically false. Inspect the PNG, test the controls and compare each spatial/causal claim to settled pixels independently; report uncertainty rather than calling a machine pass a design verdict. Do not strip existing truth/consent checks to satisfy the field contract.

### Opt-in two-state text/interaction contract

A task creator may freeze `two_state_contract` on `rex_execute` for a Standard visual task: `{"viewport":"mobile390","control":"#change","start_text":"Example A","end_text":"Example B","min_font_px":14}`. The viewport is `mobile390` (exact 390×650 CSS pixels) or `desktop` (exact 1280×800). The control is a simple ID selector for an enabled, unobscured button or link visible in the start viewport; start and end literals must differ after case/whitespace normalization. The contract and any ordinary first-view fields are frozen at task creation. An existing task is not silently upgraded.

In one local preview, capture `render.state.start` with the start literal fully visible, then call `rex_preview_action` with `{"kind":"activate_control","selector":"#change"}`. REX checks the creator-named control is visible and enabled, dispatches a real pointer click through headless Chrome, and records that action. Wait for the intended result to settle (including `wait_for_animations` where applicable), then capture `render.state.end` with the end literal fully visible at the **same exact viewport and source revision**. Cite those capture digests in the matching `rex_submit` slots. Submission fails closed if the cited start/end captures are missing, identical, not in that order in the same preview/action, lack the frozen literals, or omit the REX control activation. A new successful start capture resets the trace. The creator may also freeze `end_fields` inside `two_state_contract`: 1–8 distinct `{name,kind:"text",alternatives:["literal"],match_mode?,min_font_px?}` entries, using the first-view text matcher. All must be fully visible together with `end_text` in `render.state.end`. Supply field objects, not an array of strings. Do not repeat `end_text` as an exact `end_fields` alternative: one text node cannot satisfy two fields, so REX rejects that duplicate at task creation. `end_text` already checks the outcome; use `end_fields` for different facts. Example: `{"viewport":"mobile390","control":"#reject","start_text":"Awaiting review","end_text":"Rejected, order unchanged","min_font_px":14,"end_fields":[{"name":"decision_record","kind":"text","alternatives":["PO 418: 12 to 10 units"],"min_font_px":14}]}`. This cannot prove that the proposal is visually struck through, that wording matches the real source, or that rejecting caused a service write. The creator must freeze those exact facts from the brief and a reviewer must inspect pixels. Other four visual capture slots still apply.

This narrow gate checks visible text and a first-party click trace, **not causation**: a script could update unrelated text, the control could be inert while a timer changes the page, or the light beam could miss the work. The reviewer must inspect settled same-crop pixels, source/receiver geometry and real input-to-output meaning. This gate cannot cover a continuous drag, a select-only input, or a static page; omit it where the brief has no truthful click-driven second state. The local preview is not the hosted delivery frame.

### Local MCP preview and mandatory critique challenge

REX preview keyboard actions send native virtual keycodes for Tab, arrows, Enter, Space and other allowlisted keys, so test native focus/slider behavior in the preview rather than inferring it from code.

`rex_preview_start` takes a task capability, lease epoch and workspace-relative project directory, detects static HTML or a supported app framework, and starts a bounded loopback preview. `activate_control` only works when a creator-frozen `two_state_contract` names its exact simple ID selector and a visible `render.state.start` has been captured in that preview; use ordinary pointer actions for other controls. `rex_preview_action` drives a **local headless Chrome** renderer through pointer, keyboard, text, scroll, route, viewport and reduced-motion media actions; `rex_preview_capture` returns a PNG in base64 plus bounded DOM/AX observations, and registers that PNG as a task-bound immutable artifact. `rex_preview_stop` tears down the session. After a scroll, reveal or click, call `rex_preview_action` with `{"kind":"wait_for_animations","timeout_ms":2000}` before capturing when the claimed state depends on a CSS transition or finite Web Animation. The wait polls running finite animations for at most 5000 ms and returns a specific timeout error; it does not certify arbitrary JavaScript timers, canvas drawing, network data or infinite ambient motion, so inspect settled pixels yourself. Each `scroll` action accepts finite `delta_x` and `delta_y` values from -10000 through 10000; split longer movement across smaller actions. The host does not need to open the user's browser. This is not a browserless renderer: Chrome is still the local rendering engine. Sessions live only in the current MCP process; after a process restart, start a new preview. REX rejects a capture whose current page URL has left the reserved preview origin, including navigation caused by a page control. This is a capture guard, not a guarantee that a previewed page cannot attempt a remote request. The preview does not reproduce an external File host frame by itself, and network blocking is best-effort at this stage. Do not put untrusted remote applications or saved credentials into the local preview.

For Standard custody actions, `rex_critique_prompt` issues the action-bound adversarial review challenge before any preview action or capture. The host inspects, then `rex_critique_record` logs concrete findings tied to that action. `rex_test` and `rex_submit` reject calls until that record exists; each new action needs its own prompt and critique. REX can force the step and retain its text, not guarantee the host's judgment is severe, accurate or independent. For UI/visual tasks, generic `rex_run` also requires the recorded critique before dispatch, closing the command-as-check bypass. Edit and read remain available for construction; the preview still cannot judge whether the critique is honest. Ultra promotion retains its separate gate. The newly added preview captures are first-party within the local MCP session, whereas `rex_artifact_put` remains host-supplied; do not conflate their provenance. For Standard visual acceptance, cite six captures from `rex_preview_capture` for the current action. REX hashes the local project tree around each capture and rejects a cited set with mixed source snapshots or unbound host-supplied PNGs. Save exported PNGs, videos and review notes **outside** the preview project directory before capturing: those files change the source hash even when UI pixels do not. If mixed-source or same-PNG/new-source rejection occurs, move review outputs out, recapture the complete six-slot set from one revision, and start a fresh task when a prior identical digest was already bound to another revision. Do not exclude actual UI images or served assets from the hash to work around this. This is a local source-coherence check, not proof that the rendered DOM matches the source, that a hosted File uses it, or that a dynamic backend state was identical across captures. Static HTML previews hash served `dist`/`build` assets as well as source files and refuse requests for unhashed `node_modules`, `.next`, `target` and `.git` paths; framework dev servers still exclude generated `dist`/`build` output and dependencies, so their dynamically served output is not fully attested by this check. A byte-identical PNG may be recaptured under a *later action* of the same task when the source snapshot is unchanged. Each action still needs its own critique and six fresh `rex_preview_capture` calls; citing the previous action's digests without those calls fails. A byte-identical PNG from a later **source revision** cannot be rebound within the same task: the digest has no per-capture identity. REX returns an `idempotency_conflict` with a recovery hint. Do not make cosmetic changes solely to force new pixels; finish or stop the old task and start a fresh task for the revised source, then recapture all cited states. This preserves fail-closed source coherence rather than claiming that identical pixels authenticate a new source. Accepted Standard visual submissions expose informational `visual_capture_coverage` in `rex_submit` and the latest accepted action's summary in `rex_status`: six cited slots, unique PNG digests, and duplicate slots. Duplicate bytes do not reject a valid static/reduced-motion state, and distinct bytes do not prove a meaningful transition, settled motion, or host-frame quality. Hosts must still state which transitions were exercised or untested. Re-capture after any edit; inspect the pixels and host frame independently.
