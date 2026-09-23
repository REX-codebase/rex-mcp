# REX for JetBrains IDEs

Thin client over the same local `rex serve` daemon the VS Code extension
uses — the protocol is already proven, so this is the same five commands
against the same HTTP API.

## Commands (Tools menu)

- **REX: Run Task…** — prompts for a task, starts an interactive run.
  Plan and tool approvals arrive as sticky notifications with
  Approve/Deny actions.
- **REX: Cancel Active Run**
- **REX: Checkpoint Run Workspace**
- **REX: Rewind Run to Checkpoint…** — file-level rewind of the run's
  working copy; the agent keeps its step and plan.
- **REX: Show Run Receipt**

The daemon is spawned as `rex serve --port 0` on first use; the port is
read from its first stdout line (`{"port": N}`). The `rex` binary must be
on PATH.

## Build

```sh
cd extensions/jetbrains
gradle buildPlugin
```

The plugin zip lands in `build/distributions/`. Install via
Settings → Plugins → ⚙ → Install Plugin from Disk.
