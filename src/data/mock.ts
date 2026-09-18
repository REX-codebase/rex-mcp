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

export interface Run {
  id: string;
  receipt: string;
  task: string;
  model: string;
  state: RunState;
  summary: string;
  startedAt: string;
  durationMs: number;
  steps: EvidenceStep[];
  blockedReason?: string;
  sample: true; // INV-01: marker flows to the UI
}

export const MODELS = [
  { id: "atlas-pro", label: "Atlas 1.5 Pro", available: true },
  { id: "atlas-flash", label: "Atlas 1.5 Flash", available: true },
  { id: "meridian", label: "Meridian 2", available: true },
  { id: "local", label: "Local model", available: false, reason: "No runtime connected" },
] as const;

export type ModelId = (typeof MODELS)[number]["id"];

export const EXAMPLE_TASKS = [
  "Rename every `pexels` import to `stocksnap` across the repo",
  "Find why the onboarding emails stopped sending",
  "Add dark-mode screenshots to the release notes",
];

export const PAST_RUNS: Run[] = [
  {
    id: "run-1207",
    receipt: "F-1207",
    task: "Rename every `pexels` import to `stocksnap` across the repo",
    model: "Atlas 1.5 Pro",
    state: "done",
    summary:
      "Renamed the import in 14 files and updated the two snapshot tests that referenced it. The test suite passes.",
    startedAt: "Today, 09:41",
    durationMs: 48000,
    sample: true,
    steps: [
      { id: "s1", name: "Scanned repository for `pexels`", kind: "Command", detail: "rg --count-matches pexels → 14 files", durationMs: 1200, ok: true },
      { id: "s2", name: "Rewrote imports", kind: "File", detail: "14 files changed, 31 import statements", durationMs: 9300, ok: true },
      { id: "s3", name: "Updated snapshot tests", kind: "File", detail: "2 test files touched", durationMs: 2100, ok: true },
      { id: "s4", name: "Ran test suite", kind: "Check", detail: "486 passed, 0 failed", durationMs: 31200, ok: true },
      { id: "s5", name: "Reviewed the diff", kind: "Review", detail: "No unrelated edits in the patch", durationMs: 4200, ok: true },
    ],
  },
  {
    id: "run-1206",
    receipt: "F-1206",
    task: "Publish the billing webhook to production",
    model: "Atlas 1.5 Pro",
    state: "blocked",
    summary:
      "Stopped before making changes. The webhook secret is not available in this workspace, so the endpoint cannot be verified.",
    blockedReason: "Needs the webhook secret. Add it to the vault and run again.",
    startedAt: "Yesterday, 18:02",
    durationMs: 9000,
    sample: true,
    steps: [
      { id: "s1", name: "Read webhook handler", kind: "File", detail: "src/billing/webhook.ts", durationMs: 800, ok: true },
      { id: "s2", name: "Looked up webhook secret", kind: "Check", detail: "Not present in this workspace", durationMs: 1400, ok: false },
    ],
  },
  {
    id: "run-1205",
    receipt: "F-1205",
    task: "Tidy the settings page spacing on small screens",
    model: "Meridian 2",
    state: "done",
    summary:
      "Fixed the clipped toggle row at 360px and gave the save bar a safe-area inset. Checked at three widths.",
    startedAt: "Yesterday, 15:26",
    durationMs: 61000,
    sample: true,
    steps: [
      { id: "s1", name: "Reproduced the clipping", kind: "Check", detail: "Screenshot at 360x800", durationMs: 3200, ok: true },
      { id: "s2", name: "Adjusted layout tokens", kind: "File", detail: "2 files changed", durationMs: 26800, ok: true },
      { id: "s3", name: "Verified at 3 widths", kind: "Check", detail: "360, 768, 1440 - no overlap", durationMs: 28400, ok: true },
    ],
  },
];

const PLAN: Omit<EvidenceStep, "id" | "durationMs" | "ok">[] = [
  { name: "Read the relevant files", kind: "Command", detail: "Mapped the code this task touches" },
  { name: "Made the change", kind: "File", detail: "Edits kept to the smallest safe patch" },
  { name: "Ran the checks", kind: "Check", detail: "Tests and type checks for the touched area" },
  { name: "Reviewed the result", kind: "Review", detail: "Compared the outcome against the request" },
];

// Local mock engine: walks a run through truthful UI states with
// sample content. It simulates pacing only; it never claims real work.
export function startMockRun(
  task: string,
  model: string,
  onUpdate: (run: Run) => void,
  reducedMotion: boolean
): () => void {
  const timers: ReturnType<typeof setTimeout>[] = [];
  const base: Run = {
    id: "run-live",
    receipt: "F-1208",
    task,
    model,
    state: "working",
    summary: "",
    startedAt: "Just now",
    durationMs: 0,
    steps: [],
    sample: true,
  };
  onUpdate({ ...base });
  const stepGap = reducedMotion ? 350 : 1100;
  PLAN.forEach((p, i) => {
    timers.push(
      setTimeout(() => {
        base.steps = [
          ...base.steps,
          { ...p, id: `s${i + 1}`, durationMs: 900 + i * 700, ok: true },
        ];
        onUpdate({ ...base });
      }, stepGap * (i + 1))
    );
  });
  const doneAt = stepGap * (PLAN.length + 1);
  timers.push(
    setTimeout(() => {
      base.state = "verifying";
      onUpdate({ ...base });
    }, doneAt)
  );
  timers.push(
    setTimeout(() => {
      base.state = "done";
      base.durationMs = doneAt + 1500;
      base.summary =
        "Finished the task as described. Every change is listed in the evidence below, with the checks that back it.";
      onUpdate({ ...base });
    }, doneAt + (reducedMotion ? 400 : 1600))
  );
  return () => timers.forEach(clearTimeout);
}
