import { useEffect, useRef, useState } from "react";
import { TopBar } from "./components/TopBar";
import { Composer } from "./components/Composer";
import { StateRail } from "./components/StateRail";
import { ResultView } from "./components/ResultView";
import { HistoryList } from "./components/HistoryList";
import {
  PAST_RUNS,
  startMockRun,
  type ModelId,
  type Run,
} from "./data/mock";

export default function App() {
  const [task, setTask] = useState("");
  const [model, setModel] = useState<ModelId>("atlas-pro");
  const [run, setRun] = useState<Run | null>(null);
  const cancelRef = useRef<(() => void) | null>(null);
  const reducedMotion =
    typeof window !== "undefined" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  useEffect(() => () => cancelRef.current?.(), []);

  const onRun = () => {
    cancelRef.current?.();
    const label = task.trim();
    if (!label) return;
    cancelRef.current = startMockRun(
      label,
      model === "atlas-pro" ? "Atlas 1.5 Pro" : model === "atlas-flash" ? "Atlas 1.5 Flash" : "Meridian 2",
      setRun,
      reducedMotion
    );
  };

  const state = run?.state ?? "idle";

  return (
    <div className="flex min-h-full flex-col">
      <TopBar />
      <main className="mx-auto w-full max-w-[880px] flex-1 px-4 py-6 sm:px-6 sm:py-8">
        <div className="space-y-4">
          <Composer
            task={task}
            setTask={setTask}
            model={model}
            setModel={setModel}
            state={state}
            onRun={onRun}
          />
          <StateRail state={state} blockedReason={run?.blockedReason} />
          {run && <ResultView run={run} />}
          {run === null && (
            <p className="px-1 pt-2 text-sm text-muted">
              No run yet. Describe a task above, or open a past run to see what a
              finished receipt looks like.
            </p>
          )}
          <HistoryList
            runs={PAST_RUNS}
            selectedId={run?.id}
            onSelect={(r) => {
              cancelRef.current?.();
              setRun(r);
            }}
          />
        </div>
      </main>
      <footer className="border-t border-border px-4 py-3 text-center text-xs text-faint sm:px-6">
        REX Harness · interface preview · receipts shown are samples
      </footer>
    </div>
  );
}
