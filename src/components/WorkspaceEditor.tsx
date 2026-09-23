import { useEffect, useState } from "react";
import Editor from "@monaco-editor/react";
import {
  workspaceReadFile,
  workspaceWriteFile,
  workspaceListFiles,
} from "../data/workspace";
import { terminalDefaultWorkspace } from "../data/terminal";

// Inline workspace editor. The user picks a file, edits it in Monaco, and
// saves. This is the user's own edit — it does not go through the model
// approval gate. The backend validates the path stays under the workspace.
export function WorkspaceEditor() {
  const [workspace, setWorkspace] = useState<string | null>(null);
  const [files, setFiles] = useState<string[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [content, setContent] = useState("");
  const [dirty, setDirty] = useState(false);
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    terminalDefaultWorkspace()
      .then(async (ws) => {
        if (!alive) return;
        setWorkspace(ws);
        setFiles(await workspaceListFiles(ws));
      })
      .catch((e) => {
        if (alive) setError(String(e));
      });
    return () => {
      alive = false;
    };
  }, []);

  const openFile = async (path: string) => {
    if (!workspace) return;
    if (dirty && !window.confirm("Discard unsaved changes?")) return;
    setLoading(true);
    setError(null);
    try {
      const text = await workspaceReadFile(workspace, path);
      setSelected(path);
      setContent(text);
      setDirty(false);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };

  const save = async () => {
    if (!workspace || !selected || !dirty) return;
    setSaving(true);
    setError(null);
    try {
      await workspaceWriteFile(workspace, selected, content);
      setDirty(false);
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  if (error && !workspace) {
    return (
      <div className="workspace-editor" role="alert">
        <p>Could not open the workspace: {error}</p>
      </div>
    );
  }

  return (
    <div className="workspace-editor" aria-label="Workspace editor">
      <div className="workspace-editor-bar">
        <span className="eyebrow">Editor</span>
        <select
          aria-label="Open file"
          value={selected ?? ""}
          onChange={(e) => openFile(e.target.value)}
          disabled={loading || files.length === 0}
        >
          <option value="" disabled>
            {files.length === 0 ? "No files" : "Select a file…"}
          </option>
          {files.map((f) => (
            <option key={f} value={f}>
              {f}
            </option>
          ))}
        </select>
        <button
          type="button"
          onClick={save}
          disabled={!dirty || saving}
          className="reset-button"
        >
          {saving ? "Saving…" : dirty ? "Save ●" : "Saved"}
        </button>
      </div>
      {error && (
        <p className="workspace-editor-error" role="alert">
          {error}
        </p>
      )}
      <div className="workspace-editor-body">
        {selected ? (
          <Editor
            height="480px"
            language={guessLanguage(selected)}
            value={content}
            onChange={(v) => {
              setContent(v ?? "");
              setDirty(true);
            }}
            options={{
              minimap: { enabled: false },
              scrollBeyondLastLine: false,
              fontSize: 13,
            }}
          />
        ) : (
          <p className="workspace-editor-empty">
            Select a file to edit. Your saves write directly — no approval
            needed, this is your edit.
          </p>
        )}
      </div>
    </div>
  );
}

function guessLanguage(path: string): string {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  switch (ext) {
    case "ts":
    case "tsx":
      return "typescript";
    case "js":
    case "jsx":
      return "javascript";
    case "rs":
      return "rust";
    case "py":
      return "python";
    case "json":
      return "json";
    case "md":
      return "markdown";
    case "css":
      return "css";
    case "html":
      return "html";
    case "toml":
      return "toml";
    case "yaml":
    case "yml":
      return "yaml";
    default:
      return "plaintext";
  }
}
