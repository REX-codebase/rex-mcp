# Test REX MCP in an agent host

This is a manual test, not a claim that a model-driven run passed. CI tests the stdio protocol and launcher. Codex v0.157.0 discovered tools and read a resource without inference; OpenCode v1.17.4 reported a connection without inference. Claude Code and Hermes have not had an installed-host check here.

1. Install from this private checkout with `bash scripts/rex-mcp-install.sh`. Use the absolute path of the installed binary (normally `$HOME/.rex/bin/rex-mcp`). Set `REX_STATE_DIR` to your Harness state directory and `REX_WORKSPACE` to an absolute disposable project path. The four host config examples are in [README](../README.md). Leave `REX_APPROVE_TASK_MUTATIONS` unset for read-only tests.
2. Restart the host or reload MCP servers. Check discovery: `claude mcp list`, `codex mcp list`, `opencode mcp list`, or `hermes mcp test rex`. A configured entry alone is not proof of a live connection. For Codex, `Auth: Unsupported` for stdio is an auth label, not a connection failure; check status separately.
3. In a fresh host session, ask: "Use the REX MCP server to list its tools and read its workflow quickstart resource. Tell me the names, URI and errors. Do not edit files or run commands." Confirm the actual returned tool/resource data, not a model guess.
4. Ask: "Call `rex_status` for a nonexistent task ID. Report the structured error without trying another task or changing files." This is a harmless error-path check. For a real task ID, the `rex_task_inspect` prompt offers a read-only status/events/result path. Confirm which tool and resources were actually called.
5. Preview sessions and their `preview_id` values live only in the current MCP server process. A client restart or server failure loses the browser session but does not erase the durable task. After reconnecting, inspect task status and events, resume with the current host resume handle, call `rex_preview_start` for a new session and recapture current-action visual evidence. Never assume an old `preview_id` survives or a capture from a previous action is current evidence.
6. Only after reviewing workspace and permissions, try a tiny disposable task. Ask the host to call `rex_execute`, follow its returned `task_id`, `task_capability` and scoped leases, then inspect `rex_status` and task resources. Review every proposed mutation or command. A denied or stale lease must stop the action, not trigger a bypass. Submit completion only with real evidence. REX does not force the host to keep calling tools.

### Minimal raw-stdio read-only probe

If no host CLI is available, this Python 3 probe starts the **local checkout's** built
binary, negotiates MCP, lists tools and reads the workflow quickstart. It
creates no task and keeps the process alive for all requests. Run it in a
disposable directory with `REX_STATE_DIR` and `REX_WORKSPACE` explicitly set;
no mutation flag is needed. The protocol is line-delimited JSON-RPC, not a
length-prefixed HTTP request. Use the actual built or installed absolute binary path in the example.

```python
import json, os, subprocess
from pathlib import Path

state = Path("/absolute/disposable/rex-state")
workspace = Path("/absolute/disposable/project")
workspace.mkdir(parents=True, exist_ok=True)
server = subprocess.Popen(
    ["/absolute/path/to/rex-mcp"],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True,
    env={**os.environ, "REX_STATE_DIR": str(state), "REX_WORKSPACE": str(workspace)},
)

def request(ident, method, params):
    server.stdin.write(json.dumps({"jsonrpc": "2.0", "id": ident,
                                  "method": method, "params": params}) + "\n")
    server.stdin.flush()
    reply = json.loads(server.stdout.readline())
    if "error" in reply:
        raise RuntimeError(reply["error"])
    return reply["result"]

try:
    init = request(1, "initialize", {
        "protocolVersion": "2025-11-25", "capabilities": {},
        "clientInfo": {"name": "stdio-probe", "version": "1"},
    })
    assert init["protocolVersion"] in ("2025-11-25", "2025-06-18")
    server.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
    server.stdin.flush()
    names = [item["name"] for item in request(2, "tools/list", {})["tools"]]
    guide = request(3, "resources/read", {"uri": "rex://workflow/quickstart"})
    print("rex_execute available:", "rex_execute" in names)
    print("quickstart entries:", len(guide["contents"]))
finally:
    server.stdin.close()
    server.terminate()
    server.wait(timeout=10)
```

Read the server's `isError` flag and `structuredContent` for later `tools/call`
results; JSON-RPC transport success does not mean REX accepted an action. Do
not print task capabilities or host resume handles in logs. See the first-call
contract in [`rex-mcp.md`](rex-mcp.md) before any task
creation. This probe only checks stdio discovery and read access, not whether a
model-driven host can complete a task.

For a real task, read `rex://task/{task_id}/events/0`, then page using `rex://task/{task_id}/events/{last_seq}` while a page contains events. Each page has at most 100 entries. Check `rex://task/{task_id}/result` only when available; an active task is not a terminal result. Record the last cursor, status and any gaps rather than claiming a complete history from one page.

Record host/version, connection status, server commit, tool/resource responses, errors and whether the host actually used a REX tool. Redact credentials and private task content before sharing a log. Discovery or connection is not a live agent-use test.
