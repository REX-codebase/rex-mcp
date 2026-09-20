#!/usr/bin/env bash
# Codespace setup: install runner + deps, start dev-server and the one-shot
# key-fill server. Deterministic and fail-loud: every step appends a status
# line to a workspace-visible status file. Never handles secrets.
STATUS=/workspaces/rex-harness/bench-out/setup-status.log
mkdir -p /workspaces/rex-harness/bench-out "$HOME/bench"
: > "$STATUS"
step() { echo "$(date -u +%H:%M:%S) $1" | tee -a "$STATUS"; }

WS=/workspaces/rex-harness
step "SETUP-BEGIN"

mkdir -p "$HOME/bin" "$HOME/.config/rex-harness" "$HOME/bench/results" \
  && chmod 700 "$HOME/.config/rex-harness" \
  && step "dirs OK" || { step "SETUP-FAIL dirs"; exit 1; }

cp "$WS/benchmark/dist/linux-x86_64/rex-bench" \
   "$WS/benchmark/dist/linux-x86_64/rex-dev-server" \
   "$WS/benchmark/dist/linux-x86_64/rex-key-fill" "$HOME/bin/" \
  && chmod +x "$HOME/bin/"* \
  && step "binaries OK: $(ls "$HOME/bin" | tr '\n' ' ')" || { step "SETUP-FAIL binaries"; exit 1; }

if ! python3 -c 'import evalplus' 2>/dev/null; then
  step "python deps: installing (apt + pip, may take minutes)"
  if sudo apt-get update -qq >>"$STATUS" 2>&1 \
     && sudo apt-get install -y -qq python3 python3-pip >>"$STATUS" 2>&1; then
    step "apt OK"
  else
    step "SETUP-WARN apt failed (pip may still work)"
  fi
  if python3 -m pip install --user -q 'evalplus==0.3.1' requests datasets >>"$STATUS" 2>&1; then
    step "pip OK"
  else
    step "SETUP-WARN pip failed (smoke scorers may be unavailable)"
  fi
else
  step "python deps OK (cached)"
fi

pkill -f rex-dev-server 2>/dev/null; pkill -f rex-key-fill 2>/dev/null; sleep 1

setsid nohup "$HOME/bin/rex-dev-server" --port 8787 \
  --store-file "$HOME/.config/rex-harness" \
  >"$HOME/bench/rex-dev-server.log" 2>&1 < /dev/null &
step "dev-server spawn rc=$?"

setsid nohup "$HOME/bin/rex-key-fill" --port 8788 --provider gemini \
  >"$HOME/bench/rex-key-fill.log" 2>&1 < /dev/null &
step "key-fill spawn rc=$?"

sleep 2
H=$(curl -s -m 3 127.0.0.1:8788/health || true)
D=$(curl -s -m 3 127.0.0.1:8787/api/status || true)
step "health fill=[$H] dev=[$D]"
if [ -n "$H" ] && [ -n "$D" ]; then
  step "SETUP-OK"
else
  step "SETUP-FAIL servers-not-listening"
  step "key-fill.log: $(tail -2 "$HOME/bench/rex-key-fill.log" 2>/dev/null)"
  step "dev-server.log: $(tail -2 "$HOME/bench/rex-dev-server.log" 2>/dev/null)"
  exit 1
fi
