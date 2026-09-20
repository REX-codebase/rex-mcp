#!/usr/bin/env bash
# Codespace postCreate setup: install runner + deps, start dev-server and the
# one-shot key-fill server. No secrets are handled by this script.
set -euo pipefail
WS=/workspaces/rex-harness
mkdir -p "$HOME/bin" "$HOME/.config/rex-harness" "$HOME/bench" "$HOME/bench/results"
chmod 700 "$HOME/.config/rex-harness"
cp "$WS/benchmark/dist/linux-x86_64/rex-bench" "$WS/benchmark/dist/linux-x86_64/rex-dev-server" "$WS/benchmark/dist/linux-x86_64/rex-key-fill" "$HOME/bin/"
chmod +x "$HOME/bin/"*

# Python scorer deps (evalplus et al.) - best effort; smoke needs evalplus.
if ! python3 -c 'import evalplus' 2>/dev/null; then
  (sudo apt-get update -qq && sudo apt-get install -y -qq python3 python3-pip python3-venv >/dev/null 2>&1) || true
  python3 -m pip install --user -q 'evalplus==0.3.1' requests datasets 2>>"$HOME/bench/setup-pip.log" || true
fi

# Start rex-dev-server (FileSecretStore at default $HOME/.config/rex-harness).
pkill -f rex-dev-server 2>/dev/null || true
nohup "$HOME/bin/rex-dev-server" --port 8787 --store-file "$HOME/.config/rex-harness" >"$HOME/bench/rex-dev-server.log" 2>&1 &

# Start the one-shot key fill server; exits by itself after a successful fill.
pkill -f rex-key-fill 2>/dev/null || true
nohup "$HOME/bin/rex-key-fill" --port 8788 --provider gemini >"$HOME/bench/rex-key-fill.log" 2>&1 &

sleep 1
echo "setup done: $(curl -s 127.0.0.1:8788/health || echo fill-not-up) $(curl -s 127.0.0.1:8787/api/status || echo dev-not-up)"
