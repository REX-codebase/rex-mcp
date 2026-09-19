import type { UltraSnapshot } from "../data/ultraRun";
import { AgentRunView } from "./AgentRunView";

const PHASE_LABEL: Record<string, string> = {
  contracting: "Compiling the acceptance contract",
  building: "Builder working",
  verifying: "Re-executing every proof fresh",
  adversary: "Adversary attacking the result",
  judging: "Clean-room judge scoring obligations",
  repairing: "Repairing gate failures",
  promoted: "Promoted - every gate agreed",
  rejected: "Rejected - gates still disagree",
  failed: "Failed",
  cancelled: "Cancelled",
};

function obligationMark(status: string): string {
  switch (status) {
    case "proven": return "✓";
    case "failed": return "×";
    default: return "…";
  }
}

/** The Ultra control surface: real phases, real obligation states, no decoration. */
export function UltraRunView({
  run,
  deciding,
  cancelling,
  onDecide,
  onCancel,
}: {
  run: UltraSnapshot;
  deciding: boolean;
  cancelling: boolean;
  onDecide: (approved: boolean) => void;
  onCancel: () => void;
}) {
  return (
    <section className="ultra-run" aria-label="Ultra verification run">
      <header className="ultra-run-head">
        <span className="eyebrow">Ultra</span>
        <strong>{PHASE_LABEL[run.phase] ?? run.phase}</strong>
        <small>
          worker {run.provider}/{run.model || "resolving"} · judge {run.judge_model || "worker"} · adversary {run.adversary_model || "worker"}
          {run.repair > 0 ? ` · repair ${run.repair}/${run.max_repairs}` : ""}
        </small>
      </header>
      {run.contract && (
        <ol className="ultra-obligations" aria-label="Acceptance contract obligations">
          {run.contract.obligations.map((ob) => {
            const outcome = run.verification?.outcomes.find((o) => o.obligation_id === ob.id);
            const verdict = run.judge?.verdicts.find((v) => v.obligation_id === ob.id);
            const mark = outcome ? obligationMark(outcome.status) : "○";
            return (
              <li key={ob.id} className={`ultra-obligation ${outcome ? `is-${outcome.status}` : ""}`}>
                <span aria-hidden="true">{mark}</span>
                <span>{ob.statement}</span>
                {verdict && <em>judge: {verdict.verdict}</em>}
              </li>
            );
          })}
        </ol>
      )}
      {run.adversary && (run.adversary.defects.length > 0 || run.adversary.inconclusive) && (
        <div className="ultra-adversary" role="status">
          {run.adversary.inconclusive && <p>Adversary pass was inconclusive.</p>}
          {run.adversary.defects.map((d, i) => (
            <p key={i}>Adversary: {d.title} - {d.detail}</p>
          ))}
        </div>
      )}
      {run.terminal?.kind === "rejected" && (
        <div className="ultra-rejected" role="alert">
          {run.terminal.reasons.map((r, i) => <p key={i}>{r}</p>)}
        </div>
      )}
      {run.builder ? (
        <AgentRunView
          run={run.builder}
          deciding={deciding}
          cancelling={cancelling}
          onDecide={onDecide}
          onCancel={onCancel}
        />
      ) : (
        <p className="ultra-waiting">Preparing the contract and workspace…</p>
      )}
    </section>
  );
}
