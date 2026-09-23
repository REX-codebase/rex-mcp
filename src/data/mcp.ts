// MCP server management bindings.

import { invoke } from "@tauri-apps/api/core";

export interface McpTool {
  name: string;
  description: string;
  enabled: boolean;
}

export interface McpServer {
  id: string;
  name: string;
  command: string;
  enabled: boolean;
  tools: McpTool[];
  last_probe_ok: boolean | null;
  last_probe_error: string | null;
  last_probe_at_ms: number | null;
}

export async function mcpListServers(): Promise<McpServer[]> {
  return invoke<McpServer[]>("mcp_list_servers");
}

export async function mcpAddServer(
  name: string,
  command: string,
): Promise<McpServer> {
  return invoke<McpServer>("mcp_add_server", { name, command });
}

export async function mcpRemoveServer(id: string): Promise<void> {
  return invoke<void>("mcp_remove_server", { id });
}

export async function mcpSetServerEnabled(
  id: string,
  enabled: boolean,
): Promise<void> {
  return invoke<void>("mcp_set_server_enabled", { id, enabled });
}

export async function mcpSetToolEnabled(
  serverId: string,
  toolName: string,
  enabled: boolean,
): Promise<void> {
  return invoke<void>("mcp_set_tool_enabled", {
    serverId,
    toolName,
    enabled,
  });
}

export async function mcpProbeServer(id: string): Promise<McpServer> {
  return invoke<McpServer>("mcp_probe_server", { id });
}
