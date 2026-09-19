// Bridge to the real Rust backend. Two transports:
// - "tauri": inside the desktop app, through Tauri commands.
// - "sidecar": during vite dev, through the rex-dev-server localhost binary.
// When neither is reachable the app stays a labeled frontend preview.
// No credential is ever stored in frontend state beyond the entry field
// that sends it; the backend returns has_key booleans, catalogs, and
// truthful errors only.

export type BackendKind = "tauri" | "sidecar";

export type ProviderErrorShape =
  | { kind: "not_configured" }
  | { kind: "auth_failed" }
  | { kind: "rate_limited" }
  | { kind: "network"; detail: string }
  | { kind: "unsupported"; detail: string }
  | { kind: "empty_catalog" }
  | { kind: "invalid_response"; detail: string }
  | { kind: "store"; detail: string };

export interface ModelInfo {
  id: string;
  label: string;
  provider: string;
}

export interface ModelCatalog {
  provider: string;
  fetched_at: number;
  source: "live" | "replay";
  models: ModelInfo[];
}

// Access policy verdict mirrored from the Rust backend (policy.rs).
export interface AccessPolicy {
  kind: "api-key" | "subscription-key" | "subscription-oauth";
  status: "supported" | "tool-scoped" | "not-permitted" | "not-offered";
  label: string;
  detail: string;
  sources: string[];
  verified_on: string;
}

export interface ProviderSummary {
  id: string;
  name: string;
  protocol: "gemini" | "anthropic" | "openai-compatible";
  discovery: "native" | "openai-models" | "manual";
  access?: AccessPolicy[];
  base_url: string | null;
  has_key: boolean;
  catalog: ModelCatalog | null;
  last_error?: ProviderErrorShape;
}

export type RefreshResult = { ok: true; catalog: ModelCatalog } | { ok: false; error: ProviderErrorShape };
export type WriteResult = { ok: true } | { ok: false; error: ProviderErrorShape };

export function describeError(error: ProviderErrorShape): string {
  switch (error.kind) {
    case "not_configured":
      return "No API key stored yet";
    case "auth_failed":
      return "Provider rejected the API key";
    case "rate_limited":
      return "Rate limited - try again shortly";
    case "network":
      return "Network error - check the connection";
    case "unsupported":
      return "Model listing unsupported here";
    case "empty_catalog":
      return "Provider returned no usable models";
    case "invalid_response":
      return "Unexpected provider response";
    case "store":
      return "Credential store error";
  }
}

const SIDECAR = "http://127.0.0.1:8787";

type InvokeFn = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;

let cached: { kind: BackendKind; invoke: InvokeFn | null } | null | undefined;

async function probe(): Promise<{ kind: BackendKind; invoke: InvokeFn | null } | null> {
  const w = window as unknown as { __TAURI_INTERNALS__?: unknown };
  if (w.__TAURI_INTERNALS__) {
    try {
      const mod = await import("@tauri-apps/api/core");
      return { kind: "tauri", invoke: mod.invoke as InvokeFn };
    } catch {
      return null;
    }
  }
  if (import.meta.env.DEV) {
    try {
      const controller = new AbortController();
      const timer = window.setTimeout(() => controller.abort(), 900);
      const response = await fetch(`${SIDECAR}/api/status`, { signal: controller.signal });
      window.clearTimeout(timer);
      if (response.ok) {
        const info = await response.json();
        if (info?.kind === "sidecar") return { kind: "sidecar", invoke: null };
      }
    } catch {
      /* no sidecar running */
    }
  }
  return null;
}

export async function backendKind(): Promise<BackendKind | null> {
  if (cached === undefined) cached = await probe();
  return cached ? cached.kind : null;
}

async function backend() {
  if (cached === undefined) cached = await probe();
  if (!cached) throw new Error("no backend");
  return cached;
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(`${SIDEBAR_SAFE()}${path}`, {
    ...init,
    headers: { "content-type": "application/json", ...(init?.headers ?? {}) },
  });
  if (!response.ok) throw new Error(`sidecar HTTP ${response.status}`);
  return (await response.json()) as T;
}
function SIDEBAR_SAFE() {
  return SIDECAR;
}

export async function listSummaries(): Promise<ProviderSummary[]> {
  const b = await backend();
  if (b.kind === "tauri") return b.invoke!<ProviderSummary[]>("provider_summaries");
  return request<ProviderSummary[]>("/api/providers");
}

export async function setProviderKey(provider: string, key: string, baseUrl?: string): Promise<WriteResult> {
  const b = await backend();
  if (b.kind === "tauri") {
    try {
      await b.invoke!("provider_set_key", { provider, key, baseUrl: baseUrl ?? null });
      return { ok: true };
    } catch (error) {
      return { ok: false, error: error as ProviderErrorShape };
    }
  }
  return request<WriteResult>(`/api/providers/${provider}/key`, {
    method: "POST",
    body: JSON.stringify({ key, base_url: baseUrl ?? null }),
  });
}

export async function clearProviderKey(provider: string): Promise<WriteResult> {
  const b = await backend();
  if (b.kind === "tauri") {
    try {
      await b.invoke!("provider_clear_key", { provider });
      return { ok: true };
    } catch (error) {
      return { ok: false, error: error as ProviderErrorShape };
    }
  }
  return request<WriteResult>(`/api/providers/${provider}/key`, { method: "DELETE" });
}

export async function refreshCatalog(provider: string): Promise<RefreshResult> {
  const b = await backend();
  if (b.kind === "tauri") {
    try {
      const catalog = await b.invoke!<ModelCatalog>("provider_refresh", { provider });
      return { ok: true, catalog };
    } catch (error) {
      return { ok: false, error: error as ProviderErrorShape };
    }
  }
  return request<RefreshResult>(`/api/providers/${provider}/refresh`, { method: "POST", body: "{}" });
}

export type SearchProviderId = "rex" | "exa" | "tinyfish";
export interface SearchProviderSummary { id: SearchProviderId; name: string; active: boolean; built_in: boolean; has_key: boolean; endpoint: string; docs_url: string; }
export async function listSearchProviders(): Promise<SearchProviderSummary[]> {
  const b=await backend();
  if (b.kind === "tauri") return b.invoke!<SearchProviderSummary[]>("search_provider_summaries");
  throw new Error("search provider routing requires the desktop runtime");
}
export async function setSearchProviderKey(provider: SearchProviderId, key: string): Promise<WriteResult> {
  const b=await backend(); if(b.kind!=="tauri") throw new Error("desktop runtime required");
  try { await b.invoke!("search_provider_set_key",{provider,key}); return {ok:true}; } catch(error){ return {ok:false,error:error as ProviderErrorShape}; }
}
export async function clearSearchProviderKey(provider: SearchProviderId): Promise<WriteResult> {
  const b=await backend(); if(b.kind!=="tauri") throw new Error("desktop runtime required");
  try { await b.invoke!("search_provider_clear_key",{provider}); return {ok:true}; } catch(error){ return {ok:false,error:error as ProviderErrorShape}; }
}
export async function selectSearchProvider(provider: SearchProviderId): Promise<WriteResult> {
  const b=await backend(); if(b.kind!=="tauri") throw new Error("desktop runtime required");
  try { await b.invoke!("search_provider_select",{provider}); return {ok:true}; } catch(error){ return {ok:false,error:error as ProviderErrorShape}; }
}


export type RiskClass = "read" | "write" | "execute" | "denied";
export interface PreparedToolCall { call_id:string; tool:string; summary:string; risk:RiskClass; approval_required:boolean; policy_reason:string; }
export interface ToolReceipt { started_at_ms:number; duration_ms:number; target:string|null; command:string[]|null; exit_code:number|null; bytes_read:number; bytes_written:number; output_truncated:boolean; diff:string|null; redactions:number; }
export interface ToolResult { call_id:string; ok:boolean; tool:string; state:"pending_approval"|"ready"|"denied"|"cancelled"|"executed"; output:string|null; error:{kind:string;detail:string}|null; receipt:ToolReceipt; }
export async function resolveToolApproval(callId:string,approved:boolean):Promise<ToolResult>{const b=await backend();if(b.kind!=="tauri")throw new Error("desktop runtime required");await b.invoke!("tool_resolve_approval",{callId,approved});return b.invoke!<ToolResult>("tool_execute",{callId});}
export async function cancelToolCall(callId:string):Promise<void>{const b=await backend();if(b.kind!=="tauri")return;await b.invoke!("tool_cancel",{callId});}
