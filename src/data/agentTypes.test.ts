import { describe, expect, it } from "vitest";
import {
  formatClock,
  formatTokens,
  phaseOf,
  terminalActive,
  terminalLabel,
  type AgentSnapshot,
} from "./agentTypes";

function snap(partial: Partial<AgentSnapshot>): AgentSnapshot {
  return {
    id: "r1",
    task: "t",
    status: "running",
    terminal_reason: null,
    provider: "gemini",
    model: "m",
    plan: [],
    step: 0,
    max_steps: 24,
    tool_calls: 0,
    max_tool_calls: 80,
    tokens_used: 0,
    max_tokens: 250000,
    elapsed_ms: 0,
    max_wall_ms: 1200000,
    pending_approval: null,
    events: [],
    preview: null,
    completion_summary: null,
    error: null,
    ...partial,
  };
}

describe("agent snapshot view model", () => {
  it("maps statuses to phases truthfully", () => {
    expect(phaseOf(null)).toBe("working");
    expect(phaseOf(snap({ status: "planning" }))).toBe("working");
    expect(phaseOf(snap({ status: "running" }))).toBe("working");
    expect(phaseOf(snap({ status: "awaiting_approval" }))).toBe("approval");
    expect(phaseOf(snap({ status: "awaiting_answer" }))).toBe("question");
    expect(phaseOf(snap({ status: "verifying" }))).toBe("verifying");
    expect(phaseOf(snap({ status: "completed" }))).toBe("completed");
    expect(phaseOf(snap({ status: "blocked" }))).toBe("stopped");
    expect(phaseOf(snap({ status: "failed" }))).toBe("stopped");
    expect(phaseOf(snap({ status: "denied" }))).toBe("stopped");
    expect(phaseOf(snap({ status: "cancelled" }))).toBe("stopped");
  });

  it("detects terminal state only from a real terminal reason", () => {
    expect(terminalActive(snap({}))).toBe(false);
    expect(terminalActive(snap({ terminal_reason: { kind: "completed" } }))).toBe(true);
  });

  it("renders truthful terminal labels", () => {
    expect(terminalLabel({ kind: "completed" })).toBe("Completed");
    expect(terminalLabel({ kind: "budget_steps", max_steps: 24 })).toContain("24");
    expect(terminalLabel({ kind: "repeated_failure", tool: "read_file" })).toContain("read file");
    expect(terminalLabel({ kind: "provider_error", detail: "HTTP 401" })).toContain("401");
    expect(terminalLabel({ kind: "denied" })).toContain("denied");
  });

  it("formats clocks and token counts compactly", () => {
    expect(formatClock(0)).toBe("0:00");
    expect(formatClock(65_000)).toBe("1:05");
    expect(formatClock(20 * 60 * 1000)).toBe("20:00");
    expect(formatTokens(999)).toBe("999");
    expect(formatTokens(250_000)).toBe("250.0k");
  });
});
