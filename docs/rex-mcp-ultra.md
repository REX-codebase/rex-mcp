# REX MCP Ultra mode: host loop, recovery, and the truth boundary

Ultra is the second, host-driven orchestrator. The host agent owns all
inference; REX owns durable state, candidate workspaces, evidence,
leases, budgets and every deterministic gate. REX can keep work
resumable and refuse premature completion; it cannot force a
disconnected host to call again. Ultra never routes through managed
inference: the daemon makes no model calls on this path.

Install and host configuration are identical to Standard mode - see
docs/rex-mcp.md. The installer also guards the task store: it refuses to
downgrade onto a store written by a newer server and reports when legacy
records will migrate (store schema v1 records upgrade to v2 on first
load, with a durable `store_migrated` event).

## Standard custody vs full Ultra

Standard mode (`rex_execute`) freezes a plan and enforces custody,
leases and evidence-gated completion around the host's own work. Full
Ultra adds the orchestrated pipeline on top: a frozen acceptance
contract, at least three candidate theses sealed into isolated
workspaces, daemon-executed adversary, verifier and (for visual work)
pixel gates per candidate, and a verified promotion of exactly one
candidate. Nothing reaches the main workspace without promotion, and an
Ultra task cannot complete through Standard `rex_submit`: completion and
promotion are one joined, crash-safe state machine.

## The Ultra host loop

1. `rex_execute` with `ultra: true` creates the task and issues the
   per-task capability. Every later call must present it; a task id plus
   lease epoch is sequencing information, never authorization.
2. `rex_ultra_open` with the capability and, on first open, a
   `contract_draft`: obligations with executable proofs
   (`command_succeeds`, `command_output_contains`, `file_exists`,
   `file_contains`) plus forbidden regressions. The daemon parses and
   validates the draft deterministically, freezes it, and executes every
   proof itself. Drafts whose obligations are host-judged behavior prose
   are rejected. Later opens must hash-match the frozen contract. Open
   also compiles the repository's skill plan once, freezes it, and
   records its hash.
3. Open returns candidate requests (at least three) and, once candidates
   are in, adversary/verifier/visual evidence requests. Work kind is
   classified deterministically from the task text, conservative toward
   `visual`.
4. `rex_ultra_submit` returns candidate responses and evidence content.
   A candidate response is a sealed full-file bundle; prose is rejected,
   and a bundle with a traversal path is rejected before any write. The
   daemon materializes each bundle into its own isolated candidate
   workspace. Raw evidence bytes can go through `rex_artifact_put` into
   the content-addressed immutable artifact store first.
5. The gates run inside the daemon, never on the host's say-so: a
   deterministic adversary scan of each sealed candidate tree (symlink
   escapes, placeholder content, empty deliverables, non-text payloads
   posing as text), verifier re-execution of every contract obligation,
   and for visual contracts a daemon-decoded pixel check with a
   machine distinct-thesis comparison across candidates. A candidate
   qualifies only when its adversary report is conclusive and clean and
   every obligation is proven; every candidate must be fully evidenced
   before the kernel completes, and the highest deterministic quality
   score wins - never the first self-declared clean candidate.
6. `rex_ultra_promote` reruns every mandatory gate - the frozen skill
   plan's required gates included - on a confined staging copy, then
   atomically swaps the winning bundle into the task workspace with a
   verified rollback receipt. A gate that cannot be rerun blocks
   promotion (`gates_not_rerun`), and gate side effects never reach the
   promoted tree. Only a committed receipt completes the durable task.

The MCP also offers `rex_ultra_promote_start` for long-running gates. It takes the same `task_id`, `capability`, and `lease_epoch` as the blocking `rex_ultra_promote`, validates them before starting, and returns a process-local `operation_id` with `state: running`. Keep the MCP process alive and call `rex_ultra_promote_status` with `task_id`, `capability`, and `operation_id` for `running`, `succeeded` with the full receipt, or `failed` with a structured error. A second start for the same task in this process returns its existing operation ID instead of launching another worker. After a server restart, inspect durable task status and proof before retrying; operation handles are not durable. The blocking tool remains available.

## Recovery

A task or request id alone cannot resume anything. `rex_execute` issues
a host resume handle at creation; only its hash is stored, and it
rotates on every accepted resume. To continue after a host restart,
replay the original `request_id` with the current handle: the daemon
returns the same task with `resumed: true`. A resume without the handle
is ScopeDenied, and a mismatched handle is ScopeDenied. Ultra loop state
(contract, skill plan, candidates, evidence) is durable: reopening with
`rex_ultra_open` returns the frozen state and the currently open
requests.

## Visual work

Visual contracts demand pixel evidence: the daemon decodes the submitted
captures itself, rejects garbage bytes, measures each render, and
requires at least three machine-distinct theses (perceptual-hash
distance) before any candidate can qualify. A rejected candidate stays a
recorded artifact in the proof bundle, never silently dropped.

## Proof bundles

`rex_proof` returns the per-task proof bundle (version 2): the frozen
plan and request hash, kernel state, the qualified candidate, the frozen
skill plan and its hash, per-candidate evidence manifests (recorded vs
recomputed response hashes and the canonical hashes of the daemon's
adversary/verifier/visual records), the promotion receipt and state, the
event stream folded into an append-only hash chain, and a `bundle_hash`
over all of it, MACed with a daemon-held key. The bundle is persisted to
`<REX_STATE_DIR>/proofs/<task_id>.json` and served read-only by the
dev-server at `GET /api/rex/tasks/<task_id>/proof`.
`rex_proof_verify` independently re-checks a persisted bundle: the MAC
against the daemon-held key, content-hash consistency of every persisted
field, deterministic reassembly from the immutable records, and
candidate response-hash integrity. Editing task state, the event log or
stored candidate content after the fact invalidates verification.

## The truth boundary

- The host thinks; REX decides. Terminal truth always comes from REX's
  gates, never from the host's narration. Host submissions request
  checks; they never set verdicts.
- REX holds no provider credentials and makes no model calls on the
  Ultra path; `examples/rex-ultra-scripted-host` runs the full pipeline
  with zero network.
- A disconnected host pauses the task; it does not advance it. The
  daemon waits, the lease expires, and any later resume must present
  the rotated handle.
- Operator cancel and human Stop are different authorities: `rex_cancel`
  needs the per-task capability; `rex_human_stop` needs the trusted
  launcher's human-stop token and is final in every phase.
