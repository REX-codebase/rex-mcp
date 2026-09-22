import { useEffect, useRef, useState } from "react";
import {
  proofJourneyPhases,
  rexTaskEvents,
  rexTaskList,
  rexTaskProof,
  rexTaskStatus,
  rexTaskStop,
  rexTaskFollowUp,
  type ProofPhase,
  type RexEvent,
  type RexProof,
  type RexStatus,
  type RexTaskSummary,
} from "../data/rexTasks";

const TERMINAL = new Set(["completed", "failed", "cancelled"]);

function eventLine(event: RexEvent): string {
  switch (event.kind) {
    case "task_created":
      return "Task created and leased";
    case "action_opened":
      return "Action opened";
    case "action_submitted":
      return "Submission received";
    case "task_completed":
      return "Completed - evidence gates passed";
    case "task_failed":
      return "Failed";
    case "task_cancelled":
      return "Cancelled";
    case "ultra_skill_plan":
      return "Skill plan bound";
    case "ultra_submission": {
      const kind = String((event.detail as { kind?: string }).kind ?? "");
      if (kind === "Candidate") return "Candidate thesis received";
      if (kind === "Adversary") return "Adversary evidence recorded";
      if (kind === "Verifier") return "Verifier evidence recorded";
      if (kind === "Visual") return "Pixel evidence recorded";
      return "Ultra evidence recorded";
    }
    case "ultra_promotion": {
      const state = String((event.detail as { state?: string }).state ?? "");
      return state === "committed"
        ? "Winning candidate promoted into the workspace"
        : `Promotion ${state.replace(/_/g, " ")}`;
    }
    case "tool_finished":
      return "Tool run finished";
    case "action_accepted":
      return "Action accepted";
    case "store_migrated":
      return "Task store upgraded to the current schema";
    default:
      return event.kind.replace(/_/g, " ");
  }
}

// Agent-mode supervision: a host agent drives this durable REX task through
// rex-mcp. This view never drives the task - it watches status and events
// and holds the permanent human Stop.
export function RexTaskView({ taskId, onClose }: { taskId: string; onClose: () => void }) {
  const [status, setStatus] = useState<RexStatus | null>(null);
  const [events, setEvents] = useState<RexEvent[]>([]);
  const [proof, setProof] = useState<RexProof | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [stopping, setStopping] = useState(false);
  const [followUp, setFollowUp] = useState("");
  const [submittingFollowUp, setSubmittingFollowUp] = useState(false);
  const [followUpError, setFollowUpError] = useState<string | null>(null);
  const cursor = useRef(0);

  useEffect(() => {
    let active = true;
    cursor.current = 0;
    setEvents([]);
    setStatus(null);
    setProof(null);
    setError(null);
    const tick = () => {
      rexTaskStatus(taskId)
        .then((s) => { if (active) { setStatus(s); setError(null); } })
        .catch((e) => { if (active) setError(String(e)); });
      rexTaskEvents(taskId, cursor.current)
        .then((r) => {
          if (!active || r.events.length === 0) return;
          cursor.current = r.last_seq;
          setEvents((prev) => {
            const next = [...prev, ...r.events].slice(-50);
            if (next.some((e) => e.kind.startsWith("ultra_"))) {
              rexTaskProof(taskId)
                .then((p) => { if (active) setProof(p); })
                .catch(() => undefined);
            }
            return next;
          });
        })
        .catch(() => undefined);
    };
    tick();
    const timer = setInterval(tick, 3000);
    return () => { active = false; clearInterval(timer); };
  }, [taskId]);

  const terminal = status != null && TERMINAL.has(status.state);
  const phases: ProofPhase[] = proofJourneyPhases(events);
  const terminalLine = !status
    ? ""
    : status.state === "completed"
      ? "Completed - the daemon reran the frozen skill plan's gates and obligation proofs on a staged copy before promoting; the evidence is below."
      : status.state === "failed"
        ? "Failed - the gates refused the work; the reason is in the evidence below."
        : status.state === "cancelled"
          ? "Stopped - the human Stop is final."
          : "Work continues - the daemon waits for the host to call again.";
  const stop = () => {
    if (stopping || terminal) return;
    setStopping(true);
    rexTaskStop(taskId)
      .then((r) =>
        setStatus((prev) =>
          prev ? { ...prev, state: r.state } : prev
        )
      )
      .catch((e) => setError(String(e)))
      .finally(() => setStopping(false));
  };

  const submitFollowUp = () => {
    const task = followUp.trim();
    if (!task || submittingFollowUp) return;
    setSubmittingFollowUp(true);
    setFollowUpError(null);
    rexTaskFollowUp(taskId, task)
      .then((result) => {
        setFollowUp("");
        setStatus((prev) => prev ? { ...prev, state: result.state } : prev);
      })
      .catch((e) => setFollowUpError(String(e)))
      .finally(() => setSubmittingFollowUp(false));
  };

  return (
    <section className="live-run rex-task" aria-label="Supervising a REX task">
      <div className="live-status" role="status">
        <span className={`live-dot ${terminal ? "" : "is-live"}`} aria-hidden="true" />
        <span className="eyebrow">REX task · agent operator</span>
        <span className="live-status-text">
          {error
            ? error
            : status
              ? `${status.state.replace(/_/g, " ")} · ${status.host.replace(/_/g, " ")}`
              : "Reading task state…"}
        </span>
        {status && !terminal && (
          <button type="button" className="agent-cancel" onClick={stop} disabled={stopping}>
            {stopping ? "Stopping…" : "Stop"}
          </button>
        )}
        <button type="button" className="rex-close" onClick={onClose} aria-label="Close supervision">
          Close
        </button>
      </div>
      {status && (
        <div className="rex-task-body">
          <p className="rex-task-text">{status.task}</p>
          <p className="rex-task-id">task {status.task_id} · operator {status.operator_is_agent ? "agent" : "human"}</p>
          {status.open_action && !terminal && (
            <p className="rex-task-action">Open action: {status.open_action.instructions}</p>
          )}
          <form
            className="rex-follow-up"
            onSubmit={(event) => { event.preventDefault(); submitFollowUp(); }}
          >
            <label htmlFor="rex-follow-up-input">Follow up on this task</label>
            <div className="rex-follow-up-row">
              <input
                id="rex-follow-up-input"
                value={followUp}
                onChange={(event) => setFollowUp(event.target.value)}
                placeholder="Ask REX to continue with the same task"
                disabled={submittingFollowUp}
                aria-describedby={followUpError ? "rex-follow-up-error" : undefined}
              />
              <button type="submit" disabled={submittingFollowUp || followUp.trim() === ""}>
                {submittingFollowUp ? "Sending…" : "Send"}
              </button>
            </div>
            {followUpError && <p id="rex-follow-up-error" className="rex-follow-up-error" role="alert">{followUpError}</p>}
          </form>
        </div>
      )}
      {phases.length > 0 && (
        <div className="proof-journey" aria-label="Ultra proof journey">
          <ol className="proof-spine">
            {phases.map((phase) => (
              <li key={phase.id} className={`proof-node is-${phase.state}`}>
                <span className="proof-dot" aria-hidden="true" />
                {phase.label}
              </li>
            ))}
          </ol>
          <div className="proof-hero">
            <p className="proof-hero-line">{terminalLine}</p>
            {proof && (
              <dl className="proof-hero-grid">
                {proof.kernel_state && (
                  <div>
                    <dt>Kernel</dt>
                    <dd>{proof.kernel_state.replace(/_/g, " ")}</dd>
                  </div>
                )}
                {proof.qualified_candidate && (
                  <div>
                    <dt>Qualified candidate</dt>
                    <dd>{proof.qualified_candidate.slice(0, 12)}</dd>
                  </div>
                )}
                {proof.promotion_state && (
                  <div>
                    <dt>Promotion</dt>
                    <dd>{proof.promotion_state.replace(/_/g, " ")}</dd>
                  </div>
                )}
                {proof.skill_plan?.selected && (
                  <div>
                    <dt>Skill packs</dt>
                    <dd>{proof.skill_plan.selected.length} bound</dd>
                  </div>
                )}
                <div>
                  <dt>Proof hash</dt>
                  <dd>{proof.bundle_hash.slice(0, 12)}</dd>
                </div>
              </dl>
            )}
          </div>
        </div>
      )}
      {events.length > 0 && (
        <ul className="agent-events rex-task-events">
          {events.map((event) => (
            <li key={event.seq} className="agent-event agent-event-info">
              {eventLine(event)}
            </li>
          ))}
        </ul>
      )}
      {!status && !error && <p className="rex-task-id">Contacting rex-mcp…</p>}
    </section>
  );
}

// Durable REX tasks in the shared store: anything a host opened through
// rex-mcp shows up here for supervision.
export function RexTaskList({ onPick }: { onPick: (taskId: string) => void }) {
  const [tasks, setTasks] = useState<RexTaskSummary[]>([]);

  useEffect(() => {
    rexTaskList()
      // Terminal tasks stay listed: a promoted proof journey is exactly
      // what supervision must be able to reopen.
      .then((r) => setTasks(r.tasks))
      .catch(() => undefined);
  }, []);

  if (tasks.length === 0) return null;
  return (
    <section className="rex-task-list" aria-label="REX tasks">
      <span className="eyebrow">REX tasks</span>
      <ul>
        {tasks.map((t) => (
          <li key={t.task_id}>
            <button type="button" onClick={() => onPick(t.task_id)}>
              <span className="rex-task-list-text">{t.task}</span>
              <span className="rex-task-list-meta">{t.state.replace(/_/g, " ")} · {t.host.replace(/_/g, " ")}</span>
            </button>
          </li>
        ))}
      </ul>
    </section>
  );
}
