// @vitest-environment jsdom
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { UltraRunView } from "./UltraRunView";
import type { UltraSnapshot } from "../data/ultraRun";

const builder = {
  id: "agent-1",
  task: "t",
  status: "completed",
  terminal_reason: { kind: "completed" },
  provider: "gemini",
  model: "gemini-3.5-flash-lite",
  plan: [],
  step: 2,
  max_steps: 48,
  tool_calls: 3,
  max_tool_calls: 160,
  tokens_used: 500,
  max_tokens: 1000000,
  elapsed_ms: 1000,
  max_wall_ms: 2700000,
  pending_approval: null,
  events: [],
  preview: null,
  completion_summary: "done",
  error: null,
} as never;

function fixture(over: Partial<UltraSnapshot>): UltraSnapshot {
  return {
    id: "ultra-1",
    task: "build a page",
    phase: "promoted",
    provider: "gemini",
    model: "gemini-3.5-flash-lite",
    judge_model: "gemini/gemini-3.5-flash-lite",
    adversary_model: "gemini/gemini-3.5-flash-lite",
    repair: 0,
    max_repairs: 2,
    contract: {
      task: "build a page",
      obligations: [
        { id: "page", statement: "index.html exists with heading", proof: { kind: "file_contains" } },
      ],
    },
    verification: {
      executable_all_proven: true,
      outcomes: [
        { obligation_id: "page", status: "proven", detail: "contains needle", evidence_ids: ["ev-1"], duration_ms: 3 },
      ],
    },
    adversary: { defects: [], inconclusive: false, adversary_model: "gemini" },
    judge: {
      verdicts: [{ obligation_id: "page", verdict: "pass", reason: "proven", evidence: ["ev-1"] }],
      all_passed: true,
      judge_model: "gemini/gemini-3.5-flash-lite",
      same_model_as_worker: true,
    },
    builder,
    terminal: { kind: "promoted" },
    run_dir: "/tmp/run",
    ...over,
  };
}

describe("UltraRunView", () => {
  it("shows the real phase and obligation states", () => {
    render(<UltraRunView run={fixture({})} deciding={false} cancelling={false} onDecide={() => {}} onCancel={() => {}} />);
    expect(screen.getByText(/Promoted/)).toBeTruthy();
    expect(screen.getByText(/index.html exists with heading/)).toBeTruthy();
    expect(screen.getByText(/judge: pass/)).toBeTruthy();
  });

  it("surfaces rejection reasons instead of hiding them", () => {
    render(
      <UltraRunView
        run={fixture({ phase: "rejected", terminal: { kind: "rejected", reasons: ["verification failed for page: missing"] } })}
        deciding={false} cancelling={false} onDecide={() => {}} onCancel={() => {}}
      />,
    );
    expect(screen.getByRole("alert").textContent).toContain("verification failed for page");
  });

  it("labels the same-model judge honestly", () => {
    render(<UltraRunView run={fixture({})} deciding={false} cancelling={false} onDecide={() => {}} onCancel={() => {}} />);
    expect(screen.getAllByText(/judge gemini\/gemini-3.5-flash-lite/).length).toBeGreaterThan(0);
  });
});
