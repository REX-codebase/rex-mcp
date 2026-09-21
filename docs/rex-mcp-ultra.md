# REX MCP Ultra mode: host loop, recovery, and the truth boundary

Ultra is the second, host-driven orchestrator. The host agent owns all
inference; REX owns durable state, branch workspaces, evidence, leases,
budgets and every deterministic gate. REX can keep work resumable and
refuse premature completion; it cannot force a disconnected host to call
again. Ultra never routes through managed inference: the daemon makes no
model calls on this path.

Install and host configuration are identical to Standard mode - see
docs/rex-mcp.md. The installer also guards the task store: it refuses to
downgrade onto a store written by a newer server and reports when legacy
records will migrate (store schema v1 records upgrade to v2 on first
load, with a durable `store_migrated` event).

## Standard custody vs full Ultra

Standard mode (`rex_execute`) freezes a plan and enforces custody,
leases and evidence-gated completion around the host's own work. Full
Ultra adds the orchestrated pipeline on top: a taste contract, multiple
candidate theses built in isolated branch workspaces, evidence per
candidate, deterministic gates (tests, adversary, mutation, visual
critic, clean-room judge), repair, and a verified promotion of exactly
one candidate. Nothing reaches the main workspace without promotion.

## The Ultra host loop

1. `rex_ultra_open` with a `request_id`, the task, and `work_kind`
   (`General`, `Code`, `Visual`). Binds a compiled skill plan once and
   returns the first phase packet plus the lease.
2. Drive phases with `rex_ultra_next`. Each accepted step rotates the
   lease epoch; replayed calls are idempotent.
3. Return phase outputs and evidence with `rex_ultra_submit`. Evidence
   ids must come from harness-registered receipts (`rex_run`,
   `rex_test`, captures) - the completion claim may only cite those.
4. Gates fail closed: an inconclusive adversary is not a clean pass, an
   unproven verifier blocks, visual contracts need pixel evidence, an
   unverifiable rollback is CorruptState, a dirty destination is never a
   new base, and a handle-less resume is ScopeDenied.
5. `rex_ultra_promote` atomically commits the qualified candidate into
   the workspace with a verified rollback receipt. `rex_result` returns
   the terminal state with a plain-language reason.

## Recovery

A task or request id alone cannot resume anything. `rex_execute` and
`rex_ultra_open` issue a host resume handle at creation; only its hash
is stored, and it rotates on every accepted resume. To continue after a
host restart, replay the original `request_id` with the current handle:
the daemon returns the same task with `resumed: true` and the open
packet. Crash recovery replays the operation journal; a resume without
the handle is ScopeDenied, and a mismatched handle is ScopeDenied.

## Visual work

For `work_kind: Visual` the taste contract records references,
anti-references and forbidden defaults with provenance - a name alone is
not evidence. Every visual claim needs pixel evidence (captures at the
required viewports) submitted through the evidence path; the visual
critic gate rejects candidates without it. A rejected candidate stays a
branch artifact in the proof bundle, never silently dropped.

## Proof bundles

`rex_proof` returns the deterministic per-task proof bundle: the frozen
plan and request hash, kernel state, the qualified candidate and its
evidence outcomes, the bound skill plan, the promotion receipt and
state, the full event stream, and a deterministic `bundle_hash` over all
of it. The bundle is also persisted to
`<REX_STATE_DIR>/proofs/<task_id>.json` and served read-only by the
dev-server at `GET /api/rex/tasks/<task_id>/proof`. It is the artifact
release evidence and the supervision UI are built from.

## The truth boundary

- The host thinks; REX decides. Terminal truth always comes from REX's
  gates, never from the host's narration.
- REX holds no provider credentials and makes no model calls on the
  Ultra path; `examples/rex-ultra-scripted-host` runs the full pipeline
  with zero network.
- A disconnected host pauses the task; it does not advance it. The
  daemon waits, the lease expires, and any later resume must present
  the rotated handle.
