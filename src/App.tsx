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
import { LiveRunView } from "./components/LiveRunView";
import { liveRunAvailable, runBegin, runDecide, runTeardown, type RunSnapshot } from "./data/liveRun";

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
  const [liveRun, setLiveRun] = useState<RunSnapshot | null>(null);
  const [liveStarting, setLiveStarting] = useState(false);
  const [liveDeciding, setLiveDeciding] = useState(false);
  useEffect(() => {
    let active = true;
    liveRunAvailable().then((ok) => { if (active) setLiveCapable(ok); }).catch(() => undefined);
    return () => { active = false; };
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
    },
    []
  );

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

  const nativeProject = new URLSearchParams(window.location.search).get("native-preview");
  const previewTask = /(?:html|react|next\.?js|vite|astro|svelte|website|ui|interface|dashboard|landing page|app)/i.test(task);

  const onRun = () => {
    cancel.current?.();
    const label = task.trim();
    if (!label) return;
    if (liveCapable) {
      // Real path: catalog refresh -> provider turn -> trusted approval ->
      // Rust write -> native preview. Nothing executes before the decision.
      setLiveRun(null);
      setLiveStarting(true);
      beginSettle();
      runBegin(label)
        .then((snap) => setLiveRun(snap))
        .catch((e) => setLiveRun({ id: "run-failed", task: label, status: "failed", model: "", catalog_count: 0, events: [], approval: null, result: null, preview: null, error: String(e) }))
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
    runDecide(liveRun.id, approved)
      .then((snap) => setLiveRun(snap))
      .catch((e) => setLiveRun((prev) => (prev ? { ...prev, status: "failed", error: String(e) } : prev)))
      .finally(() => setLiveDeciding(false));
  };

  const onFollowUp = (request: string) => {
    if (!session || busy) return;
    cancel.current?.();
    cancel.current = startMockTurn(session, request, "follow-up", setSession, reduced);
  };

  const onNewTask = () => {
    cancel.current?.();
    if (settleTimer.current !== null) window.clearTimeout(settleTimer.current);
    if (liveRun && !liveRun.id.startsWith("run-failed")) runTeardown(liveRun.id).catch(() => undefined);
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
      <TopBar view={view} onView={setView} ultra={ultra} onUltra={() => { setUltra((v) => !v); setUltraPulse((v) => v + 1); }} fast={fast} onFast={toggleFast} />
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
            {(liveRun || liveStarting) && <LiveRunView run={liveRun} deciding={liveDeciding} onDecide={onLiveDecision} />}
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
