import { invoke } from "@tauri-apps/api/core";

export type PreviewFramework = "static_html" | "vite" | "next_js" | "create_react_app" | "astro" | "svelte_kit";
export type PreviewSessionState = "created" | "starting" | "running" | "iterating" | "passed" | "failed" | "cancelled" | "timed_out";

export type PreviewRecipe = {
  framework: PreviewFramework;
  project_dir: string;
  program: string;
  args: string[];
  readiness_path: string;
};

export type PreviewLaunch = {
  program: string;
  args: string[];
  cwd: string;
  bind: string;
  startup_timeout_ms: number;
  idle_timeout_ms: number;
};

export type PreviewSessionSummary = {
  id: string;
  state: PreviewSessionState;
  url: string;
  framework: PreviewFramework;
  iteration: number;
  cancellation_requested: boolean;
};

export type PreviewBrowserAction =
  | { kind: "navigate"; path: string }
  | { kind: "pointer_move"; x: number; y: number }
  | { kind: "pointer_down"; button: "primary" | "auxiliary" | "secondary" }
  | { kind: "pointer_up"; button: "primary" | "auxiliary" | "secondary" }
  | { kind: "key"; key: string; state: "down" | "up" }
  | { kind: "text"; value: string }
  | { kind: "scroll"; delta_x: number; delta_y: number }
  | { kind: "set_viewport"; width: number; height: number; scale: number };

export const previewDetect = (projectDir: string) => invoke<PreviewRecipe>("preview_detect", { projectDir });
export const previewStart = (projectDir: string) => invoke<PreviewSessionSummary>("preview_start", { projectDir });
export const previewAction = (sessionId: string, action: PreviewBrowserAction) => invoke<void>("preview_action", { sessionId, action });
export const previewCapture = (sessionId: string) => invoke<void>("preview_capture", { sessionId });
export const previewCancel = (sessionId: string) => invoke<PreviewSessionSummary>("preview_cancel", { sessionId });
export const previewTeardown = (sessionId: string) => invoke<void>("preview_teardown", { sessionId });
