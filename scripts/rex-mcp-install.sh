#!/usr/bin/env bash
# Install the REX MCP server locally. No daemons, no telemetry, no network.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="${REX_INSTALL_DIR:-$HOME/.rex/bin}"
STATE_DIR="${REX_STATE_DIR:-$HOME/.rex/harness}"
# Store-schema guard: never downgrade into a newer task store; notice legacy.
if [ -d "$STATE_DIR/tasks" ]; then
  for f in "$STATE_DIR"/tasks/*/task.json; do
    [ -e "$f" ] || continue
    # Prefer a real JSON parse so a differently-spaced or reordered field still
    # reads; fall back to a whitespace-tolerant scan when python3 is absent.
    if command -v python3 >/dev/null 2>&1; then
      v=$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1])).get("store_schema_version",""))' "$f" 2>/dev/null || true)
    else
      v=$(sed -n 's/.*"store_schema_version"[[:space:]]*:[[:space:]]*\([0-9]\+\).*/\1/p' "$f" | head -1)
    fi
    if [ -n "$v" ] && [ "$v" -gt 2 ]; then
      echo "Task store at $STATE_DIR was written by a newer REX (schema v$v)." >&2
      echo "Install the newer server instead of downgrading onto it." >&2
      exit 1
    fi
  done
  legacy=$(grep -L store_schema_version "$STATE_DIR"/tasks/*/task.json 2>/dev/null | wc -l | tr -d ' ')
  if [ "$legacy" != "0" ]; then
    echo "Note: $legacy task record(s) predate store schema v2 and migrate to"
    echo "Standard mode on first load by the new server."
  fi
fi
echo "Building rex-mcp (release)..."
cargo build --release --locked -p rex-mcp --manifest-path "$ROOT/Cargo.toml"
mkdir -p "$DEST" "$STATE_DIR"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
# Stage beside the destination, then rename: an interrupted copy must never
# replace a working MCP binary with a truncated one.
STAGED="$(mktemp "$DEST/.rex-mcp.XXXXXXXX")"
trap 'rm -f "$STAGED"' EXIT
cp "$TARGET_DIR/release/rex-mcp" "$STAGED"
chmod +x "$STAGED"
mv -f "$STAGED" "$DEST/rex-mcp"
trap - EXIT
echo "Installed: $DEST/rex-mcp"
echo "Configure Claude Code, Codex, OpenCode or Hermes with this absolute path."
echo "See README.md for current private host setup examples."
echo "REX_STATE_DIR=$STATE_DIR"
echo "Mutation approval stays off unless you explicitly set REX_APPROVE_TASK_MUTATIONS=1."
echo
echo "Read docs/rex-mcp.md for the security and error contracts before"
echo "enabling REX_APPROVE_TASK_MUTATIONS=1."
