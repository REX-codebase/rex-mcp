# REX MCP launcher

This package is the one-command launcher for the local REX Harness MCP server.
It contains a native `rex-mcp` binary and starts it over stdio. It does not
install Rust, clone a repository, run a background service, or send telemetry.

The package is staged but intentionally unpublished while REX Harness is
private. After the release checklist passes, the intended MCP command is:

```sh
npx --yes @rex-codebase/rex-mcp@latest
```

> **v0.1.0 ships the Linux x64 binary only.** macOS and Windows binaries land
> in a follow-up release built from the desktop release pipeline. On other
> platforms the launcher exits with a clear "unsupported platform" message.

MCP host configuration:

```json
{
  "mcpServers": {
    "rex": {
      "command": "npx",
      "args": ["--yes", "@rex-codebase/rex-mcp@latest"],
      "env": {
        "REX_STATE_DIR": "/absolute/path/to/.rex/harness",
        "REX_WORKSPACE": "/absolute/path/to/project",
        "REX_APPROVE_TASK_MUTATIONS": "1"
      }
    }
  }
}
```

Set `REX_APPROVE_TASK_MUTATIONS=1` only when the host is allowed to change the
selected workspace. Without it, mutation tools fail closed.

Supported release targets: Linux x64/arm64, macOS x64/arm64, and Windows x64.
The desktop Harness remains the owner of local custody, permissions and state;
this package is only its stdio entry point.
