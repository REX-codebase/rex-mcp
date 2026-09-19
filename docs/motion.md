# Motion preference

Settings > Appearance > Motion has three modes. The in-app choice always wins
over the OS; System is the only mode that delegates to it.

- **System** follows the device's `prefers-reduced-motion` setting, live. If
  the OS setting flips while the app is open, the interface updates without a
  reload (`useSyncExternalStore` over a `matchMedia` change listener).
- **Reduce** is a calm but fully usable interface: animations and transitions
  are instant, ambient effects (Ultra blades, Fast iris, shimmer skeletons,
  pulsing state dots) are removed, and timed mock sequencing shortens. Every
  control still works; nothing is only hidden.
- **Full** is the complete approved motion design. It applies even when the
  OS asks for reduced motion, because the user explicitly chose it here.

## Implementation

- `src/data/motion.ts` owns the contract: `MotionPref`, storage under the
  `rex-harness-motion` localStorage key (this device only), `resolveReduced`,
  and the live OS subscription.
- The shell carries `motion-<mode>` and, when resolved, `reduced-fx`. All
  reduced-motion suppression is scoped to those classes. The raw
  `@media (prefers-reduced-motion: reduce)` block applies only under
  `.motion-system`, so an explicit Reduce or Full choice is never overridden
  by the OS.
- JS-driven timing (hero settle, Fast engage/disengage, mock and browser
  turn sequencing) reads the same resolved `reduced` flag.

## Tests

- `npm test` (vitest): parsing, storage fallback, the System/Reduce/Full
  resolution matrix, and the OS subscription including the legacy
  `addListener` path.
- `node verification/motion-settings/verify-motion.mjs` (Playwright, real
  Chromium): persistence across reload, Reset to defaults, live OS flips in
  System mode via `page.emulateMedia`, Full overriding an OS-level reduce
  (hero animation 380ms, Fast transition overlay renders), Reduce calming
  every animation layer (hero 0.01ms, no Fast overlay, instant model menu)
  while controls stay usable. Screenshots and `checks.json` land beside the
  script.
