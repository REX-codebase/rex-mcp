import { useRef, useState, type KeyboardEvent } from "react";
import { modeDetail, type OperatorMode } from "../data/operatorMode";

// First-run entry mode. One choice, honest copy, no dark patterns: either
// option is fully functional, switching later is allowed and only affects
// the next task.
const MODES = ["human", "agent"] as const;

const OPTION_COPY: Record<OperatorMode, { title: string; line: string; after: string }> = {
  human: {
    title: "A human",
    line: "You type tasks here and approve each risky step yourself.",
    after: "Next: the task composer.",
  },
  agent: {
    title: "An agent",
    line: "Another coding agent sends REX its tasks. You watch here, approve risky steps and can stop any run.",
    after: "Next: the list of tasks the host agent has sent.",
  },
};

export function OperatorModeGate({ onChoose }: { onChoose: (mode: OperatorMode) => void }) {
  const [picked, setPicked] = useState<OperatorMode>("human");
  const refs = useRef<Record<OperatorMode, HTMLButtonElement | null>>({ human: null, agent: null });

  // Radio-group keyboard model: arrows move and select, Enter confirms.
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const step = { ArrowDown: 1, ArrowRight: 1, ArrowUp: -1, ArrowLeft: -1 }[event.key];
    if (step !== undefined) {
      event.preventDefault();
      const next = MODES[(MODES.indexOf(picked) + step + MODES.length) % MODES.length];
      setPicked(next);
      refs.current[next]?.focus();
    } else if (event.key === "Enter") {
      event.preventDefault();
      onChoose(picked);
    }
  };

  return (
    <main className="operator-gate mx-auto flex w-full max-w-[560px] flex-col justify-center px-6 py-16">
      <p className="eyebrow">REX Harness</p>
      <h1 className="mt-3 text-[32px] font-medium leading-[1.1] tracking-[-0.04em] text-text sm:text-[40px]">
        Who is operating?
      </h1>
      <p className="mt-4 max-w-[52ch] text-sm leading-relaxed text-muted">
        REX logs this with every task so it knows who the tools answer to. You
        can switch any time from the top bar; a switch never changes a task
        that is already running.
      </p>
      <div
        className="mt-9 flex flex-col gap-3"
        role="radiogroup"
        aria-label="Operator mode"
        onKeyDown={onKeyDown}
      >
        {MODES.map((mode) => {
          const copy = OPTION_COPY[mode];
          const active = picked === mode;
          return (
            <button
              key={mode}
              ref={(el) => {
                refs.current[mode] = el;
              }}
              role="radio"
              aria-checked={active}
              tabIndex={active ? 0 : -1}
              title={modeDetail(mode)}
              onClick={() => setPicked(mode)}
              onDoubleClick={() => onChoose(mode)}
              className={`operator-option ${active ? "is-active" : ""}`}
            >
              <span className="operator-radio" aria-hidden="true" />
              <span className="min-w-0">
                <b className="block text-[15px] font-medium text-text">{copy.title}</b>
                <small className="mt-1 block text-[13px] leading-relaxed text-muted">{copy.line}</small>
                <small className="operator-after">{copy.after}</small>
              </span>
            </button>
          );
        })}
      </div>
      <div className="mt-8 flex items-center gap-4">
        <button className="run-button" onClick={() => onChoose(picked)}>
          Continue as {picked}
        </button>
        <span className="operator-keys text-[11.5px] text-faint">
          <kbd>↑</kbd> <kbd>↓</kbd> to pick,{" "}
          <kbd>Enter</kbd> to continue
        </span>
      </div>
      <p className="mt-6 text-[11.5px] leading-relaxed text-faint">
        REX never asks for account passwords. In agent mode the host agent
        keeps its own login.
      </p>
    </main>
  );
}

// Persistent chip: the current mode, always visible, switchable between
// tasks.
export function OperatorModeChip({
  mode,
  onSwitch,
}: {
  mode: OperatorMode;
  onSwitch: (mode: OperatorMode) => void;
}) {
  const other: OperatorMode = mode === "human" ? "agent" : "human";
  return (
    <button
      className="operator-chip"
      title={`${modeDetail(mode)} Switch to ${other} mode for the next task.`}
      onClick={() => onSwitch(other)}
    >
      <span className="operator-chip-dot" data-mode={mode} aria-hidden="true" />
      {mode === "human" ? "Human" : "Agent"} operator
    </button>
  );
}
