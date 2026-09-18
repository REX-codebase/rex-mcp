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
- **Model status** - honest by construction. No runtime is connected, so the
  control says so and points at Settings instead of listing invented models.
- **State rail** - Idle / Working / Verifying / Done / Blocked. Blocked runs say why.
- **Result + evidence** - plain-language outcome first, then the evidence ledger
  (steps, check types, durations) and a verification summary with a receipt ID.
- **Settings** - runtime status, a working motion preference, keyboard
  shortcuts, and a reset for local preferences.
- **Past runs** - quiet history, not a dashboard.

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
