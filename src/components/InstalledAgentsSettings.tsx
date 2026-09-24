import { useEffect, useState } from "react";
import { backendKind, listInstalledAgents, type InstalledAgentSummary } from "../data/backend";

const STATE: Record<InstalledAgentSummary["state"], string> = {
  ready: "Ready",
  installed_auth_unknown: "Installed - check sign-in",
  missing: "Not installed",
  unsupported_version: "Update required",
  support_needs_review: "Support needs review",
};

export function InstalledAgentsSettings() {
  const [items, setItems] = useState<InstalledAgentSummary[]>([]);
  const [desktop, setDesktop] = useState(false);
  useEffect(() => { backendKind().then(async (kind) => { if (kind !== "tauri") return; setDesktop(true); try { setItems(await listInstalledAgents()); } catch { setItems([]); } }); }, []);
  return <section aria-label="Installed agent backends" className="settings-section">
    <h2 className="eyebrow">Installed agent backends</h2>
    <p className="settings-note">REX delegates bounded work to the vendor's own CLI. That CLI keeps its login and subscription. REX never copies tokens or cookies, and the child cannot approve REX actions.</p>
    {desktop ? items.map((item) => <div className="settings-row settings-row-wrap" key={item.id}>
      <span><span className="text-sm text-text">{item.name}</span><small className="block text-xs text-faint">{item.automation}</small></span>
      <span className="text-xs text-faint" title={`${item.compatibility.entitlement_note}. ${item.approval_boundary}`}>{STATE[item.state]}{item.version ? ` · ${item.version}` : ""}</span>
    </div>) : <div className="settings-row"><span className="text-sm text-text">OpenAI Codex CLI</span><span className="text-xs text-faint">Detected in the desktop app</span></div>}
    <details className="settings-more">
      <summary>Why only Codex CLI, and how REX checks it</summary>
      <p className="settings-note">Only OpenAI Codex CLI is offered: OpenAI documents codex exec for non-interactive automation and the CLI keeps its own ChatGPT or API-key sign-in. The Antigravity, Claude Code, Cursor and Pi routes were removed on 19 Sep 2026 because their providers' current terms do not permit third-party harness use of a consumer subscription.</p>
      <p className="settings-note">Compatibility reviewed 19 Sep 2026. REX probes the installed CLI offline and fails closed - no runs - if its documented interface, version support, or terms basis no longer matches the reviewed contract.</p>
    </details>
  </section>;
}
