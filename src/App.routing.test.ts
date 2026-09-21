import { describe, expect, it } from "vitest";
import { shouldUseDirectUltra } from "./App";

describe("operator mode run routing", () => {
  it("keeps Human + Ultra on the direct Ultra path", () => {
    expect(shouldUseDirectUltra("human", true, true)).toBe(true);
  });

  it("keeps Agent + Standard on the durable REX task path", () => {
    expect(shouldUseDirectUltra("agent", true, false)).toBe(false);
  });

  it("keeps Agent + Ultra off direct ultraBegin", () => {
    expect(shouldUseDirectUltra("agent", true, true)).toBe(false);
  });
});