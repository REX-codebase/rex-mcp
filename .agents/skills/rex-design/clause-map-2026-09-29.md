# Clause-to-clause preservation map: rex-design rework (pass 6)

**Base:** `REX-codebase/rex-mcp` main @ `7616c7b0c79017e83cb2c2d20d430f83b82685c2` (`.agents/skills/rex-design/SKILL.md` 193 content lines / 12,273 words; `references/visual-review-cases.md` 40 content lines / 2,309 words).
**Counting convention (reconciles the pass-1 41/42/43 confusion):** "content lines" exclude the repository's trailing blank line; every committed markdown file carries that blank, so `wc -l` = content + 1. Base VRC = 41 by `wc -l` / 40 content; pass-1 VRC = 42 / 41; pass-2 VRC = 79 / 78; pass-3 VRC = 82 / 81; pass-4 and pass-5 VRC = 84 / 83. Base SKILL = 194 / 193; pass-1 through pass-5 SKILL = 210 / 209.
**Result (pass 6):** SKILL.md 209 content lines / 12,542 words; visual-review-cases.md 83 content lines / 4,375 words (unchanged since pass 4).
**Method:** the pass-1 map was completed before prose cuts. Pass 2, pass 3 and pass 4 implement successive Critic + Red Team NO-MERGE findings; pass 5 implements the blind-validation and USGS-reviewer defect list; pass 6 implements the Pico blind-recheck phone-first-view defects; every finding maps to an edit in the change logs below. Status: kept = verbatim or near-verbatim; clarified = same requirement, wording improved; merged = combined with an overlapping clause, all requirements retained; moved = relocated, text intact; strengthened = trigger/evidence/exception made stricter; added = new clause; restored = base clause that pass 1 silently dropped, put back in pass 2.

## Pass-2 change log (review finding -> edit)

| Finding | Edit | Where |
|---|---|---|
| Red Team H1(a) | Restored the guard "a preselected example or pagination through presets is not equivalent", generalized to any brief with several independent choices | SKILL L45 (result contract) |
| Red Team H1(b) | Restored the operative definition of "structurally different": different organizing ideas (for example, object-led versus task-led), not two skins of the same hero; choose by the brief's evidence, audience and delivery frame; if neither has enough real material, seek it or narrow the claim | SKILL L67 (concept gate) |
| Red Team H1(c) | Restored "an editorial or object-led hero is free to avoid a widget, but it cannot quietly replace a requested choice-and-result product with a mood image or slogan" | SKILL L43 |
| Red Team H2 | "Narrow" defined: one component or state with the existing UI as baseline; any change to the first view, information hierarchy, or promised outcome forces the full gate; each skipped record section quotes the trigger that fails to fire; the declared scope carries into the handoff | SKILL L63 (definition), L22 (fast path), L28 (trigger quotes), L177 (cross-reference), L209 (handoff) |
| Red Team M3 | The builder must not simulate the blinded reviewer; when no non-builder is available, record the blinded review as not performed and the verdict as provisional | SKILL L203 |
| Red Team M4 | Scoping line at the top of the concept gate: for Production or multi-route work, the first-view and visual-language tests apply to task legibility (status, data, next action), not focal-subject art direction | SKILL L63 |
| Red Team L5 | 1920px and 320px are the default stress widths, waivable only with a recorded reason tied to the brief's audience and devices | SKILL L183 |
| Red Team L6 | The width-freeze rule is stated in the fast path with a cross-link to Implement and verify | SKILL L23 |
| Red Team L7 | The taste adversary's veto must name the specific harm with evidence: the state and the pixels where it occurs | SKILL L107 |
| Critic P1-1 | Editorial pass: the 1,843-word interaction paragraph became a 250-word executable core plus a pointer, with its 16 specialized workflow catalogs moved verbatim into the reference's new "Interaction workflow cases" section; the result contract (364 -> 345 words) and ownable proof (433 -> 437 words) paragraphs were restructured around a short executable sequence, with the workout-specific illustrations moved to the reference's "Result-contract worked examples" case; the A-F record bullets were collapsed to record contents, with D/E/F pointing to the governing sections instead of restating them | SKILL L128, L45, L47, L28-35; VRC L30, L44-78 |
| Critic P1-2 | Contradictions fixed: D's trigger and the L28 narrow-patch example aligned ("spans more than one consequential state" vs "no promised result and no new consequential state"); "every width" became "declared sample widths, with probes at each breakpoint edge and in the intervals between breakpoints" (L189); "when nothing useful needs changing or cutting" (L205); the taste adversary may close on an evidence-backed stop at the revision cap or a genuinely clean iteration - never invent a flaw (L107) | SKILL L33(D), L28, L189, L205, L107 |
| Critic P1-3 | The quiet-index / multi-route exception now stands at the operative gates, not only the preamble (L14): ownable-proof gate (L47), product-evidence gate (L83), and the reference's landing-hero check (VRC L34) | SKILL L47, L83; VRC L34 |
| Critic P1-4 | This map: the reference L3 row now quotes base and new text (see VRC inventory); line counts reconciled under the convention above | this file |
| Critic P2 | "Arms" defined at first use and at the reviewer protocol: the compared variants of a comparative validation; a narrow patch has none, and a compact route and status note is enough for it. L59's taste/task-clarity/phone-integrity scores are recorded as separate informal diagnostics, never passing evidence | SKILL L107, L203, L59 |

## Pass-3 change log (re-review findings -> edits)

| Finding | Edit | Where |
|---|---|---|
| Critic 1 (HIGH) | Full gate is now forced only by a material change to the first-view organizing idea, the primary task hierarchy, or the promised outcome; a local visible correction still gets the affected-width and affected-state review | SKILL L22, L63, L177 |
| Critic 2 (MED) | The substantive-weakness record is conditional on an observed weakness; a clean result is recorded explicitly rather than manufacturing one; missing assets or states recorded either way | SKILL L59 |
| Critic 3 (MED) | The taste-adversary protocol (second composition, data-first comparator, independent critic, frozen prior render) is scoped to full Visual design iterations; a narrow Production change reviews the affected task and state and preserves the baseline | SKILL L107 |
| Critic 4 + Red Team 1 (A/F loophole) | A and F always written; B-E only when their triggers fire; Production work always writes D (one row per touched state - a narrow patch may have exactly one row); design or redesign work always writes E; a narrow Production example added; the A/F-only case narrowed to non-Production patches with no promised result, no new consequential state and no design change | SKILL L28, L33 |
| Critic 5 (LOW) | "Result-contract worked examples" moved out of the failure-class list into its own heading | VRC L31 |
| Red Team 2 (H) | Load rule changed to "without either trigger" in both files; match-and-load made imperative with the link at the interaction core | SKILL L114, L128; VRC L3 |
| Red Team 3 (M) | Interaction-paragraph preservation count corrected with a reproducible method (see Checks) | this file |
| Red Team 4 (L) | A provisional verdict (no non-builder) is not a completed quality pass; independent review carries into the handoff as an open acceptance item | SKILL L203 |

## Pass-4 change log (final re-review findings -> edits)

| Finding | Edit | Where |
|---|---|---|
| Critic 1 (HIGH) | B's record trigger now includes the full-gate material triggers verbatim - "a material change to the first-view organizing idea, the primary task hierarchy, or the promised outcome - the same changes that force the full concept gate" - so a material change can no longer force the full gate (L22/63/177) while skipping the B record | SKILL L31 |
| Critic 2 (MED) | The result-contract heading is closed before the dogfooding paragraph; the six general cases (dogfooding, landing hero, causal behavior, continuous controls, responsive/fallback, compact regression) now sit under their own heading "### Composition, behavior, and resilience cases", so match-only loading of the result-contract section no longer swallows them | VRC L35 (new heading); former L35-45 shift to L37-47 |
| Critic 3 (LOW) | The fragment-count parenthesis now lists exactly which 8 of the 16 opener-bearing sentences were folded into case names; the other 8 stand verbatim or after normalization only (see Checks) | this file, Checks |


## Pass-5 change log (blind validation + USGS reviewer defects -> edits)

Pass 5 adds guidance only; no existing clause was weakened or removed, and the sections the blind validation scored as wins (art direction, drawn illustrative sequences, task recovery, data semantics, custody depth) were not touched.

| Finding | Edit | Where |
|---|---|---|
| Pico loss: tall product photo squeezed beside text on phone (imagery 5.8, responsive 5.8) | Tall imagery goes full width above or below its text at phone widths, never a narrow side column; crop and orientation chosen per breakpoint, rendered crop inspected at each declared width | SKILL L187 (Image semantics and delivery) |
| Pico loss: 768 host width fell into the phone layout | Breakpoints derived from the content column inside the actual host shell (padding, sidebars, chrome), never the raw window width | SKILL L189 (Implement and verify) |
| Chips/table cells wrap to two lines at desktop widths incl. 1920; USGS: "Clear filter"-style button labels must not wrap | Status chips, badges, button labels and table cells hold content on one line at every declared width up to the widest; deliberate truncation with accessible full text instead of mid-label wrapping | SKILL L120 (Production mode) |
| Code/JSON wraps mid-token at 320 | Code, JSON and long identifiers scroll horizontally at narrow widths; wrapping only at token boundaries | SKILL L120 |
| Quake-tool loss: base skill won typography and spacing | Type scale kept clearly subordinate to display type; dead-band rule (existing L135 seam rule) reinforced for header/banner/list gaps | SKILL L193 (Typography), L120 |
| Counters ignored stress-fixture rows | Every displayed count reconciles to the rows actually rendered, including stress-fixture rows; a disagreement fails | SKILL L120 |
| USGS: simulated failure messages need role=alert | Failure messages, including simulated ones, announce assertively (`role="alert"`), not via a polite status region | SKILL L120 |
| USGS: fixture copies must not inherit the source row's local review state | A copied/duplicated fixture row starts with its own clean state; no inherited review, check or approval (extends the L128 review-binding rule) | SKILL L120 |
| USGS: ~90px empty bands between header, banners and lists | Headers, banners and lists keep purposeful spacing; an unexplained empty band between them fails the density check | SKILL L120 |


## Pass-6 change log (Pico blind recheck defects -> edits)

Pass 6 adds guidance only; nothing was weakened or removed. The old build's phone flaw (headline hidden via `display:none`) is explicitly ruled out rather than copied.

| Finding | Edit | Where |
|---|---|---|
| Primary CTA below the fold at 390x650 and 320 (host chrome counted) | The primary action is part of the payoff bundle: it must land inside the first viewport at 390×650 and 320 with host chrome subtracted at its real rendered height | SKILL L85 |
| Hero photo showed as a blank gray box ~10s on first load | Critical first-view image eager-loaded (for example `fetchpriority="high"`), box reserved with intrinsic dimensions or aspect-ratio, real low-cost placeholder (dominant-color fill, tiny blurred preview, or poster); verified on a throttled first load, never a warm cache | SKILL L187 |
| "BOARD 02 ↗" static text styled like a link | Affordances must be truthful: link signals (accent color, underline, arrow glyphs such as ↗) or button styling require an actual link or button; strip the affordance from static text | SKILL L163 (Button design) |
| Header wrapped over 3 lines at 390 @200% zoom; old's display:none phone headline | Headline stays in the phone view (display:none or equivalent fails); at 200% text zoom on 390 the header reflows without ballooning past two lines - measure and size chosen for that case and tested | SKILL L85 |


## Mandatory rows (named by the plan)

| Base clause | New anchor | Trigger / decision retained | Evidence / exception retained | Status |
|---|---|---|---|---|
| L37 result contract | "First pass the brief's **result contract**" (SKILL L45) | Brief promises input/choice/outcome: map each to a real deliverable, not an echo or slogan | Source-backed or openly authored rule; legible outcome and divergent state; independent choices stay legible and effective (preselected example / preset pagination not equivalent); one axis at a time; supports honesty; quantified reconciliation; unsourced deliverable -> explicit limits, truthful worked example or stop; never invent a prescription or live operation; hidden-headline reader test | kept (restructured core-first in pass 2; workout illustrations moved to VRC L30; H1(a) guard restored) |
| L59 relationship and source proof | "apply a **relationship-proof baseline**" (SKILL L71) | Concept asserts spatial/temporal relation or reveal: name exact entailing fact and test stripped, delivery-size relation | Quote source, distinguish interpretation/fiction; reject if source or visual reading fails; unknowns visible; no inferred edge styled as fact | kept (unchanged in pass 2) |
| L85 sensitive illustrative results | "illustrative result in a safety- or suitability-sensitive area" (SKILL L89) | Exercise, health, suitability or other sensitive example: do not invent a prescription or personalized output | Supplied rule/scope or explicit illustrative label; label beside the result, not only footer | kept (unchanged in pass 2) |
| L89 causal input-to-mark | "A signature visual device must explain the product's decision" (SKILL L95) | Mark changes with input: name rule for each input and divergent outputs | Rendered pixels at real sizes; reject cosmetic input echo; deliver input values + two settled same-crop frames in the review evidence | kept (unchanged in pass 2) |
| L103 taste adversary | "Run a **taste adversary**" (SKILL L107) | Full Visual thesis: compare fact-matched plain baseline and a distinctly different composition, not a score certificate | Settled captures, both arms defined as the compared variants of a comparative validation; independent critic vs frozen prior render; substantive correction when warranted; evidence-backed stop at the revision cap or a clean iteration is a valid close - no invented flaws; veto must name the specific harm with evidence (state, pixels) | kept + strengthened in pass 2 (L7, P1-2, P2) |
| L107 + L133 motion/GSAP | Visual mode (SKILL L110) and "For scroll-led Visual scenes" (SKILL L143) | Visual mode: motion only if earned; GSAP ScrollTrigger for genuinely scroll-led explanation | Start/mid/end/reverse, keyboard, reduced motion/static, renderer/asset and context-loss recovery, pause/resume; no mandatory animation; scene map; suppressible non-essential motion | kept verbatim (unchanged in pass 2) |
| L115 real-run receipts | "one actual run" (SKILL L118) | Production run/agent UI: timeline of one actual run, not selectable demo states | Inspectable events, failures, unknowns; synthetic fixtures labeled, never passed as live receipts | kept verbatim (unchanged in pass 2) |
| L125 async stale data | Interaction before styling core (SKILL L128) | Async filters/refresh: separate requested view from displayed snapshot while loading | Status and displayed data agree; previous-snapshot label with counts paired to rows; stale responses ignored; rapid-change/failure/retry tests; mock timer is not proof of freshness | kept verbatim in the core (sub-clauses 1-7, 21, 31 of base L125); remaining sub-clauses moved verbatim to VRC Interaction workflow cases (see VRC inventory) |
| L147-155 buttons | Button design (SKILL L159-165) | Button family from existing system; greenfield/focal comparison only when warranted | Actual hover/focus/pressed/disabled/loading/touch and destructive effect; icon accessible names; cross-project challenge; no invented default house button | kept verbatim (unchanged in pass 2) |
| L157-163 state ledger/zoom | State coverage ledger (SKILL L169-173) | Consequential task states; actual 200% browser zoom, not phone proxy | Rows cover initial/normal/empty/invalid/failure/dense where applicable; word/data/color agreement; proxy labeled unverified if real zoom unavailable | kept verbatim (unchanged in pass 2) |
| L175 images | Image semantics and delivery (SKILL L187) | Informative/decorative/control/chart role determines alt or accessible explanation | Loaded/broken images, intrinsic aspect space, critical image priority; W3C/WHATWG/MDN links intact | kept verbatim (unchanged in pass 2) |
| L177 host-shell isolation | "Run relevant checks, then render the actual UI" (SKILL L189) | Embedded/hosted UI: compare actual host before/after | Shell colors/background/focus/visited link unaffected; unintended overrides block handoff; inspection at declared sample widths with breakpoint-edge and interval probes | kept (pass 2: "every width" -> declared samples + probes, per P1-2) |
| L181-185 localization | Localization and language resilience (SKILL L197) | Supported locales or locale-specific input: test expansion and actual fallback/scripts/RTL | Reflow, number/date/plural and direction semantics; untested marked unverified; out of scope not a gate | kept verbatim (unchanged in pass 2) |
| L189-193 evidence/handoff | Evidence and review (SKILL L201-205) + Handoff (SKILL L209) | Any changed flow: reproducible same-task review, then report direction/files/tests | Before/after same task; blinded non-builder answers fixed questions on both arms - never simulated by the builder; not performed + provisional verdict when no non-builder exists; no CUT / no CHANGE valid when nothing useful needs changing or cutting; declared scope and trigger quotes carried into the handoff; receipt does not redefine the original task | kept + strengthened in pass 2 (M3, P1-2, H2, P2) |
| L29 observed-page study | "The reference study is a range of 101 distinct company-produced sites" (SKILL L41) | Citing the inventory as design input | 101 sites, desktop+phone and capture limits; not designer identity/conversion evidence; private source ledger; mechanisms not templates | kept verbatim (unchanged in pass 2) |

## Full clause inventory by original section

Rows marked "(pass 2)" changed in this round; all other rows are unchanged from the pass-1 map and stand as recorded there.

### Frontmatter and opening (base L1-14)

| Base | New anchor | Notes | Status |
|---|---|---|---|
| L1-4 frontmatter | L1-4 | Unchanged | kept |
| L8 opening | L8 | Method sentence retained | kept + extended |
| L12 routing paragraph | L12 | Explicit mode record | kept + strengthened |
| L14 mode-is-not-quality | L14 | Quiet index / document / multi-route dashboard / recognizable existing UI pass via real task, navigation, artifact or known limitation (preamble instance of the exception; operative instances added in pass 2 at L47, L83, VRC L34) | kept + extended |

### Fast execution path (base L16-25)

| Base | New anchor | Notes | Status |
|---|---|---|---|
| L18 intro | L18 | Same; links to record section | kept |
| L20 item 1 Frame | L20 | Same content | kept |
| L21 item 2 Choose a mode | L21 item 2 Route | Same decision | kept |
| L22 item 3 Scale concept work | L22 | Pass 2: narrow defined inline (one component or state, existing UI baseline); full gate forced by first-view / hierarchy / promised-outcome change; accessibility or consequential-state change expands review; critique-only unchanged | kept + strengthened (pass 2, H2) |
| L23+L24 items 4-5 Plan/build | L23 item 4 | Pass 2: width-freeze rule stated here with cross-link (L6) | merged + strengthened (pass 2) |
| L25 item 6 Handoff | L24 item 5 | Same handoff content + first-time walk and KEEP/CHANGE/CUT | kept + extended |

### One review record (added section)

| Base | New anchor | Notes | Status |
|---|---|---|---|
| (none) | L26-37 | Pass 2: skipped sections quote the failing trigger (L28); A/F example aligned with D's trigger ("no promised result and no new consequential state" vs "spans more than one consequential state"); D/E/F bullets collapsed to record contents with pointers to the governing sections (P1-1, P1-2); A/B/C unchanged | added (pass-1) + collapsed (pass 2) |

### Landing-page craft (base L27-49)

| Base | New anchor | Notes | Status |
|---|---|---|---|
| L29 inventory caveat | L41 | Mandatory row above | kept |
| L31 first choose what person came to do | L43 "Lead with the visitor and their action" | Merged with L33 content/evidence map | merged |
| L33 content and evidence map | L43 (same paragraph) | All map fields retained; established-product inspection sentence retained; pass 2: restored the mood-image/slogan guard (H1(c)) | merged + restored (pass 2) |
| L35 focal-proof budget | L47 "test the **ownable proof** ... single focal-proof budget" | Pass 2: restructured around a short executable sequence; scope limited to briefs that claim a focal proof; quiet-index / document / multi-route exception added at the gate (P1-3); the constant-facts/delivery-size sentence removed as a duplicate of L69's study mechanics; the "photographed material / cause-and-effect scene / work record / editorial treatment may carry the idea" sentence folded into L43's proof-object sentence and L49's editorial-cut examples | merged + restructured (pass 2, P1-1, P1-3) |
| L37 result contract | L45 | Mandatory row above; pass 2: core-first restructure, H1(a) guard restored, workout illustrations moved to VRC L30 | kept + restructured (pass 2) |
| L37 ownable-proof half | L47 (see L35) | Brand-covered hero test, signature visual carries result data, state change holds scene steady, no fictional product photo, no fabricated capability | kept |
| L39 editorial cut | L49 | Verbatim | kept |
| L41 recompose for phone | L51 | Verbatim (390x650 real header/host height, settled full-size view, purposeful glimpse limits, control+effect inside opening, no shrinking below useful size, summary + route to detail, overlay/slow/no-media) | kept |
| L43 distinct evidence jobs | L53 | Chapter-jobs, source tracing, "example data" limits, missing source -> mark and narrow, next chapter adds consequence or is cut, restating cards cut, baseline retention, motion-only-when-it-changes-understanding | kept |
| L45 human decision choices | L55 | Verbatim | kept |
| L47 settled-state consistency audit | L57 | Verbatim (badges/labels/names/values/captions/receivers, stable option names, divergent + reversal reads, rejected-review contradictions, totals reconcile, bar/ring/segment identity, workout/lamp examples as one failure class, physical geometry audit start/intermediate/end/reverse, footprint inside beam, repair + recapture from one final revision) | kept |
| L49 before-handoff inspection | L59 | Pass 2: taste / task-clarity / phone-integrity recorded as separate informal diagnostics for spotting regressions, never passing evidence (P2) | kept + clarified (pass 2) |

### Pre-build concept gate (base L51-103)

| Base | New anchor | Notes | Status |
|---|---|---|---|
| L53 gate scope paragraph | L63 | Pass 2: Production / multi-route scoping line at the top (M4); narrow defined (H2); full-gate forcing conditions; accessibility / consequential-state expansion; declared scope carries into handoff | kept + strengthened (pass 2) |
| L55 two structurally different concepts | L67 | Pass 2: operative definition of "structurally different" restored (H1(b)); seek material or narrow when neither direction has enough | kept + restored (pass 2) |
| L57 side-by-side first-view study | L69 | Unchanged | kept |
| L59 relationship-proof baseline | L71 | Mandatory row above | kept |
| L61 claim/content contract test | L73 | Unchanged | kept |
| L63 delivery-size proof | L75 | Unchanged | kept |
| L65/67 first-view ownership gate | L79 | Unchanged | kept |
| L69/71 ordinary-product specificity | L81 | Unchanged | kept |
| L73 product's own evidence | L83 | Pass 2: quiet-index / multi-route exception added at this gate (P1-3) | kept + extended (pass 2) |
| L75 template-substitution test | L93 | Unchanged (merged L77 baseline comparison stands) | merged |
| L79/81 short-phone payoff bundle | L85 | Unchanged | kept |
| L83 audience register | L87 | Unchanged | kept |
| L85 sensitive illustrative results | L89 | Mandatory row above | kept |
| L87/89 input-to-mark ledger | L95 | Mandatory row above | kept |
| L91 concrete consequence sentence | L97 | Unchanged | kept |
| L93 below-fold chapter questions | L99 | Unchanged | merged |
| L95/97 distinctive language | L101 | Unchanged | kept |
| L99 visual-language sheet | L103 | Unchanged | kept |
| L101 different job per section | L105 | Unchanged | kept |
| L103 taste adversary | L107 | Mandatory row above; pass 2: arms defined, evidence-backed stop / clean iteration, veto names harm with evidence | kept + strengthened (pass 2) |

### Visual mode / Targeted VFC / Production mode / Brief and audit (base L105-121)

| Base | New anchor | Notes | Status |
|---|---|---|---|
| L107 Visual mode paragraph | L110 | Verbatim (mandatory motion/GSAP row) | kept |
| L111 VFC loader | L114 | Pass 2: dual trigger - visual claims, or work touching the reference's specialized interaction workflows | kept + extended (pass 2) |
| L115 one actual run | L118 | Verbatim (mandatory row) | kept |
| L117 Production planning paragraph | L120 | Verbatim | kept |
| L121 brief and audit | L124 | Verbatim | kept |

### Interaction before styling (base L123-127) - split in pass 2

| Base | New anchor | Notes | Status |
|---|---|---|---|
| L125 workflow paragraph (41 sub-clauses) | SKILL L128 core + VRC Interaction workflow cases (L44-78) | Core keeps sub-clauses 1 (map entry/decisions/action/outcomes/recovery), 2 (dashboard status/freshness/filters/next actions), 3-5 (async requested-vs-displayed snapshot, previous-snapshot label, stale responses ignored, rapid-change/failure/retry, mock timer not proof), 6 (chart provenance/denominator, missing vs recorded zero, no trend across unrecorded gap), 7 (exact values in text/table), 21 (no save service -> no write controls), 31 (forms requirements/errors/preservation), plus a generalized rule: bind every local review to the exact record, values, account and revision it checked and invalidate on change; a passing local check is not approval, acceptance, or permission to commit money. Moved verbatim to the reference's cases: 8-13 + 20 (Two-dimensional tables and reflow, incl. dense table), 14-16 (Sequence editing), 17 (Paged and cursor-based results), 18-19 (Bulk actions), 22 (Source reconciliation), 23 (Destructive flows), 24 (Approvals and quotes), 25 (Offline and queued work), 26 (Sensitive record details), 27 (Multi-account actions), 28-30 (Import previews and spreadsheet exports, incl. OWASP CSV Injection link), 32-33 (Collaborative and optimistic writes), 34-37 (Settings, permissions and dependent controls, incl. task-before-preview), 38 (Time-zone instants), 39 (Session expiry), 40-41 (Branching multi-step work). Opener phrases folded into the bold case names (see "What actually left"). | split + moved (pass 2, P1-1) |
| L127 visual direction paragraph | L137 (inside "### Choose a craft system") | Unchanged | moved (pass 1) |

### Choose a craft system / Learn from examples / Reference transfer / One product-specific interaction (base L129-145)

Unchanged from the pass-1 map: craft-system subsection (added, L131-135), collect examples (L141), scroll-led scenes (L143, mandatory motion row), Production error/recovery study (L145), reference transfer test (L149), central behavior consideration (L153), external examples challenge bar (L155).

### Button design / State ledger / Design decisions (base L147-167)

| Base | New anchor | Notes | Status |
|---|---|---|---|
| L149-155 buttons | L159-165 | Mandatory row above | kept |
| L159-163 ledger + zoom | L169-173 | Mandatory row above | kept |
| L167 design decisions under pressure | L177 | Pass 2: cross-references the gate's narrow definition and full-gate forcing conditions instead of restating them | kept + clarified (pass 2, H2) |

### Implement and verify (base L169-185)

| Base | New anchor | Notes | Status |
|---|---|---|---|
| L171 reuse/semantic intro | L181 | Verbatim | kept |
| (none) | L183 width-freeze paragraph | Pass 2: 1920px / 320px are default stress widths, waivable only with a recorded reason tied to the brief's audience and devices (L5) | added (pass 1) + strengthened (pass 2) |
| L175 image semantics | L187 | Verbatim (mandatory row) | kept |
| L177 render paragraph | L189 | Pass 2: "every width" -> declared sample widths with breakpoint-edge and interval probes (P1-2); host-shell isolation, score/verified-unavailable sentences stand | kept + clarified (pass 2) |
| L181 typography | L193 | Verbatim | kept |
| L185 localization | L197 | Verbatim (mandatory row) | kept |

### Evidence and review / Handoff (base L187-193)

| Base | New anchor | Notes | Status |
|---|---|---|---|
| L189 review record paragraph | L201 | Unchanged | kept |
| (none) | L203 pre-critique + reviewer protocol | Pass 2: arms defined (compared variants of a comparative validation; narrow patch has none - compact route/status note suffices); builder must not simulate the blinded reviewer; no non-builder -> not performed + provisional verdict (M3, P2) | added (pass 1) + strengthened (pass 2) |
| L189 reviewer challenge + custody sentences | L205 | Unchanged | kept |
| (none) | L205 KEEP/CHANGE/CUT protocol | Pass 2: "when nothing useful needs changing or cutting" (P1-2) | added (pass 1) + clarified (pass 2) |
| L193 handoff | L209 | Pass 2: handoff carries the declared scope and which record sections fired or were skipped, with their trigger quotes (H2) | kept + extended (pass 2) |

## visual-review-cases.md inventory

| Base | New anchor | Notes | Status |
|---|---|---|---|
| L3 read-only-when header | L3 | Base text: "Read this reference only when the brief or implementation includes a visual claim that needs one of these checks: data relationships, physical or causal scenes, transformations, continuously changing controls, animation, or multi-state rendered evidence. Select the applicable cases; this is not a checklist for every design task. The core workflow remains in `../SKILL.md`." New text: "Read this reference only when a trigger fires: (1) the brief or implementation includes a visual claim that needs one of these checks - data relationships, physical or causal scenes, transformations, continuously changing controls, animation, or multi-state rendered evidence; or (2) the work touches one of the specialized interaction workflows under Interaction workflow cases below. Select the applicable cases; this is not a checklist for every design task, and pages without either trigger should not load it. The general review protocol - the frozen width range, host-framed captures, the first-time walk, and KEEP / CHANGE / CUT critique - lives in `../SKILL.md`; this reference adds only the specialized cases below." The `../SKILL.md` pointer is retained (target unchanged) and now names the protocol it points to; the second trigger was added when the interaction workflow cases moved in (P1-1). Pass 3: the load rule is now "pages without either trigger should not load it" (RT2). | kept + extended (pass 2, pass 3) |
| L5 heading + L7 intro | L5, L7 | Verbatim (transferable failure classes; not a pass certificate) | kept |
| L9-30 all 22 failure classes | L8-29 | Verbatim: verdict strength/negative geometry, evidence drift, coordinate/causal mismatch, responsive geometry drift, control-semantic drift, scene identity under selection, promised consequence, idea substitution, plausibility/state integrity, transformation custody, material-vocabulary break, stock-icon payoff, house-style convergence, light/shadow source break, decorative/content collision, weak resolved state, reveal spoilers, interaction-form convergence, agency without outcome space, feedback away from action, evidence-set coherence, state coverage vs capture count | kept verbatim |
| (none) | "### Result-contract worked examples (choice-and-result briefs)" (L31) | The workout-specific illustrations moved from SKILL L45: time-axis rescaling and plan renaming, dose information for named movements, equipment/household supports under a "none" choice, unreconciled segment without scale/denominator/remainder, comparison by usable contents. Pass 3: promoted from a failure-class bullet to its own heading (Critic 5) | added (pass 2), own heading (pass 3) |
| L32 dogfood paragraph | L37 | Verbatim | kept |
| L34 landing hero paragraph | L39 | Pass 2: quiet-index / document / multi-route exception appended (P1-3) | clarified (pass 1) + extended (pass 2) |
| L36 claimed-behavior-from-source | L40 | Verbatim | kept |
| L38 continuous-control pixel-delta | L42 | Verbatim | kept |
| L40 responsive/fallback paragraph | L45 | Tightened in pass 1, all checks retained | kept |
| (none) | L47 regression example | Compact responsive regression set | added (pass 1) |
| (none) | L49-83 "Interaction workflow cases" | 16 triggered cases holding the moved interaction catalogs from base L125 (mapping in the L125 row above): tables/reflow, sequence editing, paged results, bulk actions, reconciliation, destructive flows, approvals, offline/queued, sensitive records, multi-account, import/export, collaborative/optimistic writes, settings/permissions/dependent controls, time-zone instants, session expiry, branching multi-step. Intro limits loading to matching cases and points back to `../SKILL.md` for the general interaction guidance | added (pass 2, P1-1) |

## What actually left (honest removal list - Red Team H1 fix)

**Pass-1 silent drops, restored in pass 2 (the pass-1 map failed to list these; listing them now):**
- Base L33: "If the brief asks for three independent choices, preserve three legible choices and show how each affects the result; a preselected example or pagination through presets is not equivalent." -> restored, generalized, at SKILL L45.
- Base L33: "For work that needs a full concept comparison, consider at least two *different organizing ideas* (for example, object-led versus task-led), not two skins of the same hero. Choose by the brief's evidence, audience and delivery frame. If neither has enough real material, seek it or narrow the claim." -> restored as the operative definition of "structurally different" at SKILL L67.
- Base L33: "An editorial or object-led hero is free to avoid a widget, but it cannot quietly replace a requested choice-and-result product with a mood image or slogan." -> restored at SKILL L43.

**Pass-1 duplications dropped (as recorded in the pass-1 map):** inside base L43, the workout-dose and physical-scene source-to-receiver restatements (mandatory at L45/L47/L57) and its "narrow the claim rather than invent" sentence (standing at L45).

**Pass-2 removals:**
- SKILL L47's "Hold the underlying facts and words constant across both sketches and draw them at delivery size, at the same desktop and short-phone crop." - duplicate of L69's study mechanics ("using the same honest product evidence and real words. Include both desktop and short-phone crops at delivery size"), which stands.
- SKILL L47's "A photographed material, a physical cause-and-effect scene, a legible work record or a distinct editorial treatment may carry the idea; no one form is required." - folded: the same coverage stands at L43 (proof object is one candidate evidence form; material, object or work record) and L49 (editorial-cut examples).
- SKILL L128's case opener phrases ("For paged or cursor-based results,", "For destructive flows,", "For offline or queued work,", "For a multi-account action,", "For collaborative edits,", "Settings pages also need", "When a session expires during a draft or review,", "For branching multi-step work,", "For a table needing two-dimensional layout,", "For priority or sequence editing,", "For a partial bulk result,", "When reconciling disagreeing sources,", "For approvals, quotes or other consequential decisions,", "For a screen with sensitive record details,", "For an import preview,", "For a task interface that converts a local date and time to an instant,") - folded into the bold case names of the reference's Interaction workflow cases; the operative sentences stand verbatim there.
- SKILL L45's workout-specific illustrations - MOVED, not dropped: they stand in the reference's Result-contract worked examples case (VRC L33). The general tests they illustrated remain in L45.
- A-F record bullets: D's restated ledger columns replaced by a pointer to State coverage ledger; E's restated system components replaced by a pointer to Choose a craft system; F's restated review protocol condensed to record contents.
- Nothing else. No test, guard, external link, or failure class was deleted in pass 2.

## Growth accounting (pass 2, measured)

- SKILL.md 13,091 -> 11,880 words (-1,211): interaction split -1,593 (1,843-word catalog -> 250-word core); review-finding additions (restored guards, narrow definition, M3/M4/L5/L6/L7, arms/blinded definitions, gate exceptions) about +400; landing cores net -23 (L45 364->345, L47 433->437 with a +43-word required exception and +40-word executable core offsetting -46 of duplication); A-F collapse about -40; other edits net about +45.
- visual-review-cases.md 2,391 -> 4,369 words (+1,978): Interaction workflow cases +1,766; Result-contract worked examples +158; dual-trigger header +~40; quiet-index exception +~32.
- Repository total 15,482 -> 16,249 words (+767), all additions traceable above.
- Pass 3: SKILL.md 11,880 -> 12,062 words (+182: material-change gate restriction, adversary scoping, record-section trigger honesty, imperative reference load, provisional-verdict acceptance item). visual-review-cases.md unchanged in words (4,369); +3 lines from the promoted result-contract heading (81 content lines).
- Pass 4: SKILL.md 12,062 -> 12,088 words (B trigger extended with the full-gate material triggers). visual-review-cases.md 4,369 -> 4,375 words (+6, the new general heading), 81 -> 83 content lines.
- Pass 5: SKILL.md 12,088 -> 12,352 words (+264: six blind-validation defects and four USGS-reviewer findings, all additions in L120, L187, L189, L193). visual-review-cases.md unchanged (83 content lines / 4,375 words).
- Pass 6: SKILL.md 12,352 -> 12,542 words (+190: four Pico blind-recheck defects, additions in L85, L163, L187). visual-review-cases.md unchanged (83 content lines / 4,375 words).

## Checks (pass 2, local, script-verified)

- Interaction-paragraph preservation, reproducible method: base L125's paragraph split into 96 sentences at sentence boundaries; each sentence checked against SKILL.md + the reference. Result: 77 stand verbatim, 11 stand after whitespace/case/apostrophe normalization only, 8 stand with their opener clause folded into a bold case name exactly these 8 of the 16 opener-bearing sentences: "For paged or cursor-based results,", "For destructive flows,", "For offline or queued work,", "For a multi-account action,", "For collaborative edits,", "Settings pages also need", "When a session expires during a draft or review," and "For branching multi-step work," - each folded into the bold case name of its reference case, with the operative clause standing verbatim there; the other 8 opener-bearing sentences stand verbatim (77-count) or after normalization (11-count), 0 missing. (The pass-2 figure "88 verbatim, 8 folded" conflated normalized matches with verbatim and undercounted folded openers; Red Team's independent count of 77 exact matches is confirmed.)
- 13 internal anchors in SKILL.md resolve against the new heading set; the `references/visual-review-cases.md` relative link and the reference's `../SKILL.md` pointer are intact.
- All 10 external URLs byte-identical to base; two (W3C Reflow, OWASP CSV Injection) relocated with their cases into the reference.
- Pass 2: untouched SKILL.md lines byte-identical to the pass-1 file (full-file line diff). Pass 3: only the ten lines named in the pass-3 log changed in SKILL.md; in the reference, the L3 header, the promoted result-contract heading, and line shifts from that promotion. Pass 4: only L31 changed in SKILL.md; in the reference, only the inserted "### Composition, behavior, and resilience cases" heading (with its blank line) at L35, shifting former L35-81 to L37-83. Pass 5: only four paragraphs changed in SKILL.md (L120, L187, L189, L193 - sentence appends, no deletions); the reference is untouched. Pass 6: only three paragraphs changed in SKILL.md (L85, L163, L187 - sentence appends, no deletions); the reference is untouched.
- All 22 failure classes verbatim, plus mandatory phrases; KEEP items verified present: Visual/Production route, truthful result contract, async stale / real-run receipts, input-to-mark ledger, real zoom vs proxy, host-shell checks, phone crop with chrome, no forced motion, anti-house-style craft system, the freeze-width rule, KEEP preservation, the independent critic, the 22 failure classes.
- **No fixed tokens, universal proof hero, obligatory animation, mechanical score, forced subtraction, or six separate records were introduced.**

