# Agent browser verification (2026-09-18)

Build: `npm run build` (tsc + vite) green at commit after 6acc818.

Screenshots (verification/browser/):
- 01-loading-1440.png - skeleton loading state
- 02-live-1440.png - simulated product page, SAMPLE chip, intent line, trail
- 03-targeting-1440.png / 04-targeted-1440.png - targeting armed, #price selected
- 05-permission-1440.png - permission card (Allow once / Deny)
- 06-error-1440.png - sample load error + retry
- 07-done-1440.png - cart live, receipt complete, evidence shots, follow-up
- 08-takeover-1440.png - handover banner, agent paused
- 09-denied-1440.png - denied path, agent stopped honestly
- 10-ultra-off-returns-task-1440.png - Ultra off from browser view returns to task, no artifacts
- 11-live-390.png / 12-permission-390.png - mobile
- 13-reduced-1440.png - reduced motion
- agent-browser-flow.webm - full flow recording

Checks: INV-01 DOM diff byte-identical vs pristine 6acc818; overflow clean 390/1440 all states;
keyboard walk; reduced motion; heap flat; frames 91 vs 150 during active mock (headless, honest).
Known limits: no real browser runtime; all pages/actions are local sample data; back/forward are
disabled sample controls by design; headless performance is not production parity.
