// Sample data + local mock engine.
// There is NO backend in this build. Everything below is illustrative
// content rendered so the interface can be reviewed. The UI labels it.

export type RunState = "idle" | "working" | "verifying" | "done" | "blocked";
export type StepKind = "Command" | "File" | "Check" | "Review";

export interface EvidenceStep {
  id: string;
  name: string;
  kind: StepKind;
  detail: string;
  durationMs: number;
  ok: boolean;
}

// One turn in a session: the initial task, or a follow-up that keeps
// the session's context.
export interface Turn {
  id: string;
  request: string;
  kind: "task" | "follow-up";
  state: Exclude<RunState, "idle">;
  summary: string;
  startedAt: string;
  durationMs: number;
  steps: EvidenceStep[];
  blockedReason?: string;
}

export interface Session {
  id: string;
  receipt: string;
  title: string;
  engine: string; // truthful label for what produced the turns
  turns: Turn[];
  sample: true; // INV-01: marker flows to the UI
}

// Truthful engine label. No model, provider, or API is connected in
// this build, so turns are produced by the local preview engine.
export const ENGINE_LABEL = "Preview engine";

export const EXAMPLE_TASKS = [
  "Rename every `pexels` import to `stocksnap` across the repo",
  "Find why the onboarding emails stopped sending",
  "Add dark-mode screenshots to the release notes",
];

export const EXAMPLE_FOLLOW_UPS = [
  "Same thing, but keep the old names as deprecated aliases",
  "Also update the docs that mention it",
];

export const PAST_SESSIONS: Session[] = [
  {
    id: "run-1207",
    receipt: "F-1207",
    title: "Rename every `pexels` import to `stocksnap` across the repo",
    engine: ENGINE_LABEL,
    sample: true,
    turns: [
      {
        id: "t1",
        request: "Rename every `pexels` import to `stocksnap` across the repo",
        kind: "task",
        state: "done",
        summary:
          "Renamed the import in 14 files and updated the two snapshot tests that referenced it. The test suite passes.",
        startedAt: "Today, 09:41",
        durationMs: 48000,
        steps: [
          { id: "s1", name: "Scanned repository for `pexels`", kind: "Command", detail: "rg --count-matches pexels → 14 files", durationMs: 1200, ok: true },
          { id: "s2", name: "Rewrote imports", kind: "File", detail: "14 files changed, 31 import statements", durationMs: 9300, ok: true },
          { id: "s3", name: "Updated snapshot tests", kind: "File", detail: "2 test files touched", durationMs: 2100, ok: true },
          { id: "s4", name: "Ran test suite", kind: "Check", detail: "486 passed, 0 failed", durationMs: 31200, ok: true },
          { id: "s5", name: "Reviewed the diff", kind: "Review", detail: "No unrelated edits in the patch", durationMs: 4200, ok: true },
        ],
      },
    ],
  },
  {
    id: "run-1206",
    receipt: "F-1206",
    title: "Publish the billing webhook to production",
    engine: ENGINE_LABEL,
    sample: true,
    turns: [
      {
        id: "t1",
        request: "Publish the billing webhook to production",
        kind: "task",
        state: "blocked",
        summary:
          "Stopped before making changes. The webhook secret is not available in this workspace, so the endpoint cannot be verified.",
        blockedReason: "Needs the webhook secret. Add it to the vault and run again.",
        startedAt: "Yesterday, 18:02",
        durationMs: 9000,
        steps: [
          { id: "s1", name: "Read webhook handler", kind: "File", detail: "src/billing/webhook.ts", durationMs: 800, ok: true },
          { id: "s2", name: "Looked up webhook secret", kind: "Check", detail: "Not present in this workspace", durationMs: 1400, ok: false },
        ],
      },
    ],
  },
  {
    id: "run-1205",
    receipt: "F-1205",
    title: "Tidy the settings page spacing on small screens",
    engine: ENGINE_LABEL,
    sample: true,
    turns: [
      {
        id: "t1",
        request: "Tidy the settings page spacing on small screens",
        kind: "task",
        state: "done",
        summary:
          "Fixed the clipped toggle row at 360px and gave the save bar a safe-area inset. Checked at three widths.",
        startedAt: "Yesterday, 15:26",
        durationMs: 61000,
        steps: [
          { id: "s1", name: "Reproduced the clipping", kind: "Check", detail: "Screenshot at 360x800", durationMs: 3200, ok: true },
          { id: "s2", name: "Adjusted layout tokens", kind: "File", detail: "2 files changed", durationMs: 26800, ok: true },
          { id: "s3", name: "Verified at 3 widths", kind: "Check", detail: "360, 768, 1440 - no overlap", durationMs: 28400, ok: true },
        ],
      },
    ],
  },
];

const TASK_PLAN: Omit<EvidenceStep, "id" | "durationMs" | "ok">[] = [
  { name: "Read the relevant files", kind: "Command", detail: "Mapped the code this task touches" },
  { name: "Made the change", kind: "File", detail: "Edits kept to the smallest safe patch" },
  { name: "Ran the checks", kind: "Check", detail: "Tests and type checks for the touched area" },
  { name: "Reviewed the result", kind: "Review", detail: "Compared the outcome against the request" },
];

const FOLLOW_UP_PLAN: Omit<EvidenceStep, "id" | "durationMs" | "ok">[] = [
  { name: "Read the session so far", kind: "Command", detail: "Original task and earlier turns are still in context" },
  { name: "Applied the requested change", kind: "File", detail: "Built on the previous result instead of starting over" },
  { name: "Re-ran the checks", kind: "Check", detail: "Same verification as the first turn, plus the new case" },
  { name: "Reviewed the result", kind: "Review", detail: "Compared the outcome against the follow-up" },
];

// Local mock engine: walks a turn through truthful UI states with
// sample content. It simulates pacing only; it never claims real work.
export function startMockTurn(
  session: Session,
  request: string,
  kind: Turn["kind"],
  onUpdate: (session: Session) => void,
  reducedMotion: boolean
): () => void {
  const timers: ReturnType<typeof setTimeout>[] = [];
  const turn: Turn = {
    id: `t${session.turns.length + 1}`,
    request,
    kind,
    state: "working",
    summary: "",
    startedAt: "Just now",
    durationMs: 0,
    steps: [],
  };
  const push = () => onUpdate({ ...session, turns: [...session.turns] });
  session.turns = [...session.turns, turn];
  push();
  const plan = kind === "follow-up" ? FOLLOW_UP_PLAN : TASK_PLAN;
  const stepGap = reducedMotion ? 350 : 1100;
  plan.forEach((p, i) => {
    timers.push(
      setTimeout(() => {
        turn.steps = [...turn.steps, { ...p, id: `s${i + 1}`, durationMs: 900 + i * 700, ok: true }];
        push();
      }, stepGap * (i + 1))
    );
  });
  const doneAt = stepGap * (plan.length + 1);
  timers.push(
    setTimeout(() => {
      turn.state = "verifying";
      push();
    }, doneAt)
  );
  timers.push(
    setTimeout(() => {
      turn.state = "done";
      turn.durationMs = doneAt + 1500;
      turn.summary =
        kind === "follow-up"
          ? "Made the requested iteration with the earlier work still in context. Every change is listed in the evidence below, with the checks that back it."
          : "Finished the task as described. Every change is listed in the evidence below, with the checks that back it.";
      push();
    }, doneAt + (reducedMotion ? 400 : 1600))
  );
  return () => timers.forEach(clearTimeout);
}

export function newSession(task: string): Session {
  return {
    id: "run-live",
    receipt: "F-1208",
    title: task,
    engine: ENGINE_LABEL,
    turns: [],
    sample: true,
  };
}
