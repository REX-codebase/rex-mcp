// Interactive terminal bindings: PTY sessions in the backend.
//
// The backend spawns the shell in a PTY bound to the workspace (validated in
// Rust). Output arrives as Tauri events named `terminal-output-{id}`; the
// component subscribes and writes to xterm.js.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export async function terminalSpawn(
  workspace: string,
  cols?: number,
  rows?: number,
): Promise<string> {
  return invoke<string>("terminal_spawn", { workspace, cols, rows });
}

export async function terminalWrite(id: string, data: string): Promise<void> {
  return invoke<void>("terminal_write", { id, data });
}

export async function terminalResize(
  id: string,
  cols: number,
  rows: number,
): Promise<void> {
  return invoke<void>("terminal_resize", { id, cols, rows });
}

export async function terminalKill(id: string): Promise<void> {
  return invoke<void>("terminal_kill", { id });
}

export function onTerminalOutput(
  id: string,
  handler: (data: string) => void,
): Promise<UnlistenFn> {
  return listen<string>(`terminal-output-${id}`, (event) => handler(event.payload));
}

export function onTerminalExit(
  id: string,
  handler: () => void,
): Promise<UnlistenFn> {
  return listen(`terminal-exit-${id}`, () => handler());
}

export async function terminalDefaultWorkspace(): Promise<string> {
  return invoke<string>("terminal_default_workspace");
}
