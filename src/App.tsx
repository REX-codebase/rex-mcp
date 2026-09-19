import { useEffect, useRef, useState } from "react";
import { TopBar } from "./components/TopBar";
import { Composer } from "./components/Composer";
import { StateRail } from "./components/StateRail";
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
import { liveRunAvailable } from "./data/liveRun";
import {
  agentBegin,
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
import type { InstalledAgentId } from "./data/backend";

// The hero starter composer exists only while there is no session. The moment
// the first run starts it settles - a brief blur + downward travel while its
// wrapper collapses - and unmounts, leaving exactly one composer at the
// bottom of the session. Reduced motion swaps it instantly.
export default function App() {
  const [view, setView] = useState<"task" | "settings">("task");
  const [browserPhase, setBrowserPhase] = useState<"closed" | "active" | "collapsed">("closed");
  const [browserRun, setBrowserRun] = useState(0);
  const [ultra, setUltra] = useState(false);
  const [ultraPulse, setUltraPulse] = useState(0);
  const [fast, setFast] = useState(false);
  const [fastPulse, setFastPulse] = useState(0);
  const [fastPhase, setFastPhase] = useState<"idle" | "engaging" | "disengaging">("idle");
  const fastTimer = useRef<number | null>(null);
  const [task, setTask] = useState("");
  const [session, setSession] = useState<Session | null>(null);
  const [liveCapable, setLiveCapable] = useState(false);
  const [liveRun, setLiveRun] = useState<AgentSnapshot | null>(null);
  const [liveStarting, setLiveStarting] = useState(false);
  const [liveDeciding, setLiveDeciding] = useState(false);
  const [liveCancelling, setLiveCancelling] = useState(false);
  const livePoll = useRef<number | null>(null);
  const [installedRun, setInstalledRun] = useState<InstalledRunSnapshot | null>(null);
  const [installedStarting, setInstalledStarting] = useState(false);
  const [installedDeciding, setInstalledDeciding] = useState(false);
  const [installedCancelling, setInstalledCancelling] = useState(false);
  const installedPoll = useRef<number | null>(null);
  const [nativePreviewDir, setNativePreviewDir] = useState<string | null>(null);
  useEffect(() => {
    let active = true;
    liveRunAvailable().then((ok) => { if (active) setLiveCapable(ok); }).catch(() => undefined);
    return () => { active = false; };
  }, []);
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

  const onRun = () => {
    cancel.current?.();
    const label = task.trim();
    if (!label) return;
    let installedBackend: InstalledAgentId | null = null;
    try {
      const selected = JSON.parse(window.localStorage.getItem("rex-model-selection") || "null") as { provider?: string; id?: string } | null;
      if (selected?.provider?.startsWith("installed:") && selected.id) {
        installedBackend = selected.id as InstalledAgentId;
      }
    } catch { /* no selection */ }
    if (installedBackend) {
      // Real installed-agent path: the vendor CLI runs the task on an
      // isolated workspace while REX streams its events, applies the
      // completion gate, and holds any source changes for reviewed promotion.
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
    if (liveCapable) {
      // Real path: the autonomous Rust loop plans, acts through trusted
      // approvals, verifies its own work against gates, and stops truthfully.
      setLiveRun(null);
      setLiveStarting(true);
      beginSettle();
      agentBegin(label)
        .then((snap) => {
          setLiveRun(snap);
          startPolling(snap.id);
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
    const s = newSession(label);
    cancel.current = startMockTurn(s, label, "task", setSession, reduced);
    if (ultra) {
      setBrowserRun((v) => v + 1);
      setBrowserPhase("active");
    }
    beginSettle();
  };

  const onLiveDecision = (approved: boolean) => {
    if (!liveRun || liveDeciding) return;
    setLiveDeciding(true);
    agentDecide(liveRun.id, approved)
      .then((snap) => setLiveRun(snap))
      .catch((e) => setLiveRun((prev) => (prev ? { ...prev, error: String(e) } : prev)))
      .finally(() => setLiveDeciding(false));
  };

  const onLiveCancel = () => {
    if (!liveRun || liveCancelling) return;
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
      <TopBar view={view} onView={setView} ultra={ultra} onUltra={() => { setUltra((v) => !v); setUltraPulse((v) => v + 1); }} fast={fast} onFast={toggleFast} live={liveCapable} />
      <div className="ultra-status" role="status" aria-live="polite"><span>ULTRA</span><b>{ultra ? "Premium preview engaged" : "Premium preview offline"}</b><small>No extra capabilities are active</small></div>
      <div className="fast-status" role="status" aria-live="polite"><span>FAST</span><b>{fast ? "TEMPO PROFILE ARMED" : "Fast preview off"}</b><small>Visual only · execution speed unchanged</small><i aria-hidden="true" /></div>
      <main className="main-spine mx-auto w-full max-w-[820px] px-5 pb-12 sm:px-8">
        {view === "settings" ? (
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
            {(liveRun || liveStarting) && liveRun && (
              <AgentRunView
                run={liveRun}
                deciding={liveDeciding}
                cancelling={liveCancelling}
                onDecide={onLiveDecision}
                onCancel={onLiveCancel}
              />
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
