// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";

function sidecarFetch(handler: (url: string, init?: RequestInit) => Response | undefined) {
  return vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    if (url === "http://127.0.0.1:8787/api/status") {
      return new Response(JSON.stringify({ kind: "sidecar" }), { status: 200 });
    }
    return handler(url, init) ?? new Response("not found", { status: 404 });
  });
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.resetModules();
});

describe("rex task supervision bridge", () => {
  it("lists durable tasks", async () => {
    vi.stubGlobal("fetch", sidecarFetch((url) =>
      url === "http://127.0.0.1:8787/api/rex/tasks"
        ? new Response(JSON.stringify({ tasks: [{ task_id: "task-1", task: "demo", state: "active", host: "claude_code", operator_is_agent: true }] }), { status: 200 })
        : undefined
    ));
    const { rexTaskList } = await import("./rexTasks");
    const r = await rexTaskList();
    expect(r.tasks[0].task_id).toBe("task-1");
    expect(r.tasks[0].operator_is_agent).toBe(true);
  });

  it("opens an agent-mode task through rex-mcp", async () => {
    const fetchMock = sidecarFetch((url, init) =>
      url === "http://127.0.0.1:8787/api/rex/tasks" && init?.method === "POST"
        ? new Response(JSON.stringify({ task_id: "task-9", state: "active" }), { status: 200 })
        : undefined
    );
    vi.stubGlobal("fetch", fetchMock);
    const { rexTaskBegin } = await import("./rexTasks");
    const r = await rexTaskBegin("supervise this");
    expect(r.task_id).toBe("task-9");
    const call = fetchMock.mock.calls.find(([u]) => String(u) === "http://127.0.0.1:8787/api/rex/tasks");
    expect(JSON.parse(String(call?.[1]?.body)).task).toBe("supervise this");
  });

  it("reads events with the cursor and stops with the human fence", async () => {
    const fetchMock = sidecarFetch((url, init) => {
      if (url === "http://127.0.0.1:8787/api/rex/tasks/task-9/events?since=3") {
        return new Response(JSON.stringify({ events: [{ seq: 4, ts_ms: 1, kind: "task_cancelled", detail: {} }], last_seq: 4 }), { status: 200 });
      }
      if (url === "http://127.0.0.1:8787/api/rex/tasks/task-9/stop" && init?.method === "POST") {
        return new Response(JSON.stringify({ task_id: "task-9", state: "cancelled", final_reason: "human stop from REX UI" }), { status: 200 });
      }
      return undefined;
    });
    vi.stubGlobal("fetch", fetchMock);
    const { rexTaskEvents, rexTaskStop } = await import("./rexTasks");
    const events = await rexTaskEvents("task-9", 3);
    expect(events.last_seq).toBe(4);
    const stopped = await rexTaskStop("task-9");
    expect(stopped.state).toBe("cancelled");
    expect(stopped.final_reason).toContain("human stop");
  });

  it("passes backend errors through truthfully", async () => {
    vi.stubGlobal("fetch", sidecarFetch((url) =>
      url.endsWith("/status")
        ? new Response(JSON.stringify({ error: "REX_STATE: terminal task" }), { status: 200 })
        : undefined
    ));
    const { rexTaskStatus } = await import("./rexTasks");
    await expect(rexTaskStatus("task-dead")).rejects.toThrow("terminal task");
  });
});
