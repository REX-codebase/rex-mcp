// Ultra run bridge: contract-driven, adversarially verified runs over the
// same backend bridges as Simple. Snapshots only; no credential crosses.
import { backendKind } from "./backend";
import type { AgentSnapshot } from "./agentRun";

export type UltraPhase =
  | "contracting" | "building" | "verifying" | "adversary" | "judging"
  | "repairing" | "promoted" | "rejected" | "failed" | "cancelled";

export type UltraTerminal =
  | { kind: "promoted" }
  | { kind: "rejected"; reasons: string[] }
  | { kind: "builder_failed"; detail: string }
  | { kind: "contract_failed"; errors: string[] }
  | { kind: "provider_error"; detail: string }
  | { kind: "cancelled" };

export interface ObligationView {
  id: string;
  statement: string;
  proof: { kind: string };
}

export interface ObligationOutcomeView {
  obligation_id: string;
  status: "proven" | "failed" | "awaiting_judge";
  detail: string;
  evidence_ids: string[];
  duration_ms: number;
}

export interface VerdictView {
  obligation_id: string;
  verdict: "pass" | "fail" | "unproven";
  reason: string;
  evidence: string[];
}

export interface UltraSnapshot {
  id: string;
  task: string;
  phase: UltraPhase;
  provider: string;
  model: string;
  judge_model: string;
  adversary_model: string;
  repair: number;
  max_repairs: number;
  contract: { task: string; obligations: ObligationView[] } | null;
  verification: { outcomes: ObligationOutcomeView[]; executable_all_proven: boolean } | null;
  adversary: { defects: { title: string; detail: string }[]; inconclusive: boolean; adversary_model: string } | null;
  judge: { verdicts: VerdictView[]; all_passed: boolean; judge_model: string; same_model_as_worker: boolean } | null;
  builder: AgentSnapshot | null;
  terminal: UltraTerminal | null;
  run_dir: string;
}

async function call<T>(cmd: string, path: string, args?: Record<string, unknown>): Promise<T> {
  const kind = await backendKind();
  if (kind === "tauri") {
    const mod = await import("@tauri-apps/api/core");
    return mod.invoke<T>(cmd, args);
  }
  if (kind === "sidecar") {
    throw new Error("Ultra runs through the desktop shell; the dev sidecar has no Ultra routes in this phase");
  }
  throw new Error("no backend");
}

async function get<T>(cmd: string, path: string, args?: Record<string, unknown>): Promise<T> {
  const kind = await backendKind();
  if (kind === "tauri") {
    const mod = await import("@tauri-apps/api/core");
    return mod.invoke<T>(cmd, args);
  }
  throw new Error("Ultra runs through the desktop shell; the dev sidecar has no Ultra routes in this phase");
}

export const ultraBegin = (task: string) => {
  let provider = "gemini";
  let model: string | undefined;
  try {
    const selected = JSON.parse(window.localStorage.getItem("rex-model-selection") || "null") as { provider?: string; id?: string } | null;
    if (selected?.provider) provider = selected.provider;
    if (selected?.id) model = selected.id;
  } catch { /* fall back to Gemini */ }
  return call<UltraSnapshot>("ultra_begin", "/runs", { task, provider, model });
};
export const ultraSnapshot = (runId: string) => get<UltraSnapshot>("ultra_snapshot", `/runs/${runId}`, { runId });
export const ultraDecide = (runId: string, approved: boolean) =>
  call<UltraSnapshot>("ultra_decide", `/runs/${runId}/decision`, { runId, approved });
export const ultraCancel = (runId: string) =>
  call<UltraSnapshot>("ultra_cancel", `/runs/${runId}/cancel`, { runId });
