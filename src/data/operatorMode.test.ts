// @vitest-environment jsdom
import { describe, expect, it } from "vitest";
import { loadOperatorMode, saveOperatorMode, modeDetail } from "./operatorMode";

describe("operator mode store", () => {
  it("round-trips a saved mode", () => {
    const map = new Map<string, string>();
    saveOperatorMode("agent", (k, v) => map.set(k, v));
    expect(loadOperatorMode((k) => map.get(k) ?? null)).toBe("agent");
    saveOperatorMode("human", (k, v) => map.set(k, v));
    expect(loadOperatorMode((k) => map.get(k) ?? null)).toBe("human");
  });
  it("unknown or missing values mean no choice yet", () => {
    expect(loadOperatorMode(() => null)).toBeNull();
    expect(loadOperatorMode(() => "robot")).toBeNull();
  });
  it("mode copy stays honest about custody and stop", () => {
    expect(modeDetail("human")).toContain("human-stop");
    expect(modeDetail("agent")).toContain("REX MCP");
    expect(modeDetail("agent")).not.toContain("credential");
  });
});
