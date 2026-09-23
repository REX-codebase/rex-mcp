import { useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import {
  terminalSpawn,
  terminalWrite,
  terminalResize,
  terminalKill,
  terminalDefaultWorkspace,
  onTerminalOutput,
  onTerminalExit,
} from "../data/terminal";

// Interactive terminal bound to a workspace. The PTY lives in the Rust
// backend (workspace-validated, clean env); xterm.js only renders.
// The terminal opens on mount and is killed on unmount. If no workspace is
// given, it opens in the agent runs root.
export function TerminalView({ workspace }: { workspace?: string }) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [terminalId, setTerminalId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [exited, setExited] = useState(false);
  const [resolvedWorkspace, setResolvedWorkspace] = useState<string | null>(
    workspace ?? null,
  );

  useEffect(() => {
    let alive = true;
    if (!workspace) {
      terminalDefaultWorkspace()
        .then((w) => {
          if (alive) setResolvedWorkspace(w);
        })
        .catch((e) => {
          if (alive) setError(String(e));
        });
    }
    return () => {
      alive = false;
    };
  }, [workspace]);

  useEffect(() => {
    let term: Terminal | null = null;
    let fit: FitAddon | null = null;
    let id: string | null = null;
    let unlistenOutput: (() => void) | null = null;
    let unlistenExit: (() => void) | null = null;
    let alive = true;
    let resizeObserver: ResizeObserver | null = null;

    if (!resolvedWorkspace) return;

    const setup = async () => {
      if (!containerRef.current) return;
      const ws = resolvedWorkspace;
      try {
        term = new Terminal({
          cursorBlink: true,
          fontSize: 13,
          fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
          theme: {
            background: "#0d1117",
            foreground: "#e6edf3",
            cursor: "#e6edf3",
          },
        });
        fit = new FitAddon();
        term.loadAddon(fit);
        term.open(containerRef.current);
        fit.fit();

        // Spawn the PTY with the fitted dimensions.
        id = await terminalSpawn(ws, term.cols, term.rows);
        if (!alive) {
          await terminalKill(id);
          return;
        }
        setTerminalId(id);
        const tid = id;

        unlistenOutput = await onTerminalOutput(tid, (data) => {
          term?.write(data);
        });
        unlistenExit = await onTerminalExit(tid, () => {
          if (alive) setExited(true);
        });

        // Pipe user input to the PTY.
        term.onData((data) => {
          terminalWrite(tid, data).catch(() => {});
        });

        // Keep the PTY sized to the view.
        resizeObserver = new ResizeObserver(() => {
          if (!term || !fit || !alive) return;
          fit.fit();
          terminalResize(tid, term.cols, term.rows).catch(() => {});
        });
        if (containerRef.current) resizeObserver.observe(containerRef.current);
      } catch (e) {
        if (alive) setError(String(e));
      }
    };

    setup();

    return () => {
      alive = false;
      resizeObserver?.disconnect();
      unlistenOutput?.();
      unlistenExit?.();
      const killId = id;
      if (killId) terminalKill(killId).catch(() => {});
      term?.dispose();
    };
  }, [resolvedWorkspace]);

  if (error) {
    return (
      <div className="terminal-view terminal-view--error" role="alert">
        <p className="eyebrow">Terminal</p>
        <p>Could not open a terminal in this workspace: {error}</p>
      </div>
    );
  }

  return (
    <div className="terminal-view" aria-label="Workspace terminal">
      <div className="terminal-view-bar">
        <span className="eyebrow">Terminal</span>
        {terminalId && <span className="terminal-view-id">{terminalId}</span>}
        {exited && <span className="terminal-view-exited">shell exited</span>}
      </div>
      <div ref={containerRef} className="terminal-view-body" />
    </div>
  );
}
