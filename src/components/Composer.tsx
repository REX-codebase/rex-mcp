import { useEffect, useRef, useState } from "react";
import { ModelStatus } from "./ModelStatus";
import { EXAMPLE_TASKS, type RunState } from "../data/mock";
import type { ExecutionPath } from "../data/executionPath";

// Alt+P flips Build/Plan from inside the task field. Pure so it can be
// tested without a DOM: returns the next plan-mode value, or null when the
// key event is not the shortcut.
export function planShortcut(e: { altKey: boolean; ctrlKey: boolean; metaKey: boolean; shiftKey: boolean; key: string; code?: string }, planMode: boolean): boolean | null {
  if (!e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return null;
  // macOS Option+P yields "π" in e.key; e.code stays "KeyP".
  if (e.code === "KeyP" || e.key.toLowerCase() === "p") return !planMode;
  return null;
}

export function Composer({ task, setTask, state, onRun, planMode, setPlanMode, summarizeHistory = false, setSummarizeHistory, fableGate, setFableGate, executionPath }: { task: string; setTask: (t: string) => void; state: RunState; onRun: () => void; planMode: boolean; setPlanMode: (v: boolean) => void; summarizeHistory?: boolean; setSummarizeHistory?: (v: boolean) => void; fableGate: boolean; setFableGate: (v: boolean) => void; executionPath: ExecutionPath }) {
  const busy = state === "working" || state === "verifying";
  const canRun = task.trim().length > 0 && !busy;
  const areaRef = useRef<HTMLTextAreaElement>(null);
  const optionsRef = useRef<HTMLDivElement>(null);
  const [optionsOpen, setOptionsOpen] = useState(false);
  const extrasOn = (setSummarizeHistory && summarizeHistory ? 1 : 0) + (fableGate ? 1 : 0);

  useEffect(() => {
    if (!optionsOpen) return;
    const onDown = (e: MouseEvent) => {
      if (optionsRef.current && !optionsRef.current.contains(e.target as Node)) setOptionsOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOptionsOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [optionsOpen]);

  return (
    <section aria-label="New task">
      <div className="mb-5 sm:mb-6">
        <p className="eyebrow">New task</p>
        <h1 className="mt-2 text-[30px] font-medium leading-[1.12] tracking-[-0.04em] text-text sm:text-[38px]">Tell REX what to do.</h1>
        <p className="mt-3 max-w-[54ch] text-sm leading-relaxed text-muted">One clear request. REX will work through it, verify the result, and leave a receipt.</p>
      </div>

      <div className={`task-field ${planMode ? "is-plan" : ""}`}>
        <textarea
          id="task-input"
          ref={areaRef}
          rows={4}
          value={task}
          disabled={busy}
          onChange={(e) => setTask(e.target.value)}
          onKeyDown={(e) => {
            if ((e.metaKey || e.ctrlKey) && e.key === "Enter" && canRun) onRun();
            const next = planShortcut(e, planMode);
            if (next !== null) {
              e.preventDefault();
              setPlanMode(next);
            }
          }}
          aria-label="Task description"
          placeholder={planMode ? "What should REX plan? Nothing runs until you approve." : "What needs to get done?"}
          spellCheck={false}
          className="task-input"
        />
        <div className="task-controls">
          <div className="flex min-w-0 items-center">
            <ModelStatus />
          </div>
          <div className="flex shrink-0 items-center gap-2">
            <div className="mode-seg" role="radiogroup" aria-label="Run mode" title="Alt+P switches mode">
              <button type="button" role="radio" aria-checked={!planMode} disabled={busy} onClick={() => setPlanMode(false)} className={!planMode ? "is-on" : ""}>Build</button>
              <button type="button" role="radio" aria-checked={planMode} disabled={busy} onClick={() => setPlanMode(true)} className={planMode ? "is-on" : ""}>Plan</button>
            </div>
            <div className="relative" ref={optionsRef}>
              <button type="button" className={`options-button ${optionsOpen ? "is-open" : ""}`} aria-haspopup="dialog" aria-expanded={optionsOpen} aria-label={`Run options${extrasOn ? `, ${extrasOn} on` : ""}`} onClick={() => setOptionsOpen((v) => !v)}>
                <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true"><path d="M2.5 4.5h6m3 0h2m-11 7h2m3 0h6" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" /><circle cx="10" cy="4.5" r="1.6" fill="none" stroke="currentColor" strokeWidth="1.4" /><circle cx="6" cy="11.5" r="1.6" fill="none" stroke="currentColor" strokeWidth="1.4" /></svg>
                {extrasOn > 0 && <span className="options-count" aria-hidden="true">{extrasOn}</span>}
              </button>
              {optionsOpen && (
                <div className="options-menu" role="dialog" aria-label="Run options">
                  <p className="options-title">Run options</p>
                  {setSummarizeHistory && (
                    <label className="switch-row" title="Summarize old results: when older tool results no longer fit in REX's working memory, the model writes a short summary of them. Each summary is an extra model call on your key, so this is off by default.">
                      <span className="switch-copy"><b>Summarize old results</b><small>When old tool output no longer fits, the model writes a short summary. Costs an extra model call on your key.</small></span>
                      <input type="checkbox" role="switch" className="switch" checked={summarizeHistory} disabled={busy} onChange={(e) => setSummarizeHistory(e.target.checked)} aria-label="Summarize old results" />
                    </label>
                  )}
                  <label className="switch-row" title="Fable gate: opens a THINK→PROVE→ATTACK→WRITE session with a mechanical deliberation timer and evidence-gated unlock. The countdown shows above the run.">
                    <span className="switch-copy"><b>Fable gate</b><small>Think, prove, attack, then write. A timer and evidence gate unlock each step.</small></span>
                    <input type="checkbox" role="switch" className="switch" checked={fableGate} disabled={busy} onChange={(e) => setFableGate(e.target.checked)} aria-label="Fable gate" />
                  </label>
                </div>
              )}
            </div>
            <button type="button" onClick={onRun} disabled={!canRun} title={!task.trim() ? "Describe a task first" : busy ? "A run is in progress" : `${planMode ? "Draft the plan" : "Start the run"} (Ctrl+Enter)`} className="run-button" aria-label={busy ? "Run in progress" : planMode ? "Plan task" : "Run task"}>
              <span>{busy ? "Running" : planMode ? "Plan" : "Run"}</span>
              <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true"><path d="M3.5 8h9m-3.5-3.5L12.5 8 9 11.5" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" /></svg>
            </button>
          </div>
        </div>
      </div>

      <div className="composer-meta">
        <span className="composer-meta-item" title={executionPath.detail}>
          <span className={`execution-path-dot execution-path-dot--${executionPath.kind}`} aria-hidden="true" />
          {executionPath.label}
        </span>
        {planMode && <span className="composer-meta-item text-muted">Plan mode: nothing runs until you approve the plan</span>}
        <span className="composer-meta-item" title="REX-search is the in-house zero-cost engine; it is not connected in this preview.">REX-search not connected</span>
        <span className="composer-meta-keys hidden sm:inline-flex"><kbd>Ctrl</kbd><kbd>Enter</kbd> run<span className="mx-1.5 text-line">·</span><kbd>Alt</kbd><kbd>P</kbd> plan</span>
      </div>

      {state === "idle" && task.trim() === "" && (
        <button type="button" onClick={() => { setTask(EXAMPLE_TASKS[0]); areaRef.current?.focus(); }} className="example-prompt">
          <span className="text-faint">Try</span>
          <span className="truncate">{EXAMPLE_TASKS[0]}</span>
          <span aria-hidden="true">↗</span>
        </button>
      )}
    </section>
  );
}
