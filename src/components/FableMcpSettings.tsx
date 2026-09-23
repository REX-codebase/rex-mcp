import { useState } from "react";
import {
  fableMcpProbe,
  loadFableMcpCommand,
  saveFableMcpCommand,
  type FableMcpLink,
} from "../data/fable";

// Settings panel for the harness → fable-mode MCP link. The native rex-fable
// gate (timer + unlock rule) always runs in Rust; this link is for the
// external Fable Engine MCP server, which adds the full action map
// (scraping, red-teaming, rubrics). The probe is honest: it reports what the
// server actually answers, or why the link failed.
export function FableMcpSettings() {
  const [command, setCommand] = useState(() => loadFableMcpCommand());
  const [link, setLink] = useState<FableMcpLink | null>(null);
  const [probing, setProbing] = useState(false);

  const probe = async () => {
    const cmd = command.trim();
    if (!cmd || probing) return;
    setProbing(true);
    saveFableMcpCommand(cmd);
    try {
      setLink(await fableMcpProbe(cmd));
    } catch (e) {
      setLink({
        available: false,
        server_command: cmd,
        tools: [],
        has_fable_session_tool: false,
        error: String(e),
      });
    } finally {
      setProbing(false);
    }
  };

  return (
    <section aria-label="Fable MCP link" className="settings-section">
      <h2 className="eyebrow">Fable MCP link</h2>
      <p className="settings-note">
        The native Fable gate (deliberation timer + evidence-gated unlock) is built into REX.
        Link an external Fable Engine MCP server here to also get its full action map —
        research scrapers, red-team review, goal rubrics — through the{" "}
        <code>fable_session</code> tool.
      </p>
      <div className="settings-row settings-row-wrap">
        <label className="text-sm text-text" htmlFor="fable-mcp-command">
          Server command
        </label>
        <input
          id="fable-mcp-command"
          type="text"
          value={command}
          onChange={(e) => setCommand(e.target.value)}
          placeholder="e.g. python -m fable_engine.mcp_server"
          className="settings-input"
          spellCheck={false}
        />
        <button
          type="button"
          className="reset-button"
          onClick={probe}
          disabled={!command.trim() || probing}
        >
          {probing ? "Probing…" : "Probe link"}
        </button>
      </div>
      {link && (
        <div
          className="settings-row settings-row-wrap"
          role="status"
          aria-label={link.available ? "Fable MCP server linked" : "Fable MCP server not linked"}
        >
          <span className="flex items-center gap-3">
            <span
              className={`h-1.5 w-1.5 rounded-full ${link.available ? "bg-done" : "bg-line"}`}
              aria-hidden="true"
            />
            <span className="text-sm text-text">
              {link.available
                ? `Linked · fable_session tool found (${link.tools.length} tools)`
                : "Not linked · native gate only"}
            </span>
          </span>
          {link.error && <span className="text-xs text-faint">{link.error}</span>}
        </div>
      )}
      {!link && (
        <p className="settings-note">
          No server configured. The Fable gate toggle still works — it uses the native gate.
        </p>
      )}
    </section>
  );
}
