import { useEffect, useRef, useState } from "react";
import { TopBar } from "./components/TopBar";
import { Composer } from "./components/Composer";
import { StateRail } from "./components/StateRail";
import { SessionView } from "./components/SessionView";
import { HistoryList } from "./components/HistoryList";
import { SettingsView, type MotionPref } from "./components/SettingsView";
import { BrowserView } from "./components/BrowserView";
import { PAST_SESSIONS, newSession, startMockTurn, type Session } from "./data/mock";

const MOTION_KEY = "rex-harness-motion";

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
  const [task, setTask] = useState("");
  const [session, setSession] = useState<Session | null>(null);
  const [hero, setHero] = useState<"shown" | "settling" | "gone">("shown");
  const cancel = useRef<(() => void) | null>(null);
  const settleTimer = useRef<number | null>(null);
  const systemReduce =
    typeof window !== "undefined" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  const [motion, setMotion] = useState<MotionPref>(() => {
    try {
      const v = window.localStorage.getItem(MOTION_KEY);
      return v === "reduce" || v === "full" ? v : "system";
    } catch {
      return "system";
    }
  });
  const reduced = motion === "system" ? systemReduce : motion === "reduce";
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

  const onRun = () => {
    cancel.current?.();
    const label = task.trim();
    if (!label) return;
    const s = newSession(label);
    cancel.current = startMockTurn(s, label, "task", setSession, reduced);
    if (ultra) {
      setBrowserRun((v) => v + 1);
      setBrowserPhase("active");
    }
    beginSettle();
  };

  const onFollowUp = (request: string) => {
    if (!session || busy) return;
    cancel.current?.();
    cancel.current = startMockTurn(session, request, "follow-up", setSession, reduced);
  };

  const onNewTask = () => {
    cancel.current?.();
    if (settleTimer.current !== null) window.clearTimeout(settleTimer.current);
    setSession(null);
    setTask("");
    setHero("shown");
    setBrowserPhase("closed");
  };

  return (
    <div className={`harness-shell min-h-full ${ultra ? "ultra-on" : ""} ${fast ? "fast-on" : ""} ${reduced ? "reduced-fx" : ""}`} data-ultra={ultra ? "on" : "off"}>
      <div className="ultra-transition" key={ultraPulse} aria-hidden="true"><span /><span /><span /><span /></div>
      <div className="fast-transition" key={fastPulse} aria-hidden="true">
        <span /><span /><span /><span /><span /><span /><span /><span /><span />
      </div>
      <div className="ultra-atmosphere" aria-hidden="true"><span className="ultra-horizon" /><span className="ultra-scan" /></div>
      <TopBar view={view} onView={setView} ultra={ultra} onUltra={() => { setUltra((v) => !v); setUltraPulse((v) => v + 1); }} fast={fast} onFast={() => { setFast((v) => !v); setFastPulse((v) => v + 1); }} />
      <div className="ultra-status" role="status" aria-live="polite"><span>ULTRA</span><b>{ultra ? "Premium preview engaged" : "Premium preview offline"}</b><small>No extra capabilities are active</small></div>
      <div className="fast-status" role="status" aria-live="polite"><span>FAST</span><b>{fast ? "Interface tempo preview" : "Fast preview off"}</b><small>Visual only · execution speed unchanged</small></div>
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
            {ultra && session && browserPhase === "active" && (
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
            {session ? (
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
            )}
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
