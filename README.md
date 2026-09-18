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
