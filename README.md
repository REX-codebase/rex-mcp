# REX Harness

Desktop interface for the REX general agent. Tauri 2 shell, React + TypeScript +
Tailwind frontend. **Frontend only: no backend is connected yet.** Everything the
window shows comes from a local sample-data engine and is labeled as such
(Preview pill, receipt seals, footer note).

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

Behavior is rooted in the Fable workflow (evidence, verification, receipts)
without exposing any protocol internals.

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

## Verify the frontend

```bash
npm run build      # type-check + production build into dist/
```

The interface was pixel-verified at 360x800, 768x1024, and 1440x900 with
keyboard-only and reduced-motion passes; see `verification/`.

## Layout

```
src/            React app (components/, data/mock.ts = labeled sample engine)
src-tauri/      Tauri 2 shell (no backend commands registered yet)
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
- **Dev sidecar** - `cargo run -p rex-providers --bin rex-dev-server` bridges the same core to the vite dev frontend on 127.0.0.1:8787. `--replay gemini=crates/rex-providers/fixtures/gemini-list-models-live.json` seeds the recorded live Gemini catalog, labeled as recorded in the UI.
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
