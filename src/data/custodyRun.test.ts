// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";

function sidecarFetch(routes: Record<string, unknown>) {
  return vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    if (url === "http://127.0.0.1:8787/api/status") {
      return new Response(JSON.stringify({ kind: "sidecar" }), { status: 200 });
    }
    for (const [route, body] of Object.entries(routes)) {
      if (url === `http://127.0.0.1:8787${route}`) {
        return new Response(JSON.stringify(body), { status: 200 });
      }
    }
    return new Response("not found", { status: 404 });
  });
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.resetModules();
});

describe("custody run bridge", () => {
  it("begins a custodied run through the sidecar and returns the view", async () => {
    const fetchMock = sidecarFetch({
      "/api/agent/custody/runs": {
        grant_id: "custody-1",
        task_id: "task-ui-1",
        operator: "human",
        worker: "managed_model:gemini",
        phase: "active",
        snapshot: { id: "agent-1", status: "planning" },
      },
    });
    vi.stubGlobal("fetch", fetchMock);
    const { custodyBegin } = await import("./custodyRun");
    const view = await custodyBegin("build a page");
    expect(view.grant_id).toBe("custody-1");
    expect(view.operator).toBe("human");
    expect(view.phase).toBe("active");
    const call = fetchMock.mock.calls.find(([u]) => String(u).endsWith("/api/agent/custody/runs"));
    expect(call).toBeTruthy();
    const body = JSON.parse(String(call?.[1]?.body)) as Record<string, unknown>;
    expect(body.task).toBe("build a page");
    expect(body.provider).toBe("gemini");
    expect(body.summarize_history).toBe(false);
    expect(body.summarizeHistory).toBe(false);
  });

  it("sends the history-summary opt-in only when asked", async () => {
    const fetchMock = sidecarFetch({
      "/api/agent/custody/runs": { grant_id: "custody-2", snapshot: { id: "agent-2", status: "planning" } },
    });
    vi.stubGlobal("fetch", fetchMock);
    const { custodyBegin } = await import("./custodyRun");
    await custodyBegin("long task", false, true);
    const call = fetchMock.mock.calls.find(([u]) => String(u).endsWith("/api/agent/custody/runs"));
    const body = JSON.parse(String(call?.[1]?.body)) as Record<string, unknown>;
    expect(body.summarize_history).toBe(true);
    expect(body.summarizeHistory).toBe(true);
    expect(body.plan_mode).toBe(false);
  });

  it("surfaces backend refusals as errors, never as fake success", async () => {
    vi.stubGlobal("fetch", sidecarFetch({
      "/api/agent/custody/runs": { error: "custody offer refused: task already custodied" },
    }));
    const { custodyBegin } = await import("./custodyRun");
    await expect(custodyBegin("again")).rejects.toThrow("task already custodied");
  });

  it("sends the human stop with the grant id", async () => {
    const fetchMock = sidecarFetch({
      "/api/agent/custody/runs/agent-1/stop": { ok: true, reason: { kind: "human_stop" } },
    });
    vi.stubGlobal("fetch", fetchMock);
    const { custodyStop } = await import("./custodyRun");
    const res = await custodyStop("custody-1", "agent-1");
    expect(res.ok).toBe(true);
    const call = fetchMock.mock.calls.find(([u]) => String(u).endsWith("/stop"));
    const body = JSON.parse(String(call?.[1]?.body)) as Record<string, unknown>;
    expect(body.grant_id).toBe("custody-1");
  });

  it("fails honestly when no backend exists", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => new Response("nope", { status: 404 })));
    const { custodyBegin } = await import("./custodyRun");
    await expect(custodyBegin("offline")).rejects.toThrow("no backend");
  });
});
