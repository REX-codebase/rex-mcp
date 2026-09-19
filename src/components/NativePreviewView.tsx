import { useEffect, useRef, useState } from "react";
import { previewAction, previewCancel, previewCapture, previewStart, previewTeardown, type PreviewBrowserAction, type PreviewSessionSummary } from "../data/previewRuntime";

type Evidence = { items: unknown[]; screenshot_data_url?: string; dom_text: string; accessibility_text: string };
export function NativePreviewView({ projectDir, onClosed }: { projectDir: string; onClosed?: () => void }) {
  const [session, setSession] = useState<PreviewSessionSummary | null>(null);
  const [evidence, setEvidence] = useState<Evidence | null>(null);
  const [error, setError] = useState("");
  const [cursor, setCursor] = useState({ x: 24, y: 24 });
  const id = useRef<string | null>(null);
  const send = async (action: PreviewBrowserAction) => { if (!id.current) return; await previewAction(id.current, action); };
  useEffect(() => {
    let active = true;
    previewStart(projectDir).then((s) => { if (!active) return; id.current = s.id; setSession(s); return previewCapture(s.id).then(setEvidence); }).catch((e) => setError(String(e)));
    return () => { active = false; if (id.current) void previewTeardown(id.current); };
  }, [projectDir]);
  const move = (e: React.PointerEvent<HTMLDivElement>) => { const r=e.currentTarget.getBoundingClientRect(); const next={x:e.clientX-r.left,y:e.clientY-r.top}; setCursor(next); void send({kind:"pointer_move",...next}); };
  const click = async () => { await send({kind:"pointer_down",button:"primary"}); await send({kind:"pointer_up",button:"primary"}); if (id.current) setEvidence(await previewCapture(id.current)); };
  return <section className="preview-runtime" aria-label="Native live app preview">
    <header className="preview-runtime-head"><div><p className="eyebrow">Native live app preview</p><h2>{session?.framework ?? "Starting"}</h2><p>Rust supervisor · Chrome DevTools Protocol · grounded capture</p></div><div className="preview-runtime-actions"><span className={`preview-runtime-state is-${session?.state ?? "starting"}`}>{session?.state ?? "starting"}</span><button onClick={async()=>{if(id.current) await previewCancel(id.current); onClosed?.();}}>Cancel preview</button></div></header>
    {error ? <p role="alert">{error}</p> : <div className="preview-runtime-frame"><div className="preview-runtime-nav"><span className="preview-runtime-dot is-red"/><span className="preview-runtime-dot is-yellow"/><span className="preview-runtime-dot is-green"/><span className="preview-runtime-url">{session?.url}</span><span className="preview-runtime-viewport">1280 × 800</span></div><div className="preview-runtime-canvas native-preview-canvas" onPointerMove={move} onClick={click}>{evidence?.screenshot_data_url ? <img src={evidence.screenshot_data_url} alt="Real rendered preview capture"/> : <p>Starting real rendered preview…</p>}<span className="agent-cursor" style={{left:cursor.x,top:cursor.y}}><i>REX</i></span></div></div>}
    <div className="preview-evidence-grid"><div className="preview-receipt"><p className="eyebrow">Grounded evidence</p><div><span>Evidence items</span><b>{evidence?.items.length ?? 0}</b></div><div><span>DOM bytes</span><b>{evidence?.dom_text.length ?? 0}</b></div><div><span>Accessibility bytes</span><b>{evidence?.accessibility_text.length ?? 0}</b></div><button disabled={!id.current} onClick={async()=>{if(id.current)setEvidence(await previewCapture(id.current));}}>Capture now</button></div></div>
  </section>;
}
