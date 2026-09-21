# REX Ultra Phase 5 - build and verification evidence

Base: b951b18c2918c3218ddfc37cec121133e2218517 (private main)
Commit: Ultra Phase 5: repository proof and independent reconstruction
Author: REX-codebase <aggu000000@gmail.com>
Built: 2026-09-20, Ubuntu 22.04 container (rootless), Rust 1.98.1, Node 22

## Independent validation (re-run this build)
- rex-ultra tests: 63/63 passed
- non-Tauri Rust tests: 202 passed (25+14+38+29+25+7+1+63)
- frontend vitest: 16/16 passed (3 files)
- npm build (tsc + vite): success
- npm audit --omit=dev: 0 vulnerabilities
- cargo check --workspace: passed (incl. src-tauri)
- cargo build --release -p rex-harness: linked
- target/release/rex-harness: 20,778,432 bytes
  sha256 2fcc7a4837a70fc80b8f38c8a375dc9f42a7c848633d192d7a444424c9f432b2

## Native visual verification
Rootless Xvfb (:99) + user/mount namespace overlay. Mesa userspace aligned to a
single version (libegl-mesa0, libgl1-mesa-dri, libglapi-mesa, libglx-mesa0 all
23.2.1-1ubuntu3.1~22.04.4) with software rendering (llvmpipe,
LIBGL_ALWAYS_SOFTWARE=1, WEBKIT_DISABLE_COMPOSITING_MODE=1). WebKitWebProcess /
WebKitNetworkProcess spawn via the namespace overlay at
/usr/lib/x86_64-linux-gnu/webkit2gtk-4.1 (2.50.4).
Screenshots (this archive): rex-phase5-main-surface.png (task composer),
rex-phase5-ultra-state.png (ULTRA verification tier engaged: contract,
adversary and clean-room judge active), rex-phase5-settings.png.

## Build steps
1. rustup stable (1.98.1), npm ci
2. Local sysroot of Ubuntu debs (webkit2gtk-4.1-dev 2.50.4, gtk-3 dev,
   soup-3 dev, ssl dev, aligned Mesa .4 runtime) extracted unprivileged;
   dangling .so symlinks re-pointed at base runtime libs
3. PKG_CONFIG_LIBDIR/PKG_CONFIG_SYSROOT_DIR + LIBRARY_PATH into sysroot
4. cargo check --workspace; cargo build --release -p rex-harness
5. Xvfb + unshare -rm namespace overlay launch for visual capture
No benchmark API runs were performed.

# REX Ultra in MCP - Phase 7 release gates (2026-09-21)

Head: 2aeb28967d82d077dc82bff7d7bb063588e8195a (private main, verified private)
Author: REX-codebase <aggu000000@gmail.com>
Built: GitHub Codespace, Ubuntu, Rust stable, Node 22

## Gate commands (spec section 10, Integration/E2E)
- cargo fmt --all -- --check: PASS (applied workspace-wide in 6fe9484)
- cargo clippy --workspace --all-targets -- -D warnings: PASS, exit 0 (2aeb289;
  six targeted #[allow]s carry written justifications in-tree)
- cargo test --workspace: PASS, every suite green after both gate commits
- npm test (vitest): PASS, 29/29 across 7 files
- npm run build (tsc + vite): PASS
- cargo build --release -p rex-mcp: PASS (5,731,912 bytes)
- cargo build --release -p rex-harness: PASS (workspace release build, exit 0)

## Scripted clean E2E (spec section K)
`rex-ultra-scripted-host ultra` against the fresh release rex-mcp over stdio,
zero network, fresh temp state + workspace: visual contract, 2 candidate
theses, isolated file bundles, loser rejected on grounded adversary defects,
winner clean adversary + proven verifier, winner's first visual evidence
refused by the taste gate (critic not clean), daemon restarted twice with two
replayed rex_execute calls resuming via rotated host resume handles, repaired
visual evidence accepted, atomic promotion of exactly the winning bundle
(hero-1.html verified in the workspace), terminal result, deterministic proof
bundle 1a38b990bb24f479238b808f7fe372fbe6c4e32b95e585d68fc0e40dc9492624
persisted and inspected (completed state/kernel, committed promotion,
qualified candidate, full event chain ending task_completed).

Honest mappings vs the spec's wording: the host-driven external kernel has no
separate mutation or clean-room-judge stage (those belong to the
provider-driven UltraRunService, which MCP deliberately never routes through);
mutation rejection is exercised as grounded adversary defects, the visual
critic as the taste gate, and the kernel minimum is 2 candidates, not 3.

## Acceptance checklist (spec section 12)
- Ultra over MCP traverses the real institutions: PASS
- External host inference only, zero silent managed-model calls: PASS
- Cooperative continuation boundary in protocol/docs/UI: protocol + docs PASS;
  UI pixel verification outstanding (below)
- Restart + duplicate-call survival without duplicate effects: PASS
- Branch isolation + atomic rollback-safe promotion: PASS
- Budgets, stalls, retries, Stop, terminal reasons truthful: PASS (suites)
- No visual promotion on code/DOM/build evidence alone: PASS (taste floor)
- Status names phase, branch, attempt, budgets, artifact, why: PASS
- Full tests, native builds, scripted E2E, pixel inspection: pixel inspection
  of the changed Tauri surface is the one outstanding item
- Private main commit + checksum-verified archive: private main PASS; Drive
  archive explicitly waived by the owner (GitHub checkpoints are the record)

## Pixel inspection (spec section 9 UI completion) - DONE 2026-09-21
Seeded a promoted Ultra task and a waiting-on-host Ultra task into a fresh
rex store (scripted host `ultra` and `seed-waiting` modes), served the real
supervision surface through rex-dev-server + vite, and captured it with
headless Chrome at desktop and phone viewports (same React surface the
Tauri shell renders; the shell adds only window framing). Inspected the
actual pixels, committed as rex-phase7-waiting-desktop.png,
rex-phase7-waiting-phone.png, rex-phase7-promoted-desktop.png and
rex-phase7-promoted-phone.png.

Findings: phase spine hierarchy reads correctly; the active node is the
only purple element; done nodes are green; the terminal line is plain
language; the proof hero names kernel, qualified candidate, promotion,
skill packs and proof hash; Stop stays fixed in the status bar, visually
separate from Close, and is absent on terminal tasks; the phone layout
stacks spine above hero with no clipping; the evidence ledger uses
plain-language event lines (one polish round shipped after inspection).
Two noted minors, accepted: the phone status-bar meta wraps into a narrow
column, and headless desktop captures render at 1356x847 rather than a
literal 1440x900 frame.

With this, every acceptance bullet in section 12 is PASS except the
owner-waived Drive archive.

# REX follow-up composer closeout (2026-09-21)

## Focused follow-up gates
- `cargo test -p rex-daemon follow_up_resumes`: PASS, 2/2. Active and
  completed tasks retain one task id, rotate the secured host handle, record
  `host_follow_up`, reject the stale handle, and create no second task.
- `npm test`: PASS, 32/32 across 7 files.
- `npm run build`: PASS.
- `cargo fmt --all -- --check`: PASS.
- `cargo clippy --workspace --all-targets -- -D warnings`: PASS.

## Follow-up visual verification
The real `rex-dev-server` and Vite surface were started with explicit
background PIDs and an isolated REX store. A fresh MCP stdio host seeded one
active task and one completed task; the completed task was resumed with the
same task id and a rotated handle before completion. Playwright captured and
inspected the real React supervision surface at 1440x900 and 390x844:

- `evidence/rex-follow-up/active-desktop-1440x900.png`
- `evidence/rex-follow-up/completed-desktop-1440x900.png`
- `evidence/rex-follow-up/active-phone-390x844.png`
- `evidence/rex-follow-up/completed-phone-390x844.png`
- `evidence/rex-follow-up/checks.json`
- `verification/follow-up/closeout.log`

All four captures show exactly one follow-up composer directly beneath the
task. Active state retains Stop; completed state retains the composer and
removes Stop. Pixel assertions report no horizontal overflow: content width
equals the viewport at both 1440 and 390 pixels.

The first visual retry failed before capture because the isolated workspace had
no `Cargo.toml`: MCP returned `scope_denied` / `path not found`. An earlier
attempt was also terminated by cleanup matching its own shell command. The
workspace fixture was corrected, the verifier was rerun with explicit PIDs,
and the final closeout completed with `VISUAL_RC=0`.
