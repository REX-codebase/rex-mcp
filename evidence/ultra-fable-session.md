# Ultra transformation - Fable workflow receipt

## THINK
Objective: make a future premium preview feel like the Harness changes state, while claiming no capability.

Concepts explored:
1. Purple energy skin - rejected as a generic AI treatment and too close to a theme swap.
2. Full mechanical panels and dense HUD - rejected for noise, legibility risk, and excess compositor work.
3. Precision-machine wake - selected. Four restrained light shutters establish the transition, then the layout widens, geometry tightens, typography gains tracking, a faint perspective chassis appears, and warm metal replaces action purple.

## PROVE
- Current UI is a frontend-only React preview with provider Settings and truthful not-connected model state.
- Ultra can be isolated to App and TopBar state plus CSS. No provider, run, history, logo, or sample-data logic needs to change.
- CSS transform/opacity and one repeating scan layer keep the active animation compositor-oriented.

## INVARIANTS
- Ultra never claims reasoning, tools, speed, models, runtime connectivity, or working premium behavior.
- Ultra is keyboard operable and always has a visible off path.
- The choice is ephemeral by design: refresh returns to standard Harness. No new local-storage key.
- Reduced motion keeps the complete static design change and removes transition choreography.
- No horizontal page overflow at desktop or mobile sizes.

## ATTACK
- Toggle on/off repeatedly; activate off with Enter.
- Check 1440x900, 390x844, and reduced-motion rendering.
- Inspect localStorage before/on/off to confirm Ultra is not persisted.
- Check document scroll width against client width.
- Keep model status and premium limitation visible while Ultra is on.

## WRITE
Touched only `src/App.tsx`, `src/components/TopBar.tsx`, `src/index.css`, this receipt, and the verification report.
