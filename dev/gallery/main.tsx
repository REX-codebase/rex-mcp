import React from "react";
import ReactDOM from "react-dom/client";
import "../../src/index.css";
import "../../src/preview-runtime.css";
import { AgentRunView } from "../../src/components/AgentRunView";
import type { AgentSnapshot } from "../../src/data/agentTypes";

const rc = (o: Partial<Record<string, unknown>> = {}) => ({ started_at_ms: 0, duration_ms: 42, target: null, command: null, exit_code: null, bytes_read: 0, bytes_written: 0, output_truncated: false, diff: null, redactions: 0, ...o });
const fin = (id: string, tool: string, target: string | null, extra: Record<string, unknown> = {}, ok = true, output = "") => ({ state: "tool_finished" as const, result: { call_id: id, ok, tool, state: "executed" as const, output, error: ok ? null : { kind: "exit", detail: "exit 1" }, receipt: rc({ target, ...extra }) } });

const base: AgentSnapshot = {
  id: "run-7f3a", task: "Rename every `pexels` import to `stocksnap` across the repo", status: "running", terminal_reason: null,
  provider: "gemini", model: "gemini-2.5-pro",
  plan: [
    { id: "1", title: "Find every pexels import", status: "done" },
    { id: "2", title: "Rewrite imports and call sites", status: "in_progress" },
    { id: "3", title: "Run type check and tests", status: "pending" },
  ],
  step: 6, max_steps: 24, tool_calls: 7, max_tool_calls: 80, tokens_used: 18432, max_tokens: 200000, elapsed_ms: 41000, max_wall_ms: 900000,
  pending_approval: null, pending_question: null,
  events: [
    { state: "plan_updated", items: [] },
    { state: "model_text", text: "I'll search for pexels imports first, then rewrite them in place." },
    fin("c1", "search_files", "src", {}, true, "src/lib/photos.ts:3\nsrc/pages/Gallery.tsx:5\nsrc/pages/Hero.tsx:2"),
    fin("c2", "read_file", "src/lib/photos.ts", { bytes_read: 1840 }),
    fin("c3", "edit_file", "src/lib/photos.ts", { bytes_written: 1822, diff: "@@ -1,4 +1,4 @@\n-import { createClient } from \"pexels\";\n+import { createClient } from \"stocksnap\";\n const client = createClient(key);" }),
    fin("c4", "edit_file", "src/pages/Gallery.tsx", { bytes_written: 2410, diff: "@@ -5 +5 @@\n-import { Photo } from \"pexels\";\n+import { Photo } from \"stocksnap\";" }),
    fin("c5", "run_command", null, { command: ["npm", "run", "typecheck"], exit_code: 1, duration_ms: 5400 }, false, "src/pages/Hero.tsx(2,23): error TS2307: Cannot find module 'pexels'."),
    { state: "model_text", text: "Hero.tsx still imports pexels. Fixing it." },
  ],
  preview: null, completion_summary: null, error: null,
};

const approval: AgentSnapshot = { ...base, status: "awaiting_approval", pending_approval: { call_id: "c6", tool: "run_command", summary: "npm test -- --run", risk: "execute", approval_required: true, policy_reason: "Commands run with your user permissions." } };
const done: AgentSnapshot = { ...base, status: "completed", terminal_reason: { kind: "completed" }, plan: base.plan.map((p) => ({ ...p, status: "done" })), completion_summary: "Renamed 3 imports across 3 files. Type check and 41 tests pass.", events: [...base.events, fin("c6", "edit_file", "src/pages/Hero.tsx", { bytes_written: 990, diff: "@@ -2 +2 @@\n-import pexels from \"pexels\";\n+import pexels from \"stocksnap\";" }), fin("c7", "run_command", null, { command: ["npm", "test", "--", "--run"], exit_code: 0, duration_ms: 8100 }, true, "Tests 41 passed (41)"), { state: "gate_result", attempt: 1, passed: true, failures: [] }] };

const limit: AgentSnapshot = { ...base, step: 21, tool_calls: 72, tokens_used: 201000, elapsed_ms: 512000 };
const states: Record<string, AgentSnapshot> = { running: base, approval, done, limit };
const which = new URLSearchParams(location.search).get("s") ?? "running";
const noop = () => undefined;
ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <div className="harness-shell min-h-full"><main className="main-spine mx-auto w-full max-w-[820px] px-5 py-10 sm:px-8">
      <AgentRunView run={states[which]} deciding={false} cancelling={false} onDecide={noop} onCancel={noop} custody={{ grantId: "g-19ac", phase: "active", operator: "human" }} />
    </main></div>
  </React.StrictMode>,
);
