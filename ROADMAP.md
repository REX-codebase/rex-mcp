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

- [ ] VS Code extension: talk to the local harness daemon, inline plan
      approval, checkpoint rewind from the editor
- [ ] GitHub App + Checks: proof-replay summaries posted on PRs,
      "verified by REX" check runs with bundle links
- [ ] JetBrains extension (after VS Code proves the protocol)

Exit criteria: a developer can run a REX task, approve a plan, and rewind a
checkpoint without leaving VS Code.

## Phase 3 — Desktop power

Parity items: rewind incl. shell changes, subagent dashboard, session UX,
sandbox profiles, cross-machine resume, review surface, MCP client + skills.

- [ ] Checkpoint/rewind covers shell-made changes, not just editor writes
- [ ] Background/subagent dashboard with restrained default parallelism
- [ ] Compaction, clear, named sessions, resumable context UX
- [ ] OS-level sandbox profiles, network-off defaults
- [ ] Cross-machine and cross-directory session resume
- [ ] Managed code-review surface with proof replay
- [ ] MCP **client** + open skills catalog (server management exists)

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
7. Plans that bid binding cost/proof-coverage commitments before execution —
   DONE (d62a443e, e34f6f51): `rex exec --bid` prints the binding bid
   (budgets + worst-case dollar ceiling), `--accept-bid` executes under it,
   `--budget-usd` converts a dollar cap into a token budget; the receipt
   carries the bid, `cost_usd_estimate`, and `bid_met`/`bid_gaps`.
8. Skills marketplace ranked by verified efficacy
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
