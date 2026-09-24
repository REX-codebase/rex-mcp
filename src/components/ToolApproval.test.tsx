// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("../data/workspace", () => ({ toolCallDiff: vi.fn(() => Promise.resolve(null)) }));
vi.mock("./DiffViewer", () => ({ DiffViewer: () => null }));

import { ToolApproval } from "./ToolApproval";
import type { PreparedToolCall } from "../data/backend";

afterEach(cleanup);

const call = (id: string): PreparedToolCall => ({
  call_id: id,
  tool: "run_command",
  summary: "npm test",
  risk: "execute" as PreparedToolCall["risk"],
  approval_required: true,
  policy_reason: "commands need approval",
});

describe("tool approval card", () => {
  it("puts focus on Deny when a request appears", () => {
    render(<ToolApproval call={call("a")} busy={false} onDecision={() => {}} />);
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Deny" }));
  });
  it("Enter on the default focus can only deny", () => {
    const onDecision = vi.fn();
    render(<ToolApproval call={call("a")} busy={false} onDecision={onDecision} />);
    (document.activeElement as HTMLButtonElement).click();
    expect(onDecision).toHaveBeenCalledWith(false);
    expect(onDecision).not.toHaveBeenCalledWith(true);
  });
  it("Escape denies, but not while a decision is in flight", () => {
    const onDecision = vi.fn();
    const { rerender } = render(<ToolApproval call={call("a")} busy={false} onDecision={onDecision} />);
    fireEvent.keyDown(screen.getByRole("alertdialog"), { key: "Escape" });
    expect(onDecision).toHaveBeenCalledWith(false);
    onDecision.mockClear();
    rerender(<ToolApproval call={call("a")} busy onDecision={onDecision} />);
    fireEvent.keyDown(screen.getByRole("alertdialog"), { key: "Escape" });
    expect(onDecision).not.toHaveBeenCalled();
  });
  it("a new request takes focus back to Deny", () => {
    const { rerender } = render(<ToolApproval call={call("a")} busy={false} onDecision={() => {}} />);
    screen.getByRole("button", { name: "Approve once" }).focus();
    rerender(<ToolApproval call={call("b")} busy={false} onDecision={() => {}} />);
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Deny" }));
  });
  it("offers Allow for this run only for a command with a standing key and a handler", () => {
    const onAllow = vi.fn();
    const { rerender } = render(<ToolApproval call={call("a")} busy={false} onDecision={() => {}} onAllowForRun={onAllow} />);
    expect(screen.queryByRole("button", { name: "Allow for this run" })).toBeNull();
    const keyed = { ...call("b"), standing_key: '[["npm","test"],"."]' };
    rerender(<ToolApproval call={keyed} busy={false} onDecision={() => {}} />);
    expect(screen.queryByRole("button", { name: "Allow for this run" })).toBeNull();
    rerender(<ToolApproval call={keyed} busy={false} onDecision={() => {}} onAllowForRun={onAllow} />);
    fireEvent.click(screen.getByRole("button", { name: "Allow for this run" }));
    expect(onAllow).toHaveBeenCalledTimes(1);
    // the safe default is unchanged
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Deny" }));
  });
});
