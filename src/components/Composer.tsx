import { useRef } from "react";
import { ModelStatus } from "./ModelStatus";
import { EXAMPLE_TASKS, type RunState } from "../data/mock";

export function Composer({ task, setTask, state, onRun, onOpenSettings }: { task: string; setTask: (t: string) => void; state: RunState; onRun: () => void; onOpenSettings: () => void }) {
  const busy = state === "working" || state === "verifying";
  const canRun = task.trim().length > 0 && !busy;
  const areaRef = useRef<HTMLTextAreaElement>(null);
  return (
    <section aria-label="New task">
      <div className="mb-5 sm:mb-6">
        <p className="eyebrow">New task</p>
        <h1 className="mt-2 text-[30px] font-medium leading-[1.12] tracking-[-0.04em] text-text sm:text-[38px]">Tell REX what to do.</h1>
        <p className="mt-3 max-w-[54ch] text-sm leading-relaxed text-muted">One clear request. REX will work through it, verify the result, and leave a receipt.</p>
      </div>

      <div className="task-field">
        <textarea
          id="task-input"
          ref={areaRef}
          rows={4}
          value={task}
          disabled={busy}
          onChange={(e) => setTask(e.target.value)}
          onKeyDown={(e) => {
            if ((e.metaKey || e.ctrlKey) && e.key === "Enter" && canRun) onRun();
          }}
          aria-label="Task description"
          placeholder="What needs to get done?"
          className="task-input"
        />
        <div className="task-controls">
          <div className="flex min-w-0 items-center gap-3 sm:gap-5">
            <ModelStatus onOpenSettings={onOpenSettings} />
            <span className="hidden items-center gap-2 text-[11px] text-faint sm:flex" title="Monid search is planned for a future backend integration; it is not connected in this preview.">
              <span className="h-1.5 w-1.5 rounded-full bg-line" aria-hidden="true" />
              Monid · search planned
            </span>
          </div>
          <div className="flex items-center gap-3">
            <span className="hidden text-[11px] text-faint md:inline">Ctrl + Enter</span>
            <button type="button" onClick={onRun} disabled={!canRun} title={!task.trim() ? "Describe a task first" : busy ? "A run is in progress" : "Start the run"} className="run-button" aria-label={busy ? "Run in progress" : "Run task"}>
              <span>{busy ? "Running" : "Run"}</span>
              <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true"><path d="M3.5 8h9m-3.5-3.5L12.5 8 9 11.5" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" /></svg>
            </button>
          </div>
        </div>
      </div>

      {state === "idle" && task.trim() === "" && (
        <button type="button" onClick={() => { setTask(EXAMPLE_TASKS[0]); areaRef.current?.focus(); }} className="example-prompt">
          <span className="text-faint">Try</span>
          <span className="truncate">{EXAMPLE_TASKS[0]}</span>
          <span aria-hidden="true">↗</span>
        </button>
      )}
      <p className="mt-3 flex items-center gap-2 text-[11px] text-faint sm:hidden">
        <span className="h-1.5 w-1.5 rounded-full bg-line" aria-hidden="true" />
        Monid search planned · not connected
      </p>
    </section>
  );
}
