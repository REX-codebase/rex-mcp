import { DiffEditor } from "@monaco-editor/react";
import type { FileDiff } from "../data/workspace";

// Monaco diff viewer for the approval flow. Shows the original vs proposed
// content side by side. Read-only — the user approves or denies, they don't
// edit here.
export function DiffViewer({ diff }: { diff: FileDiff }) {
  const language = guessLanguage(diff.path);
  return (
    <div className="diff-viewer" aria-label={`Diff for ${diff.path}`}>
      <p className="diff-viewer-path">{diff.path}</p>
      <DiffEditor
        height="320px"
        language={language}
        original={diff.original}
        modified={diff.modified}
        options={{
          readOnly: true,
          renderSideBySide: true,
          minimap: { enabled: false },
          scrollBeyondLastLine: false,
        }}
      />
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
