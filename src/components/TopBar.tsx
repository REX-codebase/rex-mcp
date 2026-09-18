export function TopBar() {
  return (
    <header className="flex items-center justify-between border-b border-border px-4 py-3 sm:px-6">
      <div className="flex items-center gap-2.5">
        <span className="flex h-6 w-6 items-center justify-center rounded-md bg-accent" aria-hidden="true">
          <svg width="12" height="12" viewBox="0 0 12 12" aria-hidden="true">
            <path d="M2 10V2h4.5a2.5 2.5 0 0 1 0 5H4.5M4.5 7 7 10" stroke="#0b0a10" strokeWidth="1.5" fill="none" strokeLinecap="round" strokeLinejoin="round" />
          </svg>
        </span>
        <span className="text-sm font-semibold tracking-tight text-text">REX Harness</span>
      </div>
      <span
        className="rounded-full border border-border px-2.5 py-1 text-[11px] text-faint"
        title="This build renders the interface with sample data. No backend is connected."
      >
        Preview · sample data
      </span>
    </header>
  );
}
