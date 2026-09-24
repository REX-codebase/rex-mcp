import { useState } from "react";
import type { AgentEvent } from "../data/agentTypes";
import type { ToolResult } from "../data/backend";

// The run's activity, oldest first. Every tool row opens to the evidence
// behind it (diff, command, output) instead of only the newest receipt.
// plan_updated is skipped: the plan card above already shows the plan.

const VERBS: Record<string, string> = {
  read_file: "Read",
  edit_file: "Edited",
  create_file: "Created",
  apply_patch: "Patched",
  search_files: "Searched",
  list_files: "Listed",
  run_command: "Ran",
};

export function toolVerb(tool: string): string {
  return VERBS[tool] ?? tool.replace(/_/g, " ").replace(/^./, (c) => c.toUpperCase());
}

export function toolSubject(r: ToolResult): string {
  const cmd = r.receipt.command;
  if (cmd && cmd.length > 0) return cmd.join(" ");
  return r.receipt.target ?? "";
}

// Added/removed line counts from a unified diff; header lines do not count.
export function diffStat(diff: string | null): { add: number; del: number } | null {
  if (!diff) return null;
  let add = 0;
  let del = 0;
  for (const line of diff.split("\n")) {
    if (line.startsWith("+++") || line.startsWith("---")) continue;
    if (line.startsWith("+")) add += 1;
    else if (line.startsWith("-")) del += 1;
  }
  return { add, del };
}

export function formatMs(ms: number): string {
  return ms < 1000 ? `${ms} ms` : `${(ms / 1000).toFixed(ms < 10000 ? 1 : 0)} s`;
}

function DiffBlock({ diff }: { diff: string }) {
  return (
    <pre className="tl-diff">
      {diff.split("\n").map((line, i) => {
        const kind = line.startsWith("@@") ? "hunk" : line.startsWith("+") && !line.startsWith("+++") ? "add" : line.startsWith("-") && !line.startsWith("---") ? "del" : "ctx";
        return (
          <span key={i} className={`tl-line tl-line--${kind}`}>
            {line || " "}
          </span>
        );
      })}
    </pre>
  );
}

function ToolRow({ r, latest }: { r: ToolResult; latest: boolean }) {
  const stat = diffStat(r.receipt.diff);
  const subject = toolSubject(r);
  const exit = r.receipt.exit_code;
  const hasBody = Boolean(r.receipt.diff || r.output || r.error);
  const head = (
    <>
      <span className={`tl-mark ${r.ok ? "is-ok" : "is-err"}`} aria-hidden="true">
        {r.ok ? (
          <svg width="10" height="10" viewBox="0 0 10 10"><path d="M2 5.2 4.1 7.3 8 3" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" /></svg>
        ) : (
          <svg width="10" height="10" viewBox="0 0 10 10"><path d="M3 3l4 4M7 3 3 7" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" /></svg>
        )}
      </span>
      <span className="tl-verb">{toolVerb(r.tool)}</span>
      {subject && <code className="tl-subject">{subject}</code>}
      <span className="tl-meta">
        {stat && (stat.add > 0 || stat.del > 0) && (
          <span className="tl-stat">
            <span className="tl-add">+{stat.add}</span> <span className="tl-del">−{stat.del}</span>
          </span>
        )}
        {exit != null && exit !== 0 && <span className="tl-exit">exit {exit}</span>}
        {!r.ok && exit == null && r.error && <span className="tl-exit">{r.error.kind}</span>}
        <span>{formatMs(r.receipt.duration_ms)}</span>
      </span>
    </>
  );
  if (!hasBody) {
    return <li className="tl-row tl-tool"><div className="tl-head">{head}</div></li>;
  }
  return (
    <li className="tl-row tl-tool">
      <details className="tl-details" open={!r.ok && latest}>
        <summary className="tl-head">
          {head}
          <svg className="tl-chevron" width="12" height="12" viewBox="0 0 12 12" aria-hidden="true"><path d="M4.5 3 7.5 6l-3 3" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" /></svg>
        </summary>
        <div className="tl-body">
          {r.receipt.diff && <DiffBlock diff={r.receipt.diff} />}
          {r.output && <pre className="tl-output">{r.output}</pre>}
          {r.error && !r.output && <pre className="tl-output">{r.error.detail}</pre>}
          {r.receipt.output_truncated && <p className="tl-note">Output capped</p>}
        </div>
      </details>
    </li>
  );
}

function Row({ event, skipText, latestTool }: { event: AgentEvent; skipText?: AgentEvent; latestTool?: string }) {
  switch (event.state) {
    case "plan_updated":
      return null;
    case "model_text":
      if (event === skipText) return null;
      return <li className="tl-row tl-text">{event.text.length > 280 ? `${event.text.slice(0, 280)}…` : event.text}</li>;
    case "tool_finished":
      return <ToolRow r={event.result} latest={event.result.call_id === latestTool} />;
    case "approval_required":
      return <li className="tl-row tl-wait">Approval requested · {event.call.tool.replace(/_/g, " ")}</li>;
    case "approval_resolved":
      return <li className="tl-row tl-wait">{event.approved ? "Approved once" : "Denied"}</li>;
    case "standing_approval_granted":
      return <li className="tl-row tl-wait">Allowed for this run · {event.command}</li>;
    case "approved_by_standing":
      return <li className="tl-row tl-wait">Ran under your approval for this run · {event.command}</li>;
    case "plan_approval_required":
      return <li className="tl-row tl-wait">Plan proposed · {event.items.length} step{event.items.length === 1 ? "" : "s"} awaiting your approval</li>;
    case "plan_approval_resolved":
      return <li className="tl-row tl-wait">{event.approved ? "Plan approved · executing" : "Plan rejected · run stopped before any tool ran"}</li>;
    case "question_asked":
      return <li className="tl-row tl-wait">Question · {event.question.question}</li>;
    case "question_resolved":
      return <li className="tl-row tl-wait">{event.answered ? "Answered" : "Left to REX"}</li>;
    case "gate_result":
      return <li className={`tl-row ${event.passed ? "tl-gate-ok" : "tl-gate-err"}`}>{event.passed ? "Completion gates passed" : `Gates failed · ${event.failures.length}`}</li>;
    case "retry":
      return <li className="tl-row tl-wait">Retry {event.attempt} · {event.reason}</li>;
    case "info":
      return <li className="tl-row tl-text">{event.message}</li>;
    default:
      return null;
  }
}

export const TIMELINE_TAIL = 12;

export function ActivityTimeline({ events, skipText }: { events: AgentEvent[]; skipText?: AgentEvent }) {
  const [showAll, setShowAll] = useState(false);
  const visible = events.filter((e) => e.state !== "plan_updated" && e !== skipText);
  const hidden = showAll ? 0 : Math.max(0, visible.length - TIMELINE_TAIL);
  const shown = visible.slice(hidden);
  if (visible.length === 0) return null;
  // A failed step opens itself only while it is the newest tool call, so a
  // failure the run already recovered from does not stay expanded.
  const lastTool = [...visible].reverse().find((e) => e.state === "tool_finished");
  const latestTool = lastTool && lastTool.state === "tool_finished" ? lastTool.result.call_id : undefined;
  return (
    <section className="tl" aria-label="Run activity">
      <p className="eyebrow tl-title">Activity</p>
      {hidden > 0 && (
        <button type="button" className="tl-more" onClick={() => setShowAll(true)}>
          Show {hidden} earlier step{hidden === 1 ? "" : "s"}
        </button>
      )}
      <ol className="tl-list" aria-label="Run events">
        {shown.map((event, i) => (
          <Row key={hidden + i} event={event} latestTool={latestTool} />
        ))}
      </ol>
    </section>
  );
}
