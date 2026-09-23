// First-run setup checklist. Shown when no backend transport answered the
// probe, instead of silently animating a mock run. The mock tour stays
// reachable, but only behind an explicit opt-in: pressing Run with no backend
// and no tour mode just highlights this panel.

export function SetupChecklist({
  hot,
  tour,
  onTour,
  onOpenSettings,
  onRecheck,
}: {
  hot: boolean;
  tour: boolean;
  onTour: (on: boolean) => void;
  onOpenSettings: () => void;
  onRecheck: () => void;
}) {
  if (tour) {
    return (
      <section className="setup-panel" aria-label="Tour mode">
        <p className="eyebrow">Tour mode</p>
        <p className="mt-2 text-[13px] leading-relaxed text-muted">
          You are browsing a simulated interface tour. Runs are sample data —
          nothing reaches a model.{" "}
          <button type="button" className="setup-link" onClick={() => onTour(false)}>
            Turn tour mode off
          </button>
        </p>
      </section>
    );
  }
  return (
    <section className={`setup-panel ${hot ? "attn" : ""}`} aria-label="Connect a backend">
      <p className="eyebrow">No backend connected</p>
      <h2 className="mt-2 text-[17px] font-medium tracking-[-0.01em] text-text">
        Three steps to a real run
      </h2>
      <ol className="setup-steps">
        <li>
          <span className="step-n" aria-hidden="true">1</span>
          <span>
            Start the backend — the dev sidecar or the desktop shell:
            <br />
            <code>cargo run -p rex-mcp --bin rex-dev-server</code>
            <span className="text-faint"> or </span>
            <code>npm run tauri dev</code>
          </span>
        </li>
        <li>
          <span className="step-n" aria-hidden="true">2</span>
          <span>
            Connect a model key in Settings. Keys are stored by the Rust
            backend in a 0600 file — the UI never sees them.{" "}
            <button type="button" className="setup-link" onClick={onOpenSettings}>
              Open Settings
            </button>
          </span>
        </li>
        <li>
          <span className="step-n" aria-hidden="true">3</span>
          <span>
            Describe a task above and press Run. The agent loop plans, asks
            for approvals, and seals an evidence receipt.
          </span>
        </li>
      </ol>
      <div className="setup-actions">
        <button type="button" className="setup-btn primary" onClick={onRecheck}>
          I started it — check again
        </button>
      </div>
      <p className="setup-tour">
        Just looking around?{" "}
        <button type="button" className="setup-link" onClick={() => onTour(true)}>
          Take the simulated interface tour instead
        </button>
      </p>
    </section>
  );
}
