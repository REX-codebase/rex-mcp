// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { ActivityTimeline, diffStat, formatMs, TIMELINE_TAIL, toolSubject, toolVerb } from "./ActivityTimeline";
import type { AgentEvent } from "../data/agentTypes";

const rc = (o: Record<string, unknown> = {}) => ({ started_at_ms: 0, duration_ms: 42, target: null, command: null, exit_code: null, bytes_read: 0, bytes_written: 0, output_truncated: false, diff: null, redactions: 0, ...o });
const tool = (id: string, name: string, o: Record<string, unknown> = {}, ok = true, output: string | null = null): AgentEvent => ({
  state: "tool_finished",
  result: { call_id: id, ok, tool: name, state: "executed", output, error: ok ? null : { kind: "exit", detail: "exit 1" }, receipt: rc(o) as never },
});

afterEach(cleanup);

describe("timeline helpers", () => {
  it("counts diff lines without headers", () => {
    expect(diffStat("--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n+more\n ctx")).toEqual({ add: 2, del: 1 });
    expect(diffStat(null)).toBeNull();
    expect(diffStat("")).toBeNull();
  });
  it("names tools and subjects", () => {
    expect(toolVerb("edit_file")).toBe("Edited");
    expect(toolVerb("web_fetch")).toBe("Web fetch");
    const r = (tool("c", "run_command", { command: ["npm", "test"], target: "x" }) as Extract<AgentEvent, { state: "tool_finished" }>).result;
    expect(toolSubject(r)).toBe("npm test");
    const f = (tool("c", "read_file", { target: "a.ts" }) as Extract<AgentEvent, { state: "tool_finished" }>).result;
    expect(toolSubject(f)).toBe("a.ts");
  });
  it("formats durations", () => {
    expect(formatMs(999)).toBe("999 ms");
    expect(formatMs(1000)).toBe("1.0 s");
    expect(formatMs(5400)).toBe("5.4 s");
    expect(formatMs(12000)).toBe("12 s");
  });
});

describe("ActivityTimeline", () => {
  it("shows every tool with its evidence, skips plan updates and the headline text", () => {
    const head: AgentEvent = { state: "model_text", text: "Now fixing Hero" };
    render(
      <ActivityTimeline
        skipText={head}
        events={[
          { state: "plan_updated", items: [] },
          { state: "model_text", text: "Searching first" },
          tool("c1", "edit_file", { target: "a.ts", diff: "@@ -1 +1 @@\n-old\n+new" }),
          head,
        ]}
      />,
    );
    expect(screen.queryByText(/Plan updated/)).toBeNull();
    expect(screen.queryByText("Now fixing Hero")).toBeNull();
    expect(screen.getByText("Searching first")).toBeTruthy();
    expect(screen.getByText("Edited")).toBeTruthy();
    expect(screen.getByText("+1")).toBeTruthy();
    expect(screen.getByText("+new")).toBeTruthy();
    expect(screen.getByText("-old")).toBeTruthy();
  });

  it("opens a failed step only while it is the newest tool call", () => {
    const fail = tool("c1", "run_command", { command: ["npm", "run", "typecheck"], exit_code: 1 }, false, "TS2307");
    const { rerender, container } = render(<ActivityTimeline events={[fail]} />);
    expect(screen.getByText("exit 1")).toBeTruthy();
    expect((container.querySelector("details") as HTMLDetailsElement).open).toBe(true);
    rerender(<ActivityTimeline events={[fail, tool("c2", "edit_file", { target: "b.ts", diff: "+x" })]} />);
    const all = container.querySelectorAll("details");
    expect((all[0] as HTMLDetailsElement).open).toBe(false);
    expect((all[1] as HTMLDetailsElement).open).toBe(false);
  });

  it("keeps the whole history reachable instead of dropping old steps", () => {
    const events = Array.from({ length: TIMELINE_TAIL + 3 }, (_, i) => tool(`c${i}`, "read_file", { target: `f${i}.ts` }));
    render(<ActivityTimeline events={events} />);
    expect(screen.queryByText("f0.ts")).toBeNull();
    expect(screen.getByText(`f${TIMELINE_TAIL + 2}.ts`)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Show 3 earlier steps" }));
    expect(screen.getByText("f0.ts")).toBeTruthy();
  });

  it("renders nothing without visible events", () => {
    const { container } = render(<ActivityTimeline events={[{ state: "plan_updated", items: [] }]} />);
    expect(container.innerHTML).toBe("");
  });
  it("names standing approvals so nothing runs unannounced", () => {
    render(
      <ActivityTimeline
        events={[
          { state: "standing_approval_granted", call_id: "c1", command: "npm test" },
          { state: "approved_by_standing", call_id: "c2", command: "npm test" },
        ]}
      />
    );
    expect(screen.getByText("Allowed for this run · npm test")).toBeTruthy();
    expect(screen.getByText("Ran under your approval for this run · npm test")).toBeTruthy();
  });
});
