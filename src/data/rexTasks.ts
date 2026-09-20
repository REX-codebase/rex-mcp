// Agent-mode supervision bridge: durable REX tasks opened by host agents
// through rex-mcp. The UI reads status and events and holds the permanent
// human Stop - all through the same MCP tool surface hosts use, never a
// private side channel into the daemon.
import { backendKind } from "./backend";

export interface RexTaskSummary {
  task_id: string;
  task: string;
  state: string;
  host: string;
  operator_is_agent: boolean;
}

export interface RexStatus {
  task_id: string;
  state: string;
  task: string;
  operator_is_agent: boolean;
  host: string;
  open_action?: { action_id: string; instructions: string } | null;
  last_event_seq: number;
}

export interface RexEvent {
  seq: number;
  ts_ms: number;
  kind: string;
  detail: Record<string, unknown>;
}

export interface RexEvents {
  events: RexEvent[];
  last_seq: number;
}

const SIDECAR = "http://127.0.0.1:8787";

async function post<T>(cmd: string, path: string, args?: Record<string, unknown>): Promise<T> {
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
    if (!response.ok) throw new Error(`rex HTTP ${response.status}`);
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
  if (kind === "sidecar") {
    const response = await fetch(`${SIDECAR}${path}`);
    if (!response.ok) throw new Error(`rex HTTP ${response.status}`);
    const data = (await response.json()) as T & { error?: string };
    if ((data as { error?: string }).error) throw new Error((data as { error: string }).error);
    return data;
  }
  throw new Error("no backend");
}

export const rexTaskList = () => get<{ tasks: RexTaskSummary[] }>("rex_tasks", "/api/rex/tasks");
export const rexTaskBegin = (task: string) =>
  post<{ task_id: string; state: string }>("rex_task_begin", "/api/rex/tasks", { task });
export const rexTaskStatus = (taskId: string) =>
  get<RexStatus>("rex_task_status", `/api/rex/tasks/${taskId}/status`, { taskId });
export const rexTaskEvents = (taskId: string, since: number) =>
  get<RexEvents>("rex_task_events", `/api/rex/tasks/${taskId}/events?since=${since}`, {
    taskId,
    afterSeq: since,
  });
export const rexTaskStop = (taskId: string) =>
  post<{ task_id: string; state: string; final_reason: string }>(
    "rex_task_stop",
    `/api/rex/tasks/${taskId}/stop`,
    { taskId }
  );
