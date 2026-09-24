import { describe, expect, it } from "vitest";
import { tabScrollLeft } from "./TopBar";

describe("tab strip scroll", () => {
  it("leaves a visible tab alone", () => {
    expect(tabScrollLeft(0, 200, 40, 60)).toBe(0);
  });
  it("scrolls right just enough to show a tab past the edge", () => {
    expect(tabScrollLeft(0, 200, 180, 60)).toBe(48);
  });
  it("scrolls left to show a tab before the edge, never below zero", () => {
    expect(tabScrollLeft(120, 200, 60, 50)).toBe(52);
    expect(tabScrollLeft(120, 200, 4, 50)).toBe(0);
  });
});
