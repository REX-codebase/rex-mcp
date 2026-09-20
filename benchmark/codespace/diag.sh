#!/usr/bin/env bash
# Diagnostic dump into a workspace-visible file.
OUT=/workspaces/rex-harness/bench-out/diag.txt
mkdir -p /workspaces/rex-harness/bench-out
{
  echo "== date =="; date -u
  echo "== ~/bin =="; ls -la "$HOME/bin" 2>&1
  echo "== processes =="; pgrep -fa 'rex-key-fill|rex-dev-server|setup.sh|pip' 2>&1 || echo none
  echo "== listening =="; ss -ltn 2>/dev/null | grep -E '8787|8788' || echo "neither port listening"
  echo "== setup-status.log =="; cat /workspaces/rex-harness/bench-out/setup-status.log 2>&1 || echo missing
  echo "== rex-key-fill.log =="; cat "$HOME/bench/rex-key-fill.log" 2>&1 || echo missing
  echo "== rex-dev-server.log =="; cat "$HOME/bench/rex-dev-server.log" 2>&1 || echo missing
  echo "== setup-pip.log tail =="; tail -5 "$HOME/bench/setup-pip.log" 2>&1 || echo missing
  echo "== creation log tail =="; tail -10 /tmp/creation.log 2>/dev/null || tail -10 /var/log/codespace-creation.log 2>/dev/null || echo "no creation log found"
  echo "DIAG-DONE"
} > "$OUT" 2>&1
cat "$OUT"
