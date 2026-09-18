import rexLogo from "../assets/rex-logo.svg";

export function TopBar({ view, onView, ultra, onUltra }: { view: "task" | "settings"; onView: (v: "task" | "settings") => void; ultra: boolean; onUltra: () => void }) {
  return (
    <header className="top-shell mx-auto flex w-full max-w-[820px] items-center justify-between px-5 py-5 sm:px-8 sm:py-7">
      <div className="brand-lockup flex min-w-0 items-center gap-3">
        <span className="brand-frame"><img className="rex-logo" src={rexLogo} alt="REX" /></span>
        <span className="brand-name truncate text-sm font-medium tracking-[-0.01em] text-text">Harness</span>
      </div>
      <div className="top-actions flex shrink-0 items-center gap-2 sm:gap-4">
        <nav className="view-tabs" aria-label="View">
          <button type="button" aria-current={view === "task" ? "page" : undefined} className={view === "task" ? "is-current" : ""} onClick={() => onView("task")}>Task</button>
          <button type="button" aria-current={view === "settings" ? "page" : undefined} className={view === "settings" ? "is-current" : ""} onClick={() => onView("settings")}>Settings</button>
        </nav>
        <button type="button" className={`ultra-control ${ultra ? "is-on" : ""}`} aria-pressed={ultra} aria-label={`${ultra ? "Turn off" : "Preview"} Ultra mode`} onClick={onUltra}>
          <span className="ultra-glyph" aria-hidden="true"><i /><i /><i /></span>
          <span className="ultra-label">Ultra</span>
          <span className="ultra-tier">Preview</span>
        </button>
        <span className="preview-state flex shrink-0 items-center gap-2 text-[10px] font-medium tracking-wide text-faint sm:text-[11px]" title="No backend is connected. All runs shown are sample data.">
          <span className="h-1.5 w-1.5 rounded-full bg-blocked" aria-hidden="true" />
          <span className="hidden lg:inline">PREVIEW · SAMPLE DATA</span><span className="lg:hidden">PREVIEW</span>
        </span>
      </div>
    </header>
  );
}
