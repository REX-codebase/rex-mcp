// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { SessionView } from "./SessionView";
import type { Session } from "../data/mock";

const session = (state: "working" | "done"): Session => ({
  id: "s", receipt: "F-1", title: "t", engine: "Preview engine", sample: true,
  turns: [{ id: "t1", request: "Do it", kind: "task", state, summary: "Done it", startedAt: "Just now", durationMs: 900,
    steps: [{ id: "e1", name: "Read the relevant files", kind: "File", detail: "Mapped the code", durationMs: 900, ok: true }] }],
});

afterEach(cleanup);

describe("SessionView evidence", () => {
  it("shows a live row for the step in progress while the turn runs", () => {
    render(<SessionView session={session("working")} busy onFollowUp={() => {}} onNewTask={() => {}} focusComposer={false} />);
    expect(screen.getByText("Read the relevant files")).toBeTruthy();
    expect(screen.getByText("next step in progress")).toBeTruthy();
  });
  it("drops the live row once the turn is finished", () => {
    render(<SessionView session={session("done")} busy={false} onFollowUp={() => {}} onNewTask={() => {}} focusComposer={false} />);
    expect(screen.getByText("Done it")).toBeTruthy();
    expect(screen.getByText("Read the relevant files")).toBeTruthy();
    expect(screen.queryByText("next step in progress")).toBeNull();
  });
});
