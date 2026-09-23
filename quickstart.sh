#!/usr/bin/env bash
# REX Harness quickstart: one command to go from clone to running app.
#
# Usage: ./quickstart.sh
#
# What it does:
#   1. Checks for Node.js, npm, Rust, and system deps
#   2. Installs frontend dependencies
#   3. Installs the headless `rex` CLI (rex exec --json for CI)
#   4. Starts the Tauri dev app (or prints build instructions)
#
# For a production build: ./quickstart.sh --build

set -euo pipefail

cd "$(dirname "$0")"

info() { echo "→ $1"; }
ok() { echo "✓ $1"; }
fail() { echo "✗ $1" >&2; exit 1; }

# 1. Check prerequisites.
info "Checking prerequisites…"
command -v node >/dev/null || fail "Node.js not found. Install from https://nodejs.org (v20+)"
command -v npm >/dev/null || fail "npm not found. Install Node.js first."
command -v cargo >/dev/null || fail "Rust not found. Install from https://rustup.rs"

NODE_MAJOR=$(node -p "process.versions.node.split('.')[0]")
[ "$NODE_MAJOR" -ge 20 ] || fail "Node.js v20+ required (found v$(node -p process.versions.node))"

# Tauri on Linux needs system libraries.
if [ "$(uname)" = "Linux" ]; then
  for pkg in libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf; do
    dpkg -s "$pkg" >/dev/null 2>&1 || info "Note: $pkg not installed — Tauri build may need: sudo apt install $pkg"
  done
fi
ok "Prerequisites look good."

# 2. Install frontend deps.
if [ ! -d node_modules ]; then
  info "Installing frontend dependencies…"
  npm install
else
  info "Frontend dependencies already installed."
fi
ok "Dependencies ready."

# 2b. Install the headless CLI (idempotent).
info "Installing rex CLI…"
cargo install --path crates/rex-cli --quiet 2>/dev/null \
  || cargo install --path crates/rex-cli
if command -v rex >/dev/null; then
  ok "rex CLI installed ($(rex --version))."
  info "Try: rex exec --dry-run --task \"say hello\" --json"
else
  fail "cargo install of the rex CLI failed."
fi

# 3. Build or dev.
if [ "${1:-}" = "--build" ]; then
  info "Building production app…"
  npm run tauri build
  ok "Build complete. See src-tauri/target/release/bundle/"
else
  info "Starting REX Harness in dev mode…"
  info "Press Ctrl+C to stop."
  npm run tauri dev
fi
