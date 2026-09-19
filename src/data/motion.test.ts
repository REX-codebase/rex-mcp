// @vitest-environment jsdom
import { describe, expect, it, vi } from "vitest";
import {
  MOTION_KEY,
  loadMotionPref,
  parseMotionPref,
  resolveReduced,
  subscribeSystemReducedMotion,
} from "./motion";

describe("parseMotionPref", () => {
  it("accepts the stored reduce and full values", () => {
    expect(parseMotionPref("reduce")).toBe("reduce");
    expect(parseMotionPref("full")).toBe("full");
  });
  it("falls back to system for missing or unknown values", () => {
    expect(parseMotionPref(null)).toBe("system");
    expect(parseMotionPref("system")).toBe("system");
    expect(parseMotionPref("garbage")).toBe("system");
    expect(parseMotionPref("")).toBe("system");
  });
});

describe("loadMotionPref", () => {
  it("reads through the provided getter", () => {
    expect(loadMotionPref((key) => (key === MOTION_KEY ? "full" : null))).toBe("full");
    expect(loadMotionPref(() => "reduce")).toBe("reduce");
    expect(loadMotionPref(() => null)).toBe("system");
  });
  it("falls back to system when storage throws", () => {
    expect(
      loadMotionPref(() => {
        throw new Error("denied");
      })
    ).toBe("system");
  });
});

describe("resolveReduced", () => {
  it("system delegates to the OS flag", () => {
    expect(resolveReduced("system", true)).toBe(true);
    expect(resolveReduced("system", false)).toBe(false);
  });
  it("reduce always reduces, full never reduces", () => {
    expect(resolveReduced("reduce", true)).toBe(true);
    expect(resolveReduced("reduce", false)).toBe(true);
    expect(resolveReduced("full", true)).toBe(false);
    expect(resolveReduced("full", false)).toBe(false);
  });
});

describe("subscribeSystemReducedMotion", () => {
  it("returns a no-op unsubscribe when matchMedia is unavailable", () => {
    const original = window.matchMedia;
    // @ts-expect-error intentional removal for the fallback path
    window.matchMedia = undefined;
    const off = subscribeSystemReducedMotion(() => {});
    expect(typeof off).toBe("function");
    expect(() => off()).not.toThrow();
    window.matchMedia = original;
  });

  it("registers a change listener and unsubscribes cleanly", () => {
    const listeners = new Set<() => void>();
    const mql = {
      matches: true,
      addEventListener: (_: "change", l: () => void) => listeners.add(l),
      removeEventListener: (_: "change", l: () => void) => listeners.delete(l),
    };
    const spy = vi.spyOn(window, "matchMedia").mockReturnValue(mql as unknown as MediaQueryList);
    const onChange = vi.fn();
    const off = subscribeSystemReducedMotion(onChange);
    expect(listeners.size).toBe(1);
    listeners.forEach((l) => l());
    expect(onChange).toHaveBeenCalledTimes(1);
    off();
    expect(listeners.size).toBe(0);
    spy.mockRestore();
  });

  it("supports the legacy addListener interface", () => {
    const listeners = new Set<() => void>();
    const mql = {
      matches: false,
      addListener: (l: () => void) => listeners.add(l),
      removeListener: (l: () => void) => listeners.delete(l),
    };
    const spy = vi.spyOn(window, "matchMedia").mockReturnValue(mql as unknown as MediaQueryList);
    const off = subscribeSystemReducedMotion(() => {});
    expect(listeners.size).toBe(1);
    off();
    expect(listeners.size).toBe(0);
    spy.mockRestore();
  });
});
