// Git integration bindings: status/diff/commit through rex-tools.
// User-initiated from the Git panel; the user clicks Commit themselves.

import { invoke } from "@tauri-apps/api/core";

export interface GitFile {
  status: string;
  path: string;
}

export async function gitStatus(workspace: string): Promise<GitFile[]> {
  return invoke<GitFile[]>("git_status", { workspace });
}

export async function gitDiff(
  workspace: string,
  path: string,
): Promise<string> {
  return invoke<string>("git_diff", { workspace, path });
}

export async function gitCommit(
  workspace: string,
  message: string,
): Promise<string> {
  return invoke<string>("git_commit", { workspace, message });
}
