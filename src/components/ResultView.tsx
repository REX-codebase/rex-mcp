import { useState } from "react";
import type { Run } from "../data/mock";

const KIND_STYLES: Record<string, string> = {
  Command: "text-accent",
  File: "text-done",
  Check: "text-blocked",
  Review: "text-muted",
};

function fmtDuration(ms: number) {
  if (ms < 1000) return `${ms} ms`;
  return `${(ms / 1000).toFixed(1)} s`;
}

export function ResultView({ run }: { run: Run }) {
  const [open, setOpen] = useState(true);
  const finished = run.state === "done" || run.state === "blocked";

  return (
    <section aria-label="Result" className="rounded-xl border border-border bg-surface">
      <div className="border-b border-border px-4 py-3 sm:px-5">
        <div className="flex items-center justify-between gap-3">
          <h2 className="text-sm font-medium text-text">Result</h2>
          <span className="font-mono text-xs text-faint">Receipt {run.receipt}</span>
        </div>
      </div>
      <div className="px-4 py-4 sm:px-5">
        {finished ? (
          <p className="max-w-[65ch] text-[15px] leading-relaxed text-text">{run.summary}</p>
        ) : (
          <p className="text-[15px] text-muted">Working through the task. Evidence appears here as it lands.</p>
        )}

        <ol className="mt-4 space-y-1.5" aria-label="Evidence">
          {run.steps.map((s) => (
            <li key={s.id} className="flex items-center gap-3 rounded-lg px-2 py-2">
              <span
                className={`h-1.5 w-1.5 shrink-0 rounded-full ${s.ok ? "bg-done" : "bg-danger"}`}
                aria-hidden="true"
              />
              <div className="min-w-0 flex-1">
                <p className="truncate text-sm text-text">{s.name}</p>
                <p className="truncate text-xs text-muted">{s.detail}</p>
              </div>
              <span className={`shrink-0 font-mono text-[11px] uppercase tracking-wide ${KIND_STYLES[s.kind]}`}>
                {s.kind}
              </span>
              <span className="w-14 shrink-0 text-right font-mono text-xs text-faint">
                {fmtDuration(s.durationMs)}
              </span>
            </li>
          ))}
          {run.steps.length === 0 && (
            <li className="px-2 py-2 text-sm text-muted">No evidence yet.</li>
          )}
        </ol>

        {finished && run.steps.length > 0 && (
          <div className="mt-4 border-t border-border pt-3">
            <button
              type="button"
              aria-expanded={open}
              onClick={() => setOpen(!open)}
              className="flex min-h-11 w-full items-center justify-between rounded-lg px-2 text-sm text-muted hover:text-text"
            >
              <span>
                Verification <span className="text-faint">· {run.steps.length} checks recorded</span>
              </span>
              <svg width="10" height="6" viewBox="0 0 10 6" className={`motion-safe-fade ${open ? "rotate-180" : ""}`} aria-hidden="true">
                <path d="M1 1l4 4 4-4" stroke="currentColor" strokeWidth="1.5" fill="none" strokeLinecap="round" />
              </svg>
            </button>
            {open && (
              <dl className="grid grid-cols-2 gap-x-6 gap-y-2 px-2 pb-1 pt-1 text-sm sm:grid-cols-4">
                <div>
                  <dt className="text-xs text-faint">Checks passed</dt>
                  <dd className="font-mono text-text">{run.steps.filter((s) => s.ok).length}/{run.steps.length}</dd>
                </div>
                <div>
                  <dt className="text-xs text-faint">Run time</dt>
                  <dd className="font-mono text-text">{fmtDuration(run.durationMs)}</dd>
                </div>
                <div>
                  <dt className="text-xs text-faint">Model</dt>
                  <dd className="truncate text-text">{run.model}</dd>
                </div>
                <div>
                  <dt className="text-xs text-faint">Started</dt>
                  <dd className="text-text">{run.startedAt}</dd>
                </div>
              </dl>
            )}
          </div>
        )}

        <p className="mt-4 flex items-center gap-2 rounded-lg bg-canvas px-3 py-2 text-xs text-faint">
          <svg width="12" height="12" viewBox="0 0 12 12" className="shrink-0 text-blocked" aria-hidden="true">
            <path d="M6 1l5 9H1l5-9z" stroke="currentColor" strokeWidth="1.2" fill="none" strokeLinejoin="round" />
            <path d="M6 4.5v2.5" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round" />
            <circle cx="6" cy="8.7" r="0.7" fill="currentColor" />
          </svg>
          Sample data. No backend is connected in this build, so this run is an interface preview, not real agent output.
        </p>
      </div>
    </section>
  );
}
