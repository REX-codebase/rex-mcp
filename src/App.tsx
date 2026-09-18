import { useEffect, useRef, useState } from "react";
import { TopBar } from "./components/TopBar";
import { Composer } from "./components/Composer";
import { StateRail } from "./components/StateRail";
import { SessionView } from "./components/SessionView";
import { HistoryList } from "./components/HistoryList";
import { SettingsView, type MotionPref } from "./components/SettingsView";
import { PAST_SESSIONS, newSession, startMockTurn, type Session } from "./data/mock";

const MOTION_KEY = "rex-harness-motion";

export default function App() {
  const [view, setView] = useState<"task" | "settings">("task");
  const [task, setTask] = useState("");
  const [session, setSession] = useState<Session | null>(null);
  const cancel = useRef<(() => void) | null>(null);
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
  useEffect(() => () => cancel.current?.(), []);

  const latest = session?.turns[session.turns.length - 1];
  const state = latest?.state ?? "idle";
  const busy = state === "working" || state === "verifying";

  const onRun = () => {
    cancel.current?.();
    const label = task.trim();
    if (!label) return;
    const s = newSession(label);
    cancel.current = startMockTurn(s, label, "task", setSession, reduced);
  };

  const onFollowUp = (request: string) => {
    if (!session || busy) return;
    cancel.current?.();
    cancel.current = startMockTurn(session, request, "follow-up", setSession, reduced);
  };

  return (
    <div className={`min-h-full ${reduced ? "reduced-fx" : ""}`}>
      <TopBar view={view} onView={setView} />
      <main className="mx-auto w-full max-w-[820px] px-5 pb-12 sm:px-8">
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
            <div className="mb-9 mt-3 sm:mb-12 sm:mt-7">
              <Composer
                task={task}
                setTask={setTask}
                state={state}
                onRun={onRun}
                onOpenSettings={() => setView("settings")}
              />
            </div>
            <StateRail state={state} blockedReason={latest?.blockedReason} />
            {session ? (
              <SessionView session={session} busy={busy} onFollowUp={onFollowUp} />
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
              }}
            />
          </>
        )}
      </main>
    </div>
  );
}
