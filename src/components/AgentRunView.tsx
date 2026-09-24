import { useCallback, useEffect, useRef, useState } from "react";
import { ToolApproval, ToolReceiptCard } from "./ToolApproval";
import {
  agentAnswer,
  agentAnswerMany,
  agentCapture,
  agentPreviewAction,
  agentUndo,
  type AgentSnapshot,
} from "../data/agentRun";
import {
  formatClock,
  formatTokens,
  phaseOf,
  terminalLabel,
  type AgentEvent,
} from "../data/agentTypes";

const PAGE_W = 1280;

function planMark(status: string): string {
  switch (status) {
    case "done":
      return "✓";
    case "in_progress":
      return "●";
    case "blocked":
      return "×";
    default:
      return "○";
  }
}

function EventLine({ event }: { event: AgentEvent }) {
  switch (event.state) {
    case "plan_updated":
      return <li className="agent-event agent-event-plan">Plan updated · {event.items.length} item{event.items.length === 1 ? "" : "s"}</li>;
    case "model_text":
      return <li className="agent-event agent-event-text">{event.text.length > 180 ? `${event.text.slice(0, 180)}…` : event.text}</li>;
    case "tool_finished":
      return (
        <li className={`agent-event ${event.result.ok ? "agent-event-ok" : "agent-event-err"}`}>
          <span>{event.result.ok ? "✓" : "×"}</span>
          <span>
            {event.result.tool.replace(/_/g, " ")}
            {event.result.receipt.target ? ` · ${event.result.receipt.target}` : ""}
            {event.result.error ? ` · ${event.result.error.detail}` : ""}
          </span>
        </li>
      );
    case "approval_required":
      return <li className="agent-event agent-event-wait">Approval requested · {event.call.tool.replace(/_/g, " ")}</li>;
    case "approval_resolved":
      return <li className="agent-event agent-event-wait">{event.approved ? "Approved once" : "Denied"}</li>;
    case "plan_approval_required":
      return <li className="agent-event agent-event-wait">Plan proposed · {event.items.length} step{event.items.length === 1 ? "" : "s"} awaiting your approval</li>;
    case "plan_approval_resolved":
      return <li className="agent-event agent-event-wait">{event.approved ? "Plan approved · executing" : "Plan rejected · run stopped before any tool ran"}</li>;
    case "question_asked":
      return <li className="agent-event agent-event-wait">Question for you · {event.question.question.length > 140 ? `${event.question.question.slice(0, 140)}…` : event.question.question}</li>;
    case "question_resolved":
      return <li className="agent-event agent-event-wait">{event.answered ? "You answered · continuing" : "No answer · REX decides and states its assumption"}</li>;
    case "gate_result":
      return (
        <li className={`agent-event ${event.passed ? "agent-event-ok" : "agent-event-err"}`}>
          {event.passed ? "Completion gates passed" : `Gate attempt ${event.attempt} failed: ${event.failures.join("; ")}`}
        </li>
      );
    case "retry":
      return <li className="agent-event agent-event-wait">Provider retry {event.attempt} · {event.reason.length > 120 ? `${event.reason.slice(0, 120)}…` : event.reason}</li>;
    case "info":
      return <li className="agent-event agent-event-info">{event.message}</li>;
  }
}

export function AgentRunView({
  run,
  deciding,
  cancelling,
  onDecide,
  onCancel,
  custody,
  onPlanDecision,
  planDeciding,
}: {
  run: AgentSnapshot;
  deciding: boolean;
  cancelling: boolean;
  onDecide: (approved: boolean) => void;
  onCancel: () => void;
  custody?: { grantId: string; phase: string; operator: string } | null;
  onPlanDecision?: (approved: boolean) => void;
  planDeciding?: boolean;
}) {
  const phase = phaseOf(run);
  const [shot, setShot] = useState<string | null>(null);
  const [cursor, setCursor] = useState({ x: 140, y: 120 });
  const canvasRef = useRef<HTMLDivElement>(null);
  const runId = run.id;
  const previewLive = run.preview != null;
  const [answerText, setAnswerText] = useState("");
  const [batchAnswers, setBatchAnswers] = useState<string[]>([]);
  const [answering, setAnswering] = useState(false);
  const [answerError, setAnswerError] = useState<string | null>(null);
  const [undoing, setUndoing] = useState(false);
  const [undoNote, setUndoNote] = useState<string | null>(null);
  const questionId = run.pending_question?.call_id ?? null;

  useEffect(() => {
    // a new question starts with an empty box
    setAnswerText("");
    setBatchAnswers([]);
    setAnswerError(null);
  }, [questionId]);

  const sendAnswer = useCallback(
    async (text: string | null) => {
      setAnswering(true);
      setAnswerError(null);
      try {
        await agentAnswer(runId, text);
      } catch (err) {
        setAnswerError(err instanceof Error ? err.message : String(err));
      } finally {
        setAnswering(false);
      }
    },
    [runId]
  );

  const sendBatch = useCallback(
    async (answers: (string | null)[]) => {
      setAnswering(true);
      setAnswerError(null);
      try {
        await agentAnswerMany(runId, answers);
      } catch (err) {
        setAnswerError(err instanceof Error ? err.message : String(err));
      } finally {
        setAnswering(false);
      }
    },
    [runId]
  );
  const batch = run.pending_question?.batch ?? [];
  const showBatchForm = batch.length > 1 && (run.pending_question?.batch_index ?? 1) === 1;

  const undoLast = useCallback(async () => {
    setUndoing(true);
    try {
      const r = await agentUndo(runId);
      setUndoNote(`Undid ${r.tool.replace(/_/g, " ")} · ${r.files.join(", ")}`);
    } catch (err) {
      setUndoNote(`Undo refused · ${err instanceof Error ? err.message : String(err)}`);
    } finally {
      setUndoing(false);
    }
  }, [runId]);

  useEffect(() => {
    setShot(run.preview?.desktop_shot ?? null);
  }, [run.preview?.desktop_shot]);

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
      if (!previewLive) return;
      const p = toPage(e.clientX, e.clientY);
      setCursor({ x: p.cssX, y: p.cssY });
      void agentPreviewAction(runId, { kind: "pointer_move", x: p.x, y: p.y }).catch(() => undefined);
    },
    [previewLive, runId, toPage]
  );

  const onClick = useCallback(
    async (e: React.MouseEvent<HTMLDivElement>) => {
      if (!previewLive) return;
      const p = toPage(e.clientX, e.clientY);
      setCursor({ x: p.cssX, y: p.cssY });
      try {
        await agentPreviewAction(runId, { kind: "pointer_move", x: p.x, y: p.y });
        await agentPreviewAction(runId, { kind: "pointer_down", button: "primary" });
        await agentPreviewAction(runId, { kind: "pointer_up", button: "primary" });
        const next = await agentCapture(runId);
        if (next.screenshot_data_url) setShot(next.screenshot_data_url);
      } catch {
        /* transient capture failure keeps the last frame */
      }
    },
    [previewLive, runId, toPage]
  );

  const latestText = [...run.events].reverse().find((e) => e.state === "model_text");
  const receipts = run.events.filter((e): e is Extract<AgentEvent, { state: "tool_finished" }> => e.state === "tool_finished");
  const lastReceipt = receipts[receipts.length - 1]?.result ?? null;
  const terminal = run.terminal_reason;
  const working = phase === "working" || phase === "verifying";

  return (
    <section className="live-run agent-run" aria-label="Autonomous run">
      <div className="live-status" role="status">
        <span className={`live-dot ${working ? "is-live" : ""}`} aria-hidden="true" />
        <span className="eyebrow">Agent loop</span>
        <span className="live-status-text">
          {terminal
            ? terminalLabel(terminal)
            : phase === "approval"
              ? "Waiting on your approval"
              : phase === "question"
                ? "REX has a question for you"
              : phase === "plan"
                ? "Plan proposed · waiting on your approval"
                : phase === "verifying"
                  ? "Verifying the work…"
                  : run.model
                    ? `${run.model} · step ${run.step}/${run.max_steps}`
                    : "Contacting the live model catalog…"}
        </span>
        {(working || phase === "approval" || phase === "plan" || phase === "question") && (
          <button type="button" className="agent-cancel" onClick={onCancel} disabled={cancelling}>
            {cancelling ? "Cancelling…" : "Cancel"}
          </button>
        )}
      </div>
      {custody && (
        <p className="custody-line">
          Custody {custody.phase.replace(/_/g, " ")} · operator {custody.operator} · grant {custody.grantId.slice(0, 18)}
        </p>
      )}
      {/* Provenance: which engine produced this run. Stated on the run itself,
          never inferred from the transport badge in the top bar. */}
      <p className="engine-line">
        Engine · model <b>{run.provider}/{run.model || "resolving…"}</b>
      </p>

      <div className="agent-budgets" aria-label="Budgets">
        <span>Step {run.step}/{run.max_steps}</span>
        <span>Tools {run.tool_calls}/{run.max_tool_calls}</span>
        <span>Tokens {formatTokens(run.tokens_used)}/{formatTokens(run.max_tokens)}</span>
        <span>{formatClock(run.elapsed_ms)} / {formatClock(run.max_wall_ms)}</span>
      </div>

      {run.plan.length > 0 && (
        <ol className="agent-plan" aria-label="Todo plan">
          {run.plan.map((item) => (
            <li key={item.id} className={`agent-plan-item is-${item.status}`}>
              <span className="agent-plan-mark" aria-hidden="true">{planMark(item.status)}</span>
              <span className="agent-plan-title">{item.title}</span>
              {item.note && <small>{item.note}</small>}
            </li>
          ))}
        </ol>
      )}

      {latestText && phase !== "completed" && "text" in latestText && (
        <p className="live-model-text">{latestText.text.length > 220 ? `${latestText.text.slice(0, 220)}…` : latestText.text}</p>
      )}

      {run.pending_approval && phase === "approval" && (
        <ToolApproval call={run.pending_approval} busy={deciding} onDecision={onDecide} />
      )}

      {phase === "plan" && onPlanDecision && (
        <section className="plan-approval" role="alertdialog" aria-labelledby="plan-approval-title" aria-describedby="plan-approval-detail">
          <div className="tool-approval-head">
            <span className="approval-shield" aria-hidden="true">✓</span>
            <div>
              <p className="eyebrow">Plan approval</p>
              <h3 id="plan-approval-title">REX proposed a plan · nothing has run yet</h3>
            </div>
          </div>
          <p id="plan-approval-detail" className="approval-note">Review the steps above. Approve to let REX execute them, or reject to stop the run before any tool runs.</p>
          <div className="approval-actions">
            <button type="button" disabled={planDeciding} onClick={() => onPlanDecision(false)} className="approval-deny">Reject plan</button>
            <button type="button" disabled={planDeciding} onClick={() => onPlanDecision(true)} className="approval-allow">{planDeciding ? "Sending…" : "Approve plan"}</button>
          </div>
        </section>
      )}

      {phase === "question" && run.pending_question && showBatchForm && (
        <section className="plan-approval question-panel" role="alertdialog" aria-labelledby="batch-title">
          <div className="tool-approval-head">
            <span className="approval-shield" aria-hidden="true">?</span>
            <div>
              <p className="eyebrow">REX is asking</p>
              <h3 id="batch-title">{batch.length} questions</h3>
            </div>
          </div>
          {batch.map((q, i) => (
            <div key={i} className="question-batch-item">
              <p className="question-batch-text">{`${i + 1}. ${q.question}`}</p>
              {q.choices.length > 0 && (
                <div className="question-choices">
                  {q.choices.map((choice) => (
                    <button
                      key={choice}
                      type="button"
                      aria-pressed={batchAnswers[i] === choice}
                      disabled={answering}
                      onClick={() => setBatchAnswers((prev) => { const next = [...prev]; next[i] = choice; return next; })}
                    >
                      {choice}
                    </button>
                  ))}
                </div>
              )}
              <textarea
                className="question-input"
                aria-label={`Answer ${i + 1}`}
                value={batchAnswers[i] ?? ""}
                maxLength={2000}
                onChange={(e) => { const v = e.target.value; setBatchAnswers((prev) => { const next = [...prev]; next[i] = v; return next; }); }}
                placeholder="Pick a choice, type an answer, or leave blank to let REX decide"
              />
            </div>
          ))}
          {answerError && <p className="approval-note">{answerError}</p>}
          <div className="approval-actions">
            <button type="button" disabled={answering} onClick={() => void sendBatch(batch.map(() => null))} className="approval-deny">Let REX decide all</button>
            <button
              type="button"
              disabled={answering || !batch.some((_, i) => (batchAnswers[i] ?? "").trim())}
              onClick={() => void sendBatch(batch.map((_, i) => ((batchAnswers[i] ?? "").trim() ? batchAnswers[i] : null)))}
              className="approval-allow"
            >
              {answering ? "Sending…" : "Send answers"}
            </button>
          </div>
        </section>
      )}

      {phase === "question" && run.pending_question && !showBatchForm && (
        <section className="plan-approval question-panel" role="alertdialog" aria-labelledby="question-title">
          <div className="tool-approval-head">
            <span className="approval-shield" aria-hidden="true">?</span>
            <div>
              <p className="eyebrow">
                {run.pending_question.batch_total && run.pending_question.batch_total > 1
                  ? `REX is asking · question ${run.pending_question.batch_index ?? 1} of ${run.pending_question.batch_total}`
                  : "REX is asking"}
              </p>
              <h3 id="question-title">{run.pending_question.question}</h3>
            </div>
          </div>
          {run.pending_question.choices.length > 0 && (
            <div className="question-choices">
              {run.pending_question.choices.map((choice) => (
                <button key={choice} type="button" disabled={answering} onClick={() => void sendAnswer(choice)}>
                  {choice}
                </button>
              ))}
            </div>
          )}
          <textarea
            className="question-input"
            aria-label="Your answer"
            value={answerText}
            maxLength={2000}
            onChange={(e) => setAnswerText(e.target.value)}
            placeholder="Or type your own answer"
          />
          {answerError && <p className="approval-note">{answerError}</p>}
          <div className="approval-actions">
            <button type="button" disabled={answering} onClick={() => void sendAnswer(null)} className="approval-deny">Let REX decide</button>
            <button type="button" disabled={answering || !answerText.trim()} onClick={() => void sendAnswer(answerText)} className="approval-allow">{answering ? "Sending…" : "Send answer"}</button>
          </div>
        </section>
      )}

      <ul className="agent-events" aria-label="Run events">
        {run.events.slice(-8).map((event, i) => (
          <EventLine key={i} event={event} />
        ))}
      </ul>

      {lastReceipt && !working && (
        <details className="live-receipt">
          <summary>
            <span className={`live-receipt-mark ${lastReceipt.ok ? "ok" : "err"}`}>{lastReceipt.ok ? "✓" : "×"}</span>
            <span className="live-receipt-title">{lastReceipt.tool.replace(/_/g, " ")}</span>
            <span className="live-receipt-meta">
              {lastReceipt.ok ? `${lastReceipt.receipt.bytes_written.toLocaleString()} B written · ${lastReceipt.receipt.duration_ms} ms` : "stopped"}
            </span>
          </summary>
          <ToolReceiptCard result={lastReceipt} />
        </details>
      )}

      {terminal && receipts.some((r) => r.result.ok && ["create_file", "edit_file", "apply_patch"].includes(r.result.tool)) && (
        <div className="agent-undo">
          <button type="button" disabled={undoing} onClick={() => void undoLast()}>
            {undoing ? "Undoing…" : "Undo last file change"}
          </button>
          {undoNote && <span className="agent-undo-note">{undoNote}</span>}
        </div>
      )}

      {phase === "completed" && run.completion_summary && (
        <p className="agent-done-note">✓ {run.completion_summary}</p>
      )}
      {terminal && terminal.kind === "gates_failed" && (
        <ul className="agent-gate-failures">
          {terminal.failures.map((f, i) => (
            <li key={i}>{f}</li>
          ))}
        </ul>
      )}

      {run.preview && (
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
              aria-label="Verified preview. REX cursor clicks reach the real page."
            >
              {shot ? <img src={shot} alt="Verified rendered preview" /> : <p className="live-canvas-wait">Opening the preview…</p>}
              <span className="agent-cursor" style={{ left: cursor.x, top: cursor.y }} aria-hidden="true"><i>REX</i></span>
            </div>
          </div>
        </div>
      )}
    </section>
  );
}
