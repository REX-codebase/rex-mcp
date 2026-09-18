import type { Run } from "../data/mock";

const DOT: Record<string, string> = {
  done: "bg-done",
  blocked: "bg-blocked",
  working: "bg-accent",
  verifying: "bg-accent",
  idle: "bg-border",
};

export function HistoryList({
  runs,
  onSelect,
  selectedId,
}: {
  runs: Run[];
  onSelect: (r: Run) => void;
  selectedId?: string;
}) {
  return (
    <section aria-label="Past runs" className="rounded-xl border border-border bg-surface">
      <h2 className="border-b border-border px-4 py-3 text-sm font-medium text-text sm:px-5">Past runs</h2>
      <ul>
        {runs.map((r) => (
          <li key={r.id}>
            <button
              type="button"
              onClick={() => onSelect(r)}
              aria-current={selectedId === r.id ? "true" : undefined}
              className={`flex min-h-11 w-full items-center gap-3 px-4 py-2.5 text-left hover:bg-raised sm:px-5 ${
                selectedId === r.id ? "bg-raised" : ""
              }`}
            >
              <span className={`h-1.5 w-1.5 shrink-0 rounded-full ${DOT[r.state]}`} aria-hidden="true" />
              <span className="min-w-0 flex-1 truncate text-sm text-text">{r.task}</span>
              <span className="shrink-0 font-mono text-xs text-faint">{r.receipt}</span>
              <span className="hidden w-24 shrink-0 text-right text-xs text-faint sm:inline">{r.startedAt}</span>
            </button>
          </li>
        ))}
      </ul>
    </section>
  );
}
