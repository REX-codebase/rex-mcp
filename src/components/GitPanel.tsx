import { useEffect, useState } from "react";
import { gitStatus, gitDiff, gitCommit, type GitFile } from "../data/git";
import { terminalDefaultWorkspace } from "../data/terminal";

// Git panel: status, per-file diff, and commit. All user-initiated — the
// user clicks Commit themselves. Runs through rex-tools' sandboxed git.
export function GitPanel() {
  const [workspace, setWorkspace] = useState<string | null>(null);
  const [files, setFiles] = useState<GitFile[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [diff, setDiff] = useState<string | null>(null);
  const [message, setMessage] = useState("");
  const [loading, setLoading] = useState(false);
  const [committing, setCommitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<string | null>(null);

  const refresh = async (ws: string) => {
    setLoading(true);
    setError(null);
    try {
      setFiles(await gitStatus(ws));
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    let alive = true;
    terminalDefaultWorkspace()
      .then((ws) => {
        if (!alive) return;
        setWorkspace(ws);
        refresh(ws);
      })
      .catch((e) => {
        if (alive) setError(String(e));
      });
    return () => {
      alive = false;
    };
  }, []);

  const showDiff = async (path: string) => {
    if (!workspace) return;
    setSelected(path);
    setDiff(null);
    try {
      setDiff(await gitDiff(workspace, path));
    } catch (e) {
      setError(String(e));
    }
  };

  const commit = async () => {
    if (!workspace || !message.trim() || committing) return;
    setCommitting(true);
    setError(null);
    setResult(null);
    try {
      const out = await gitCommit(workspace, message.trim());
      setResult(out);
      setMessage("");
      setSelected(null);
      setDiff(null);
      await refresh(workspace);
    } catch (e) {
      setError(String(e));
    } finally {
      setCommitting(false);
    }
  };

  if (!workspace) {
    return (
      <div className="git-panel" aria-label="Git">
        <p className="eyebrow">Git</p>
        {error ? <p role="alert">{error}</p> : <p>Loading…</p>}
      </div>
    );
  }

  return (
    <div className="git-panel" aria-label="Git">
      <div className="git-panel-bar">
        <span className="eyebrow">Git</span>
        <button
          type="button"
          className="reset-button"
          onClick={() => refresh(workspace)}
          disabled={loading}
        >
          Refresh
        </button>
      </div>
      {error && (
        <p className="git-panel-error" role="alert">
          {error}
        </p>
      )}
      {result && <pre className="git-panel-result">{result}</pre>}
      {loading ? (
        <p>Loading status…</p>
      ) : files.length === 0 ? (
        <p className="git-panel-empty">Working tree clean.</p>
      ) : (
        <ul className="git-panel-files">
          {files.map((f) => (
            <li key={f.path}>
              <button
                type="button"
                className={selected === f.path ? "is-selected" : ""}
                onClick={() => showDiff(f.path)}
              >
                <span className="git-status">{f.status.trim() || "?"}</span>
                <span className="git-path">{f.path}</span>
              </button>
            </li>
          ))}
        </ul>
      )}
      {selected && diff !== null && (
        <div className="git-panel-diff">
          <p className="git-path">{selected}</p>
          <pre>{diff || "(no diff)"}</pre>
        </div>
      )}
      <div className="git-panel-commit">
        <input
          type="text"
          value={message}
          onChange={(e) => setMessage(e.target.value)}
          placeholder="Commit message…"
          aria-label="Commit message"
          disabled={committing || files.length === 0}
        />
        <button
          type="button"
          className="approval-allow"
          onClick={commit}
          disabled={!message.trim() || committing || files.length === 0}
        >
          {committing ? "Committing…" : "Commit"}
        </button>
      </div>
      <p className="settings-note">
        Commits run <code>git add -A</code> then <code>git commit</code> in the
        workspace. This is your action — no model approval involved.
      </p>
    </div>
  );
}
