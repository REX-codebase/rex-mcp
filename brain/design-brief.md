# REX Harness UI - design brief card

User: a developer running the REX Harness general agent on their desktop (Agrim is first user).
Job: hand the agent one task, watch it execute truthfully, and receive a result they can trust.
Primary action: submit a task (one input, one button).
Proof the user needs: what the agent did, what it checked, what it produced - a human-readable receipt with evidence behind it.
Content shape: task composer -> run state -> result summary -> expandable evidence ledger.
Constraints: Tauri 2 desktop, frontend only, no backend (sample data labeled), restrained high-end purple-black, no generic AI aesthetics, keyboard + reduced motion + small-to-large viewports.

Design thesis: trust is the product, so the receipt chain is the spatial spine - the whole window is organized around one task's journey from instruction to verified receipt.

Distinctive move: a vertical task spine. Instruction at top, live state rail in the middle, result and receipt below. The evidence ledger is the only ornamental structure: hairline connectors, monospace receipt IDs, quiet timestamps. Color is reserved for state change (idle/working/done/blocked) - everything else stays in the neutral violet-black range.

What stays quiet: model picker is one compact dropdown; settings are one icon; no sidebar nav, no hero, no gradients-as-decoration, no glow, no chat bubbles.

## Product language (Fable-rooted, no raw protocol)
- Run states: Idle / Working / Verifying / Done / Blocked (truthful, never invented progress).
- Result: plain-language outcome first, "Evidence" expander second (steps, checks, files, receipts).
- Evidence items show: step name, check type (Test / File / Command / Review), outcome, duration.
- Fable is present as behavior: a "Verification" section in each run receipt, worded for humans ("3 invariants checked", "Receipt #F-1207"), never JSON-RPC or session internals.
- Capability truth: with no backend connected the app says so - a persistent, quiet "Preview - sample data" marker on the run area, and a disabled-with-reason Run action only when no task text exists. The Run action itself works against the local mock engine and is labeled as such.
