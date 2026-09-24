import { formatClock, formatTokens } from "../data/agentTypes";

// Budget meters for a run. Each budget is a hard stop in the agent loop,
// so the bar shows how close the run is to it; at 80% it turns amber and
// at 100% red, and the fullest budget is named for screen readers.

export interface Meter {
  label: string;
  used: number;
  max: number;
  text: string;
}

export function meterLevel(used: number, max: number): "ok" | "warn" | "full" {
  if (max <= 0) return "ok";
  const r = used / max;
  return r >= 1 ? "full" : r >= 0.8 ? "warn" : "ok";
}

export function meterPct(used: number, max: number): number {
  if (max <= 0) return 0;
  return Math.max(0, Math.min(100, (used / max) * 100));
}

export function runMeters(run: { step: number; max_steps: number; tool_calls: number; max_tool_calls: number; tokens_used: number; max_tokens: number; elapsed_ms: number; max_wall_ms: number }): Meter[] {
  return [
    { label: "Steps", used: run.step, max: run.max_steps, text: `${run.step}/${run.max_steps}` },
    { label: "Tools", used: run.tool_calls, max: run.max_tool_calls, text: `${run.tool_calls}/${run.max_tool_calls}` },
    { label: "Tokens", used: run.tokens_used, max: run.max_tokens, text: `${formatTokens(run.tokens_used)}/${formatTokens(run.max_tokens)}` },
    { label: "Time", used: run.elapsed_ms, max: run.max_wall_ms, text: `${formatClock(run.elapsed_ms)}/${formatClock(run.max_wall_ms)}` },
  ];
}

export function RunMeters({ meters }: { meters: Meter[] }) {
  return (
    <div className="run-meters" aria-label="Budgets">
      {meters.map((m) => {
        const level = meterLevel(m.used, m.max);
        return (
          <div key={m.label} className={`run-meter is-${level}`}>
            <div className="run-meter-top">
              <span>{m.label}</span>
              <b>{m.text}</b>
            </div>
            <div className="run-meter-track" role="meter" aria-label={`${m.label} budget`} aria-valuemin={0} aria-valuemax={m.max} aria-valuenow={Math.min(m.used, m.max)}>
              <i style={{ width: `${meterPct(m.used, m.max)}%` }} />
            </div>
          </div>
        );
      })}
    </div>
  );
}
