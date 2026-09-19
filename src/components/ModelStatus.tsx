import { useCallback, useEffect, useRef, useState } from "react";
import { PROVIDER_PRESETS, accessStatusLabel, type AccessLine, type AccessStatus } from "../data/providers";
import {
  backendKind,
  clearProviderKey,
  describeError,
  listSummaries,
  refreshCatalog,
  setProviderKey,
  type BackendKind,
  type ProviderSummary,
} from "../data/backend";

type ConnectFlow = "subscription" | "api" | null;

type Selection = { provider: string; id: string; label: string };

const SELECTION_KEY = "rex-model-selection";

function loadSelection(): Selection | null {
  try {
    const raw = window.localStorage.getItem(SELECTION_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw);
    if (parsed && typeof parsed.id === "string" && typeof parsed.provider === "string") return parsed;
  } catch {
    /* ignore */
  }
  return null;
}

export function ModelStatus() {
  const [open, setOpen] = useState(false);
  const [flow, setFlow] = useState<ConnectFlow>(null);
  const [kind, setKind] = useState<BackendKind | null>(null);
  const [summaries, setSummaries] = useState<ProviderSummary[]>([]);
  const [loading, setLoading] = useState(false);
  const [refreshing, setRefreshing] = useState<string | null>(null);
  const [selection, setSelection] = useState<Selection | null>(loadSelection);
  // Connect-API form state. The key lives only in this field until it is
  // handed to the backend; it is cleared immediately after.
  const [provider, setProvider] = useState(PROVIDER_PRESETS[0].id);
  const [keyField, setKeyField] = useState("");
  const [baseUrlField, setBaseUrlField] = useState("");
  const [modelIdField, setModelIdField] = useState("");
  const [connectBusy, setConnectBusy] = useState(false);
  const [connectError, setConnectError] = useState<string | null>(null);
  const ref = useRef<HTMLDivElement>(null);

  const reload = useCallback(async () => {
    setLoading(true);
    try {
      setSummaries(await listSummaries());
    } catch {
      setSummaries([]);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    backendKind().then((k) => {
      setKind(k);
      if (k) reload();
    });
  }, [reload]);

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
    setConnectError(null);
  };

  const choose = (next: Selection) => {
    setSelection(next);
    try {
      window.localStorage.setItem(SELECTION_KEY, JSON.stringify(next));
    } catch {
      /* ignore */
    }
    setOpen(false);
  };

  const refresh = async (providerId: string) => {
    setRefreshing(providerId);
    await refreshCatalog(providerId);
    await reload();
    setRefreshing(null);
  };

  const submitKey = async () => {
    if (!keyField.trim() || connectBusy) return;
    setConnectBusy(true);
    setConnectError(null);
    const preset = PROVIDER_PRESETS.find((p) => p.id === provider);
    const baseUrl = preset?.protocol === "openai-compatible" ? baseUrlField.trim() || undefined : undefined;
    const written = await setProviderKey(provider, keyField.trim(), baseUrl);
    setKeyField("");
    if (!written.ok) {
      setConnectError(describeError(written.error));
      setConnectBusy(false);
      return;
    }
    if (preset?.modelDiscovery === "manual") {
      // Manual providers have no documented listing API, so REX never probes
      // one: the key is saved and the typed model ID becomes the selection.
      const id = modelIdField.trim();
      setModelIdField("");
      setConnectBusy(false);
      if (id) choose({ provider, id, label: id });
      setFlow(null);
      await reload();
      return;
    }
    const fetched = await refreshCatalog(provider);
    if (!fetched.ok) {
      setConnectError(describeError(fetched.error));
      setConnectBusy(false);
      await reload();
      return;
    }
    setConnectBusy(false);
    setFlow(null);
    await reload();
  };

  const removeKey = async (providerId: string) => {
    await clearProviderKey(providerId);
    if (selection?.provider === providerId) {
      setSelection(null);
      try {
        window.localStorage.removeItem(SELECTION_KEY);
      } catch {
        /* ignore */
      }
    }
    await reload();
  };

  const totalModels = summaries.reduce((sum, s) => sum + (s.catalog?.models.length ?? 0), 0);
  const connected = summaries.filter((s) => s.has_key || s.catalog);
  const selectedPreset = PROVIDER_PRESETS.find((p) => p.id === provider)!;

  // -------- Preview (no backend): exactly the honest preview UI --------
  if (!kind) {
    return <PreviewPicker open={open} setOpen={setOpen} flow={flow} launch={launch} setFlow={setFlow} provider={provider} setProvider={setProvider} refObj={ref} />;
  }

  // -------- Real backend --------
  return (
    <div ref={ref} className="relative model-picker">
      <button
        type="button"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={selection ? `Choose model. Current: ${selection.label}` : "Choose model. No model selected"}
        onClick={() => {
          setOpen((value) => !value);
          if (!open) reload();
        }}
        className="model-trigger"
      >
        <span className={`model-mark ${selection ? "is-live" : ""}`} aria-hidden="true" />
        <span>
          <small>{selection ? selection.provider : "Model"}</small>
          <b>{selection ? selection.id : totalModels > 0 ? `${totalModels} models available` : "Connect a model"}</b>
        </span>
        <svg width="12" height="12" viewBox="0 0 16 16" aria-hidden="true"><path d="m4 6 4 4 4-4" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" /></svg>
      </button>

      {open && (
        <div role="menu" aria-label="Models and provider connections" className="model-menu provider-menu live">
          <div className="provider-menu-head">
            <span>Available models</span>
            <b>{loading ? "…" : totalModels}</b>
          </div>

          <div className="provider-model-scroll">
            {loading && <div className="provider-empty"><span aria-hidden="true" /><div><b>Contacting providers</b><small>Fetching live model catalogs.</small></div></div>}

            {!loading && connected.length === 0 && (
              <div className="provider-empty">
                <span aria-hidden="true" />
                <div><b>No providers connected</b><small>Add an API key and its available models appear here, fetched live.</small></div>
              </div>
            )}

            {!loading && connected.map((s) => (
              <section key={s.id} className="provider-block" aria-label={`${s.name} models`}>
                <header>
                  <span>
                    <b>{s.name}</b>
                    {s.catalog && (
                      <small>
                        {s.catalog.models.length} models · {s.catalog.source === "live" ? "live" : "recorded live"} {new Date(s.catalog.fetched_at * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}
                      </small>
                    )}
                  </span>
                  <span className="provider-block-actions">
                    {s.discovery !== "manual" && (
                    <button type="button" onClick={() => refresh(s.id)} disabled={refreshing === s.id} aria-label={`Refresh ${s.name} models`}>
                      {refreshing === s.id ? "…" : "Refresh"}
                    </button>
                    )}
                    {s.has_key && <button type="button" onClick={() => removeKey(s.id)} aria-label={`Disconnect ${s.name}`}>Disconnect</button>}
                  </span>
                </header>
                {s.catalog && (
                  <ul>
                    {s.catalog.models.map((m) => (
                      <li key={m.id}>
                        <button
                          type="button"
                          role="menuitemradio"
                          aria-checked={selection?.provider === s.id && selection.id === m.id}
                          className={`model-option ${selection?.provider === s.id && selection.id === m.id ? "is-active" : ""}`}
                          onClick={() => choose({ provider: s.id, id: m.id, label: m.label })}
                        >
                          <span><b>{m.label}</b><small>{m.id}</small></span>
                          {selection?.provider === s.id && selection.id === m.id && <i aria-hidden="true">✓</i>}
                        </button>
                      </li>
                    ))}
                  </ul>
                )}
                {!s.catalog && s.last_error && (
                  <p className="provider-err-line">{describeError(s.last_error)}</p>
                )}
              </section>
            ))}
          </div>

          <div className="provider-menu-actions">
            <button type="button" role="menuitem" onClick={() => launch("api")}>
              <span className="provider-action-icon" aria-hidden="true">{`{ }`}</span>
              <span><b>Connect API</b><small>Add a provider key - stored by the Rust backend</small></span>
              <i aria-hidden="true">→</i>
            </button>
            <button type="button" role="menuitem" onClick={() => launch("subscription")}>
              <span className="provider-action-icon" aria-hidden="true">S</span>
              <span><b>Connect subscription</b><small>Eligible plans, each checked against provider policy</small></span>
              <i aria-hidden="true">→</i>
            </button>
          </div>
          <p className="provider-menu-note">{kind === "tauri" ? "LIVE · Rust desktop runtime" : "DEV · Rust sidecar on 127.0.0.1"}</p>
        </div>
      )}

      {flow && (
        <div className="connect-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) setFlow(null); }}>
          <section role="dialog" aria-modal="true" aria-labelledby="connect-title" className="connect-dialog">
            <button type="button" className="dialog-close" aria-label="Close connection dialog" onClick={() => setFlow(null)}>×</button>
            <span className="dialog-orbit" aria-hidden="true"><i /><i /></span>
            <p className="eyebrow">{flow === "subscription" ? "Provider subscription" : "Provider API"}</p>
            <h2 id="connect-title">{flow === "subscription" ? "Connect the plan you already use." : "Connect your own API access."}</h2>
            {flow === "subscription" ? (
              <>
                <p className="dialog-copy">Only plans whose provider officially allows third-party harness use can connect. REX refuses the rest - each row says why.</p>
                <SubscriptionChoices onConnect={(id) => { setProvider(id); setFlow("api"); }} />
                <p className="dialog-truth"><span aria-hidden="true">i</span>Verdicts come from each provider's official documentation, verified 2026-09-19. Sources and reasoning: docs/subscription-policy.md.</p>
              </>
            ) : (
              <>
                <p className="dialog-copy">Choose a provider and paste its key. The key goes straight to the Rust backend's credential store; this interface never keeps or displays it.</p>
                <div className="api-preview">
                  <label>
                    <span>Provider</span>
                    <select value={provider} onChange={(event) => setProvider(event.target.value)}>
                      {PROVIDER_PRESETS.map((item) => <option key={item.id} value={item.id}>{item.name}</option>)}
                    </select>
                  </label>
                  <label>
                    <span>API key</span>
                    <input
                      type="password"
                      value={keyField}
                      autoComplete="off"
                      placeholder="Paste the key"
                      onChange={(event) => setKeyField(event.target.value)}
                    />
                  </label>
                  {selectedPreset.protocol === "openai-compatible" && (
                    <label className="provider-field-wide" style={{ gridColumn: "1 / -1" }}>
                      <span>Base URL</span>
                      <input
                        value={baseUrlField}
                        placeholder={selectedPreset.baseUrl || "https://api.example.com/v1"}
                        spellCheck={false}
                        onChange={(event) => setBaseUrlField(event.target.value)}
                      />
                    </label>
                  )}
                  {selectedPreset.modelDiscovery === "manual" && (
                    <label className="provider-field-wide" style={{ gridColumn: "1 / -1" }}>
                      <span>Model ID</span>
                      <input
                        value={modelIdField}
                        placeholder="Exact model ID from the provider's docs"
                        spellCheck={false}
                        onChange={(event) => setModelIdField(event.target.value)}
                      />
                      {selectedPreset.examples && <small className="api-hint">{selectedPreset.examples}</small>}
                    </label>
                  )}
                </div>
                {connectError && <p className="provider-err-line" role="alert">{connectError}</p>}
                <button
                  type="button"
                  className="dialog-primary is-active"
                  disabled={!keyField.trim() || connectBusy || (selectedPreset.modelDiscovery === "manual" && !modelIdField.trim())}
                  onClick={submitKey}
                >
                  {connectBusy ? "Connecting…" : selectedPreset.modelDiscovery === "manual" ? "Save key & use model" : "Save key & fetch models"}
                </button>
                <p className="dialog-truth"><span aria-hidden="true">i</span>{selectedPreset.modelDiscovery === "manual"
                  ? "This provider has no documented model-listing API, so REX saves the key and uses the model ID you entered. Nothing is probed."
                  : "After saving, REX immediately asks the provider for its real model list. Auth, network, and empty-catalog failures show up here verbatim."}
                </p>
              </>
            )}
          </section>
        </div>
      )}
    </div>
  );
}

function PreviewPicker({ open, setOpen, flow, launch, setFlow, provider, setProvider, refObj }: {
  open: boolean;
  setOpen: (v: boolean | ((p: boolean) => boolean)) => void;
  flow: ConnectFlow;
  launch: (f: Exclude<ConnectFlow, null>) => void;
  setFlow: (f: ConnectFlow) => void;
  provider: string;
  setProvider: (p: string) => void;
  refObj: React.RefObject<HTMLDivElement | null>;
}) {
  return (
    <div ref={refObj as React.RefObject<HTMLDivElement>} className="relative model-picker">
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
                ? "Only plans whose provider officially allows third-party harness use can connect. REX refuses the rest - each row says why."
                : "Choose a provider. Keys will be stored by the desktop runtime, never inside this interface."}
            </p>
            {flow === "subscription" ? (
              <SubscriptionChoices onConnect={(id) => { setProvider(id); setFlow("api"); }} />
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

type SubscriptionRow = { providerId: string; line: AccessLine };

function subscriptionRows(): SubscriptionRow[] {
  const rank = (status: AccessStatus) => (status === "supported" ? 0 : status === "tool-scoped" ? 1 : status === "not-offered" ? 2 : 3);
  return PROVIDER_PRESETS.flatMap((preset) =>
    preset.access.filter((line) => line.kind !== "api-key").map((line) => ({ providerId: preset.id, line }))
  ).sort((a, b) => rank(a.line.status) - rank(b.line.status));
}

// Every consumer-subscription route REX evaluated, with its verdict. Only a
// "supported" row is clickable, and it connects through the mechanism the
// provider officially documents (for Kimi for Coding: its own API key).
function SubscriptionChoices({ onConnect }: { onConnect: (providerId: string) => void }) {
  return (
    <div className="connect-choice-list is-scrollable" aria-label="Subscription verdicts">
      {subscriptionRows().map(({ providerId, line }) => (
        <button
          key={`${providerId}-${line.label}`}
          type="button"
          disabled={line.status !== "supported"}
          onClick={line.status === "supported" ? () => onConnect(providerId) : undefined}
        >
          <span>
            <b>{line.label}</b>
            <small>{line.detail}</small>
          </span>
          <small className="sub-status">{accessStatusLabel[line.status]}</small>
        </button>
      ))}
    </div>
  );
}
