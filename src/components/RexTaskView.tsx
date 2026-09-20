import { useEffect, useRef, useState } from "react";
import {
  rexTaskEvents,
  rexTaskList,
  rexTaskStatus,
  rexTaskStop,
  type RexEvent,
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
  const [error, setError] = useState<string | null>(null);
  const [stopping, setStopping] = useState(false);
  const cursor = useRef(0);

  useEffect(() => {
    let active = true;
    cursor.current = 0;
    setEvents([]);
    setStatus(null);
    setError(null);
    const tick = () => {
      rexTaskStatus(taskId)
        .then((s) => { if (active) { setStatus(s); setError(null); } })
        .catch((e) => { if (active) setError(String(e)); });
      rexTaskEvents(taskId, cursor.current)
        .then((r) => {
          if (!active || r.events.length === 0) return;
          cursor.current = r.last_seq;
          setEvents((prev) => [...prev, ...r.events].slice(-50));
        })
        .catch(() => undefined);
    };
    tick();
    const timer = setInterval(tick, 3000);
    return () => { active = false; clearInterval(timer); };
  }, [taskId]);

  const terminal = status != null && TERMINAL.has(status.state);
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
      .then((r) => setTasks(r.tasks.filter((t) => !TERMINAL.has(t.state))))
      .catch(() => undefined);
  }, []);

  if (tasks.length === 0) return null;
  return (
    <section className="rex-task-list" aria-label="Active REX tasks">
      <span className="eyebrow">Active REX tasks</span>
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
