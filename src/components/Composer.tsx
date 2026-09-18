import { useRef } from "react";
import { ModelSelect } from "./ModelSelect";
import { EXAMPLE_TASKS, type ModelId, type RunState } from "../data/mock";

export function Composer({
  task,
  setTask,
  model,
  setModel,
  state,
  onRun,
}: {
  task: string;
  setTask: (t: string) => void;
  model: ModelId;
  setModel: (m: ModelId) => void;
  state: RunState;
  onRun: () => void;
}) {
  const busy = state === "working" || state === "verifying";
  const canRun = task.trim().length > 0 && !busy;
  const areaRef = useRef<HTMLTextAreaElement>(null);

  return (
    <section aria-label="New task" className="rounded-xl border border-border bg-surface p-4 sm:p-5">
      <label htmlFor="task-input" className="mb-2 block text-sm font-medium text-text">
        What should the agent do?
      </label>
      <textarea
        id="task-input"
        ref={areaRef}
        rows={3}
        value={task}
        disabled={busy}
        onChange={(e) => setTask(e.target.value)}
        onKeyDown={(e) => {
          if ((e.metaKey || e.ctrlKey) && e.key === "Enter" && canRun) onRun();
        }}
        placeholder="Describe one task in plain language."
        className="w-full resize-y rounded-lg border border-border bg-canvas p-3 text-[15px] leading-relaxed text-text placeholder:text-faint disabled:opacity-60"
      />
      <div className="mt-3 flex flex-wrap items-center justify-between gap-3">
        <ModelSelect value={model} onChange={setModel} />
        <div className="flex items-center gap-3">
          <span className="hidden text-xs text-faint sm:inline">Ctrl+Enter to run</span>
          <button
            type="button"
            onClick={onRun}
            disabled={!canRun}
            title={!task.trim() ? "Describe a task first" : busy ? "A run is in progress" : "Start the run"}
            className="h-11 min-w-24 rounded-lg bg-accent px-5 text-sm font-medium text-canvas transition-colors hover:bg-[#9c90f2] disabled:cursor-not-allowed disabled:bg-border disabled:text-faint"
          >
            {busy ? "Running…" : "Run task"}
          </button>
        </div>
      </div>
      {state === "idle" && task.trim() === "" && (
        <div className="mt-4 flex flex-wrap gap-2" aria-label="Example tasks">
          {EXAMPLE_TASKS.map((t) => (
            <button
              key={t}
              type="button"
              onClick={() => {
                setTask(t);
                areaRef.current?.focus();
              }}
              className="min-h-11 rounded-full border border-border px-3 py-1.5 text-left text-xs text-muted hover:border-faint hover:text-text"
            >
              {t}
            </button>
          ))}
        </div>
      )}
    </section>
  );
}
