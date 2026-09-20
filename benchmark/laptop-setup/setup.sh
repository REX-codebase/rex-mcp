#!/usr/bin/env bash
# REX benchmark laptop setup (route A). Run on Agrim's own laptop.
# Usage: bash setup.sh
set -euo pipefail
REPO=git@github.com:REX-codebase/rex-harness.git
DIR="$HOME/rex-harness"

echo "== 1/4 clone =="
if [ ! -d "$DIR/.git" ]; then
  git clone "$REPO" "$DIR"   # SSH remote: uses Agrim's own GitHub auth, no tokens shared
fi
cd "$DIR"
git fetch origin && git checkout main && git pull --ff-only

echo "== 2/4 rust toolchain =="
if ! command -v cargo >/dev/null; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  source "$HOME/.cargo/env"
fi

echo "== 3/4 build sidecar + bench runner (release) =="
cargo build --release --bin rex-dev-server --bin rex-bench

echo "== 4/4 start sidecar (localhost only) =="
mkdir -p "$HOME/.config/rex-harness"
pkill -f rex-dev-server 2>/dev/null || true
nohup ./target/release/rex-dev-server --store-file "$HOME/.config/rex-harness" \
  > /tmp/rex-dev-server.log 2>&1 &
sleep 1
curl -s http://127.0.0.1:8787/api/status && echo

cat <<MSG

Setup done. Next:
1. Open benchmark/laptop-setup/enter-key.html in any browser on THIS laptop.
2. Paste the Gemini API key (AI Studio) and click Save. It is written to
   \$HOME/.config/rex-harness/secrets.json (0600) and never leaves this machine.
3. Click "Check store" - it should say: gemini has_key = true.
4. Tell Instinct "key is in" - the benchmark runner takes it from there.
MSG
