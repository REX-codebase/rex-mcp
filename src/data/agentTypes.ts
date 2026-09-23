// Shared snapshot types for the autonomous agent loop. Mirrors
// rex-providers/src/autonomous.rs exactly; keep snake_case fields in sync.
import type { PreparedToolCall as PreparedCall, ToolResult } from "./backend";

export const SIDECAR_AGENT = "http://127.0.0.1:8787/api/agent";

export interface PlanItem {
  id: string;
  title: string;
  status: "pending" | "in_progress" | "done" | "blocked";
  note?: string;
}

export type AgentStatus =
  | "planning"
  | "running"
  | "awaiting_approval"
  | "awaiting_plan"
  | "verifying"
  | "completed"
  | "blocked"
  | "denied"
  | "cancelled"
  | "failed";

export type TerminalReason =
  | { kind: "completed" }
  | { kind: "gates_failed"; failures: string[] }
  | { kind: "blocked"; detail: string }
  | { kind: "budget_steps"; max_steps: number }
  | { kind: "budget_time"; max_wall_ms: number }
  | { kind: "budget_tokens"; max_tokens: number }
  | { kind: "budget_tool_calls"; max_tool_calls: number }
  | { kind: "repeated_failure"; tool: string }
  | { kind: "no_progress"; turns: number }
  | { kind: "cancelled" }
  | { kind: "denied" }
  | { kind: "approval_timeout" }
  | { kind: "provider_error"; detail: string }
  | { kind: "model_stalled" };

export type AgentEvent =
  | { state: "plan_updated"; items: PlanItem[] }
  | { state: "model_text"; text: string }
  | { state: "tool_finished"; result: ToolResult }
  | { state: "approval_required"; call: PreparedCall }
  | { state: "approval_resolved"; call_id: string; approved: boolean }
  | { state: "plan_approval_required"; items: PlanItem[] }
  | { state: "plan_approval_resolved"; approved: boolean }
  | { state: "gate_result"; attempt: number; passed: boolean; failures: string[] }
  | { state: "retry"; attempt: number; reason: string }
  | { state: "info"; message: string };

export interface IterationReceipt {
  iteration: number;
  accepted: boolean;
  diff_id: string;
  evidence_ids: string[];
  failed_gates: string[];
  reason: string;
}

export interface AgentPreview {
  session_id: string;
  url: string;
  desktop_shot: string | null;
  mobile_shot: string | null;
  receipts: IterationReceipt[];
}

export interface AgentSnapshot {
  id: string;
  task: string;
  status: AgentStatus;
  terminal_reason: TerminalReason | null;
  provider: string;
  model: string;
  plan: PlanItem[];
  step: number;
  max_steps: number;
  tool_calls: number;
  max_tool_calls: number;
  tokens_used: number;
  max_tokens: number;
  elapsed_ms: number;
  max_wall_ms: number;
  pending_approval: PreparedCall | null;
  events: AgentEvent[];
  preview: AgentPreview | null;
  completion_summary: string | null;
  error: string | null;
}

export type AgentPhase = "working" | "approval" | "plan" | "verifying" | "completed" | "stopped";

export function phaseOf(snap: AgentSnapshot | null): AgentPhase {
  if (!snap) return "working";
  if (snap.status === "awaiting_approval") return "approval";
  if (snap.status === "awaiting_plan") return "plan";
  if (snap.status === "verifying") return "verifying";
  if (snap.status === "completed") return "completed";
  if (
    snap.status === "blocked" ||
    snap.status === "denied" ||
    snap.status === "cancelled" ||
    snap.status === "failed"
  ) {
    return "stopped";
  }
  return "working";
}

export function terminalActive(snap: AgentSnapshot | null): boolean {
  return snap?.terminal_reason != null;
}

export function terminalLabel(reason: TerminalReason): string {
  switch (reason.kind) {
    case "completed":
      return "Completed";
    case "gates_failed":
      return "Blocked - completion gates failed";
    case "blocked":
      return `Blocked - ${reason.detail}`;
    case "budget_steps":
      return `Blocked - step budget (${reason.max_steps}) exhausted`;
    case "budget_time":
      return "Blocked - time budget exhausted";
    case "budget_tokens":
      return "Blocked - token budget exhausted";
    case "budget_tool_calls":
      return "Blocked - tool-call budget exhausted";
    case "repeated_failure":
      return `Stopped - ${reason.tool.replace(/_/g, " ")} kept failing`;
    case "no_progress":
      return "Stopped - no progress across turns";
    case "cancelled":
      return "Cancelled";
    case "denied":
      return "Stopped - write denied";
    case "approval_timeout":
      return "Blocked - approval wait timed out";
    case "provider_error":
      return `Failed - ${reason.detail}`;
    case "model_stalled":
      return "Stopped - model stopped calling tools";
  }
}

export function formatClock(ms: number): string {
  const total = Math.floor(ms / 1000);
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${m}:${String(s).padStart(2, "0")}`;
}

export function formatTokens(n: number): string {
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
  return String(n);
}
