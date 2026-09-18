import { useEffect, useRef, useState } from "react";

// Truthful replacement for the old model dropdown: no runtime is
// connected, so there are no models to list. The control says so and
// points at Settings instead of inventing names.
export function ModelStatus({ onOpenSettings }: { onOpenSettings: () => void }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, [open]);
  return (
    <div ref={ref} className="relative">
      <button
        type="button"
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-label="Model: not connected"
        onClick={() => setOpen(!open)}
        onKeyDown={(e) => {
          if (e.key === "Escape") setOpen(false);
        }}
        className="model-trigger"
      >
        <span className="text-faint">Model</span>
        <span className="text-muted">Not connected</span>
        <span className="h-1.5 w-1.5 rounded-full bg-blocked" aria-hidden="true" />
      </button>
      {open && (
        <div role="dialog" aria-label="Model status" className="model-menu model-status-menu">
          <p className="model-status-title">No runtime connected</p>
          <p className="model-status-copy">
            Runs in this preview are simulated locally. When a runtime is connected, the models it
            offers will appear here.
          </p>
          <button
            type="button"
            className="model-status-link"
            onClick={() => {
              setOpen(false);
              onOpenSettings();
            }}
          >
            Open Settings
            <svg width="12" height="12" viewBox="0 0 16 16" aria-hidden="true">
              <path d="M3.5 8h9m-3.5-3.5L12.5 8 9 11.5" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" />
            </svg>
          </button>
        </div>
      )}
    </div>
  );
}
