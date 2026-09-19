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
    </div>) : <div className="settings-row"><span className="text-sm text-text">Desktop detection</span><span className="text-xs text-faint">Open the REX app</span></div>}
    <p className="settings-note">Compatibility reviewed 19 Sep 2026. If a vendor changes its documented interface or entitlement, REX fails closed until the adapter is reviewed. Google Antigravity v1.2.7 is protocol-tested from its verified official artifact and headless contract; live subscription behavior is not yet verified.</p>
  </section>;
}
