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

1. Machine-verifiable signed run certificates
2. Repo-specific adversarial immune system
3. Cross-provider tournaments with deterministic promotion
4. Epistemic provenance at diff-hunk level
5. Proof replay as the primary review primitive
6. Time-locked disconnected operation (dead-man custody, verified rollback)
7. Plans that bid binding cost/proof-coverage commitments before execution
8. Skills marketplace ranked by verified efficacy
9. Air-gapped offline verification
10. Cross-repo proof contracts

Exit criteria per bet: a demo where the capability is *exercised*, not
described — e.g. a certificate that verifies on a second machine with no
REX installed.

## Non-goals

- Chasing model quality. REX is model-agnostic by design; every frontier
  model release makes the harness stronger for free. The moat is mechanical.
- Feature parity for its own sake. If a parity item doesn't serve the
  proof story, it gets cut or deferred.
