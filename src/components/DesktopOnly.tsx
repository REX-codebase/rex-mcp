// Git, checkpoints, the terminal and the editor act on files on this
// machine, so they run only inside the desktop shell (Tauri). In the
// browser preview there is no bridge to call; show what the tab needs
// instead of a raw "cannot read properties of undefined" error.

export function isDesktopRuntime(w: unknown = typeof window === "undefined" ? undefined : window): boolean {
  return Boolean(w && (w as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__);
}

const COPY: Record<string, { title: string; body: string }> = {
  terminal: { title: "The terminal runs in the desktop app", body: "It opens a real shell in your workspace, which a browser tab cannot do." },
  editor: { title: "The editor runs in the desktop app", body: "It reads and writes files in your workspace, which a browser tab cannot do." },
  git: { title: "Git and checkpoints run in the desktop app", body: "They read your repository and snapshot your files, which a browser tab cannot do." },
};

export function DesktopOnly({ feature }: { feature: "terminal" | "editor" | "git" }) {
  const c = COPY[feature];
  return (
    <section className="desktop-only" aria-label={c.title}>
      <span className="desktop-only-icon" aria-hidden="true">
        <svg width="20" height="20" viewBox="0 0 20 20"><rect x="2.5" y="3.5" width="15" height="10" rx="2" fill="none" stroke="currentColor" strokeWidth="1.4" /><path d="M7 16.5h6M10 13.5v3" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" /></svg>
      </span>
      <h2 className="desktop-only-title">{c.title}</h2>
      <p className="desktop-only-body">{c.body}</p>
      <p className="desktop-only-cmd">Start it with <code>npm run tauri dev</code></p>
    </section>
  );
}
