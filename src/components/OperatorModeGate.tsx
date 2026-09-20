import { useState } from "react";
import { modeDetail, type OperatorMode } from "../data/operatorMode";

// First-run entry mode. One choice, honest copy, no dark patterns: either
// option is fully functional, switching later is allowed and only affects
// the next task.
export function OperatorModeGate({ onChoose }: { onChoose: (mode: OperatorMode) => void }) {
  const [picked, setPicked] = useState<OperatorMode>("human");
  return (
    <main className="mx-auto flex min-h-full w-full max-w-[560px] flex-col justify-center px-6 py-16">
      <p className="eyebrow">REX Harness</p>
      <h1 className="mt-3 text-[32px] font-medium leading-[1.1] tracking-[-0.04em] text-text sm:text-[40px]">
        Who is operating?
      </h1>
      <p className="mt-4 max-w-[52ch] text-sm leading-relaxed text-muted">
        REX records this in custody for every task and it decides who the
        tools answer to. You can change it later; a change never rewrites a
        running task.
      </p>
      <div className="mt-9 flex flex-col gap-3" role="radiogroup" aria-label="Operator mode">
        {(["human", "agent"] as const).map((mode) => (
          <button
            key={mode}
            role="radio"
            aria-checked={picked === mode}
            onClick={() => setPicked(mode)}
            className={`operator-option ${picked === mode ? "is-active" : ""}`}
          >
            <span className="flex items-center justify-between">
              <b className="text-[15px] font-medium text-text">
                {mode === "human" ? "A human" : "An agent"}
              </b>
              <i aria-hidden="true" />
            </span>
            <small className="mt-1.5 block text-left text-[12.5px] leading-relaxed text-muted">
              {modeDetail(mode)}
            </small>
          </button>
        ))}
      </div>
      <button className="run-button mt-8 self-start" onClick={() => onChoose(picked)}>
        Continue
      </button>
      <p className="mt-6 text-[11.5px] leading-relaxed text-faint">
        REX never asks for account credentials. In agent mode the host agent
        keeps its own login; REX only sees typed MCP requests.
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
