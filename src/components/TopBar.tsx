import rexLogo from "../assets/rex-logo.svg";

export function TopBar() {
  return (
    <header className="mx-auto flex w-full max-w-[820px] items-center justify-between px-5 py-5 sm:px-8 sm:py-7">
      <div className="flex min-w-0 items-center gap-3">
        <img className="rex-logo" src={rexLogo} alt="REX" />
        <span className="truncate text-sm font-medium tracking-[-0.01em] text-text">Harness</span>
      </div>
      <span className="flex shrink-0 items-center gap-2 text-[10px] font-medium tracking-wide text-faint sm:text-[11px]" title="No backend is connected. All runs shown are sample data.">
        <span className="h-1.5 w-1.5 rounded-full bg-blocked" aria-hidden="true" />
        <span className="hidden sm:inline">PREVIEW · SAMPLE DATA</span>
        <span className="sm:hidden">PREVIEW</span>
      </span>
    </header>
  );
}
