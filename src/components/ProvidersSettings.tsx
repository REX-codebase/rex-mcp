import { useEffect, useMemo, useState } from "react";
import { PROVIDER_PRESETS, accessStatusLabel, protocolLabel, type ProviderDraft, type ProviderPreset } from "../data/providers";
import { backendKind, describeError, listSummaries, refreshCatalog, type BackendKind, type ProviderSummary } from "../data/backend";

function makeDraft(preset: ProviderPreset): ProviderDraft {
  return {
    presetId: preset.id,
    displayName: preset.name,
    protocol: preset.protocol,
    baseUrl: preset.baseUrl ?? "",
    modelId: "",
    keyReference: "desktop-keychain",
    status: "not-configured",
  };
}

export function ProvidersSettings() {
  const [backend, setBackend] = useState<BackendKind | null>(null);
  const [live, setLive] = useState<Record<string, ProviderSummary>>({});
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<string | null>(null);
  useEffect(() => {
    backendKind().then(async (k) => {
      setBackend(k);
      if (!k) return;
      try {
        const summaries = await listSummaries();
        setLive(Object.fromEntries(summaries.map((s) => [s.id, s])));
      } catch {
        /* backend went away; stay in draft mode */
      }
    });
  }, []);
  const [selectedId, setSelectedId] = useState("gemini");
  const [drafts, setDrafts] = useState<Record<string, ProviderDraft>>(() =>
    Object.fromEntries(PROVIDER_PRESETS.map((p) => [p.id, makeDraft(p)]))
  );
  const selected = useMemo(() => PROVIDER_PRESETS.find((p) => p.id === selectedId)!, [selectedId]);
  const draft = drafts[selected.id];
  const update = (patch: Partial<ProviderDraft>) =>
    setDrafts((current) => ({
      ...current,
      [selected.id]: { ...current[selected.id], status: "draft", ...patch },
    }));
  const isCompatible = selected.protocol === "openai-compatible";
  const liveState = live[selected.id];
  const discoveryLabel = selected.modelDiscovery === "native" ? "Native model list" : selected.modelDiscovery === "openai-models" ? "Try GET /models" : "Manual model ID";
  const statusLabel = draft.status === "draft" ? "Unsaved draft" : draft.status === "needs-runtime" ? "Draft · runtime needed" : "Not connected";

  return (
    <div className="providers-layout">
      <aside className="provider-list" aria-label="Provider presets">
        <div className="provider-list-head">
          <p className="eyebrow">Providers</p>
          <span>{PROVIDER_PRESETS.length}</span>
        </div>
        {PROVIDER_PRESETS.map((provider) => (
          <button
            type="button"
            key={provider.id}
            className={`provider-choice ${selected.id === provider.id ? "is-selected" : ""}`}
            aria-current={selected.id === provider.id ? "true" : undefined}
            onClick={() => setSelectedId(provider.id)}
          >
            <span>
              <strong>{provider.name}</strong>
              <small>{protocolLabel[provider.protocol]}</small>
            </span>
            <i aria-hidden="true" />
          </button>
        ))}
      </aside>

      <section className="provider-detail" aria-label={`${selected.name} configuration`}>
        <div className="provider-title-row">
          <div>
            <p className="eyebrow">{protocolLabel[selected.protocol]}</p>
            <h2>{selected.name}</h2>
          </div>
          <span className={`status-chip ${draft.status !== "not-configured" ? "has-draft" : ""}`}>{statusLabel}</span>
        </div>
        <p className="provider-summary">{selected.summary}</p>

        {selected.access.length > 0 && (
          <div className="policy-block" aria-label="Access policy">
            {selected.access.map((line) => (
              <div className="policy-line" key={line.label}>
                <span className={`policy-chip ${line.status === "supported" ? "is-ok" : ""}`}>{accessStatusLabel[line.status]}</span>
                <p className="policy-text">
                  <strong>{line.label}</strong>
                  {line.detail}
                </p>
              </div>
            ))}
            <p className="policy-note">
              Verdicts come from each provider's official documentation, verified 2026-09-19. REX connects an account only where the
              provider clearly permits third-party harness use. Full matrix: docs/subscription-policy.md.
            </p>
          </div>
        )}

        <div className="provider-form">
          <label>
            <span>Display name</span>
            <input value={draft.displayName} onChange={(e) => update({ displayName: e.target.value })} />
          </label>
          <label>
            <span>Protocol</span>
            <input value={protocolLabel[selected.protocol]} disabled />
          </label>
          {isCompatible && (
            <label className="provider-field-wide">
              <span>Base URL</span>
              <input
                value={draft.baseUrl}
                placeholder="https://api.example.com/v1"
                spellCheck={false}
                onChange={(e) => update({ baseUrl: e.target.value })}
              />
              <small>Editable preset. Compatibility is checked per endpoint, not assumed.</small>
            </label>
          )}
          <label>
            <span>API key</span>
            <button type="button" className="locked-field" disabled>
              {backend ? (liveState?.has_key ? "Stored by Rust backend" : "Not stored") : "Desktop keychain"}{" "}
              <b>{backend ? (liveState?.has_key ? "Connected" : "Missing") : "Pending runtime"}</b>
            </button>
            <small>{backend ? "Keys live in the backend credential store, never in this interface. Add one from the model menu in the task box." : "No key is entered or stored in this frontend preview."}</small>
          </label>
          <label>
            <span>Model discovery</span>
            <button type="button" className="locked-field" disabled>
              {discoveryLabel} <b>{backend ? (liveState?.catalog ? `${liveState.catalog.models.length} models` : "Ready") : "Unavailable"}</b>
            </button>
            <small>{backend ? (liveState?.catalog ? `Fetched ${liveState.catalog.source === "live" ? "live" : "from a recorded live response"}.` : "Fetch runs from the model menu or Test connection.") : "REX will discover where supported, with manual entry as fallback."}</small>
          </label>
          <label className="provider-field-wide">
            <span>Default model ID</span>
            <input
              value={draft.modelId}
              placeholder="Enter the exact provider model ID"
              spellCheck={false}
              onChange={(e) => update({ modelId: e.target.value })}
            />
          </label>
        </div>

        <div className="provider-actions">
          {testResult && <span className="provider-err-line" role="status" style={{ marginRight: "auto" }}>{testResult}</span>}
          <button
            type="button"
            className="test-button"
            disabled={!backend || !liveState?.has_key || testing}
            title={!backend ? "Needs the desktop runtime or dev sidecar" : !liveState?.has_key ? "Store an API key first (model menu in the task box)" : "Fetch the live model catalog"}
            onClick={async () => {
              setTesting(true);
              setTestResult(null);
              const result = await refreshCatalog(selected.id);
              if (result.ok) {
                setTestResult(`Live: ${result.catalog.models.length} models from ${selected.name}`);
                try {
                  const summaries = await listSummaries();
                  setLive(Object.fromEntries(summaries.map((s) => [s.id, s])));
                } catch { /* ignore */ }
              } else {
                setTestResult(describeError(result.error));
              }
              setTesting(false);
            }}
          >
            {testing ? "Testing…" : "Test connection"}
          </button>
          <button type="button" className="stage-button" onClick={() => update({ status: "needs-runtime" })}>{draft.status === "needs-runtime" ? "Draft kept in memory" : "Keep draft for this session"}</button>
        </div>
        <div className="provider-truth" role="note">
          <span aria-hidden="true">i</span>
          <p>
            {backend
              ? "Connected to the Rust provider backend. Keys sit in its credential store; this page only reads status and model counts."
              : "Configuration UI only. Live tests, encrypted persistence, model discovery, and inference need the desktop runtime. Draft fields stay in memory and disappear when this preview closes."}
          </p>
        </div>
        {isCompatible && (
          <p className="compat-note">
            OpenAI-compatible means a shared request shape, not identical behavior. Streaming, tools, reasoning fields, model listing, and authentication can differ by provider.
          </p>
        )}
      </section>
    </div>
  );
}
