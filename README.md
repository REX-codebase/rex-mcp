# REX MCP

A local stdio MCP bridge into REX custody. The host agent still does the work; REX keeps the task record, limits the workspace, checks leases and budgets, and requires evidence for completion. It cannot force a host to continue.

This is a private repository. The npm launcher is not published, so `npx @rex-codebase/rex-mcp@latest` does not yet work. From a checkout with Rust installed, run `bash scripts/rex-mcp-install.sh`. It builds and copies `rex-mcp` to `$HOME/.rex/bin/rex-mcp` (or `REX_INSTALL_DIR`). Use a literal absolute path in JSON or YAML settings, not `$HOME`.

Pin `REX_STATE_DIR` and `REX_WORKSPACE` to the intended absolute paths. Without them, state defaults to `$HOME/.rex/harness` and workspace to the process current directory. Leave `REX_APPROVE_TASK_MUTATIONS` unset for read-only use; set it to `1` only when you intend to allow file edits and allowlisted commands in that workspace.

## Claude Code

```sh
claude mcp add rex --scope user --env REX_STATE_DIR="$HOME/.rex/harness" --env REX_WORKSPACE="/absolute/project" -- "$HOME/.rex/bin/rex-mcp"
```

## Codex

```sh
codex mcp add rex --env REX_STATE_DIR="$HOME/.rex/harness" --env REX_WORKSPACE="/absolute/project" -- "$HOME/.rex/bin/rex-mcp"
```

Codex's [MCP guide](https://developers.openai.com/codex/mcp) documents the stdio command and `--env` flags. Use `/mcp` in the Codex TUI to inspect the connection.

## OpenCode V2

In `opencode.jsonc`, replace the sample absolute paths:

```jsonc
{"mcp":{"servers":{"rex":{"type":"local","command":["/home/USER/.rex/bin/rex-mcp"],"environment":{"REX_STATE_DIR":"/home/USER/.rex/harness","REX_WORKSPACE":"/absolute/project"}}}}}
```

This matches [OpenCode's V2 local MCP schema](https://opencode.ai/v2/docs/mcp-servers/).

## Hermes Agent

Add to Hermes's MCP configuration with real absolute paths:

```yaml
mcp_servers:
  rex:
    command: "/home/USER/.rex/bin/rex-mcp"
    args: []
    env:
      REX_STATE_DIR: "/home/USER/.rex/harness"
      REX_WORKSPACE: "/absolute/project"
```

Run `hermes mcp test rex`, then start a new session or `/reload-mcp`. See [Hermes config](https://hermes-agent.nousresearch.com/docs/reference/mcp-config-reference) and [verification](https://hermes-agent.nousresearch.com/docs/guides/use-mcp-with-hermes).

First verify a harmless status/read call. `rex_execute` starts an idempotent task; the host uses `rex_next`, scoped tools and `rex_submit` with evidence. A denied or stale lease is a stop, not a bypass. REX does not use the host's model credentials. [Full MCP contract](docs/rex-mcp.md) and [Ultra gates](docs/rex-mcp-ultra.md). CI runs Rust workspace and npm launcher tests; nothing publishes to npm.
