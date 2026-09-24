import { useEffect, useRef, useState } from "react";
import { BackendBadge, TopBar } from "./components/TopBar";
import { Composer } from "./components/Composer";
import { StateRail } from "./components/StateRail";
import { DesktopOnly, isDesktopRuntime } from "./components/DesktopOnly";
import { SessionView } from "./components/SessionView";
import { HistoryList } from "./components/HistoryList";
import { SettingsView } from "./components/SettingsView";
import {
  MOTION_KEY,
  loadMotionPref,
  resolveReduced,
  useSystemReducedMotion,
  type MotionPref,
} from "./data/motion";
import { BrowserView } from "./components/BrowserView";
import { PreviewRuntimeView } from "./components/PreviewRuntimeView";
import { NativePreviewView } from "./components/NativePreviewView";
import { ToolApprovalPreview } from "./components/ToolApprovalPreview";
import "./preview-runtime.css";
import { PAST_SESSIONS, newSession, startMockTurn, type Session } from "./data/mock";
import { AgentRunView } from "./components/AgentRunView";
import { UltraRunView } from "./components/UltraRunView";
import { ultraBegin, ultraSnapshot, ultraDecide, ultraCancel, type UltraSnapshot } from "./data/ultraRun";
import { liveRunAvailable } from "./data/liveRun";
import { custodyBegin, custodyDecidePlan, custodyStop } from "./data/custodyRun";
import { rexTaskBegin } from "./data/rexTasks";
import { FableCountdown } from "./components/FableCountdown";
import { loadFableSessionName, saveFableSessionName, fableCreateSession } from "./data/fable";
import { TerminalView } from "./components/TerminalView";
import { WorkspaceEditor } from "./components/WorkspaceEditor";
import { GitPanel } from "./components/GitPanel";
import { CheckpointPanel } from "./components/CheckpointPanel";
import { RexTaskList, RexTaskView } from "./components/RexTaskView";
import {
  agentCancel,
  agentDecide,
  agentSnapshot,
  agentTeardown,
  type AgentSnapshot,
} from "./data/agentRun";
import { terminalActive } from "./data/agentTypes";
import { InstalledAgentRunView } from "./components/InstalledAgentRunView";
import {
  installedAgentBegin,
  installedAgentCancel,
  installedAgentDecide,
  installedAgentSnapshot,
  installedRunTerminal,
  type InstalledRunSnapshot,
} from "./data/installedAgentRun";
import { loadInstalledAgentOptions } from "./components/ModelStatus";
import { OperatorModeChip, OperatorModeGate } from "./components/OperatorModeGate";
import { loadOperatorMode, saveOperatorMode, type OperatorMode } from "./data/operatorMode";
import { SetupChecklist } from "./components/SetupChecklist";
import type { InstalledAgentId } from "./data/backend";
import { resolveExecutionPath, type ExecutionPath } from "./data/executionPath";

const TOUR_KEY = "rex-tour-mode";

// Reads the installed-agent selection from settings. When an installed agent
// is selected it becomes the explicit execution path (see executionPath.ts);
// otherwise the custody loop is the default. Read at render for the path
// label and re-read inside onRun so the click always sees the latest choice.
export function readInstalledAgentSelection(): { id: InstalledAgentId; label: string } | null {
  try {
    const selected = JSON.parse(window.localStorage.getItem("rex-model-selection") || "null") as { provider?: string; id?: string; label?: string } | null;
    if (selected?.provider?.startsWith("installed:") && selected.id) {
      return { id: selected.id as InstalledAgentId, label: selected.label ?? selected.id };
    }
  } catch { /* no selection */ }
  return null;
}

// The hero starter composer exists only while there is no session. The moment
// the first run starts it settles - a brief blur + downward travel while its
// wrapper collapses - and unmounts, leaving exactly one composer at the
// bottom of the session. Reduced motion swaps it instantly.
export default function App() {
  const [operatorMode, setOperatorMode] = useState<OperatorMode | null>(() =>
    loadOperatorMode((k) => window.localStorage.getItem(k))
  );
  const chooseOperatorMode = (mode: OperatorMode) => {
    try { saveOperatorMode(mode); } catch { /* storage unavailable */ }
    setOperatorMode(mode);
  };
  const [view, setView] = useState<"task" | "settings" | "terminal" | "editor" | "git">("task");
  const [browserPhase, setBrowserPhase] = useState<"closed" | "active" | "collapsed">("closed");
  const [browserRun, setBrowserRun] = useState(0);
  const [ultra, setUltra] = useState(false);
  const [ultraRun, setUltraRun] = useState<UltraSnapshot | null>(null);
  const ultraPoll = useRef<number | null>(null);
  const [ultraPulse, setUltraPulse] = useState(0);
  const [fast, setFast] = useState(false);
  const [fastPulse, setFastPulse] = useState(0);
  const [fastPhase, setFastPhase] = useState<"idle" | "engaging" | "disengaging">("idle");
  const fastTimer = useRef<number | null>(null);
  const [task, setTask] = useState("");
  const [session, setSession] = useState<Session | null>(null);
  // Active Fable gate session, if the user started one. The countdown is
  // visible here; the Composer toggle creates sessions.
  const [fableSession, setFableSession] = useState<string | null>(() => loadFableSessionName());
  // Fable gate toggle: when on, runs create a native fable session first.
  const [fableGate, setFableGate] = useState<boolean>(() => {
    try {
      return window.localStorage.getItem("rex-fable-gate") === "1";
    } catch {
      return false;
    }
  });
  const toggleFableGate = (v: boolean) => {
    try {
      window.localStorage.setItem("rex-fable-gate", v ? "1" : "0");
    } catch {
      /* storage unavailable */
    }
    setFableGate(v);
    if (!v) {
      // Turning the gate off clears the active session from the UI.
      // The session persists on disk; the countdown simply stops showing.
      saveFableSessionName(null);
      setFableSession(null);
    }
  };
  const [liveCapable, setLiveCapable] = useState(false);
  const [backendProbed, setBackendProbed] = useState(false);
  const [tourMode, setTourMode] = useState(() => {
    try {
      return window.localStorage.getItem(TOUR_KEY) === "1";
    } catch {
      return false;
    }
  });
  const [checklistHot, setChecklistHot] = useState(false);
  const checklistTimer = useRef<number | null>(null);
  const [liveRun, setLiveRun] = useState<AgentSnapshot | null>(null);
  const [custody, setCustody] = useState<{ grantId: string; phase: string; operator: string } | null>(null);
  const [rexTaskId, setRexTaskId] = useState<string | null>(null);
  const [liveStarting, setLiveStarting] = useState(false);
  const [liveDeciding, setLiveDeciding] = useState(false);
  const [liveCancelling, setLiveCancelling] = useState(false);
  // Plan mode: the run proposes a plan and parks in awaiting_plan until the
  // user approves it. Reset on each new run start.
  const [planMode, setPlanMode] = useState(false);
  // Opt-in per run: each history summary is an extra model call.
  const [summarizeHistory, setSummarizeHistory] = useState(false);
  const [planDeciding, setPlanDeciding] = useState(false);
  const livePoll = useRef<number | null>(null);
  const [installedRun, setInstalledRun] = useState<InstalledRunSnapshot | null>(null);
  const [installedStarting, setInstalledStarting] = useState(false);
  const [installedDeciding, setInstalledDeciding] = useState(false);
  const [installedCancelling, setInstalledCancelling] = useState(false);
  const installedPoll = useRef<number | null>(null);
  const [nativePreviewDir, setNativePreviewDir] = useState<string | null>(null);
  useEffect(() => {
    let active = true;
    liveRunAvailable()
      .then((ok) => {
        if (active) {
          setLiveCapable(ok);
          setBackendProbed(true);
        }
      })
      .catch(() => {
        if (active) setBackendProbed(true);
      });
    return () => { active = false; };
  }, []);
  // Re-probe on demand (the setup checklist's "check again" button): the
  // user may have started the sidecar after the first probe.
  const reprobeBackend = () => {
    liveRunAvailable()
      .then((ok) => {
        setLiveCapable(ok);
        setBackendProbed(true);
      })
      .catch(() => undefined);
  };
  const chooseTourMode = (on: boolean) => {
    setTourMode(on);
    try {
      if (on) window.localStorage.setItem(TOUR_KEY, "1");
      else window.localStorage.removeItem(TOUR_KEY);
    } catch {
      /* storage unavailable; tour mode just won't persist */
    }
  };
  // Deep link: #run=<id> reattaches to an existing agent loop run (e.g.
  // after a reload). Read-only viewers and the original driver share the
  // same snapshot stream.
  useEffect(() => {
    const m = window.location.hash.match(/^#run=(.+)$/);
    if (!m) return;
    const id = m[1];
    agentSnapshot(id)
      .then((snap) => {
        setLiveRun(snap);
        if (!terminalActive(snap)) startPolling(id);
      })
      .catch(() => undefined);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  const [hero, setHero] = useState<"shown" | "settling" | "gone">("shown");
  const cancel = useRef<(() => void) | null>(null);
  const settleTimer = useRef<number | null>(null);
  const systemReduce = useSystemReducedMotion();
  const [motion, setMotion] = useState<MotionPref>(() =>
    loadMotionPref((key) => window.localStorage.getItem(key))
  );
  const reduced = resolveReduced(motion, systemReduce);
  useEffect(() => {
    try {
      window.localStorage.setItem(MOTION_KEY, motion);
    } catch {
      /* storage unavailable; preference just won't persist */
    }
  }, [motion]);
  // Ultra owns the browser as a task tool, never as a destination. Turning
  // Ultra off removes the tool without changing the Standard task surface.
  useEffect(() => {
    if (!ultra) setBrowserPhase("closed");
  }, [ultra]);
  useEffect(
    () => () => {
      cancel.current?.();
      if (settleTimer.current !== null) window.clearTimeout(settleTimer.current);
      if (fastTimer.current !== null) window.clearTimeout(fastTimer.current);
      if (livePoll.current !== null) window.clearInterval(livePoll.current);
      if (installedPoll.current !== null) window.clearInterval(installedPoll.current);
    },
    []
  );

  const startInstalledPolling = (id: string) => {
    if (installedPoll.current !== null) window.clearInterval(installedPoll.current);
    installedPoll.current = window.setInterval(() => {
      installedAgentSnapshot(id)
        .then((snap) => {
          setInstalledRun(snap);
          if (snap.status !== "running" && installedPoll.current !== null) {
            window.clearInterval(installedPoll.current);
            installedPoll.current = null;
          }
        })
        .catch(() => undefined);
    }, 900);
  };

  // Poll the Rust loop while it is non-terminal; the snapshot is the whole
  // truth, so rendering never depends on event timing.
  const startUltraPolling = (id: string) => {
    if (ultraPoll.current !== null) window.clearInterval(ultraPoll.current);
    ultraPoll.current = window.setInterval(() => {
      ultraSnapshot(id)
        .then((snap) => {
          setUltraRun(snap);
          if (snap.terminal && ultraPoll.current !== null) {
            window.clearInterval(ultraPoll.current);
            ultraPoll.current = null;
          }
        })
        .catch(() => undefined);
    }, 900);
  };

  const startPolling = (id: string) => {
    if (livePoll.current !== null) window.clearInterval(livePoll.current);
    livePoll.current = window.setInterval(() => {
      agentSnapshot(id)
        .then((snap) => {
          setLiveRun(snap);
          if (terminalActive(snap) && livePoll.current !== null) {
            window.clearInterval(livePoll.current);
            livePoll.current = null;
          }
        })
        .catch(() => undefined);
    }, 900);
  };

  const latest = session?.turns[session.turns.length - 1];
  const state = latest?.state ?? "idle";
  const busy = state === "working" || state === "verifying";

  const beginSettle = () => {
    setHero((prev) => (prev === "gone" ? prev : "settling"));
    if (settleTimer.current !== null) window.clearTimeout(settleTimer.current);
    settleTimer.current = window.setTimeout(() => setHero("gone"), reduced ? 40 : 520);
  };

  const toggleFast = () => {
    const next = !fast;
    if (fastTimer.current !== null) window.clearTimeout(fastTimer.current);
    setFastPhase(reduced ? "idle" : next ? "engaging" : "disengaging");
    setFast(next);
    setFastPulse((v) => v + 1);
    if (!reduced) fastTimer.current = window.setTimeout(() => setFastPhase("idle"), 1120);
  };

  const nativeProject = nativePreviewDir ?? new URLSearchParams(window.location.search).get("native-preview");
  const previewTask = /(?:html|react|next\.?js|vite|astro|svelte|website|ui|interface|dashboard|landing page|app)/i.test(task);

  // One default execution path: the custody loop. Every alternative is an
  // explicit opt-in resolved here, so the Composer label and onRun below can
  // never disagree about what the Run button will do.
  const installedSelection = readInstalledAgentSelection();
  const executionPath: ExecutionPath = resolveExecutionPath({
    operatorMode,
    ultra,
    installedAgentId: installedSelection?.id ?? null,
    installedAgentLabel: installedSelection?.label ?? null,
    liveCapable,
    tourMode,
  });

  const onRun = async () => {
    cancel.current?.();
    const label = task.trim();
    if (!label) return;
    // Re-resolve at click time: the settings selection may have changed
    // since the last render.
    const selection = readInstalledAgentSelection();
    const path = resolveExecutionPath({
      operatorMode,
      ultra,
      installedAgentId: selection?.id ?? null,
      installedAgentLabel: selection?.label ?? null,
      liveCapable,
      tourMode,
    });
    if (path.kind === "unavailable") {
      // No backend and no explicit tour: never silently animate a fake run.
      // Pulse the setup checklist instead — it is already rendered above.
      setChecklistHot(true);
      if (checklistTimer.current) window.clearTimeout(checklistTimer.current);
      checklistTimer.current = window.setTimeout(() => setChecklistHot(false), 1600);
      beginSettle();
      return;
    }
    if (path.kind === "tour") {
      const s = newSession(label);
      cancel.current = startMockTurn(s, label, "task", setSession, reduced);
      if (ultra) {
        setBrowserRun((v) => v + 1);
        setBrowserPhase("active");
      }
      beginSettle();
      return;
    }
    // Fable gate: when enabled, open a native fable session for this task
    // before the run starts. The session's authority timer and unlock rule
    // are enforced in Rust; the countdown appears above the run. If the
    // gate fails to open, the run does not start ungated.
    if (fableGate && liveCapable) {
      const sessionName = `fable-${Date.now().toString(36)}`;
      try {
        await fableCreateSession(sessionName, label);
        saveFableSessionName(sessionName);
        setFableSession(sessionName);
      } catch (e) {
        console.error("Fable gate failed to open:", e);
        setChecklistHot(true);
        if (checklistTimer.current) window.clearTimeout(checklistTimer.current);
        checklistTimer.current = window.setTimeout(() => setChecklistHot(false), 1600);
        beginSettle();
        return;
      }
    }
    if (path.kind === "installed") {
      // Settings integration: the vendor CLI runs the task on an isolated
      // workspace while REX streams its events, applies the completion gate,
      // and holds any source changes for reviewed promotion.
      const installedBackend = selection?.id ?? installedSelection?.id;
      if (!installedBackend) return;
      const options = loadInstalledAgentOptions();
      setInstalledRun(null);
      setInstalledStarting(true);
      beginSettle();
      installedAgentBegin(installedBackend, label, options.workspace.trim())
        .then((snap) => {
          setInstalledRun(snap);
          if (snap.status === "running") startInstalledPolling(snap.id);
        })
        .catch((e) => setInstalledRun({
          id: "run-failed",
          backend: installedBackend,
          status: "failed",
          prompt: label,
          workspace: "",
          staging_workspace: "",
          preview_dir: "",
          model: null,
          effort: null,
          created_at_ms: Date.now(),
          updated_at_ms: Date.now(),
          exit_code: null,
          events: [],
          stderr_tail: "",
          diff: null,
          promotion: "not_required",
          completion: null,
          error: String(e),
        }))
        .finally(() => setInstalledStarting(false));
      return;
    }
    if (path.kind === "ultra") {
      // Explicit verified-pipeline mode: contract -> build -> verify ->
      // adversary -> judge. A separate backend, chosen deliberately via the
      // Ultra toggle — not a silent branch.
      setUltraRun(null);
      setLiveRun(null);
      setLiveStarting(true);
      beginSettle();
      ultraBegin(label)
        .then((snap) => {
          setUltraRun(snap);
          startUltraPolling(snap.id);
        })
        .catch((e) => {
          setUltraRun(null);
          setLiveRun({
            id: "run-failed",
            task: label,
            status: "failed",
            terminal_reason: { kind: "provider_error", detail: String(e) },
            provider: "gemini",
            model: "",
            plan: [],
            step: 0,
            max_steps: 0,
            tool_calls: 0,
            max_tool_calls: 0,
            tokens_used: 0,
            max_tokens: 0,
            elapsed_ms: 0,
            max_wall_ms: 0,
            pending_approval: null,
            events: [],
            preview: null,
            completion_summary: null,
            error: String(e),
          });
        })
        .finally(() => setLiveStarting(false));
      return;
    }
    if (path.kind === "delegate") {
      // Explicit "delegate to external host": opens a durable REX task an
      // external host drives through rex-mcp; this UI supervises with the
      // permanent human Stop.
      setLiveRun(null);
      setCustody(null);
      setLiveStarting(true);
      beginSettle();
      rexTaskBegin(label, ultra)
        .then((res) => {
          const id = res?.task_id;
          if (id) {
            setRexTaskId(id);
          } else {
            throw new Error("rex-mcp returned no task id");
          }
        })
        .catch((e) =>
          setLiveRun({
            id: "run-failed",
            task: label,
            status: "failed",
            terminal_reason: { kind: "provider_error", detail: String(e) },
            provider: "gemini",
            model: "",
            plan: [],
            step: 0,
            max_steps: 0,
            tool_calls: 0,
            max_tool_calls: 0,
            tokens_used: 0,
            max_tokens: 0,
            elapsed_ms: 0,
            max_wall_ms: 0,
            pending_approval: null,
            events: [],
            preview: null,
            completion_summary: null,
            error: String(e),
          })
        )
        .finally(() => setLiveStarting(false));
      return;
    }
    // Default: the custody autonomous loop plans, acts through trusted
    // approvals, verifies its own work against gates, and stops truthfully.
    {
      // Capture the plan-mode choice for this run, then reset the checkbox so
      // the next run starts in the default execution mode unless the user
      // opts in again.
      const mode = planMode;
      const summarize = summarizeHistory;
      setPlanMode(false);
      setSummarizeHistory(false);
      custodyBegin(label, mode, summarize)
        .then((view) => {
          setCustody({ grantId: view.grant_id, phase: view.phase, operator: view.operator });
          setLiveRun(view.snapshot);
          startPolling(view.snapshot.id);
        })
        .catch((e) =>
          setLiveRun({
            id: "run-failed",
            task: label,
            status: "failed",
            terminal_reason: { kind: "provider_error", detail: String(e) },
            provider: "gemini",
            model: "",
            plan: [],
            step: 0,
            max_steps: 0,
            tool_calls: 0,
            max_tool_calls: 0,
            tokens_used: 0,
            max_tokens: 0,
            elapsed_ms: 0,
            max_wall_ms: 0,
            pending_approval: null,
            events: [],
            preview: null,
            completion_summary: null,
            error: String(e),
          })
        )
        .finally(() => setLiveStarting(false));
    }
  };

  const onLiveDecision = (approved: boolean) => {
    if (ultraRun && !ultraRun.terminal) {
      if (liveDeciding) return;
      setLiveDeciding(true);
      ultraDecide(ultraRun.id, approved)
        .then((snap) => setUltraRun(snap))
        .catch(() => undefined)
        .finally(() => setLiveDeciding(false));
      return;
    }
    if (!liveRun || liveDeciding) return;
    setLiveDeciding(true);
    agentDecide(liveRun.id, approved)
      .then((snap) => setLiveRun(snap))
      .catch((e) => setLiveRun((prev) => (prev ? { ...prev, error: String(e) } : prev)))
      .finally(() => setLiveDeciding(false));
  };

  // Plan decision for a plan-gated custody run: approval resumes the run
  // into the normal loop; rejection ends it as denied. The decision travels
  // through the custody channel, which shares the same run registry, so the
  // gate's trusted-UI checks still apply.
  const onPlanDecision = (approved: boolean) => {
    if (!liveRun || planDeciding) return;
    setPlanDeciding(true);
    custodyDecidePlan(liveRun.id, approved)
      .then((snap) => setLiveRun(snap))
      .catch((e) => setLiveRun((prev) => (prev ? { ...prev, error: String(e) } : prev)))
      .finally(() => setPlanDeciding(false));
  };

  const onLiveCancel = () => {
    if (ultraRun && !ultraRun.terminal) {
      if (liveCancelling) return;
      setLiveCancelling(true);
      ultraCancel(ultraRun.id)
        .then((snap) => setUltraRun(snap))
        .catch(() => undefined)
        .finally(() => setLiveCancelling(false));
      return;
    }
    if (!liveRun || liveCancelling) return;
    if (custody) {
      // Custodied run: the Stop lands in custody first (terminal fence),
      // then the run loop is cancelled. Late success cannot win.
      setLiveCancelling(true);
      custodyStop(custody.grantId, liveRun.id)
        .then(() => agentSnapshot(liveRun.id))
        .then((snap) => {
          setLiveRun(snap);
          setCustody((prev) => (prev ? { ...prev, phase: "released" } : prev));
        })
        .catch(() => undefined)
        .finally(() => setLiveCancelling(false));
      return;
    }
    setLiveCancelling(true);
    agentCancel(liveRun.id)
      .then((snap) => setLiveRun(snap))
      .catch(() => undefined)
      .finally(() => setLiveCancelling(false));
  };

  const onInstalledDecision = (approved: boolean) => {
    if (!installedRun || installedDeciding) return;
    setInstalledDeciding(true);
    installedAgentDecide(installedRun.id, approved)
      .then((snap) => setInstalledRun(snap))
      .catch((e) => setInstalledRun((prev) => (prev ? { ...prev, error: String(e) } : prev)))
      .finally(() => setInstalledDeciding(false));
  };

  const onInstalledCancel = () => {
    if (!installedRun || installedCancelling) return;
    setInstalledCancelling(true);
    installedAgentCancel(installedRun.id)
      .then((snap) => setInstalledRun(snap))
      .catch(() => undefined)
      .finally(() => setInstalledCancelling(false));
  };

  const onFollowUp = (request: string) => {
    if (!session || busy) return;
    cancel.current?.();
    cancel.current = startMockTurn(session, request, "follow-up", setSession, reduced);
  };

  const onNewTask = () => {
    cancel.current?.();
    if (settleTimer.current !== null) window.clearTimeout(settleTimer.current);
    if (livePoll.current !== null) {
      window.clearInterval(livePoll.current);
      livePoll.current = null;
    }
    if (installedPoll.current !== null) {
      window.clearInterval(installedPoll.current);
      installedPoll.current = null;
    }
    if (liveRun && !liveRun.id.startsWith("run-failed")) agentTeardown(liveRun.id).catch(() => undefined);
    setInstalledRun(null);
    setInstalledStarting(false);
    setInstalledDeciding(false);
    setNativePreviewDir(null);
    setLiveRun(null);
    setLiveStarting(false);
    setLiveDeciding(false);
    setSession(null);
    setTask("");
    setHero("shown");
    setBrowserPhase("closed");
  };

  const desktop = isDesktopRuntime();

  if (operatorMode === null) {
    return (
      <div className="harness-shell min-h-full">
        <OperatorModeGate onChoose={chooseOperatorMode} />
      </div>
    );
  }

  return (
    <div className={`harness-shell min-h-full motion-${motion} ${ultra ? "ultra-on" : ""} ${fast ? "fast-on" : ""} ${fastPhase !== "idle" ? `fast-${fastPhase}` : ""} ${reduced ? "reduced-fx" : ""}`} data-motion={motion} data-ultra={ultra ? "on" : "off"}>
      <div className="ultra-transition" key={ultraPulse} aria-hidden="true"><span /><span /><span /><span /></div>
      {fastPhase !== "idle" && (
        <div className="fast-transition" key={fastPulse} aria-hidden="true">
          <div className="fast-iris"><i /><i /><i /></div>
          <div className="fast-rails"><i /><i /><i /><i /><i /><i /></div>
          <div className="fast-word"><span>FAST</span><small>LATENCY PROFILE / VISUAL PREVIEW</small></div>
          <div className="fast-cut fast-cut-a" /><div className="fast-cut fast-cut-b" />
        </div>
      )}
      <div className="ultra-atmosphere" aria-hidden="true"><span className="ultra-horizon" /><span className="ultra-scan" /></div>
      <TopBar view={view} onView={setView} ultra={ultra} onUltra={() => { setUltra((v) => !v); setUltraPulse((v) => v + 1); }} fast={fast} onFast={toggleFast} />
      <div className="ultra-status" role="status" aria-live="polite"><span>ULTRA</span><b>{ultra ? "Verification tier engaged" : "Verification tier offline"}</b><small>{ultra ? "Contract, adversary and clean-room judge active" : "No extra capabilities are active"}</small></div>
      <div className="mx-auto w-full max-w-[820px] px-5 pt-3 sm:px-8">
        <div className="status-row">
          <OperatorModeChip mode={operatorMode} onSwitch={chooseOperatorMode} />
          <BackendBadge live={liveCapable} />
        </div>
        {fableSession && <FableCountdown sessionName={fableSession} />}
      </div>
      <div className="fast-status" role="status" aria-live="polite"><span>FAST</span><b>{fast ? "TEMPO PROFILE ARMED" : "Fast preview off"}</b><small>Visual only · execution speed unchanged</small><i aria-hidden="true" /></div>
      <main className="main-spine mx-auto w-full max-w-[820px] px-5 pb-12 sm:px-8">
        {view === "terminal" ? (
          desktop ? <TerminalView /> : <DesktopOnly feature="terminal" />
        ) : view === "editor" ? (
          desktop ? <WorkspaceEditor /> : <DesktopOnly feature="editor" />
        ) : view === "git" ? (
          desktop ? (
            <>
              <GitPanel />
              <CheckpointPanel />
            </>
          ) : (
            <DesktopOnly feature="git" />
          )
        ) : view === "settings" ? (
          <SettingsView
            motion={motion}
            setMotion={setMotion}
            onReset={() => {
              try {
                window.localStorage.removeItem(MOTION_KEY);
              } catch {
                /* ignore */
              }
              setMotion("system");
            }}
          />
        ) : (
          <>
            {backendProbed && !liveCapable && (
              <SetupChecklist
                hot={checklistHot}
                tour={tourMode}
                onTour={chooseTourMode}
                onOpenSettings={() => setView("settings")}
                onRecheck={reprobeBackend}
              />
            )}
            {hero !== "gone" && (
              <div className={`hero-wrap ${hero === "settling" ? "settling" : ""}`}>
                <div>
                  <div
                    className="hero-settle-inner mb-9 mt-3 sm:mb-12 sm:mt-7"
                    aria-hidden={hero === "settling"}
                  >
                    <Composer
                      task={task}
                      setTask={setTask}
                      state={state}
                      onRun={onRun}
                      planMode={planMode}
                      setPlanMode={setPlanMode}
                      summarizeHistory={summarizeHistory}
                      setSummarizeHistory={setSummarizeHistory}
                      fableGate={fableGate}
                      setFableGate={toggleFableGate}
                      executionPath={executionPath}
                    />
                  </div>
                </div>
              </div>
            )}
            <StateRail state={state} blockedReason={latest?.blockedReason} />
            {import.meta.env.DEV && new URLSearchParams(window.location.search).has("approval-preview") && <ToolApprovalPreview />}
            {(installedRun || installedStarting) && installedRun && (
              <InstalledAgentRunView
                run={installedRun}
                deciding={installedDeciding}
                cancelling={installedCancelling}
                onDecide={onInstalledDecision}
                onCancel={onInstalledCancel}
                onOpenPreview={(dir) => setNativePreviewDir(dir)}
              />
            )}
            {(installedRun || installedStarting) && !installedRun && (
              <section className="live-run" aria-label="Starting the installed agent">
                <div className="live-status" role="status">
                  <span className="live-dot" aria-hidden="true" />
                  <span className="eyebrow">Installed agent</span>
                  <span className="live-status-text">Starting the vendor CLI…</span>
                </div>
              </section>
            )}
            {(ultraRun || liveStarting) && ultraRun && (
              <UltraRunView
                run={ultraRun}
                deciding={liveDeciding}
                cancelling={liveCancelling}
                onDecide={onLiveDecision}
                onCancel={onLiveCancel}
              />
            )}
            {(liveRun || liveStarting) && liveRun && (
              <AgentRunView
                run={liveRun}
                deciding={liveDeciding}
                cancelling={liveCancelling}
                onDecide={onLiveDecision}
                onCancel={onLiveCancel}
                custody={custody}
                onPlanDecision={onPlanDecision}
                planDeciding={planDeciding}
              />
            )}
            {rexTaskId && (
              <RexTaskView taskId={rexTaskId} onClose={() => setRexTaskId(null)} />
            )}
            {!rexTaskId && operatorMode === "agent" && !liveRun && !liveStarting && (
              <RexTaskList onPick={setRexTaskId} />
            )}
            {(liveRun || liveStarting) && !liveRun && (
              <section className="live-run" aria-label="Starting the agent loop">
                <div className="live-status" role="status">
                  <span className="live-dot" aria-hidden="true" />
                  <span className="eyebrow">Agent loop</span>
                  <span className="live-status-text">Contacting the live model catalog…</span>
                </div>
              </section>
            )}
            {!liveRun && !liveStarting && (nativeProject ? <NativePreviewView projectDir={nativeProject} /> : session && previewTask && <PreviewRuntimeView />)}
            {ultra && session && !previewTask && browserPhase === "active" && (
              <BrowserView
                key={browserRun}
                reduced={reduced}
                onFinished={() => setBrowserPhase("collapsed")}
              />
            )}
            {ultra && session && browserPhase === "collapsed" && (
              <button
                type="button"
                className="browser-collapsed"
                onClick={() => { setBrowserRun((v) => v + 1); setBrowserPhase("active"); }}
                aria-label="Reopen the simulated browser work receipt"
              >
                <span><b>Browser work complete</b><small>Tool closed · receipt B-0119 · SAMPLE</small></span>
                <span>Review</span>
              </button>
            )}
            {!liveRun && !liveStarting && (session ? (
              <SessionView
                key={session.id}
                session={session}
                busy={busy}
                onFollowUp={onFollowUp}
                onNewTask={onNewTask}
                focusComposer={session.id === "run-live"}
              />
            ) : (
              <div className="empty-state">
                <span className="empty-mark" />
                <p>Receipts and checks will appear here after a run.</p>
              </div>
            ))}
            <HistoryList
              sessions={PAST_SESSIONS}
              selectedId={session?.id}
              onSelect={(s) => {
                cancel.current?.();
                setSession(s);
                beginSettle();
              }}
            />
          </>
        )}
      </main>
    </div>
  );
}
