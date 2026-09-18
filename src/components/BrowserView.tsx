import { useEffect, useRef, useState } from "react";
import {
  startBrowserPreview,
  PAGE_ELEMENTS,
  type BrowserSession,
} from "../data/browser";

function SimPage({ s, onPick }: { s: BrowserSession; onPick: (id: string) => void }) {
  if (s.page === "loading") {
    return (
      <div className="sim-page sim-loading" aria-label="Sample page loading">
        <span className="sim-skel w-40" />
        <span className="sim-skel w-64" />
        <span className="sim-skel w-52" />
        <span className="sim-skel w-28" />
      </div>
    );
  }
  if (s.page === "error") {
    return (
      <div className="sim-page sim-error" aria-label="Sample page failed to load">
        <p className="sim-error-title">This sample page failed to load</p>
        <p className="sim-error-copy">A simulated network error, rendered so the error state can be reviewed. Nothing was requested.</p>
      </div>
    );
  }
  const el = (id: string, node: React.ReactNode, extra = "") => (
    <span
      key={id}
      data-el={id}
      className={`sim-el ${extra} ${s.targeting ? "is-targetable" : ""} ${s.targetedEl === id ? "is-targeted" : ""}`}
      onClick={s.targeting ? () => onPick(id) : undefined}
      role={s.targeting ? "button" : undefined}
      tabIndex={s.targeting ? 0 : undefined}
      onKeyDown={s.targeting ? (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); onPick(id); } } : undefined}
    >
      {node}
      {s.targetedEl === id && <i className="sim-el-tag">#{id}</i>}
    </span>
  );
  if (s.url.includes("/cart")) {
    return (
      <div className="sim-page" aria-label="Sample cart page">
        <p className="sim-brand">cornitos.example<span>sample shop</span></p>
        {el("product-title", <span className="sim-h1">Your cart</span>, "block-el")}
        <span className="sim-cart-row">
          {el("pack-size", <span>Sizzlin&rsquo; Jalapeno Nacho Crisps - 150 g x 1</span>)}
          {el("price", <b>₹45</b>)}
        </span>
        <span className="sim-cart-total">
          {el("qty-stepper", <span>Total</span>)}
          {el("add-to-cart", <b>₹45</b>)}
        </span>
      </div>
    );
  }
  return (
    <div className="sim-page" aria-label="Sample product page">
      <p className="sim-brand">cornitos.example<span>sample shop</span></p>
      {el("product-title", <span className="sim-h1">Sizzlin&rsquo; Jalapeno Nacho Crisps</span>, "block-el")}
      {el("pack-size", <span className="sim-sub">Nacho crisps - 150 g pack</span>, "block-el")}
      <span className="sim-buy">
        {el("price", <span className="sim-price">₹45</span>)}
        {el("qty-stepper", <span className="sim-qty"><i>-</i>1<i>+</i></span>)}
        {el("add-to-cart", <span className="sim-cta">Add to cart</span>)}
      </span>
      <span className="sim-desc">Sample product copy stands in for a real page so the agent surface can be reviewed without any network access.</span>
    </div>
  );
}

function Shot({ kind }: { kind: string }) {
  return (
    <figure className="shot">
      <span className={`shot-canvas shot-${kind}`} aria-hidden="true">
        <i /><i /><i />
      </span>
      <figcaption>{kind === "receipt" ? "Receipt" : "Screenshot"} · sample</figcaption>
    </figure>
  );
}

export function BrowserView({ reduced, onFinished }: { reduced: boolean; onFinished: () => void }) {
  const [session, setSession] = useState<BrowserSession | null>(null);
  const ctl = useRef<ReturnType<typeof startBrowserPreview> | null>(null);

  const boot = () => {
    ctl.current?.cancel();
    const c = startBrowserPreview(setSession, reduced);
    ctl.current = c;
  };
  useEffect(() => {
    boot();
    return () => ctl.current?.cancel();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [reduced]);

  useEffect(() => {
    if (!session?.done) return;
    const timer = window.setTimeout(onFinished, reduced ? 180 : 1100);
    return () => window.clearTimeout(timer);
  }, [session?.done, reduced, onFinished]);

  useEffect(() => {
    if (!session?.targeting) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") ctl.current?.setTargeting(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [session?.targeting]);

  if (!session) return null;
  const s = session;
  const userDriving = s.ownership === "user";

  return (
    <section aria-label="Agent browser tool (simulated preview)" className="browser-view">
      <div className="browser-tool-head">
        <p className="browser-intent">
          <span className="eyebrow">Browser called</span>
          <span className="browser-intent-text">{s.intent}</span>
        </p>
        <span className="browser-tool-state">Inside this task · SAMPLE</span>
      </div>

      <div className="browser-frame">
        <div className="browser-nav">
          <div className="browser-nav-buttons" role="group" aria-label="Navigation (sample)">
            <button type="button" className="nav-btn" disabled title="Sample control - no real navigation" aria-label="Back (unavailable in preview)">
              <svg width="13" height="13" viewBox="0 0 14 14" aria-hidden="true"><path d="M9 2.5 4.5 7 9 11.5" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" /></svg>
            </button>
            <button type="button" className="nav-btn" disabled title="Sample control - no real navigation" aria-label="Forward (unavailable in preview)">
              <svg width="13" height="13" viewBox="0 0 14 14" aria-hidden="true"><path d="M5 2.5 9.5 7 5 11.5" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" /></svg>
            </button>
            <button type="button" className="nav-btn" onClick={boot} title="Restart the simulated session" aria-label="Restart the simulated session">
              <svg width="13" height="13" viewBox="0 0 14 14" aria-hidden="true"><path d="M12 7a5 5 0 1 1-1.46-3.54M12 2v3H9" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" /></svg>
            </button>
          </div>
          <span className="browser-url" title={s.url}>
            <span className="browser-url-lock" aria-hidden="true">
              <svg width="10" height="10" viewBox="0 0 12 12"><path d="M3 5V4a3 3 0 0 1 6 0v1m-7 0h8v5H2z" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinejoin="round" /></svg>
            </span>
            <span className="browser-url-text">{s.url}</span>
          </span>
          <button
            type="button"
            className={`target-btn ${s.targeting ? "is-armed" : ""}`}
            disabled={s.page !== "live" || userDriving || s.done}
            aria-pressed={s.targeting}
            title={s.page !== "live" ? "Available once the sample page loads" : "Arm element targeting on the simulated page"}
            onClick={() => ctl.current?.setTargeting(!s.targeting)}
          >
            <svg width="12" height="12" viewBox="0 0 14 14" aria-hidden="true"><circle cx="7" cy="7" r="4.4" fill="none" stroke="currentColor" strokeWidth="1.4" /><path d="M7 1v2.4M7 10.6V13M1 7h2.4M10.6 7H13" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" /></svg>
            Target
          </button>
          <button
            type="button"
            className="takeover-btn"
            disabled={s.done}
            onClick={() => ctl.current?.setOwnership(userDriving ? "agent" : "user")}
          >
            {userDriving ? "Return control" : "Take control"}
          </button>
        </div>

        <div className={`browser-stage ${s.targeting ? "is-targeting" : ""} ${userDriving ? "has-handover" : ""}`} aria-live="polite">
          <span className="sample-chip">SIMULATED PAGE · SAMPLE</span>
          {userDriving && (
            <div className="handover-banner" role="status">
              <span>You have control. The agent is paused and will not act.</span>
              <button type="button" onClick={() => ctl.current?.setOwnership("agent")}>Return control</button>
            </div>
          )}
          <SimPage s={s} onPick={(id) => ctl.current?.pickElement(id)} />
          {s.permission && (
            <div className="permission-card" role="alertdialog" aria-label="Permission requested">
              <div className="permission-copy">
                <p className="permission-title">Permission requested</p>
                <p className="permission-action">{s.permission.action}</p>
                <p className="permission-detail">{s.permission.detail}</p>
              </div>
              <div className="permission-actions">
                <button type="button" className="deny-btn" onClick={() => ctl.current?.deny()}>Deny</button>
                <button type="button" className="allow-btn" onClick={() => ctl.current?.allow()}>Allow once</button>
              </div>
            </div>
          )}
          {s.page === "error" && !userDriving && (
            <button type="button" className="retry-btn" onClick={() => ctl.current?.retry()}>Retry sample load</button>
          )}
        </div>
      </div>

      <div className="browser-lower">
        <div className="trail-block">
          <p className="eyebrow">Action trail</p>
          <ol className="trail-list">
            {s.trail.map((t) => (
              <li key={t.id} className={`trail-row st-${t.status}`}>
                <span className="trail-dot" aria-hidden="true" />
                <span className="trail-verb">{t.verb}</span>
                <span className="trail-target">{t.target}</span>
                <span className="trail-state">{t.status === "pending" ? "waiting for you" : t.status}</span>
              </li>
            ))}
          </ol>
        </div>

        <div className="evidence-block">
          <p className="eyebrow">Evidence</p>
          {s.shots.length === 0 ? (
            <p className="evidence-empty">Screenshots land here as the agent works.</p>
          ) : (
            <div className="shot-row">
              {s.shots.map((k, i) => <Shot key={`${k}-${i}`} kind={k} />)}
            </div>
          )}
          <p className="receipt-line">
            Receipt <b>{s.receipt}</b> · {s.done ? (s.blockedReason ? "stopped" : "complete") : "in progress"} · SAMPLE
          </p>
          {s.blockedReason && <p className="blocked-line">{s.blockedReason}</p>}

        </div>
      </div>

      <p className="browser-truth">
        Preview only. No real browser, navigation, network, credentials, or page access. The page above is drawn
        from local sample data so the agent-browser surface can be reviewed; the runtime adapter is not built yet.
      </p>
    </section>
  );
}
