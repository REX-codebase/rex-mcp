export function TopBar() {
  return (
    <header className="mx-auto flex w-full max-w-[820px] items-center justify-between px-5 py-5 sm:px-8 sm:py-7">
      <div className="flex items-center gap-3">
        <span className="grid h-7 w-7 place-items-center rounded-lg bg-accent text-[13px] font-semibold text-ink" aria-hidden="true">R</span>
        <span className="text-sm font-semibold tracking-[-0.01em] text-text">REX Harness</span>
      </div>
      <span className="flex items-center gap-2 text-[11px] font-medium tracking-wide text-faint" title="No backend is connected. All runs shown are sample data.">
        <span className="h-1.5 w-1.5 rounded-full bg-blocked" aria-hidden="true" />
        PREVIEW · SAMPLE DATA
      </span>
    </header>
  );
}
