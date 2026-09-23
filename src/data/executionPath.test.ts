// Tests for execution path resolution: the custody loop is the default,
// every alternative is explicit, and the resolver never promises a path
// onRun would not take.

import { describe, expect, it } from "vitest";
import { resolveExecutionPath, type PathInputs } from "./executionPath";

const base: PathInputs = {
  operatorMode: "human",
  ultra: false,
  installedAgentId: null,
  installedAgentLabel: null,
  liveCapable: true,
  tourMode: false,
};

describe("resolveExecutionPath", () => {
  it("defaults to the custody loop", () => {
    const p = resolveExecutionPath(base);
    expect(p.kind).toBe("custody");
    expect(p.label).toMatch(/custody/i);
  });

  it("routes to ultra when the toggle is on", () => {
    const p = resolveExecutionPath({ ...base, ultra: true });
    expect(p.kind).toBe("ultra");
  });

  it("does not route to ultra in agent operator mode", () => {
    const p = resolveExecutionPath({ ...base, ultra: true, operatorMode: "agent" });
    expect(p.kind).toBe("delegate");
  });

  it("routes to the installed agent when one is selected", () => {
    const p = resolveExecutionPath({
      ...base,
      ultra: true,
      installedAgentId: "claude-code",
      installedAgentLabel: "Claude Code",
    });
    expect(p.kind).toBe("installed");
    expect(p.label).toContain("Claude Code");
  });

  it("routes to delegate in agent operator mode", () => {
    const p = resolveExecutionPath({ ...base, operatorMode: "human" });
    expect(p.kind).toBe("custody");
    const q = resolveExecutionPath({ ...base, operatorMode: "agent" });
    expect(q.kind).toBe("delegate");
  });

  it("uses tour mode only when no backend is live and tour is explicit", () => {
    const p = resolveExecutionPath({ ...base, liveCapable: false, tourMode: true });
    expect(p.kind).toBe("tour");
  });

  it("is unavailable — never a fake run — with no backend and no tour", () => {
    const p = resolveExecutionPath({ ...base, liveCapable: false, tourMode: false });
    expect(p.kind).toBe("unavailable");
  });

  it("installed agent wins over ultra and delegate", () => {
    const p = resolveExecutionPath({
      ...base,
      operatorMode: "agent",
      ultra: true,
      installedAgentId: "codex",
      installedAgentLabel: "Codex",
    });
    expect(p.kind).toBe("installed");
  });
});
