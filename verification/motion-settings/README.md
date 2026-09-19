# Motion settings verification

`verify-motion.mjs` drives the production build (`npm run build`, then
`npm run preview -- --port 4173 --strictPort --host 127.0.0.1`) in headless
Chromium and asserts the Settings > Motion contract end to end:

1. Default is System with full motion under an OS no-preference.
2. Reduce is chosen, persisted to localStorage, and survives a reload.
3. Reset to defaults returns to System.
4. System mode follows live OS `prefers-reduced-motion` flips without reload.
5. Full overrides an OS-level reduce: hero animation runs at 380ms and the
   Fast transition overlay renders.
6. Reduce under an OS no-preference calms every layer (hero 0.01ms, no Fast
   overlay, instant dropdown) while Fast still arms and menus still open.
7. Full keeps the dropdown's intended 220ms entrance.

Results: `checks.json`; decisive states in `01`-`06` PNGs.
`shot-appearance.mjs` captures the Appearance section itself.
