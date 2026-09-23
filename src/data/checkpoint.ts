// Checkpoint bindings: workspace snapshots for one-click restore.

import { invoke } from "@tauri-apps/api/core";

export interface Checkpoint {
  id: string;
  workspace: string;
  label: string;
  created_at_ms: number;
  file_count: number;
}

export async function checkpointCreate(
  workspace: string,
  label: string,
): Promise<Checkpoint> {
  return invoke<Checkpoint>("checkpoint_create", { workspace, label });
}

export async function checkpointList(
  workspace: string,
): Promise<Checkpoint[]> {
  return invoke<Checkpoint[]>("checkpoint_list", { workspace });
}

export async function checkpointRestore(
  workspace: string,
  id: string,
): Promise<Checkpoint> {
  // Returns the auto-backup checkpoint, so the restore is reversible.
  return invoke<Checkpoint>("checkpoint_restore", { workspace, id });
}
