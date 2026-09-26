# REX MCP

A local stdio MCP bridge into REX custody. The host agent still does the work; REX keeps the task record, limits the workspace, checks leases and budgets, and requires evidence for completion. It cannot force a host to continue.

This is a private repository. The npm launcher is not published, so `npx @rex-codebase/rex-mcp@latest` does not yet work. From a checkout with Rust installed, run `bash scripts/rex-mcp-install.sh`. It builds and copies `rex-mcp` to `$HOME/.rex/bin/rex-mcp` (or `REX_INSTALL_DIR`). Use a literal absolute path in JSON or YAML settings, not `$HOME`.

The stdio server negotiates MCP `2025-11-25` and `2025-06-18`. A client offering an unsupported version receives the server's latest supported version and decides whether to disconnect. CI checks this handshake through a simulated stdio client; these setup examples are not verified installed-host results.

Pin `REX_STATE_DIR` and `REX_WORKSPACE` to the intended absolute paths. Without them, state defaults to `$HOME/.rex/harness` and workspace to the process current directory. Leave `REX_APPROVE_TASK_MUTATIONS` unset for read-only use; set it to `1` only when you intend to allow file edits and allowlisted commands in that workspace.

For a cautious host-by-host check, see [manual host test](docs/HOST-TEST.md).

### Ultra workflow from a host

The MCP prompt `rex_ultra_workflow` gives the host a step-by-step Ultra path
for a task; `rex_task_workflow` remains the standard path. Start with
`rex_execute` using a fresh `request_id`, the intended host, `operator_is_agent=true`,
and `ultra=true`. Retain the returned `task_id`, capability, and lease epoch.
The host, not REX, produces candidate work and submits sealed evidence. REX
runs deterministic gates; a submitted candidate is not a completed task.
See [Ultra gates](docs/rex-mcp-ultra.md) for the contract and proof format.

After a candidate qualifies, `rex_ultra_promote_start` returns a process-local
`operation_id` and a `running` state without waiting for the gate rerun. Keep
the MCP server process alive and call `rex_ultra_promote_status` with the same
`task_id`, capability, and `operation_id` until it returns `succeeded` with a
receipt or `failed` with an error. `running` is not completion. Do not retry
`start` just because it is still running. An operation handle does not survive
a server restart; inspect durable `rex_status` and `rex_proof` before any
retry. The blocking `rex_ultra_promote` remains available where the host can
wait for the complete result. No host integration here supplies model
credentials or a live agent session.


## Claude Code

```sh
claude mcp add rex --scope user --env REX_STATE_DIR="$HOME/.rex/harness" --env REX_WORKSPACE="/absolute/project" -- "$HOME/.rex/bin/rex-mcp"
```

## Codex

```sh
codex mcp add rex --env REX_STATE_DIR="$HOME/.rex/harness" --env REX_WORKSPACE="/absolute/project" -- "$HOME/.rex/bin/rex-mcp"
```

Codex's [MCP guide](https://developers.openai.com/codex/mcp) documents the stdio command and `--env` flags. Use `/mcp` in the Codex TUI to inspect the connection.

Codex CLI v0.157.0 in an isolated no-inference test discovered REX tools and resources via app-server. Reading the workflow quickstart through Codex also worked. This does not establish live agent tool use. The Auth: Unsupported column in codex mcp list is not a connection failure. See https://developers.openai.com/codex/app-server.

## OpenCode

In `opencode.jsonc`, replace the sample absolute paths:

```jsonc
{"mcp":{"rex":{"type":"local","command":["/home/USER/.rex/bin/rex-mcp"],"environment":{"REX_STATE_DIR":"/home/USER/.rex/harness","REX_WORKSPACE":"/absolute/project"}}}}
```

This matches [OpenCode's local MCP schema](https://opencode.ai/docs/mcp-servers/). A no-inference smoke test with OpenCode CLI v1.17.4 reported `rex connected` for a built local REX binary; this checks connection, not live agent tool use.

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
