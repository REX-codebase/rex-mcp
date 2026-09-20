// Operator mode: who is accountable for tasks started from this UI.
// The choice is recorded in custody as the task's operator identity
// (daemon side, tested): human mode records operator:human and the stop
// control is the terminal human-stop fence; agent mode means an external
// host agent (Claude Code, Antigravity, any MCP client) drives REX
// through rex-mcp and this UI supervises - approvals and Stop stay human.
// A mode change never mutates in-flight custody; it applies to the next
// task only. Tasks an agent opens through rex-mcp always record
// operator:agent regardless of this setting.

export type OperatorMode = "human" | "agent";

const KEY = "rex-operator-mode";

export function loadOperatorMode(
  get: (key: string) => string | null = (k) => window.localStorage.getItem(k)
): OperatorMode | null {
  const raw = get(KEY);
  return raw === "human" || raw === "agent" ? raw : null;
}

export function saveOperatorMode(
  mode: OperatorMode,
  set: (key: string, value: string) => void = (k, v) => window.localStorage.setItem(k, v)
): void {
  set(KEY, mode);
}

export function modeTitle(mode: OperatorMode): string {
  return mode === "human" ? "Human" : "Agent";
}

export function modeDetail(mode: OperatorMode): string {
  return mode === "human"
    ? "You operate REX directly. Custody records operator: human, and Stop is the final human-stop fence."
    : "An external host agent drives REX through REX MCP. You supervise here: approvals and Stop stay yours.";
}
