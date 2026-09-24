import { useEffect, useRef, useState } from "react";
import type { PreparedToolCall, ToolResult } from "../data/backend";
import { toolCallDiff, type FileDiff } from "../data/workspace";
import { DiffViewer } from "./DiffViewer";

export function ToolApproval({
  call,
  busy,
  onDecision,
}: {
  call: PreparedToolCall;
  busy: boolean;
  onDecision: (approved: boolean) => void;
}) {
  const [diff, setDiff] = useState<FileDiff | null>(null);
  const [diffLoading, setDiffLoading] = useState(false);
  const denyRef = useRef<HTMLButtonElement>(null);

  // A new request takes focus on Deny, the safe choice: a stray Enter or
  // Space can only refuse. Approving always needs a deliberate move.
  useEffect(() => {
    denyRef.current?.focus();
  }, [call.call_id]);

  useEffect(() => {
    let alive = true;
    // Only file-writing tools have diffs.
    if (call.tool === "create_file" || call.tool === "edit_file") {
      setDiffLoading(true);
      toolCallDiff(call.call_id)
        .then((d) => {
          if (alive) setDiff(d);
        })
        .catch(() => {
          if (alive) setDiff(null);
        })
        .finally(() => {
          if (alive) setDiffLoading(false);
        });
    }
    return () => {
      alive = false;
    };
  }, [call.call_id, call.tool]);

  return (
    <section
      className="tool-approval"
      role="alertdialog"
      aria-labelledby="tool-approval-title"
      aria-describedby="tool-approval-detail"
      onKeyDown={(event) => {
        if (event.key === "Escape" && !busy) {
          event.preventDefault();
          onDecision(false);
        }
      }}
    >
      <div className="tool-approval-head">
        <span className="approval-shield" aria-hidden="true">
          !
        </span>
        <div>
          <p className="eyebrow">Approval required</p>
          <h3 id="tool-approval-title">
            REX wants to {call.risk === "execute" ? "run a command" : "change files"}
          </h3>
        </div>
      </div>
      <p id="tool-approval-detail" className="tool-summary">
        {call.summary}
      </p>
      <dl>
        <div>
          <dt>Capability</dt>
          <dd>{call.tool.replace(/_/g, " ")}</dd>
        </div>
        <div>
          <dt>Why paused</dt>
          <dd>{call.policy_reason}</dd>
        </div>
      </dl>
      {diffLoading && <p className="approval-note">Loading diff…</p>}
      {diff && <DiffViewer diff={diff} />}
      <p className="approval-note">
        This request came from the model. Only your click can approve it.
      </p>
      <div className="approval-actions">
        <span className="approval-keys" aria-hidden="true">
          <kbd>Esc</kbd> denies
        </span>
        <button ref={denyRef} disabled={busy} onClick={() => onDecision(false)} className="approval-deny">
          Deny
        </button>
        <button disabled={busy} onClick={() => onDecision(true)} className="approval-allow">
          {busy ? "Working…" : "Approve once"}
        </button>
      </div>
    </section>
  );
}

export function ToolReceiptCard({result}:{result:ToolResult}){const r=result.receipt;return <section className={`tool-receipt ${result.ok?"receipt-ok":"receipt-error"}`} aria-label="Tool receipt"><div className="receipt-title"><span>{result.ok?"✓":"×"}</span><div><b>{result.tool.replace(/_/g," ")}</b><small>{result.ok?"Completed":"Stopped"} · {r.duration_ms} ms</small></div></div>{r.diff&&<pre className="receipt-diff">{r.diff}</pre>}{result.output&&<pre className="receipt-output">{result.output}</pre>}<footer><span>{r.bytes_written} B written</span><span>{r.redactions} redaction{r.redactions===1?"":"s"}</span>{r.output_truncated&&<span>Output capped</span>}</footer></section>}
