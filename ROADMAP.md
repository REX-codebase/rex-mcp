# REX — Roadmap to Frontier Harness

Derived from the REX vs Claude Code vs Codex battle plan (2026-09-23).
Order is deliberate: **parity first, then leapfrog**. A harness nobody can
run where they work loses no matter how good its proof engine is.

## Phase 0 — Proof engine core (DONE)

Shipped across 14 audit commits + 7 CI-fix commits. CI green.

- Plan-approval gate (backend + Composer plan view)
- One default execution path (custody default, alternatives explicit)
- `rex-fable`: THINK→PROVE→ATTACK→WRITE state machine, epistemic ledger,
  unlock rule, mechanical authority timer + countdown UI
- Fable gate toggle in Composer + harness→fable MCP link
- Interactive terminal (xterm.js + backend PTY, rex-tools sandbox discipline)
- Monaco diff viewer in approval flow + inline workspace editor
- Git integration (status/diff/commit through rex-tools)
- Checkpoints (workspace snapshots, one-click restore)
- MCP server management (attach, probe, per-tool toggles)
- Cost visibility (price tables, per-run receipts, budgets)
- One-command quickstart + Tauri updater config

## Phase 1 — Headless & CI (DONE)

Parity item: `rex exec --json`. Makes REX scriptable: CI jobs, cron,
GitHub Actions, eval harnesses. No GUI, no TTY required.

- [x] `rex` CLI crate: `rex exec --json` drives the autonomous run loop
      headlessly, auto-resolves approvals only with explicit `--yes`,
      prints one JSON receipt on stdout (logs on stderr), honest exit codes
- [x] Env-based credentials for CI (`REX_<PROVIDER>_API_KEY` overrides file store)
- [x] `rex --version`, `REX_STATE_DIR` / `REX_WORKSPACE` honored like `rex-mcp`
- [x] GitHub Action (`rex-run`): checks out repo, runs `rex exec --json`,
      uploads receipt + proof bundle as artifacts, fails the job on
      non-completed terminal states
- [x] quickstart.sh installs the CLI alongside the desktop app
- [x] Signed run certificates (leapfrog bet 1, pulled forward): every
      receipt carries an Ed25519 certificate; `rex verify` checks it offline

Exit criteria: `rex exec --task "write hello.py and prove it runs" --yes --json`
completes in CI with a parseable receipt and a non-zero exit on failure.

## Phase 2 — Where developers live

Parity items: IDE extensions, GitHub App.

- [x] Local harness daemon protocol: `rex serve` binds 127.0.0.1 and exposes
      POST /v1/runs, GET /v1/runs, GET /v1/runs/:id (live snapshot with
      pending approval / awaiting plan), POST /v1/runs/:id/approve,
      POST /v1/runs/:id/cancel, plus workspace checkpoints
      (POST /v1/runs/:id/checkpoint, list, rewind). Runs are interactive
      by default: tool and plan approvals park until the operator decides.
      First stdout line is `{"port": N}` for extension discovery.
- [x] VS Code extension (`extensions/vscode`): spawns the daemon on
      activation, REX: Run task…, plan/tool approvals as editor modals,
      status-bar live status, checkpoint + rewind from the editor.
      Exit criterion met: run a task, approve a plan, rewind a checkpoint
      without leaving VS Code.
- [x] GitHub proof summaries + checks: the `rex-run` action now runs
      `rex replay --json` after every run and posts a proof-replay summary
      (status, budgets, cost ceiling/estimate, bid terms, per-check
      verdicts) as a "verified by REX" check run (conclusion follows the
      verdict) with a bundle link to the workflow run's receipt artifact,
      and as a PR comment (created once, updated on re-runs via the
      `<!-- rex-proof-summary -->` marker). Delivered through the Action +
      `gh api`, which is the whole behavior the App would have; a
      standalone App registration is not needed for it.
- [x] JetBrains extension (`extensions/jetbrains`): Kotlin plugin over the
      same local `rex serve` HTTP API — Run Task, Cancel, Checkpoint,
      Rewind, Show Receipt; plan/tool approvals as IDE notifications.
      Compiled clean via `gradle buildPlugin` in CI.

Exit criteria: a developer can run a REX task, approve a plan, and rewind a
checkpoint without leaving VS Code.

## Phase 3 — Desktop power

Parity items: rewind incl. shell changes, subagent dashboard, session UX,
sandbox profiles, cross-machine resume, review surface, MCP client + skills.

- [x] Checkpoint/rewind covers shell-made changes, not just editor writes:
      verified — every tool (shell included) is confined to the staged
      workspace root (`resolve_existing` rejects outside paths, absolute
      paths refused, symlinks rejected), so workspace snapshots capture
      all tool side effects. New test: `shell_writes_land_inside_workspace`.
      Honest boundary: only the staged copy is rewound; the operator's
      original workspace is never touched.
- [ ] Background/subagent dashboard with restrained default parallelism
- [x] Named sessions and resumable context UX: `rex exec --name NAME` records
      a session label on the brief and receipt; `rex resume RUN_ID|NAME`
      [--task T] starts a fresh run seeded from the previous run's final
      workspace state, records `continued_from`, and carries the name over;
      `rex runs` / `rex show` display names and the session chain.
      Honest boundary: resume is same-machine only (see cross-machine item).
- [x] Compaction and clear (context reset within a session): `rex compact
      NAME` collapses a session's `continued_from` run chain into one
      mechanical summary at `$REX_STATE_DIR/sessions/<name>.json` (run ids,
      tasks, results, token spend, newest first, plus oldest-first one-line
      history); the next `rex resume NAME` injects the 8 most recent history
      lines into the continuation task so the new run gets distilled context.
      `rex clear NAME` drops the compacted context (idempotent). Honest
      boundary: the ledger is never rewritten — compaction is an index over
      immutable history, and the summary is mechanical (tasks/results from
      receipts), not a model-written semantic digest.
- [ ] OS-level sandbox profiles, network-off defaults
- [ ] Cross-machine and cross-directory session resume
- [ ] Managed code-review surface with proof replay
- [x] MCP **client**: `rex mcp list | tools [SERVER] [--json] | call SERVER TOOL`
      against external MCP servers over stdio (new `rex-mcp-ext` crate),
      configured in `.rex/mcp.json` (workspace) or `$REX_STATE_DIR/mcp.json`.
      Handshake, version-mismatch warning, per-server spawn with 30s RPC
      timeouts, honest failures. Runs also connect configured servers at
      start: the model sees an `mcp_call` tool (Execute risk, approval-gated,
      custody-scoped) plus a prompt catalog, so external tools work inside
      `rex exec` like Codex/Claude Code MCP. Tested against a fake MCP
      server (3 CLI integration tests), plus run-level decode/declaration
      tests and 4 `rex-tools` unit tests for the caller path.

Exit criteria: every Claude Code / Codex table-stakes feature has a REX
answer, verified against the battle-plan parity checklist.

## Phase 4 — Leapfrog (the 10 bets)

Only after parity. These are what make REX a different category, not a copy.
(Status 2026-09-23: bets 1, 3, and 7 were pulled forward into the headless
CLI during Phase 1 work and are live in `rex`; the rest remain sequenced
after Phase 2/3 parity.)

1. Machine-verifiable signed run certificates — DONE (b24ce024):
   Ed25519 per-machine keys, `rex keygen` / `rex verify`, every receipt
   signed. Local ledger (`rex runs` / `rex show`) added in 15e528da.
2. Repo-specific adversarial immune system — DONE (4f538ca3):
   `rex redteam --task T --yes` runs a builder then an adversarial critic
   that attacks the result; the signed redteam receipt binds both receipts
   plus a mechanical verdict (sound/broken/inconclusive). Exit 0 only if
   the result survived the attack.
3. Cross-provider tournaments with deterministic promotion — DONE (380d58e9):
   `rex tournament --task T --providers a,b`, signed bracket receipts.
4. Epistemic provenance at diff-hunk level — DONE (1f99f2d2 + 2d80ca2d):
   audit receipts now carry real unified diffs (Myers, hunk headers);
   every exec receipt has a `provenance` map attributing each hunk to
   the event/tool call that wrote it.
5. Proof replay as the primary review primitive — DONE (ac62b803):
   `rex replay RECEIPT.json` re-derives every mechanical claim check by
   check: signature, bid terms, bid ceiling (recomputed from the price
   table), policy, provenance shape, ledger membership. Exit 0/3.
6. Time-locked disconnected operation (dead-man custody, verified rollback)
   — DONE (7ca695a5): `rex exec --deadman-mins 30` arms a liveness switch;
   check in via `rex checkin --file F` or the timer lapses and the run is
   cancelled, trip recorded on the receipt. Verified rollback = runs happen
   on a staged copy; the original is never touched.
7. Plans that bid binding cost/proof-coverage commitments before execution —
   DONE (d62a443e, e34f6f51): `rex exec --bid` prints the binding bid
   (budgets + worst-case dollar ceiling), `--accept-bid` executes under it,
   `--budget-usd` converts a dollar cap into a token budget; the receipt
   carries the bid, `cost_usd_estimate`, and `bid_met`/`bid_gaps`.
8. Skills marketplace — DONE (e059e489): `rex skill pack/install/verify/
   remove/list/show` — hash-verified skill packs with install lockfiles;
   `rex exec --skill NAME` loads skills into the run (fail-closed on
   tamper) and the receipt records name/version/entry-sha256. ranked by verified efficacy
9. Air-gapped offline verification — DONE (6b34f680):
   `tools/verify-airgap/verify_receipt.py` verifies a receipt's Ed25519
   certificate with stdlib-only Python 3.8+, no REX install, no network.
   Cross-checked against OpenSSL signatures and Rust-signed fixtures.
10. Cross-repo proof contracts — DONE (85aa481c):
    `.rex/policy.json` declares a repo's contract (require_bid,
    max_cost_usd, allowed_providers); `rex exec` enforces it as a
    fail-closed gate and records it on the receipt; `rex policy`
    shows the effective contract.

Exit criteria per bet: a demo where the capability is *exercised*, not
described — e.g. a certificate that verifies on a second machine with no
REX installed.

## Non-goals

- Chasing model quality. REX is model-agnostic by design; every frontier
  model release makes the harness stronger for free. The moat is mechanical.
- Feature parity for its own sake. If a parity item doesn't serve the
  proof story, it gets cut or deferred.
