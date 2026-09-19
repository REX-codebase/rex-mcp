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
