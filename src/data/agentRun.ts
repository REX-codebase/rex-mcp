// Autonomous agent run bridge: the real Rust loop (plan -> bounded turns ->
// trusted approvals -> tool receipts -> gate verification) over either
// desktop bridge. Snapshots only; no credential ever crosses this layer.
import { backendKind } from "./backend";
import type { PreparedToolCall as PreparedCall, ToolResult } from "./backend";
import type { CaptureEvidence, PreviewPointerAction } from "./liveRun";
import {
  SIDECAR_AGENT,
  type AgentEvent,
  type AgentPreview,
  type AgentSnapshot,
  type AgentStatus,
  type PlanItem,
  type TerminalReason,
} from "./agentTypes";

export type {
  AgentEvent,
  AgentPreview,
  AgentSnapshot,
  AgentStatus,
  PlanItem,
  TerminalReason,
};

async function call<T>(cmd: string, path: string, args?: Record<string, unknown>): Promise<T> {
  const kind = await backendKind();
  if (kind === "tauri") {
    const mod = await import("@tauri-apps/api/core");
    return mod.invoke<T>(cmd, args);
  }
  if (kind === "sidecar") {
    const response = await fetch(`${SIDECAR_AGENT}${path}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(args ?? {}),
    });
    if (!response.ok) throw new Error(`agent HTTP ${response.status}`);
    const data = (await response.json()) as T & { error?: string };
    if ((data as { error?: string }).error) throw new Error((data as { error: string }).error);
    return data;
  }
  throw new Error("no backend");
}

async function get<T>(cmd: string, path: string, args?: Record<string, unknown>): Promise<T> {
  const kind = await backendKind();
  if (kind === "tauri") {
    const mod = await import("@tauri-apps/api/core");
    return mod.invoke<T>(cmd, args);
  }
  const response = await fetch(`${SIDECAR_AGENT}${path}`);
  if (!response.ok) throw new Error(`agent HTTP ${response.status}`);
  return (await response.json()) as T;
}

export const agentBegin = (task: string) => {
  let provider = "gemini";
  let model: string | undefined;
  try {
    const selected = JSON.parse(window.localStorage.getItem("rex-model-selection") || "null") as { provider?: string; id?: string } | null;
    if (selected?.provider) provider = selected.provider;
    if (selected?.id) model = selected.id;
  } catch { /* fall back to Gemini */ }
  return call<AgentSnapshot>("agent_begin", "/runs", { task, provider, model });
};
export const agentSnapshot = (runId: string) => get<AgentSnapshot>("agent_snapshot", `/runs/${runId}`, { runId });
export const agentDecide = (runId: string, approved: boolean) =>
  call<AgentSnapshot>("agent_decide", `/runs/${runId}/decision`, { runId, approved });
// "Allow for this run": approve the pending command and every later run of
// the exact same command line until the run ends. Desktop app only; the dev
// sidecar has no route for it, so it is never offered there.
export const agentDecideAlways = async (runId: string) => {
  if ((await backendKind()) !== "tauri") throw new Error("Allow for this run needs the desktop app");
  return call<AgentSnapshot>("agent_decide_always", `/runs/${runId}/decision`, { runId });
};
export const canAllowForRun = async () => (await backendKind()) === "tauri";
export const agentCancel = (runId: string) =>
  call<AgentSnapshot>("agent_cancel", `/runs/${runId}/cancel`, { runId });
export const agentResume = (runId: string) =>
  call<AgentSnapshot>("agent_resume", `/runs/${runId}/resume`, { runId });
export const agentAnswer = (runId: string, answer: string | null) =>
  call<AgentSnapshot>("agent_answer", `/runs/${runId}/answer`, { runId, answer });
/** Answer every question of an ask_user batch at once (null declines one). */
export const agentAnswerMany = (runId: string, answers: (string | null)[]) =>
  call<AgentSnapshot>("agent_answer", `/runs/${runId}/answer`, { runId, answer: null, answers });
export type UndoResult = {
  ok: boolean;
  call_id: string | null;
  tool: string | null;
  files: string[];
  undone?: { seq: number; tool: string; call_id: string }[];
};
/**
 * Undo the newest write; or rewind (all or nothing) to just after the write
 * made by tool call `afterCall`, or to journal entry `to` (0 = every write).
 */
export const agentUndo = (runId: string, target?: { afterCall?: string; to?: number }) =>
  call<UndoResult>("agent_undo", `/runs/${runId}/undo`, { runId, ...(target ?? {}) });
export const agentPreviewAction = (runId: string, action: PreviewPointerAction) =>
  call<{ ok: boolean }>("agent_preview_action", `/runs/${runId}/action`, { runId, action });
export const agentCapture = (runId: string) =>
  call<CaptureEvidence>("agent_capture", `/runs/${runId}/capture`, { runId });
export const agentTeardown = (runId: string) =>
  call<{ ok: boolean }>("agent_teardown", `/runs/${runId}/teardown`, { runId });
