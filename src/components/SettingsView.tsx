export type MotionPref = "system" | "reduce" | "full";

const MOTION_OPTIONS: { id: MotionPref; label: string; hint: string }[] = [
  { id: "system", label: "System", hint: "Follow the device setting" },
  { id: "reduce", label: "Reduce", hint: "Minimal animation" },
  { id: "full", label: "Full", hint: "All transitions" },
];

export function SettingsView({ motion, setMotion, onReset }: { motion: MotionPref; setMotion: (m: MotionPref) => void; onReset: () => void }) {
  return (
    <div className="mt-3 sm:mt-7">
      <div className="mb-7 sm:mb-9">
        <p className="eyebrow">Settings</p>
        <h1 className="mt-2 text-[30px] font-medium leading-[1.12] tracking-[-0.04em] text-text sm:text-[38px]">Quiet defaults, honest state.</h1>
      </div>

      <section aria-label="Runtime" className="settings-section">
        <h2 className="eyebrow">Runtime</h2>
        <div className="settings-row">
          <span className="flex items-center gap-3">
            <span className="h-1.5 w-1.5 rounded-full bg-blocked" aria-hidden="true" />
            <span className="text-sm text-text">Not connected</span>
          </span>
        </div>
        <p className="settings-note">
          REX Harness is a frontend preview. Runs are simulated by a local preview engine and
          labeled as sample data. When a runtime is connected, its models and real runs will
          appear here and in the task view.
        </p>
      </section>

      <section aria-label="Appearance" className="settings-section">
        <h2 className="eyebrow">Appearance</h2>
        <div className="settings-row settings-row-wrap">
          <span className="text-sm text-text">Motion</span>
          <div className="seg-control" role="radiogroup" aria-label="Motion preference">
            {MOTION_OPTIONS.map((o) => (
              <button
                key={o.id}
                type="button"
                role="radio"
                aria-checked={motion === o.id}
                title={o.hint}
                className={`seg-option ${motion === o.id ? "is-on" : ""}`}
                onClick={() => setMotion(o.id)}
              >
                {o.label}
              </button>
            ))}
          </div>
        </div>
        <p className="settings-note">Applies immediately and is saved on this device only.</p>
      </section>

      <section aria-label="Keyboard shortcuts" className="settings-section">
        <h2 className="eyebrow">Keyboard</h2>
        <dl className="shortcut-list">
          <div>
            <dt>Ctrl + Enter</dt>
            <dd>Run the task, or send a follow-up</dd>
          </div>
          <div>
            <dt>Esc</dt>
            <dd>Close an open menu</dd>
          </div>
          <div>
            <dt>Tab</dt>
            <dd>Move through controls</dd>
          </div>
        </dl>
      </section>

      <section aria-label="Preview data" className="settings-section">
        <h2 className="eyebrow">Preview data</h2>
        <div className="settings-row settings-row-wrap">
          <span className="text-sm text-text">Local preferences</span>
          <button type="button" className="reset-button" onClick={onReset}>
            Reset to defaults
          </button>
        </div>
        <p className="settings-note">
          Clears preferences stored on this device (like the motion setting). Sample runs are
          built into the preview and are not affected.
        </p>
      </section>

      <p className="mt-8 text-xs leading-relaxed text-faint">REX Harness · Preview build · Frontend only, no backend connected.</p>
    </div>
  );
}
