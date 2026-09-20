#!/usr/bin/env bash
# Post-canary-fill verification. Canary value is fixed: CANARY-KEY-12345
# (this script is the only repo file containing it; grep targets exclude it).
C='CANARY-KEY-12345'
OUT=/workspaces/rex-harness/bench-out/verify-canary.txt
mkdir -p /workspaces/rex-harness/bench-out
{
  echo "== store perms (expect 600/700) =="
  stat -c '%a %n' "$HOME/.config/rex-harness/secrets.json" "$HOME/.config/rex-harness" 2>&1
  echo "== store holds canary (expected - intended destination) =="
  grep -c "$C" "$HOME/.config/rex-harness/secrets.json" 2>&1
  echo "== app logs (expect 0 hits) =="
  grep -c "$C" "$HOME/bench/rex-key-fill.log" "$HOME/bench/rex-dev-server.log" "$HOME/bench/setup-pip.log" 2>&1
  echo "== shell history (expect no hits) =="
  grep -c "$C" "$HOME/.bash_history" 2>/dev/null || echo "0 (no history file or no hits)"
  echo "== process args/env (expect no hits) =="
  ps auxww | grep "$C" | grep -v grep | wc -l
  sudo grep -l "$C" /proc/[0-9]*/environ 2>/dev/null | wc -l
  echo "== repo outside this script (expect no hits) =="
  grep -r "$C" /workspaces/rex-harness --exclude=verify-canary.sh -l 2>/dev/null | wc -l
  echo "== home artifacts outside store (expect no hits) =="
  grep -r "$C" "$HOME" --exclude-dir=.config -l 2>/dev/null | wc -l
  echo "== cleanup: removing canary store so the real fill writes fresh =="
  shred -u "$HOME/.config/rex-harness/secrets.json" 2>/dev/null || rm -f "$HOME/.config/rex-harness/secrets.json"
  ls -la "$HOME/.config/rex-harness/" 2>&1
  echo "VERIFY-CANARY-DONE"
} > "$OUT" 2>&1
cat "$OUT"
