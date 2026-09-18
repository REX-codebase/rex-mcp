import { useEffect, useRef, useState } from "react";
import { PROVIDER_PRESETS } from "../data/providers";

type ConnectFlow = "subscription" | "api" | null;

export function ModelStatus() {
  const [open, setOpen] = useState(false);
  const [flow, setFlow] = useState<ConnectFlow>(null);
  const [provider, setProvider] = useState(PROVIDER_PRESETS[0].id);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      if (flow) setFlow(null);
      else setOpen(false);
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [flow]);

  useEffect(() => {
    if (!open) return;
    const close = (event: MouseEvent) => {
      if (!flow && ref.current && !ref.current.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, [open, flow]);

  const launch = (next: Exclude<ConnectFlow, null>) => {
    setOpen(false);
    setFlow(next);
  };

  return (
    <div ref={ref} className="relative model-picker">
      <button
        type="button"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label="Choose model. No model connected"
        onClick={() => setOpen((value) => !value)}
        className="model-trigger"
      >
        <span className="model-mark" aria-hidden="true" />
        <span><small>Model</small><b>Connect a model</b></span>
        <svg width="12" height="12" viewBox="0 0 16 16" aria-hidden="true"><path d="m4 6 4 4 4-4" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" /></svg>
      </button>

      {open && (
        <div role="menu" aria-label="Models and provider connections" className="model-menu provider-menu">
          <div className="provider-menu-head"><span>Connected models</span><b>0</b></div>
          <div className="provider-empty">
            <span aria-hidden="true" />
            <div><b>No models connected</b><small>Connect a subscription or API to see its available models here.</small></div>
          </div>
          <div className="provider-menu-actions">
            <button type="button" role="menuitem" onClick={() => launch("subscription")}>
              <span className="provider-action-icon" aria-hidden="true">S</span>
              <span><b>Connect subscription</b><small>Use an eligible provider plan</small></span>
              <i aria-hidden="true">→</i>
            </button>
            <button type="button" role="menuitem" onClick={() => launch("api")}>
              <span className="provider-action-icon" aria-hidden="true">{`{ }`}</span>
              <span><b>Connect API</b><small>Bring a provider key or endpoint</small></span>
              <i aria-hidden="true">→</i>
            </button>
          </div>
          <p className="provider-menu-note">PREVIEW · Connections are not active</p>
        </div>
      )}

      {flow && (
        <div className="connect-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) setFlow(null); }}>
          <section role="dialog" aria-modal="true" aria-labelledby="connect-title" className="connect-dialog">
            <button type="button" className="dialog-close" aria-label="Close connection dialog" onClick={() => setFlow(null)}>×</button>
            <span className="dialog-orbit" aria-hidden="true"><i /><i /></span>
            <p className="eyebrow">{flow === "subscription" ? "Provider subscription" : "Provider API"}</p>
            <h2 id="connect-title">{flow === "subscription" ? "Connect the plan you already use." : "Connect your own API access."}</h2>
            <p className="dialog-copy">
              {flow === "subscription"
                ? "REX will show official sign-in routes supported by each provider, then add the models available to your account."
                : "Choose a provider. Keys will be stored by the desktop runtime, never inside this interface."}
            </p>
            {flow === "subscription" ? (
              <div className="connect-choice-list" aria-label="Subscription providers">
                {["Google", "OpenAI", "Anthropic"].map((name) => <button type="button" key={name} disabled><span>{name}</span><small>Runtime needed</small></button>)}
              </div>
            ) : (
              <div className="api-preview">
                <label><span>Provider</span><select value={provider} onChange={(event) => setProvider(event.target.value)}>{PROVIDER_PRESETS.map((item) => <option key={item.id} value={item.id}>{item.name}</option>)}</select></label>
                <label><span>API key</span><input value="Stored in desktop keychain" disabled /></label>
              </div>
            )}
            <button type="button" className="dialog-primary" disabled>Continue when runtime is connected</button>
            <p className="dialog-truth"><span aria-hidden="true">i</span>Simulated setup preview. No sign-in opens, no key is requested, and no network connection is made.</p>
          </section>
        </div>
      )}
    </div>
  );
}
