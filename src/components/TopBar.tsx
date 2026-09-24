import { useEffect, useRef } from "react";
import rexLogo from "../assets/rex-logo.svg";

export function TopBar({ view, onView, ultra, onUltra, fast, onFast }: { view: "task" | "settings" | "terminal" | "editor" | "git"; onView: (v: "task" | "settings" | "terminal" | "editor" | "git") => void; ultra: boolean; onUltra: () => void; fast: boolean; onFast: () => void }) {
  const tabsRef = useRef<HTMLElement>(null);
  // In a narrow window the tab strip scrolls; keep the current tab visible
  // when the view changes from a shortcut or a link, not just a click.
  // Only the strip scrolls sideways; the page itself never moves.
  useEffect(() => {
    const nav = tabsRef.current;
    const current = nav?.querySelector<HTMLElement>(".is-current");
    if (!nav || !current || nav.scrollWidth <= nav.clientWidth) return;
    nav.scrollLeft = tabScrollLeft(nav.scrollLeft, nav.clientWidth, current.offsetLeft, current.offsetWidth);
  }, [view]);
  return (
    <header className="top-shell mx-auto flex w-full max-w-[820px] items-center justify-between px-5 py-5 sm:px-8 sm:py-7">
      <div className="brand-lockup flex min-w-0 items-center gap-3">
        <span className="brand-frame"><img className="rex-logo" src={rexLogo} alt="REX" /></span>
        <span className="brand-name truncate text-sm font-medium tracking-[-0.01em] text-text">Harness</span>
      </div>
      <div className="top-actions flex shrink-0 items-center gap-2 sm:gap-4">
        <nav ref={tabsRef} className="view-tabs" aria-label="View">
          <button type="button" aria-current={view === "task" ? "page" : undefined} className={view === "task" ? "is-current" : ""} onClick={() => onView("task")}>Task</button>
          <button type="button" aria-current={view === "terminal" ? "page" : undefined} className={view === "terminal" ? "is-current" : ""} onClick={() => onView("terminal")}>Terminal</button>
          <button type="button" aria-current={view === "editor" ? "page" : undefined} className={view === "editor" ? "is-current" : ""} onClick={() => onView("editor")}>Editor</button>
          <button type="button" aria-current={view === "git" ? "page" : undefined} className={view === "git" ? "is-current" : ""} onClick={() => onView("git")}>Git</button>
          <button type="button" aria-current={view === "settings" ? "page" : undefined} className={view === "settings" ? "is-current" : ""} onClick={() => onView("settings")}>Settings</button>
        </nav>
        <button type="button" className={`fast-control ${fast ? "is-on" : ""}`} aria-pressed={fast} aria-label={`${fast ? "Turn off" : "Preview"} Fast mode`} onClick={onFast}>
          <span className="fast-glyph" aria-hidden="true"><i /><i /><i /><i /><i /></span>
          <span className="fast-copy"><b>Fast</b><small>Preview</small></span>
        </button>
        <button type="button" className={`ultra-control ${ultra ? "is-on" : ""}`} aria-pressed={ultra} aria-label={`${ultra ? "Turn off" : "Preview"} Ultra mode`} onClick={onUltra}>
          <span className="ultra-glyph" aria-hidden="true"><i /><i /><i /></span>
          <span className="ultra-label">Ultra</span>
          <span className="ultra-tier">{ultra ? "Verified runs" : "Preview"}</span>
        </button>
      </div>
    </header>
  );
}

// Smallest horizontal scroll that shows the whole tab, with a little room.
export function tabScrollLeft(scrollLeft: number, width: number, tabLeft: number, tabWidth: number, pad = 8): number {
  if (tabLeft - pad < scrollLeft) return Math.max(0, tabLeft - pad);
  if (tabLeft + tabWidth + pad > scrollLeft + width) return tabLeft + tabWidth + pad - width;
  return scrollLeft;
}

// Transport x provenance: `live` only means a backend transport (Tauri or
// the dev sidecar) answered the probe. It does NOT mean the current run is
// real - the sidecar can serve seeded demo tasks. Provenance is stated per
// run on its own receipt/header, never by this badge. It sits in the status
// row under the top bar so the bar itself never truncates the product name.
export function BackendBadge({ live }: { live?: boolean }) {
  return live ? (
    <span className="backend-badge" title="A backend transport is connected. Each run's receipt states the engine that produced it.">
      <span className="h-1.5 w-1.5 rounded-full bg-done" aria-hidden="true" />
      Backend connected
    </span>
  ) : (
    <span className="backend-badge" title="No backend is connected. All runs shown are sample data.">
      <span className="h-1.5 w-1.5 rounded-full bg-blocked" aria-hidden="true" />
      Preview · sample data
    </span>
  );
}
