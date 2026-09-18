import type { Session } from "../data/mock";

export function HistoryList({ sessions, onSelect, selectedId }: { sessions: Session[]; onSelect: (s: Session) => void; selectedId?: string }) {
  return (
    <section aria-label="Past runs" className="history-section">
      <div className="flex items-center justify-between">
        <h2 className="eyebrow">Recent</h2>
        <span className="text-xs text-faint">Sample runs</span>
      </div>
      <ul className="mt-3">
        {sessions.map((s) => {
          const latest = s.turns[s.turns.length - 1];
          return (
            <li key={s.id} className="border-t border-line first:border-t-0">
              <button type="button" onClick={() => onSelect(s)} aria-current={selectedId === s.id ? "true" : undefined} className="history-row">
                <span className={`h-1.5 w-1.5 shrink-0 rounded-full ${latest.state === "done" ? "bg-done" : latest.state === "blocked" ? "bg-blocked" : "bg-accent"}`} />
                <span className="min-w-0 flex-1 truncate text-sm text-text">{s.title}</span>
                <span className="hidden text-xs text-faint sm:block">{latest.startedAt}</span>
              </button>
            </li>
          );
        })}
      </ul>
    </section>
  );
}
