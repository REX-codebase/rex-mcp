import { useMemo, useState } from "react";
import { PROVIDER_PRESETS, protocolLabel, type ProviderDraft, type ProviderPreset } from "../data/providers";

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
              Desktop keychain <b>Pending runtime</b>
            </button>
            <small>No key is entered or stored in this frontend preview.</small>
          </label>
          <label>
            <span>Model discovery</span>
            <button type="button" className="locked-field" disabled>
              {discoveryLabel} <b>Unavailable</b>
            </button>
            <small>REX will discover where supported, with manual entry as fallback.</small>
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
          <button type="button" className="test-button" disabled>Test connection</button>
          <button type="button" className="stage-button" onClick={() => update({ status: "needs-runtime" })}>{draft.status === "needs-runtime" ? "Draft kept in memory" : "Keep draft for this session"}</button>
        </div>
        <div className="provider-truth" role="note">
          <span aria-hidden="true">i</span>
          <p>
            Configuration UI only. Live tests, encrypted persistence, model discovery, and inference need the desktop runtime. Draft fields stay in memory and disappear when this preview closes.
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
