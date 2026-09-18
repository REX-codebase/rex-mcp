import type { RunState } from "../data/mock";

const STAGES: { key: RunState; label: string }[] = [
  { key: "working", label: "Working" },
  { key: "verifying", label: "Verifying" },
  { key: "done", label: "Done" },
];

function stageStatus(stage: RunState, state: RunState): "pending" | "active" | "complete" {
  if (state === "idle" || state === "blocked") return "pending";
  if (state === "done") return "complete";
  const order: RunState[] = ["working", "verifying", "done"];
  const si = order.indexOf(stage);
  const ci = order.indexOf(state);
  if (si < ci) return "complete";
  if (si === ci) return "active";
  return "pending";
}

export function StateRail({ state, blockedReason }: { state: RunState; blockedReason?: string }) {
  if (state === "idle") return null;
  return (
    <div aria-live="polite" className="rounded-xl border border-border bg-surface px-4 py-3 sm:px-5">
      {state === "blocked" ? (
        <div className="flex items-start gap-3">
          <span className="mt-1 h-2 w-2 shrink-0 rounded-full bg-blocked" aria-hidden="true" />
          <div>
            <p className="text-sm font-medium text-text">
              Blocked <span className="sr-only">- run stopped</span>
            </p>
            <p className="mt-0.5 text-sm text-muted">{blockedReason ?? "The agent stopped and explained why."}</p>
          </div>
        </div>
      ) : (
        <ol className="flex items-center gap-2 sm:gap-3">
          {STAGES.map((s, i) => {
            const st = stageStatus(s.key, state);
            return (
              <li key={s.key} className="flex min-w-0 items-center gap-2 sm:gap-3">
                {i > 0 && <span className="h-px w-6 shrink-0 bg-border sm:w-10" aria-hidden="true" />}
                <span
                  className={`h-2 w-2 shrink-0 rounded-full ${
                    st === "complete" ? "bg-done" : st === "active" ? "working-dot bg-accent" : "bg-border"
                  }`}
                  aria-hidden="true"
                />
                <span
                  className={`truncate text-sm ${
                    st === "active" ? "font-medium text-text" : st === "complete" ? "text-muted" : "text-faint"
                  }`}
                  aria-current={st === "active" ? "step" : undefined}
                >
                  {s.label}
                </span>
              </li>
            );
          })}
        </ol>
      )}
    </div>
  );
}
