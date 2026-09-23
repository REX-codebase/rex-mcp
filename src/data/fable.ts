// Fable gate bindings: native THINK → PROVE → ATTACK → WRITE sessions.
//
// The authority timer is enforced in Rust (rex-fable). These bindings only
// read status and request transitions; the countdown display can never open
// the gate early.

import { invoke } from "@tauri-apps/api/core";

export interface FableStatus {
  name: string;
  objective: string;
  phase: string;
  unlocked: boolean;
  timer_remaining_ms: number;
  timer_remaining_human: string;
  timer_elapsed: boolean;
  proven_count: number;
  invariant_count: number;
  unlock_ready: boolean;
}

export async function fableCreateSession(
  name: string,
  objective: string,
  timeBudgetMinutes?: number,
): Promise<FableStatus> {
  return invoke<FableStatus>("fable_create_session", {
    name,
    objective,
    timeBudgetMinutes: timeBudgetMinutes ?? null,
  });
}

export async function fableSessionStatus(name: string): Promise<FableStatus> {
  return invoke<FableStatus>("fable_session_status", { name });
}

export async function fableUnlockSession(
  name: string,
  rationale: string,
): Promise<FableStatus> {
  return invoke<FableStatus>("fable_unlock_session", { name, rationale });
}

export interface FableMcpLink {
  available: boolean;
  server_command: string;
  tools: string[];
  has_fable_session_tool: boolean;
  error: string | null;
}

export async function fableMcpProbe(serverCommand: string): Promise<FableMcpLink> {
  return invoke<FableMcpLink>("fable_mcp_probe", { serverCommand });
}

const MCP_COMMAND_KEY = "rex-fable-mcp-command";

export function loadFableMcpCommand(): string {
  try {
    return window.localStorage.getItem(MCP_COMMAND_KEY) || "";
  } catch {
    return "";
  }
}

export function saveFableMcpCommand(cmd: string): void {
  try {
    if (cmd) window.localStorage.setItem(MCP_COMMAND_KEY, cmd);
    else window.localStorage.removeItem(MCP_COMMAND_KEY);
  } catch {
    /* storage unavailable */
  }
}

const SESSION_KEY = "rex-fable-session";

export function loadFableSessionName(): string | null {
  try {
    return window.localStorage.getItem(SESSION_KEY);
  } catch {
    return null;
  }
}

export function saveFableSessionName(name: string | null): void {
  try {
    if (name) window.localStorage.setItem(SESSION_KEY, name);
    else window.localStorage.removeItem(SESSION_KEY);
  } catch {
    /* storage unavailable */
  }
}
