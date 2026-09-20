// Custody-wired run bridge: human-mode runs carry an operator grant,
// grant-clamped budgets and gated completion. Snapshots and views only;
// the capability token never crosses this layer.
import { backendKind } from "./backend";
import type { AgentSnapshot } from "./agentTypes";

export interface CustodiedRunView {
  grant_id: string;
  task_id: string;
  operator: string;
  worker: string;
  phase: string;
  snapshot: AgentSnapshot;
}

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
    if (!response.ok) throw new Error(`custody HTTP ${response.status}`);
    const data = (await response.json()) as T & { error?: string };
    if ((data as { error?: string }).error) throw new Error((data as { error: string }).error);
    return data;
  }
  throw new Error("no backend");
}

function selectedModel(): { provider: string; model?: string } {
  let provider = "gemini";
  let model: string | undefined;
  try {
    const selected = JSON.parse(window.localStorage.getItem("rex-model-selection") || "null") as { provider?: string; id?: string } | null;
    if (selected?.provider) provider = selected.provider;
    if (selected?.id) model = selected.id;
  } catch { /* fall back to Gemini */ }
  return { provider, model };
}

export const custodyBegin = (task: string) => {
  const { provider, model } = selectedModel();
  return call<CustodiedRunView>("custody_begin", "/api/agent/custody/runs", { task, provider, model });
};

// The permanent human Stop: terminal fence in custody first, then the run
// loop is cancelled. Tauri reads camelCase args, the sidecar snake_case;
// each transport ignores the other's spelling.
export const custodyStop = (grantId: string, runId: string) =>
  call<{ ok: boolean; reason?: string }>("custody_stop", `/api/agent/custody/runs/${runId}/stop`, {
    grantId,
    runId,
    grant_id: grantId,
  });
