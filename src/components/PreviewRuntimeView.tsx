import { useMemo, useState } from "react";

export type PreviewEvidence = {
  iteration: number;
  viewport: string;
  screenshot: string;
  consoleErrors: number;
  failedRequests: number;
  gatesPassed: number;
  gatesTotal: number;
  accepted: boolean;
};

export type LivePreviewState = {
  url: string;
  framework: "HTML" | "React" | "Next.js" | "Vite" | "Astro" | "SvelteKit";
  status: "starting" | "running" | "iterating" | "passed" | "failed" | "cancelled";
  cursor: { x: number; y: number; label: string };
  viewport: { width: number; height: number };
  evidence: PreviewEvidence[];
  selectedIteration: number;
};

const initial: LivePreviewState = {
  url: "http://127.0.0.1:41027/",
  framework: "React",
  status: "iterating",
  cursor: { x: 63, y: 46, label: "REX" },
  viewport: { width: 1440, height: 900 },
  selectedIteration: 3,
  evidence: [
    { iteration: 1, viewport: "1440 × 900", screenshot: "desktop", consoleErrors: 2, failedRequests: 1, gatesPassed: 6, gatesTotal: 10, accepted: false },
    { iteration: 2, viewport: "390 × 844", screenshot: "mobile", consoleErrors: 0, failedRequests: 0, gatesPassed: 8, gatesTotal: 10, accepted: false },
    { iteration: 3, viewport: "1440 × 900", screenshot: "desktop", consoleErrors: 0, failedRequests: 0, gatesPassed: 10, gatesTotal: 10, accepted: true },
  ],
};

export function PreviewRuntimeView({ state = initial, onCancel }: { state?: LivePreviewState; onCancel?: () => void }) {
  const [active, setActive] = useState(state.selectedIteration);
  const receipt = useMemo(() => state.evidence.find((item) => item.iteration === active) ?? state.evidence[state.evidence.length - 1], [active, state.evidence]);

  return (
    <section className="preview-runtime" aria-label="Live app preview">
      <header className="preview-runtime-head">
        <div>
          <p className="eyebrow">Live app preview</p>
          <h2>{state.framework} · iteration {state.selectedIteration}</h2>
          <p>Rust-owned process · loopback only · evidence recording</p>
        </div>
        <div className="preview-runtime-actions">
          <span className={`preview-runtime-state is-${state.status}`}>{state.status}</span>
          {!(["passed", "failed", "cancelled"] as string[]).includes(state.status) && <button type="button" onClick={onCancel}>Cancel preview</button>}
        </div>
      </header>

      <div className="preview-runtime-frame">
        <div className="preview-runtime-nav">
          <span className="preview-runtime-dot is-red" /><span className="preview-runtime-dot is-yellow" /><span className="preview-runtime-dot is-green" />
          <span className="preview-runtime-url">{state.url}</span>
          <span className="preview-runtime-viewport">{state.viewport.width} × {state.viewport.height}</span>
        </div>
        <div className="preview-runtime-canvas" aria-label="Rendered app viewport">
          <div className="preview-demo-sidebar"><b>Northstar</b><span>Overview</span><span>Activity</span><span>Reports</span></div>
          <div className="preview-demo-content">
            <p className="preview-demo-kicker">OPERATIONS</p><h3>Everything important, in one place.</h3>
            <div className="preview-demo-metrics"><span><b>98.7%</b><small>Uptime</small></span><span><b>2.4s</b><small>Median task</small></span><span><b>128</b><small>Checks passed</small></span></div>
            <div className="preview-demo-chart"><i /><i /><i /><i /><i /><i /><i /><i /></div>
          </div>
          <span className="agent-cursor" style={{ left: `${state.cursor.x}%`, top: `${state.cursor.y}%` }} aria-label={`${state.cursor.label} agent cursor`}><i>{state.cursor.label}</i></span>
        </div>
      </div>

      <div className="preview-evidence-grid">
        <div className="preview-iterations">
          <p className="eyebrow">Iterations · rejected work preserved</p>
          {state.evidence.map((item) => <button type="button" className={active === item.iteration ? "is-active" : ""} onClick={() => setActive(item.iteration)} key={item.iteration}><span>#{item.iteration} · {item.viewport}</span><b className={item.accepted ? "pass" : "reject"}>{item.accepted ? "accepted" : "rejected"}</b></button>)}
        </div>
        {receipt && <div className="preview-receipt">
          <p className="eyebrow">Grounded receipt</p>
          <div><span>Production gates</span><b>{receipt.gatesPassed}/{receipt.gatesTotal}</b></div>
          <div><span>Console errors</span><b>{receipt.consoleErrors}</b></div>
          <div><span>Failed requests</span><b>{receipt.failedRequests}</b></div>
          <div><span>Screenshot</span><b>captured · {receipt.screenshot}</b></div>
          <p>{receipt.accepted ? "Concrete gates passed. This iteration may finish." : "This iteration stays in history with its diff and evidence."}</p>
        </div>}
      </div>
    </section>
  );
}
