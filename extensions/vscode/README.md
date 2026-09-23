# REX for VS Code

Drives the local REX harness without leaving the editor. The extension
spawns `rex serve` (the loopback-only harness daemon), discovers its port
from the daemon's first stdout line, and talks to it over HTTP.

## Commands

- **REX: Run task…** — prompts for a task, starts an interactive run.
  Approvals arrive as editor modals: plan approval shows the plan, tool
  approval shows the tool, summary and policy reason.
- **REX: Cancel active run** — cancels through the daemon.
- **REX: Show run receipt** — opens the live snapshot or finished receipt
  as JSON.
- **REX: Checkpoint run workspace** — snapshots the run's staged working
  copy on the daemon.
- **REX: Rewind run to checkpoint…** — picks a checkpoint and restores the
  run's working copy to it. File-level only: the agent keeps its current
  step and plan.

The status bar shows the active run's status, step count, tool calls and
tokens.

## Settings

- `rex.path` — path to the `rex` binary (default `rex`; must be on PATH).
- `rex.provider` — default provider (empty = `REX_PROVIDER` env or
  `anthropic`).

## Build

```sh
cd extensions/vscode
npm install
npm run compile
```

Package with `vsce` to install locally.
