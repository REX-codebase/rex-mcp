import { useSyncExternalStore } from "react";

// Motion preference contract.
// - "system" follows the OS prefers-reduced-motion setting, live.
// - "reduce" is a calm, still-usable interface regardless of the OS.
// - "full" is the complete approved motion design regardless of the OS.
// The in-app choice always wins over the OS; "system" is the only mode that
// delegates to it. Stored per device in localStorage under MOTION_KEY.
export type MotionPref = "system" | "reduce" | "full";

export const MOTION_KEY = "rex-harness-motion";
export const REDUCED_MOTION_QUERY = "(prefers-reduced-motion: reduce)";

export function parseMotionPref(value: string | null): MotionPref {
  return value === "reduce" || value === "full" ? value : "system";
}

export function loadMotionPref(get: (key: string) => string | null): MotionPref {
  try {
    return parseMotionPref(get(MOTION_KEY));
  } catch {
    return "system";
  }
}

export function resolveReduced(motion: MotionPref, systemReduced: boolean): boolean {
  if (motion === "reduce") return true;
  if (motion === "full") return false;
  return systemReduced;
}

type MediaQueryListLike = {
  matches: boolean;
  addEventListener?: (type: "change", listener: () => void) => void;
  removeEventListener?: (type: "change", listener: () => void) => void;
  addListener?: (listener: () => void) => void;
  removeListener?: (listener: () => void) => void;
};

function queryList(): MediaQueryListLike | null {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
    return null;
  }
  return window.matchMedia(REDUCED_MOTION_QUERY);
}

export function getSystemReducedMotion(): boolean {
  return queryList()?.matches ?? false;
}

export function subscribeSystemReducedMotion(onChange: () => void): () => void {
  const mql = queryList();
  if (!mql) return () => {};
  if (typeof mql.addEventListener === "function") {
    mql.addEventListener("change", onChange);
    return () => mql.removeEventListener?.("change", onChange);
  }
  mql.addListener?.(onChange);
  return () => mql.removeListener?.(onChange);
}

// Live OS reduced-motion state. Re-renders subscribers the moment the OS
// setting flips, so System mode tracks the OS without an app reload.
export function useSystemReducedMotion(): boolean {
  return useSyncExternalStore(
    subscribeSystemReducedMotion,
    getSystemReducedMotion,
    () => false
  );
}
