import { useEffect, useRef, useState } from "react";
import type { Session, Turn } from "../data/mock";

function fmt(ms: number) {
  return ms < 1000 ? `${ms} ms` : `${(ms / 1000).toFixed(1)} s`;
}

function TurnBlock({ turn, receipt, engine, expanded }: { turn: Turn; receipt: string; engine: string; expanded: boolean }) {
  const [open, setOpen] = useState(expanded);
  const finished = turn.state === "done" || turn.state === "blocked";
  const passed = turn.steps.filter((s) => s.ok).length;
  return (
    <article className={turn.kind === "follow-up" ? "turn-block" : ""} aria-label={turn.kind === "follow-up" ? "Follow-up" : "Result"}>
      <p className="turn-request">
        <span className="eyebrow">{turn.kind === "follow-up" ? "Follow-up" : "Task"}</span>
        <span className="turn-request-text">&ldquo;{turn.request}&rdquo;</span>
      </p>
      <div className="flex items-baseline justify-between gap-4">
        <p className="eyebrow">{turn.state === "blocked" ? "Stopped" : turn.kind === "follow-up" ? "Updated result" : "Result"}</p>
        <span className="font-mono text-[11px] text-faint">
          {receipt}
          {turn.kind === "follow-up" ? ` · ${turn.id.toUpperCase()}` : ""} · SAMPLE
        </span>
      </div>
      {finished ? (
        <p className="mt-4 max-w-[62ch] text-[17px] leading-[1.65] tracking-[-0.01em] text-text">{turn.summary}</p>
      ) : (
        <p className="mt-4 text-[16px] text-muted">
          {turn.kind === "follow-up" ? "Working through the follow-up with the earlier context kept." : "Working through the task. Evidence appears as it lands."}
        </p>
      )}
      {finished && turn.steps.length > 0 && !expanded && (
        <button type="button" aria-expanded={open} onClick={() => setOpen(!open)} className="verification-toggle">
          <span>
            Evidence <span className="text-faint">{passed}/{turn.steps.length} passed</span>
          </span>
          <svg width="10" height="6" viewBox="0 0 10 6" className={`transition-transform ${open ? "rotate-180" : ""}`} aria-hidden="true">
            <path d="M1 1l4 4 4-4" stroke="currentColor" strokeWidth="1.4" fill="none" strokeLinecap="round" />
          </svg>
        </button>
      )}
      {(expanded || open) && (
        <ol className="tl-list session-evidence" aria-label="Evidence">
          {turn.steps.map((s) => (
            <li key={s.id} className="tl-row">
              <div className="tl-head">
                <span className={`tl-mark ${s.ok ? "is-ok" : "is-err"}`} aria-hidden="true">
                  {s.ok ? (
                    <svg width="10" height="10" viewBox="0 0 10 10"><path d="M2 5.2 4.1 7.3 8 3" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" /></svg>
                  ) : (
                    <svg width="10" height="10" viewBox="0 0 10 10"><path d="M3 3l4 4M7 3 3 7" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" /></svg>
                  )}
                </span>
                <span className="tl-verb">{s.name}</span>
                <span className="tl-detail">{s.detail}</span>
                <span className="tl-meta"><span>{fmt(s.durationMs)}</span></span>
              </div>
            </li>
          ))}
          {!finished && (
            <li className="tl-row" aria-live="polite">
              <div className="tl-head">
                <span className="tl-mark is-live" aria-hidden="true"><i /></span>
                <span className="tl-verb text-muted">{turn.state === "verifying" ? "Verifying" : "Working"}</span>
                <span className="tl-detail">next step in progress</span>
              </div>
            </li>
          )}
          {finished && turn.steps.length === 0 && <li className="tl-row tl-text">No evidence yet.</li>}
        </ol>
      )}
      {finished && turn.steps.length > 0 && (expanded || open) && (
        <dl className="verification-grid border-t border-line pt-4">
          <div>
            <dt>Run time</dt>
            <dd>{fmt(turn.durationMs)}</dd>
          </div>
          <div>
            <dt>Engine</dt>
            <dd>{engine}</dd>
          </div>
          <div>
            <dt>Started</dt>
            <dd>{turn.startedAt}</dd>
          </div>
        </dl>
      )}
    </article>
  );
}

function FollowUpComposer({ busy, onFollowUp, focusWhenReady }: { busy: boolean; onFollowUp: (request: string) => void; focusWhenReady: boolean }) {
  const [text, setText] = useState("");
  const areaRef = useRef<HTMLTextAreaElement>(null);
  const canSend = text.trim().length > 0 && !busy;
  useEffect(() => {
    if (focusWhenReady && !busy) areaRef.current?.focus();
  }, [focusWhenReady, busy]);
  const send = () => {
    const request = text.trim();
    if (!request) return;
    onFollowUp(request);
    setText("");
  };
  return (
    <div className="followup-field composer-arrive" aria-label="Follow up on this run">
      <textarea
        ref={areaRef}
        rows={2}
        value={text}
        disabled={busy}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if ((e.metaKey || e.ctrlKey) && e.key === "Enter" && canSend) send();
        }}
        aria-label="Follow-up request"
        placeholder="Ask for a change. The run's context is kept."
        className="followup-input"
      />
      <div className="followup-controls">
        <span className="composer-meta-keys hidden text-[11px] text-faint md:inline-flex"><kbd>Ctrl</kbd><kbd>Enter</kbd> send</span>
        <button type="button" onClick={send} disabled={!canSend} title={!text.trim() ? "Describe the change first" : busy ? "A turn is in progress" : "Send the follow-up"} className="run-button" aria-label={busy ? "Follow-up in progress" : "Send follow-up"}>
          <span>{busy ? "Working" : "Follow up"}</span>
          <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true"><path d="M3.5 8h9m-3.5-3.5L12.5 8 9 11.5" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" /></svg>
        </button>
      </div>
    </div>
  );
}

export function SessionView({ session, busy, onFollowUp, onNewTask, focusComposer }: { session: Session; busy: boolean; onFollowUp: (request: string) => void; onNewTask: () => void; focusComposer: boolean }) {
  const latestIndex = session.turns.length - 1;
  return (
    <section aria-label="Session" className="result-section">
      <div className="session-head">
        <span className="eyebrow">Session</span>
        <button type="button" className="newtask-button" onClick={onNewTask}>
          <svg width="11" height="11" viewBox="0 0 12 12" aria-hidden="true"><path d="M6 1.5v9M1.5 6h9" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" /></svg>
          New task
        </button>
      </div>
      {session.turns.map((turn, i) => (
        <TurnBlock key={turn.id} turn={turn} receipt={session.receipt} engine={session.engine} expanded={i === latestIndex} />
      ))}
      <FollowUpComposer busy={busy} onFollowUp={onFollowUp} focusWhenReady={focusComposer} />
      <p className="mt-3 text-[11px] leading-relaxed text-faint">
        Follow-ups keep this run&rsquo;s context instead of starting over. Preview: responses are
        simulated locally, nothing is sent to a model.
      </p>
      <p className="mt-6 text-xs leading-relaxed text-faint">Preview only. No backend is connected, so this is sample interface data, not real agent output.</p>
    </section>
  );
}
