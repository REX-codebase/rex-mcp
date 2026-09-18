# Agent browser (Ultra-only) - Fable session receipt

Real Fable Engine session `rex-agent-browser-surface` (fable-engine via `fable-mode call`, one long-lived process, session JSON at ~/.local/share/fable-engine/data/sessions/rex-agent-browser-surface.json).

## Grounding (THINK)
- 4 PROVEN with on-disk evidence: current app structure (App/TopBar/index.css reads), owner scope constraint ("Only in ultra", WhatsApp 2026-09-18 19:55 IST), mock-engine truth discipline, commit-path constraint.
- 1 HYPOTHESIS (dedicated Browser view is the smallest coherent shape), 1 UNKNOWN (mobile overflow, later measured).
- Design thesis: the agent browser is an instrument surface - a simulated page under observation - not browser chrome. One page-first viewport, one intent line, one action trail.

## Concepts (3 structurally different, mutation cycle)
- A: dedicated Browser workbench view, page-first viewport, thin intent line (selected).
- B: browser embedded in SessionView beside turns (rejected: couples browser state to task turns, clutters receipts).
- C: takeover-centric stage with edge overlays and bottom scrubber (rejected as primary; its explicit takeover handoff kept as a banner state).

## Invariants
- INV-01 Ultra-only scope: standard-mode rendered DOM identical to commit 6acc818; zero browser artifacts with Ultra off.
- INV-02 Truthful simulation: no real navigation/network/credential/page-access claims; SIMULATED PAGE chip inside viewport.
- INV-03 Layout/a11y: no horizontal overflow 360-1440 in any state; keyboard reachable; reduced motion functional.

## Gates
- 3 rethink-refine cycles: concept mutation, invariant stress, attack-loop adjustment.
- Unlock after full 2-minute authority budget: UNLOCKED receipt at Phase 3.
- Rubric: 98% composite (5 weighted criteria, receipts per pointer).

## ATTACK
- fable red_team_code_review refuses non-Python targets by design; executable probes ran in the host (Playwright) as the skill prescribes.
- Breakages found and fixed: (1) targeted-element tag persisted across navigation - cleared on navigate, re-verified; (2) takeover banner overlaid SAMPLE chip/brand line - banner made in-flow, chip shifted, pixels re-verified.
- No unresolved risk.

## Receipts (WRITE)
- tsc + Vite production build green.
- INV-01: standard-mode DOM byte-identical to pristine baseline (Playwright, new vs 6acc818 tree).
- Overflow: scrollWidth == clientWidth at 1440 and 390 in loading/live/error/permission/takeover/denied/done.
- Keyboard: Tab walk reaches Restart, Target, Take control, Deny; aria-pressed on Target; Escape exits targeting.
- Reduced motion: shimmer disabled, pacing shortened, full function kept.
- Performance: JS heap flat (10 MB coarse counter before/after); frame cadence 91 vs 150 over ~2.5s during the active mock (timers + shimmer) in this headless container - no zero-cost claim, CSS-only animations.
- Screenshots + full-flow recording in verification/browser/.
