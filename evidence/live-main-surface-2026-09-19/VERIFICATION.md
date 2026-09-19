# Live main-surface demo — verification notes (2026-09-19)

## What this proves
The real model-run lifecycle lives inside the approved REX Harness main task
frontend, end to end, with no separate verification window:

1. Task typed into the hero composer of the main task surface.
2. Live Gemini catalog refresh (41 models) and a real generateContent turn on
   `gemini-3.5-flash-lite` (picked from the live catalog at run time).
3. One restrained in-context approval card ("REX wants to change files",
   Approve once / Deny). Nothing executes before the click.
4. Trusted approval executes the prepared write through the Rust tool runtime;
   the receipt (11,738 B written, 14 ms) collapses into a details row.
5. The native preview takes over as the dominant task surface: real headless
   Chrome + CDP, bounded iteration gates (iteration 1 rejected - desktop
   capture only, missing mobile_viewport gate; iteration 2 accepted - mobile
   390x844 + desktop 1280x800 captures, no console/network failures).
6. The visible REX cursor travels to the model-built "Reserve a piece" button
   and clicks it; pointer events reach the real page via CDP and the capture
   refreshes.
7. Evidence (iterations, DOM/AX/console/network) stays behind one toggle.

## How it was recorded
- Xvfb 1280x800 display, ffmpeg x11grab 30 fps, H.264 yuv420p +faststart
  (phone-playable). Full-session capture trimmed to the flow (43 s).
- Playwright drove the real UI: physical typing, one click on Approve once,
  pointer travel over the live canvas, one click on the model-built button.
- The preview Chrome is headless (`--headless=new`); only the driven window
  appears in the recording.
- Frames were inspected after capture: approval card, preview live with REX
  cursor mid-travel, cursor resting on the Reserve button with evidence open.
- Static build served from 127.0.0.1:4173; sidecar rex-dev-server on
  127.0.0.1:8787 with the provider key in its encrypted store. Keys never
  appear in snapshots, logs, this evidence directory, or the recording.

## State checks
- Mock surface unchanged when no backend is reachable (INV-01): the sidecar
  probe runs only on vite dev or localhost-served builds.
- TopBar reads LIVE - REAL RUN while a backend is connected and PREVIEW -
  SAMPLE DATA otherwise.
- rex-providers test suite: 15 + 29 passed, including live approve -> preview
  with real Chrome capture. Frontend: tsc clean, vite build clean.

## Commits
- 0afc827 Run live model tasks inside the main task surface
- c55310b Probe the sidecar backend on localhost production builds
- 71132a7 Fix live canvas squeeze and truthful backend badge

## Files
- rex-live-main-surface-demo.mp4 - the recording (43 s)
- 01-approval-in-context.png - the approval moment
- 02-preview-live-rex-cursor.png - preview dominant, REX cursor mid-travel
- 03-cursor-on-reserve-evidence.png - cursor on the model-built button,
  evidence toggle open with rejected + accepted iterations
- 04-mobile-390-live.png / 05-desktop-1440-live.png - pixel passes
- run-snapshot-sanitized.json - final run snapshot (data-url screenshots
  stripped; no credentials)
