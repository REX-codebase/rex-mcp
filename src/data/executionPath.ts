// Execution path resolution: one default path, alternatives explicit.
//
// The custody autonomous loop is the primary execution path — the only one
// where REX itself runs the agent end-to-end (describe → plan → approve →
// execute with tools → verify → receipt). Every other path is an explicit
// opt-in. This module is the single source of truth: the Composer label and
// onRun both resolve through it, so the UI can never promise one path and
// run another.
//
// Priority (highest first): installed agent > ultra > delegate > custody.
// Tour and unavailable only apply when no backend is live.

import type { OperatorMode } from "./operatorMode";

export type ExecutionPathKind =
  | "custody"
  | "ultra"
  | "installed"
  | "delegate"
  | "tour"
  | "unavailable";

export interface ExecutionPath {
  kind: ExecutionPathKind;
  /** Short label shown next to the Run button. */
  label: string;
  /** One honest line describing what pressing Run will do. */
  detail: string;
}

export interface PathInputs {
  operatorMode: OperatorMode | null;
  ultra: boolean;
  installedAgentId: string | null;
  installedAgentLabel: string | null;
  liveCapable: boolean;
  tourMode: boolean;
}

export function resolveExecutionPath(inputs: PathInputs): ExecutionPath {
  const {
    operatorMode,
    ultra,
    installedAgentId,
    installedAgentLabel,
    liveCapable,
    tourMode,
  } = inputs;
  if (!liveCapable && !tourMode) {
    return {
      kind: "unavailable",
      label: "Setup needed",
      detail: "Connect a backend in Settings — REX will not fake a run.",
    };
  }
  if (!liveCapable && tourMode) {
    return {
      kind: "tour",
      label: "Tour mode",
      detail: "Animated preview. Nothing really runs.",
    };
  }
  if (installedAgentId) {
    return {
      kind: "installed",
      label: `Installed agent · ${installedAgentLabel ?? installedAgentId}`,
      detail: "The vendor CLI runs the task; REX supervises and gates promotion.",
    };
  }
  if (operatorMode === "agent") {
    return {
      kind: "delegate",
      label: "Delegate to external host",
      detail: "Opens a durable task your agent drives via rex-mcp. Stop stays human.",
    };
  }
  if (ultra && operatorMode === "human") {
    return {
      kind: "ultra",
      label: "Ultra · verified pipeline",
      detail: "Contract → build → adversary → judge → promotion. Slower, verified.",
    };
  }
  return {
    kind: "custody",
    label: "Custody run",
    detail: "REX plans, acts through your approvals, verifies, and leaves a receipt.",
  };
}
