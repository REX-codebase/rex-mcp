// Workspace file bindings: direct read/write for the inline editor,
// and diff fetch for the approval flow.

import { invoke } from "@tauri-apps/api/core";

export interface FileDiff {
  path: string;
  original: string;
  modified: string;
}

export async function toolCallDiff(callId: string): Promise<FileDiff | null> {
  return invoke<FileDiff | null>("tool_call_diff", { callId });
}

export async function workspaceReadFile(
  workspace: string,
  path: string,
): Promise<string> {
  return invoke<string>("workspace_read_file", { workspace, path });
}

export async function workspaceWriteFile(
  workspace: string,
  path: string,
  content: string,
): Promise<void> {
  return invoke<void>("workspace_write_file", { workspace, path, content });
}

export async function workspaceListFiles(workspace: string): Promise<string[]> {
  return invoke<string[]>("workspace_list_files", { workspace });
}
