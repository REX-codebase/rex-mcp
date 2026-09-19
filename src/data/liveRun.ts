// Live run bridge: the real Rust pipeline (catalog -> model turn -> trusted
// approval -> tool receipt -> native preview) over either desktop bridge.
// No credential ever crosses this layer; the backend returns snapshots only.
import { backendKind } from "./backend";
import type { PreparedToolCall as PreparedCall, ToolResult } from "./backend";

export type RunStatus = "awaiting_approval" | "live" | "denied" | "failed";
export type LoopEvent =
  | { state: "model_text"; text: string }
  | { state: "approval_required"; call: PreparedCall }
  | { state: "tool_finished"; result: ToolResult }
  | { state: "finished" }
  | { state: "cancelled" }
  | { state: "limit_reached"; max_steps: number };

export interface IterationReceipt {
  iteration: number;
  accepted: boolean;
  diff_id: string;
  evidence_ids: string[];
  failed_gates: string[];
  reason: string;
}

export interface PreviewSnapshot {
  session_id: string;
  url: string;
  state: string;
  framework: string;
  iteration: number;
  receipts: IterationReceipt[];
  desktop_shot: string | null;
  mobile_shot: string | null;
}

export interface RunSnapshot {
  id: string;
  task: string;
  status: RunStatus;
  model: string;
  catalog_count: number;
  events: LoopEvent[];
  approval: PreparedCall | null;
  result: ToolResult | null;
  preview: PreviewSnapshot | null;
  error: string | null;
}

export interface CaptureEvidence {
  items: unknown[];
  screenshot_data_url?: string;
  dom_text: string;
  accessibility_text: string;
}

export type PreviewPointerAction =
  | { kind: "pointer_move"; x: number; y: number }
  | { kind: "pointer_down"; button: "primary" }
  | { kind: "pointer_up"; button: "primary" }
  | { kind: "set_viewport"; width: number; height: number; scale: number };

const SIDECAR = "http://127.0.0.1:8787";

async function call<T>(cmd: string, path: string, args?: Record<string, unknown>): Promise<T> {
  const kind = await backendKind();
  if (kind === "tauri") {
    const mod = await import("@tauri-apps/api/core");
    return mod.invoke<T>(cmd, args);
  }
  if (kind === "sidecar") {
    const response = await fetch(`${SIDECAR}${path}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(args ?? {}),
    });
    if (!response.ok) throw new Error(`run HTTP ${response.status}`);
    const data = (await response.json()) as T & { error?: string };
    if ((data as { error?: string }).error) throw new Error((data as { error: string }).error);
    return data;
  }
  throw new Error("no backend");
}

export const liveRunAvailable = () => backendKind().then((k) => k === "tauri" || k === "sidecar");
export const runBegin = (task: string) => call<RunSnapshot>("run_begin", "/api/runs", { task });
export async function runSnapshot(runId: string): Promise<RunSnapshot> {
  const kind = await backendKind();
  if (kind === "tauri") {
    const mod = await import("@tauri-apps/api/core");
    return mod.invoke<RunSnapshot>("run_snapshot", { runId });
  }
  const response = await fetch(`${SIDECAR}/api/runs/${runId}`);
  if (!response.ok) throw new Error(`run HTTP ${response.status}`);
  return (await response.json()) as RunSnapshot;
}
export const runDecide = (runId: string, approved: boolean) =>
  call<RunSnapshot>("run_decide", `/api/runs/${runId}/decision`, { runId, approved });
export const runPreviewAction = (runId: string, action: PreviewPointerAction) =>
  call<{ ok: boolean }>("run_preview_action", `/api/runs/${runId}/action`, { runId, action });
export const runCapture = (runId: string) =>
  call<CaptureEvidence>("run_capture", `/api/runs/${runId}/capture`, { runId });
export const runTeardown = (runId: string) =>
  call<{ ok: boolean }>("run_teardown", `/api/runs/${runId}/teardown`, { runId });
