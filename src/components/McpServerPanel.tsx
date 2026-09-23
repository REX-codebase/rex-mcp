import { useEffect, useState } from "react";
import {
  mcpAddServer,
  mcpListServers,
  mcpProbeServer,
  mcpRemoveServer,
  mcpSetServerEnabled,
  mcpSetToolEnabled,
  type McpServer,
} from "../data/mcp";

// MCP server management: attach servers by command, probe them for their
// tool list, and toggle servers/tools individually. Disabled tools are not
// offered to the model.
export function McpServerPanel() {
  const [servers, setServers] = useState<McpServer[]>([]);
  const [name, setName] = useState("");
  const [command, setCommand] = useState("");
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = async () => {
    try {
      setServers(await mcpListServers());
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    refresh();
  }, []);

  const add = async () => {
    if (!name.trim() || !command.trim() || busy) return;
    setBusy(true);
    setError(null);
    try {
      await mcpAddServer(name.trim(), command.trim());
      setName("");
      setCommand("");
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const probe = async (id: string) => {
    setBusy(true);
    setError(null);
    try {
      await mcpProbeServer(id);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const remove = async (id: string, serverName: string) => {
    if (!window.confirm(`Remove MCP server “${serverName}”?`)) return;
    setBusy(true);
    try {
      await mcpRemoveServer(id);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const toggleServer = async (id: string, enabled: boolean) => {
    try {
      await mcpSetServerEnabled(id, !enabled);
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  const toggleTool = async (
    serverId: string,
    toolName: string,
    enabled: boolean,
  ) => {
    try {
      await mcpSetToolEnabled(serverId, toolName, !enabled);
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <section aria-label="MCP servers" className="settings-section">
      <h2 className="eyebrow">MCP servers</h2>
      <p className="settings-note">
        Attach external MCP servers by command. Probe to discover their tools,
        then toggle servers or individual tools. Disabled tools are never
        offered to the model.
      </p>
      {error && (
        <p className="mcp-error" role="alert">
          {error}
        </p>
      )}
      <div className="mcp-add">
        <input
          type="text"
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="Server name…"
          aria-label="Server name"
          disabled={busy}
        />
        <input
          type="text"
          value={command}
          onChange={(e) => setCommand(e.target.value)}
          placeholder="Command, e.g. npx -y @modelcontextprotocol/server-filesystem /tmp"
          aria-label="Server command"
          disabled={busy}
          spellCheck={false}
        />
        <button
          type="button"
          className="approval-allow"
          onClick={add}
          disabled={!name.trim() || !command.trim() || busy}
        >
          Add
        </button>
      </div>
      {loading ? (
        <p>Loading…</p>
      ) : servers.length === 0 ? (
        <p className="mcp-empty">No MCP servers attached.</p>
      ) : (
        <ul className="mcp-list">
          {servers.map((s) => (
            <li key={s.id} className="mcp-server">
              <div className="mcp-server-head">
                <div>
                  <b>{s.name}</b>
                  <code>{s.command}</code>
                  {s.last_probe_at_ms !== null && (
                    <small>
                      {s.last_probe_ok
                        ? `✓ probed ${new Date(s.last_probe_at_ms).toLocaleString()}`
                        : `✗ probe failed: ${s.last_probe_error ?? "unknown"}`}
                    </small>
                  )}
                </div>
                <div className="mcp-server-actions">
                  <label>
                    <input
                      type="checkbox"
                      checked={s.enabled}
                      onChange={() => toggleServer(s.id, s.enabled)}
                    />
                    Enabled
                  </label>
                  <button
                    type="button"
                    className="reset-button"
                    onClick={() => probe(s.id)}
                    disabled={busy}
                  >
                    Probe
                  </button>
                  <button
                    type="button"
                    className="reset-button"
                    onClick={() => remove(s.id, s.name)}
                    disabled={busy}
                  >
                    Remove
                  </button>
                </div>
              </div>
              {s.tools.length > 0 && (
                <ul className="mcp-tools">
                  {s.tools.map((t) => (
                    <li key={t.name}>
                      <label>
                        <input
                          type="checkbox"
                          checked={t.enabled}
                          onChange={() => toggleTool(s.id, t.name, t.enabled)}
                          disabled={!s.enabled}
                        />
                        <b>{t.name}</b>
                        {t.description && <span>{t.description}</span>}
                      </label>
                    </li>
                  ))}
                </ul>
              )}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
