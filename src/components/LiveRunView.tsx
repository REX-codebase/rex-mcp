import { useCallback, useEffect, useRef, useState } from "react";
import { ToolApproval, ToolReceiptCard } from "./ToolApproval";
import {
  runCapture,
  runDecide,
  runPreviewAction,
  type RunSnapshot,
} from "../data/liveRun";

type Phase = "thinking" | "deciding" | "live" | "denied" | "failed";

function phaseOf(run: RunSnapshot | null, deciding: boolean): Phase {
  if (deciding) return "deciding";
  if (!run) return "thinking";
  switch (run.status) {
    case "awaiting_approval":
      return "thinking";
    case "live":
      return "live";
    case "denied":
      return "denied";
    case "failed":
      return "failed";
  }
}

const PAGE_W = 1280;

function statusLine(run: RunSnapshot | null, deciding: boolean): string {
  if (!run) return "Contacting the live Gemini catalog…";
  if (deciding) return "Approved once · REX tools writing…";
  if (run.status === "awaiting_approval") return `${run.model} · live turn received`;
  return "";
}

export function LiveRunView({ run, deciding, onDecide }: { run: RunSnapshot | null; deciding: boolean; onDecide: (approved: boolean) => void }) {
  const phase = phaseOf(run, deciding);
  const [shot, setShot] = useState<string | null>(null);
  const [cursor, setCursor] = useState({ x: 140, y: 120 });
  const [evidenceOpen, setEvidenceOpen] = useState(false);
  const [evidence, setEvidence] = useState<{ dom: number; ax: number; consoleErrors: boolean; networkFailures: boolean } | null>(null);
  const canvasRef = useRef<HTMLDivElement>(null);
  const runId = run?.id ?? null;
  const live = phase === "live" && runId;

  useEffect(() => {
    setShot(run?.preview?.desktop_shot ?? null);
    if (run?.preview) setEvidence(null);
  }, [run?.preview?.desktop_shot, run?.preview]);

  const toPage = useCallback((clientX: number, clientY: number) => {
    const el = canvasRef.current;
    if (!el) return { x: 0, y: 0, cssX: 0, cssY: 0 };
    const rect = el.getBoundingClientRect();
    const cssX = clientX - rect.left;
    const cssY = clientY - rect.top;
    const scale = rect.width / PAGE_W;
    return { x: cssX / scale, y: cssY / scale, cssX, cssY };
  }, []);

  const onMove = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      if (!live) return;
      const p = toPage(e.clientX, e.clientY);
      setCursor({ x: p.cssX, y: p.cssY });
      void runPreviewAction(runId, { kind: "pointer_move", x: p.x, y: p.y }).catch(() => undefined);
    },
    [live, runId, toPage]
  );

  const onClick = useCallback(
    async (e: React.MouseEvent<HTMLDivElement>) => {
      if (!live) return;
      const p = toPage(e.clientX, e.clientY);
      setCursor({ x: p.cssX, y: p.cssY });
      try {
        await runPreviewAction(runId, { kind: "pointer_move", x: p.x, y: p.y });
        await runPreviewAction(runId, { kind: "pointer_down", button: "primary" });
        await runPreviewAction(runId, { kind: "pointer_up", button: "primary" });
        const next = await runCapture(runId);
        if (next.screenshot_data_url) setShot(next.screenshot_data_url);
        setEvidence({
          dom: next.dom_text.length,
          ax: next.accessibility_text.length,
          consoleErrors: next.items.some((i) => (i as { kind?: string; level?: string }).kind === "console" && (i as { level?: string }).level === "error"),
          networkFailures: next.items.some((i) => (i as { kind?: string }).kind === "network_failure"),
        });
      } catch {
        /* transient capture failure keeps the last frame */
      }
    },
    [live, runId, toPage]
  );

  const modelText = run?.events.find((e) => e.state === "model_text");
  const receipt = run?.result ?? null;
  const iterations = run?.preview?.receipts ?? [];

  return (
    <section className="live-run" aria-label="Live run">
      <div className="live-status" role="status">
        <span className={`live-dot ${phase === "live" ? "is-live" : ""}`} aria-hidden="true" />
        <span className="eyebrow">Live run</span>
        <span className="live-status-text">
          {phase === "thinking" || phase === "deciding"
            ? statusLine(run, deciding)
            : phase === "live"
              ? `${run?.model} · preview live`
              : phase === "denied"
                ? "Stopped · write denied"
                : `Stopped · ${run?.error ?? "the run failed"}`}
        </span>
      </div>

      {phase === "thinking" && (
        <div className="live-thinking">
          <p className="live-thinking-line">Model is thinking</p>
          <p className="live-thinking-sub">{run ? "Preparing the requested change…" : "Refreshing the live model catalog and asking for the change."}</p>
        </div>
      )}

      {modelText && "text" in modelText && phase === "thinking" && (
        <p className="live-model-text">{modelText.text}</p>
      )}

      {run?.approval && phase === "thinking" && (
        <ToolApproval call={run.approval} busy={deciding} onDecision={onDecide} />
      )}

      {receipt && (phase === "live" || phase === "denied" || phase === "failed") && (
        <details className="live-receipt">
          <summary>
            <span className={`live-receipt-mark ${receipt.ok ? "ok" : "err"}`}>{receipt.ok ? "✓" : "×"}</span>
            <span className="live-receipt-title">{receipt.tool.replace(/_/g, " ")}</span>
            <span className="live-receipt-meta">
              {receipt.ok ? `${receipt.receipt.bytes_written.toLocaleString()} B written · ${receipt.receipt.duration_ms} ms` : "stopped"}
            </span>
          </summary>
          <ToolReceiptCard result={receipt} />
        </details>
      )}

      {phase === "live" && run?.preview && (
        <div className="preview-runtime live-preview">
          <div className="preview-runtime-frame">
            <div className="preview-runtime-nav">
              <span className="preview-runtime-dot is-red" /><span className="preview-runtime-dot is-yellow" /><span className="preview-runtime-dot is-green" />
              <span className="preview-runtime-url">{run.preview.url}</span>
              <span className="preview-runtime-viewport">1280 × 800</span>
            </div>
            <div
              ref={canvasRef}
              className="preview-runtime-canvas native-preview-canvas live-canvas"
              onPointerMove={onMove}
              onClick={(e) => void onClick(e)}
              role="application"
              aria-label="Live app preview. REX cursor clicks reach the real page."
            >
              {shot ? <img src={shot} alt="Live rendered preview" /> : <p className="live-canvas-wait">Opening the live preview…</p>}
              <span className="agent-cursor" style={{ left: cursor.x, top: cursor.y }} aria-hidden="true"><i>REX</i></span>
            </div>
          </div>

          <button type="button" className="verification-toggle live-evidence-toggle" aria-expanded={evidenceOpen} onClick={() => setEvidenceOpen(!evidenceOpen)}>
            <span>Evidence <span className="text-faint">{iterations.filter((i) => i.accepted).length}/{iterations.length} iterations accepted</span></span>
            <svg width="10" height="6" viewBox="0 0 10 6" className={`transition-transform ${evidenceOpen ? "rotate-180" : ""}`} aria-hidden="true">
              <path d="M1 1l4 4 4-4" stroke="currentColor" strokeWidth="1.4" fill="none" strokeLinecap="round" />
            </svg>
          </button>

          {evidenceOpen && (
            <div className="live-evidence">
              <ol className="live-iterations" aria-label="Iterations">
                {iterations.map((item) => (
                  <li key={item.iteration} className="live-iteration">
                    <span className={`live-iteration-mark ${item.accepted ? "pass" : "reject"}`}>{item.accepted ? "accepted" : "rejected"}</span>
                    <div>
                      <p>Iteration {item.iteration}</p>
                      <small>{item.reason}</small>
                      {item.failed_gates.length > 0 && <small>Missing gates: {item.failed_gates.join(", ")}</small>}
                    </div>
                    {item.accepted && item.iteration === 2 && run.preview?.mobile_shot && (
                      <img className="live-iteration-shot" src={run.preview.mobile_shot} alt="Mobile 390x844 capture" />
                    )}
                  </li>
                ))}
              </ol>
              {evidence && (
                <dl className="live-evidence-grid">
                  <div><dt>DOM snapshot</dt><dd>{evidence.dom.toLocaleString()} B</dd></div>
                  <div><dt>Accessibility tree</dt><dd>{evidence.ax.toLocaleString()} B</dd></div>
                  <div><dt>Console errors</dt><dd>{evidence.consoleErrors ? "present" : "none"}</dd></div>
                  <div><dt>Failed requests</dt><dd>{evidence.networkFailures ? "present" : "none"}</dd></div>
                </dl>
              )}
            </div>
          )}
        </div>
      )}
    </section>
  );
}
