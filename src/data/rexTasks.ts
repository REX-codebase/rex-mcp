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
export const rexTaskBegin = (task: string, ultra = false) =>
  post<{ task_id: string; state: string }>("rex_task_begin", "/api/rex/tasks", { task, ultra });
export const rexTaskFollowUp = (taskId: string, task: string) =>
  post<{ task_id: string; state: string; resumed: boolean }>(
    "rex_task_follow_up",
    `/api/rex/tasks/${taskId}/follow-up`,
    { taskId, task }
  );
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

// The deterministic per-task proof bundle: frozen plan, kernel state, the
// qualified candidate, promotion receipt, full event stream and a
// deterministic hash - the artifact the proof journey is rendered from.
export interface RexProof {
  task_id: string;
  state: string;
  kernel_state?: string | null;
  qualified_candidate?: string | null;
  promotion_state?: string | null;
  skill_plan?: { selected?: string[]; unsupported?: string[] } | null;
  bundle_hash: string;
}

export const rexTaskProof = (taskId: string) =>
  get<RexProof>("rex_task_proof", `/api/rex/tasks/${taskId}/proof`, { taskId });

export interface ProofPhase {
  id: string;
  label: string;
  state: "done" | "active" | "pending" | "failed";
}

// One durable proof journey, derived only from the recorded event stream:
// a vertical phase spine whose nodes flip done/active/failed exactly when
// the daemon's own events say so. Standard tasks have no Ultra events and
// get no spine - the plain event list stays their truthful surface.
export function proofJourneyPhases(events: RexEvent[]): ProofPhase[] {
  if (!events.some((e) => e.kind.startsWith("ultra_"))) return [];
  const has = (kind: string) => events.some((e) => e.kind === kind);
  const submissions = events.filter((e) => e.kind === "ultra_submission");
  const kindOf = (e: RexEvent) => String((e.detail as { kind?: string }).kind ?? "");
  const hasCandidate = submissions.some((e) => kindOf(e) === "Candidate");
  const hasEvidence = submissions.some((e) =>
    ["Adversary", "Verifier", "Visual"].includes(kindOf(e))
  );
  const promotion = events.find((e) => e.kind === "ultra_promotion");
  const promoState = promotion
    ? String((promotion.detail as { state?: string }).state ?? "")
    : "";
  const terminal = events.find(
    (e) => e.kind === "task_completed" || e.kind === "task_failed" || e.kind === "task_cancelled"
  );
  const phases: ProofPhase[] = [
    { id: "created", label: "Task created", state: has("task_created") ? "done" : "pending" },
    { id: "skills", label: "Skill plan frozen (enforced at promotion)", state: has("ultra_skill_plan") ? "done" : "pending" },
    { id: "candidates", label: "Candidate theses collected", state: hasCandidate ? "done" : "pending" },
    {
      id: "evidence",
      label: "Evidence submitted (gates not independently enforced)",
      state: hasEvidence ? (promotion || terminal ? "done" : "active") : "pending",
    },
    {
      id: "promotion",
      label: "Promotion (gated by the frozen plan)",
      state: promotion ? (promoState === "committed" ? "done" : "failed") : "pending",
    },
    {
      id: "terminal",
      label: terminal
        ? terminal.kind === "task_completed"
          ? "Completed"
          : terminal.kind === "task_failed"
            ? "Failed"
            : "Cancelled"
        : "Terminal truth",
      state: terminal ? (terminal.kind === "task_completed" ? "done" : "failed") : "pending",
    },
  ];
  if (!terminal) {
    const next = phases.find((p) => p.state === "pending");
    if (next) next.state = "active";
  }
  return phases;
}
