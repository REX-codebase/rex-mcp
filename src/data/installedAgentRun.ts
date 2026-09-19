// Installed-agent run bridge: the real asynchronous Rust lifecycle (begin ->
// streamed events -> completion gate -> staged diff review -> explicit
// promotion or discard) over the desktop bridge. Snapshots only; the child
// vendor CLI keeps its own login and never touches REX credentials.
import { backendKind, type InstalledAgentId } from "./backend";

export type InstalledRunStatus = "running" | "awaiting_review" | "completed" | "failed" | "cancelled";
export type PromotionState = "not_required" | "pending" | "promoted" | "discarded";
export type DiffKind = "added" | "modified" | "deleted";

export interface DiffEntry {
  path: string;
  kind: DiffKind;
  bytes: number;
}
export interface DiffSummary {
  entries: DiffEntry[];
}
export interface InstalledAgentEvent {
  event: string;
  payload: unknown;
}
export interface InstalledRunSnapshot {
  id: string;
  backend: InstalledAgentId;
  status: InstalledRunStatus;
  prompt: string;
  workspace: string;
  staging_workspace: string;
  preview_dir: string;
  model: string | null;
  effort: string | null;
  created_at_ms: number;
  updated_at_ms: number;
  exit_code: number | null;
  events: InstalledAgentEvent[];
  stderr_tail: string;
  diff: DiffSummary | null;
  promotion: PromotionState;
  completion: string | null;
  error: string | null;
}

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const kind = await backendKind();
  if (kind !== "tauri") throw new Error("installed agents require the desktop runtime");
  const mod = await import("@tauri-apps/api/core");
  return mod.invoke<T>(cmd, args);
}

export const installedAgentAvailable = () =>
  backendKind().then((kind) => kind === "tauri").catch(() => false);

export const installedAgentBegin = (
  backend: InstalledAgentId,
  prompt: string,
  workspace = "",
  model?: string,
  effort?: string,
) => invoke<InstalledRunSnapshot>("installed_agent_begin", { backend, prompt, workspace, model: model ?? null, effort: effort ?? null });

export const installedAgentSnapshot = (runId: string) =>
  invoke<InstalledRunSnapshot>("installed_agent_snapshot", { runId });

export const installedAgentDecide = (runId: string, approved: boolean) =>
  invoke<InstalledRunSnapshot>("installed_agent_decide", { runId, approved });

export const installedAgentCancel = (runId: string) =>
  invoke<InstalledRunSnapshot>("installed_agent_cancel", { runId });

export const installedRunTerminal = (run: InstalledRunSnapshot) =>
  run.status === "completed" || run.status === "failed" || run.status === "cancelled";
