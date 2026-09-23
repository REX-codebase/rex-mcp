import { useEffect, useState } from "react";
import { fableSessionStatus, type FableStatus } from "../data/fable";

// Visible countdown for the Fable authority timer. The timer itself is
// enforced in Rust; this component only displays what the backend reports.
// Polls every second while the timer runs, stops when it elapses.
export function FableCountdown({ sessionName }: { sessionName: string }) {
  const [status, setStatus] = useState<FableStatus | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    let timer: number | null = null;
    const poll = async () => {
      try {
        const s = await fableSessionStatus(sessionName);
        if (!alive) return;
        setStatus(s);
        setError(null);
        // Keep polling once a second until the gate opens, then stop.
        if (!s.timer_elapsed) {
          timer = window.setTimeout(poll, 1000);
        }
      } catch (e) {
        if (!alive) return;
        setError(String(e));
      }
    };
    poll();
    return () => {
      alive = false;
      if (timer) window.clearTimeout(timer);
    };
  }, [sessionName]);

  if (error) {
    return (
      <div className="fable-countdown fable-countdown--error" role="status">
        <span className="eyebrow">Fable gate</span>
        <span className="fable-countdown-text">Could not read the timer: {error}</span>
      </div>
    );
  }
  if (!status) {
    return (
      <div className="fable-countdown" role="status">
        <span className="eyebrow">Fable gate</span>
        <span className="fable-countdown-text">Reading the authority timer…</span>
      </div>
    );
  }
  return (
    <div
      className={`fable-countdown ${status.timer_elapsed ? "fable-countdown--open" : ""}`}
      role="status"
      aria-label={`Fable gate ${status.phase}, authority timer ${status.timer_remaining_human} remaining`}
    >
      <span className="eyebrow">Fable gate · {status.phase}</span>
      <span className="fable-countdown-text">
        {status.timer_elapsed ? (
          <>Authority timer elapsed — unlock is gated on evidence only.</>
        ) : (
          <>
            Deliberation lock: <b>{status.timer_remaining_human}</b> remaining.
            Evidence: {status.proven_count} PROVEN · {status.invariant_count} invariant
            {status.invariant_count === 1 ? "" : "s"}.
          </>
        )}
      </span>
    </div>
  );
}
