// @vitest-environment jsdom
import { fireEvent, render, screen, waitFor, cleanup } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi, afterEach } from "vitest";

const calls = vi.hoisted(() => ({ answer: [] as unknown[][], many: [] as unknown[][], undo: [] as unknown[][] }));
vi.mock("../data/agentRun", () => ({
  agentAnswer: (...args: unknown[]) => {
    calls.answer.push(args);
    return Promise.resolve({});
  },
  agentAnswerMany: (...args: unknown[]) => {
    calls.many.push(args);
    return Promise.resolve({});
  },
  agentUndo: (...args: unknown[]) => {
    calls.undo.push(args);
    return Promise.resolve({ ok: true, call_id: "c1", tool: "create_file", files: ["index.html"] });
  },
  agentCapture: () => Promise.resolve({}),
  agentPreviewAction: () => Promise.resolve({ ok: true }),
}));

import { AgentRunView } from "./AgentRunView";
import type { AgentSnapshot } from "../data/agentTypes";

function snap(partial: Partial<AgentSnapshot>): AgentSnapshot {
  return {
    id: "run-1",
    task: "t",
    status: "running",
    terminal_reason: null,
    provider: "gemini",
    model: "m",
    plan: [],
    step: 1,
    max_steps: 24,
    tool_calls: 1,
    max_tool_calls: 80,
    tokens_used: 10,
    max_tokens: 1000,
    elapsed_ms: 0,
    max_wall_ms: 1000,
    pending_approval: null,
    events: [],
    preview: null,
    completion_summary: null,
    error: null,
    ...partial,
  };
}

const noop = () => undefined;

describe("AgentRunView ask_user and undo", () => {
  afterEach(cleanup);
  beforeEach(() => {
    calls.answer.length = 0;
    calls.many.length = 0;
    calls.undo.length = 0;
  });

  it("shows a whole batch on one form and sends every answer together", async () => {
    const batch = [
      { question: "Which database?", choices: ["Postgres", "SQLite"] },
      { question: "Which port?", choices: [] },
      { question: "Auth?", choices: ["none", "token"] },
    ];
    render(
      <AgentRunView
        run={snap({
          status: "awaiting_answer",
          pending_question: { call_id: "c#1", question: batch[0].question, choices: batch[0].choices, batch_index: 1, batch_total: 3, batch },
        })}
        deciding={false}
        cancelling={false}
        onDecide={noop}
        onCancel={noop}
      />
    );
    expect(screen.getByRole("heading", { name: "3 questions" })).toBeTruthy();
    const send = screen.getByRole("button", { name: "Send answers" }) as HTMLButtonElement;
    expect(send.disabled).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "SQLite" }));
    fireEvent.change(screen.getByLabelText("Answer 2"), { target: { value: "8080" } });
    expect((screen.getByLabelText("Answer 1") as HTMLTextAreaElement).value).toBe("SQLite");
    fireEvent.click(send);
    await waitFor(() => expect(calls.many).toEqual([["run-1", ["SQLite", "8080", null]]]));
    fireEvent.click(screen.getByRole("button", { name: "Let REX decide all" }));
    await waitFor(() => expect(calls.many[1]).toEqual(["run-1", [null, null, null]]));
    expect(calls.answer).toEqual([]);
  });

  it("labels a question that is part of a batch", () => {
    render(
      <AgentRunView
        run={snap({
          status: "awaiting_answer",
          pending_question: { call_id: "q#2", question: "Which port?", choices: [], batch_index: 2, batch_total: 3 },
        })}
        deciding={false}
        cancelling={false}
        onDecide={noop}
        onCancel={noop}
      />
    );
    expect(screen.getByText("REX is asking · question 2 of 3")).toBeTruthy();
  });

  it("shows the question, sends a choice, a typed answer or a decline", async () => {
    render(
      <AgentRunView
        run={snap({
          status: "awaiting_answer",
          pending_question: { call_id: "q1", question: "Which database?", choices: ["Postgres", "SQLite"] },
        })}
        deciding={false}
        cancelling={false}
        onDecide={noop}
        onCancel={noop}
      />
    );
    expect(screen.getByText("REX has a question for you")).toBeTruthy();
    expect(screen.getByRole("heading", { name: "Which database?" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "SQLite" }));
    await waitFor(() => expect(calls.answer).toEqual([["run-1", "SQLite"]]));
    const send = screen.getByRole("button", { name: "Send answer" }) as HTMLButtonElement;
    expect(send.disabled).toBe(true);
    fireEvent.change(screen.getByLabelText("Your answer"), { target: { value: "use sqlite for the demo" } });
    fireEvent.click(send);
    await waitFor(() => expect(calls.answer[1]).toEqual(["run-1", "use sqlite for the demo"]));
    fireEvent.click(screen.getByRole("button", { name: "Let REX decide" }));
    await waitFor(() => expect(calls.answer[2]).toEqual(["run-1", null]));
  });

  it("offers undo only after a finished run that wrote files", async () => {
    const wrote = {
      state: "tool_finished",
      result: { call_id: "c1", ok: true, tool: "create_file", state: "executed", output: "", error: null, receipt: { target: "index.html", bytes_written: 10, duration_ms: 1 } },
    } as never;
    const { rerender } = render(
      <AgentRunView run={snap({ events: [wrote] })} deciding={false} cancelling={false} onDecide={noop} onCancel={noop} />
    );
    expect(screen.queryByRole("button", { name: "Undo last file change" })).toBeNull();
    rerender(
      <AgentRunView
        run={snap({ status: "completed", terminal_reason: { kind: "completed" }, events: [wrote] })}
        deciding={false}
        cancelling={false}
        onDecide={noop}
        onCancel={noop}
      />
    );
    fireEvent.click(screen.getByRole("button", { name: "Undo last file change" }));
    await waitFor(() => expect(screen.getByText("Undid create file · index.html")).toBeTruthy());
    expect(calls.undo).toEqual([["run-1"]]);
  });
});
