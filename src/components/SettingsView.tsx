import { SearchProviderSettings } from "./SearchProviderSettings";
import type { MotionPref } from "../data/motion";
export type { MotionPref };
const MOTION_OPTIONS: { id: MotionPref; label: string; hint: string }[] = [
  { id: "system", label: "System", hint: "Follow the device setting" },
  { id: "reduce", label: "Reduce", hint: "Minimal animation" },
  { id: "full", label: "Full", hint: "All transitions" },
];
export function SettingsView({ motion, setMotion, onReset }: { motion: MotionPref; setMotion: (m: MotionPref) => void; onReset: () => void }) {
  return <div className="mt-3 sm:mt-7">
    <div className="mb-6 sm:mb-8"><p className="eyebrow">Settings</p><h1 className="mt-2 text-[30px] font-medium leading-[1.12] tracking-[-0.04em] text-text sm:text-[38px]">Quiet defaults, honest state.</h1></div>
    <section aria-label="Runtime" className="settings-section"><h2 className="eyebrow">Runtime</h2><div className="settings-row settings-row-wrap"><span className="flex items-center gap-3"><span className="h-1.5 w-1.5 rounded-full bg-done" aria-hidden="true" /><span className="text-sm text-text">Local tools protected</span></span><span className="text-xs text-faint">Workspace only</span></div><p className="settings-note">REX can read and search the selected workspace. File changes and commands pause for your approval. Paths outside the workspace, symlink escapes, risky commands, and unrestricted shell access are blocked in Rust.</p></section>
    <SearchProviderSettings />
    <section aria-label="Appearance" className="settings-section"><h2 className="eyebrow">Appearance</h2><div className="settings-row settings-row-wrap"><span className="text-sm text-text">Motion</span><div className="seg-control" role="radiogroup" aria-label="Motion preference">{MOTION_OPTIONS.map((option) => <button key={option.id} type="button" role="radio" aria-checked={motion === option.id} title={option.hint} className={`seg-option ${motion === option.id ? "is-on" : ""}`} onClick={() => setMotion(option.id)}>{option.label}</button>)}</div></div><p className="settings-note">Applies immediately and is saved on this device only.</p></section>
    <section aria-label="Keyboard shortcuts" className="settings-section"><h2 className="eyebrow">Keyboard</h2><dl className="shortcut-list"><div><dt>Ctrl + Enter</dt><dd>Run the task, or send a follow-up</dd></div><div><dt>Esc</dt><dd>Close an open menu or dialog</dd></div><div><dt>Tab</dt><dd>Move through controls</dd></div></dl></section>
    <section aria-label="Preview data" className="settings-section"><h2 className="eyebrow">Preview data</h2><div className="settings-row settings-row-wrap"><span className="text-sm text-text">Local preferences</span><button type="button" className="reset-button" onClick={onReset}>Reset to defaults</button></div><p className="settings-note">Clears preferences stored on this device. Sample runs are built into the preview and are not affected.</p></section>
    <section aria-label="Support" className="settings-section"><h2 className="eyebrow">Support</h2><p className="settings-note">REX Harness is built independently by one student. If it is useful to you, a <a className="support-link" href="https://github.com/REX-codebase/rex-harness" target="_blank" rel="noreferrer">star on GitHub</a> helps REX grow. If you can, a <a className="support-link" href="https://github.com/sponsors/REX-codebase" target="_blank" rel="noreferrer">donation</a> supports the independent student building it.</p></section>
    <p className="mt-8 text-xs leading-relaxed text-faint">REX Harness · Preview build · Local tools are enforced by the desktop runtime.</p>
  </div>;
}
