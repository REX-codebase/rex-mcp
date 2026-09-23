# REX Harness

Desktop interface for the REX general agent. Tauri 2 shell, React + TypeScript +
Tailwind frontend, backed by a real Rust backend: **62 Tauri commands**, a real
model-calling agent loop with tool use, a model-driven adversary and clean-room
judge, a permission-locked (0600) credential store, and atomic daemon
persistence.

Two transports, one truthful UI:

- **Desktop** (`npm run tauri dev`) — the frontend talks to the Rust backend
  through Tauri commands.
- **Dev sidecar** (`cargo run -p rex-mcp --bin rex-dev-server`) — the same
  backend core over localhost HTTP on 127.0.0.1:8787, so `npm run dev` in a
  plain browser still reaches real providers.

When no backend is reachable, the UI runs a clearly labeled tour on sample
data (Preview pill, receipt seals, footer note) and shows a first-run setup
checklist — start the sidecar, connect a model key in Settings, run a real
task — instead of silently animating.

## What it is

One task surface, one truthful session spine:

- **Composer** - describe one task, run it (Ctrl+Enter).
- **Follow-ups** - keep talking to the agent after a run: ask for changes and
  iterate with the run's context kept, on the same receipt.
- **Prompt-native model picker** - connected models will appear inside the composer. The same menu opens animated subscription and API setup dialogs; the preview invents no connected models and performs no real connection.
- **State rail** - Idle / Working / Verifying / Done / Blocked. Blocked runs say why.
- **Result + evidence** - plain-language outcome first, then the evidence ledger
  (steps, check types, durations) and a verification summary with a receipt ID.
- **Settings** - general runtime status, motion, keyboard shortcuts, and local-preference reset. Provider setup is not a separate settings surface.
- **Past runs** - quiet history, not a dashboard.
- **Ultra browser tool** - appears inside the active task only when the sample agent calls it, then closes to a compact receipt when browser work finishes. It is never a separate tab or product surface. Standard mode has no browser UI.

Behavior is inspired by the Fable workflow (evidence, verification, receipts)
without exposing any protocol internals. The mechanical Fable gate
(Think → Prove → Attack → Write with enforced time-locks) is not yet
implemented in the harness — that is tracked work, not a current feature.

## Architecture

The frontend (`src/`) never touches credentials or models directly. It reaches
the backend through one of two transports, both serving the same Rust core:

- `src-tauri/` — Tauri 2 shell registering **62 commands** in `main.rs`
  (`provider_*`, `tool_*`, `preview_*`, `run_*`, `custody_*`, `rex_task_*`,
  `agent_*`, `ultra_*`, `installed_agent_*`, `search_*`).
- `rex-dev-server` (`crates/rex-mcp/src/bin/rex-dev-server.rs`) — the same
  provider/tool/run services over localhost-only HTTP (127.0.0.1:8787) for
  browser dev. `--replay <provider>=<fixture.json>` seeds a recorded catalog,
  labeled `source: "replay"` so the UI cannot mistake it for a fresh fetch.

Workspace crates (root `Cargo.toml`):

- `rex-providers` — provider core: live model catalogs (Gemini, Anthropic,
  OpenAI, xAI, DeepSeek, Kimi, Qwen, GLM, local, custom endpoints), access
  policy (`policy.rs`), 0600 credential store (`secrets.rs`), and the
  autonomous model-calling agent loop (`autonomous.rs`) with tool use.
- `rex-tools` — capability-scoped local tool runtime: bounded argv commands,
  path/symlink defenses, trusted-UI approvals, receipts with bounded diffs.
- `rex-custody` — task-scoped custody kernel: step/tool/time/token budgets
  that survive crashes, custody grants recording the operator.
- `rex-daemon` — durable task store (`atomic_json` task writes) and scoped
  execution for external host agents.
- `rex-mcp` — local stdio MCP server exposing the custody loop
  (`rex_execute` / `rex_next` / `rex_submit`); shippable via npx
  (`packages/rex-mcp-npm`); also hosts the dev sidecar.
- `rex-protocol` — versioned protocol types for the MCP execution surface.
- `rex-prompt` — modular system-prompt architecture (constitution, roles,
  tool contracts, epistemic state, gates).
- `rex-preview` — live-app preview trust boundary: argv-only loopback launch
  plans, evidence-driven gates (startup, console, network, desktop, mobile,
  keyboard, accessibility, screenshot, test, build).
- `rex-ultra` — Ultra mode: contract-driven runs with a model-driven adversary
  and a clean-room judge.
- `rex-installed-agents` — safe adapters for officially scriptable local
  coding agents (vendor CLIs).
- `rex-search` — provider-independent, robots-aware live web evidence
  retrieval.

## Run it

```bash
npm install
npm run dev        # web preview on http://localhost:1420
```

Desktop shell (needs a Rust toolchain and Tauri system prerequisites,
see https://v2.tauri.app/start/prerequisites/):

```bash
npm run tauri dev
npm run tauri build
```

## Test

```bash
cargo test --workspace
```

Push and pull-request CI (`.github/workflows/ci.yml`) enforces `cargo fmt
--check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace`, `npm test`, and `npm run build`.

The full suite runs on a clean Linux machine once the Tauri system
prerequisites and a Rust toolchain are installed. One rex-preview test
drives a real headless browser: install Chrome or Chromium
(`google-chrome-stable`, `google-chrome`, `chromium`, or
`chromium-browser` on PATH) first, or that single test is denied. On
Ubuntu, `chromium-browser` is a snap shim; use the Google Chrome .deb in
containers.

## Verify the frontend

```bash
npm run build      # type-check + production build into dist/
```

The interface was pixel-verified at 360x800, 768x1024, and 1440x900 with
keyboard-only and reduced-motion passes; see `verification/`.

## Layout

```
src/            React app (components/, data/mock.ts = labeled sample engine)
src-tauri/      Tauri 2 shell (62 backend commands registered in main.rs)
verification/   screenshots + recording of the verified states
brain/          design brief, thesis, token system
evidence/       grounding receipts from the build session
```

## Backend: provider connectivity (Rust)

The first real backend slice lives in `crates/rex-providers` (workspace root `Cargo.toml`):

- **Provider core** - registry for Gemini, Anthropic, OpenAI, xAI, DeepSeek, Kimi, Kimi for Coding, Qwen, GLM, local, and custom endpoints; live model-catalog fetch with pagination; normalization into one model shape; truthful error taxonomy (not-configured, auth, rate-limit, network, unsupported, empty catalog, invalid response).
- **Access policy** - `crates/rex-providers/src/policy.rs` decides which account types REX may connect, grounded in each provider's official documentation (verified 2026-09-19). Consumer subscriptions are offered only where the provider explicitly permits third-party harness use (today: Kimi for Coding); Claude Pro/Max sign-in is forbidden by Anthropic, and ChatGPT, X Premium/SuperGrok, Google AI, GLM Coding Plan, Alibaba Coding Plan, and GitHub Copilot subscriptions are refused with sourced reasons. Full matrix: `docs/subscription-policy.md`.
- **Credentials** - keys enter through the backend only, land in a permission-locked store (0600), and are never returned to the frontend, logged, or serialized.
- **Desktop app** - `src-tauri` exposes the core through Tauri commands (`provider_summaries`, `provider_set_key`, `provider_clear_key`, `provider_refresh`, `provider_catalog`).
- **Dev sidecar** - `cargo run -p rex-mcp --bin rex-dev-server` bridges the same core to the vite dev frontend on 127.0.0.1:8787. `--replay gemini=crates/rex-providers/fixtures/gemini-list-models-live.json` seeds the recorded live Gemini catalog, labeled as recorded in the UI.
- **Frontend** - the prompt-box model dropdown is driven by the backend: live catalogs, selection, refresh, disconnect, and a working Connect-API dialog. With no backend reachable it stays the labeled preview.

Tests: `cargo test -p rex-providers` (26 tests, including policy guards, a real-socket status mapping, and the recorded live Gemini response).

## Backend: local agent tools (Rust)

`crates/rex-tools` is the capability boundary for local work. Connected models
use a normalized protocol to read/search files, request exact creates/edits,
and request bounded argv-based commands inside one explicit workspace. Rust
owns path and symlink defenses, hard command denials, separate trusted-UI
approval for every write/command, time/resource/output limits, process-tree
cancellation, secret redaction, truthful typed errors, and audit receipts with
bounded diffs. See `docs/local-tools.md`.


## Live app preview runtime (Rust policy milestone)

`crates/rex-preview` defines the native trust boundary for UI projects created
inside the selected workspace. It detects static HTML, Vite, React Scripts,
Next.js, Astro, and SvelteKit from bounded manifest snapshots; creates argv-only
loopback launch plans in a reserved port range; refuses non-loopback or
wrong-port URLs; and exposes normalized pointer, keyboard, text, scroll, and
viewport actions. Screenshots, DOM/accessibility snapshots, console errors,
failed requests, viewports, gates, accepted iterations, and rejected diffs use a
bounded typed evidence protocol.

Completion is evidence-driven. A run can pass only when startup, console,
network, desktop, mobile, keyboard, accessibility, screenshot, test, and build
gates all pass. There is no self-score completion API. Cancellation is terminal
and tells the native supervisor to kill the process tree.

The current milestone contains the Rust policy/lifecycle, Tauri command shapes,
frontend bridge, visible agent-cursor/iteration/evidence UI, and adversarial unit
tests. Native preview process spawning and browser-engine capture are still open;
the Tauri bridge truthfully leaves a newly started preview in `starting` until
that supervisor lands.
