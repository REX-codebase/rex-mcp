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

## Outstanding
- Desktop (1440x900) and phone (390x844) pixel inspection of the Ultra
  supervision surface under Xvfb, per the spec's UI completion rule.
