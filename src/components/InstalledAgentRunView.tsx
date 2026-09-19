import { useState } from "react";
import type { InstalledAgentEvent, InstalledRunSnapshot } from "../data/installedAgentRun";

function eventText(payload: unknown, path: string[]): string | null {
  let node: unknown = payload;
  for (const key of path) {
    if (node == null || typeof node !== "object") return null;
    node = (node as Record<string, unknown>)[key];
  }
  return typeof node === "string" ? node : null;
}

function EventLine({ event }: { event: InstalledAgentEvent }) {
  switch (event.event) {
    case "init": {
      const mode = eventText(event.payload, ["init", "permission_mode"]);
      return <li className="agent-event agent-event-info">Session opened{mode ? ` · permission mode ${mode}` : ""}</li>;
    }
    case "step_update": {
      const state = eventText(event.payload, ["step_update", "state"]) ?? "";
      const tool = eventText(event.payload, ["step_update", "tool_name"]) ?? eventText(event.payload, ["step_update", "step_type"]) ?? "step";
      const err = eventText(event.payload, ["step_update", "tool_info", "error", "message"]);
      return (
        <li className={`agent-event ${err ? "agent-event-err" : "agent-event-ok"}`}>
          <span>{err ? "×" : state === "DONE" ? "✓" : "●"}</span>
          <span>{tool.replace(/_/g, " ")}{err ? ` · ${err}` : state ? ` · ${state.toLowerCase()}` : ""}</span>
        </li>
      );
    }
    case "result": {
      const status = eventText(event.payload, ["result", "status"]) ?? "finished";
      const response = eventText(event.payload, ["result", "response"]);
      return (
        <li className={`agent-event ${status === "SUCCESS" ? "agent-event-ok" : "agent-event-err"}`}>
          <span>{status === "SUCCESS" ? "✓" : "×"}</span>
          <span>Turn finished · {status.toLowerCase()}{response ? ` · ${response.length > 140 ? `${response.slice(0, 140)}…` : response}` : ""}</span>
        </li>
      );
    }
    default:
      return <li className="agent-event agent-event-info">{event.event}</li>;
  }
}

const STATUS_LABEL: Record<InstalledRunSnapshot["status"], string> = {
  running: "Working",
  awaiting_review: "Review staged changes",
  completed: "Completed",
  failed: "Failed",
  cancelled: "Cancelled",
};

export function InstalledAgentRunView({
  run,
  deciding,
  cancelling,
  onDecide,
  onCancel,
  onOpenPreview,
}: {
  run: InstalledRunSnapshot;
  deciding: boolean;
  cancelling: boolean;
  onDecide: (approved: boolean) => void;
  onCancel: () => void;
  onOpenPreview: (dir: string) => void;
}) {
  const [showStderr, setShowStderr] = useState(false);
  const working = run.status === "running";
  const review = run.status === "awaiting_review";
  const canPreview = run.status === "completed" && run.completion === "completed" && run.promotion !== "discarded";
  return (
    <section className="live-run agent-run" aria-label="Installed agent run">
      <div className="live-status" role="status">
        <span className={`live-dot ${working ? "is-live" : ""}`} aria-hidden="true" />
        <span className="eyebrow">Installed agent</span>
        <span className="live-status-text">
          {STATUS_LABEL[run.status]}
          {run.model ? ` · ${run.model}` : " · CLI default model"}
          {run.effort ? ` · effort ${run.effort}` : ""}
        </span>
        {working && (
          <button type="button" className="live-cancel" disabled={cancelling} onClick={onCancel}>
            {cancelling ? "Cancelling…" : "Cancel"}
          </button>
        )}
      </div>

      <ul className="agent-events" aria-label="Run events">
        {run.events.length === 0 && <li className="agent-event agent-event-info">Starting the vendor CLI…</li>}
        {run.events.map((event, index) => <EventLine key={index} event={event} />)}
        {run.status === "failed" && run.error && <li className="agent-event agent-event-err"><span>×</span><span>{run.error}</span></li>}
      </ul>

      {review && run.diff && (
        <div className="tool-approval" role="group" aria-label="Review staged changes">
          <p className="eyebrow">Staged changes · approval required</p>
          <p className="tool-approval-copy">
            The agent worked on an isolated copy. Nothing in your workspace has changed yet. Promote applies exactly these {run.diff.entries.length} change{run.diff.entries.length === 1 ? "" : "s"}; discard deletes the staged copy.
          </p>
          <ul className="diff-list">
            {run.diff.entries.map((entry) => (
              <li key={`${entry.kind}-${entry.path}`} className={`diff-entry is-${entry.kind}`}>
                <span className="diff-kind">{entry.kind}</span>
                <span className="diff-path">{entry.path}</span>
                {entry.kind !== "deleted" && <span className="diff-bytes">{entry.bytes} B</span>}
              </li>
            ))}
          </ul>
          <div className="tool-approval-actions">
            <button type="button" className="approve" disabled={deciding} onClick={() => onDecide(true)}>
              {deciding ? "Applying…" : "Promote into workspace"}
            </button>
            <button type="button" className="deny" disabled={deciding} onClick={() => onDecide(false)}>
              Discard
            </button>
          </div>
        </div>
      )}

      {run.status === "completed" && (
        <div className="run-completion">
          <p className="run-completion-line">
            {run.promotion === "promoted" && "Changes promoted into the workspace."}
            {run.promotion === "discarded" && "Staged changes were discarded; the workspace is untouched."}
            {run.promotion === "not_required" && "Work completed in its own task workspace."}
            {run.exit_code != null && ` Exit code ${run.exit_code}.`}
          </p>
          {canPreview && (
            <button type="button" className="dialog-primary is-active" onClick={() => onOpenPreview(run.preview_dir)}>
              Open native preview
            </button>
          )}
        </div>
      )}

      {run.stderr_tail && (
        <div className="run-stderr">
          <button type="button" className="run-stderr-toggle" onClick={() => setShowStderr((v) => !v)} aria-expanded={showStderr}>
            Child diagnostics {showStderr ? "▾" : "▸"}
          </button>
          {showStderr && <pre>{run.stderr_tail}</pre>}
        </div>
      )}
    </section>
  );
}
