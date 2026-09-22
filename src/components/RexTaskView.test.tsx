// @vitest-environment jsdom
import { render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi, beforeEach } from "vitest";
import { RexTaskView } from "./RexTaskView";

vi.mock("../data/rexTasks", async (importOriginal) => {
  const orig = await importOriginal<typeof import("../data/rexTasks")>();
  return {
    ...orig,
    rexTaskStatus: vi.fn(async () => ({
      task_id: "t-1",
      state: "completed",
      task: "demo",
      operator_is_agent: false,
      host: "ultra",
      open_action: null,
      last_event_seq: 0,
    })),
    rexTaskEvents: vi.fn(async () => ({
      events: [
        { seq: 1, ts_ms: 1, kind: "ultra_skill_plan", detail: {} },
        { seq: 2, ts_ms: 2, kind: "task_completed", detail: {} },
      ],
    })),
    rexTaskProof: vi.fn(async () => ({
      task_id: "t-1",
      state: "completed",
      kernel_state: "completed",
      qualified_candidate: "candidate-1",
      promotion_state: "committed",
      skill_plan: { selected: ["rust-core"], unsupported: [] },
      bundle_hash: "ab".repeat(32),
    })),
    rexTaskStop: vi.fn(),
    rexTaskFollowUp: vi.fn(),
  };
});

describe("RexTaskView completion copy", () => {
  beforeEach(() => vi.clearAllMocks());

  it("states that gates and proofs ran before promotion", async () => {
    render(<RexTaskView taskId="t-1" onClose={() => undefined} />);
    await waitFor(() =>
      expect(
        screen.getByText(/reran the frozen skill plan's gates and obligation proofs/i)
      ).toBeTruthy()
    );
    expect(screen.queryByText(/disabled pending rebuild/i)).toBeNull();
    expect(screen.queryByText(/not yet a verified result/i)).toBeNull();
  });
});
