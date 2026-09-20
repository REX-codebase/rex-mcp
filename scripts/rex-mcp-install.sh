#!/usr/bin/env bash
# Install the REX MCP server locally. No daemons, no telemetry, no network.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="${REX_INSTALL_DIR:-$HOME/.rex/bin}"
echo "Building rex-mcp (release)..."
cargo build --release -p rex-mcp --manifest-path "$ROOT/Cargo.toml"
mkdir -p "$DEST" "$HOME/.rex/harness"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
cp "$TARGET_DIR/release/rex-mcp" "$DEST/rex-mcp"
chmod +x "$DEST/rex-mcp"
echo "Installed: $DEST/rex-mcp"
echo
echo "Claude Code:"
echo "  claude mcp add rex --scope user \\"
echo "    --env REX_STATE_DIR=$HOME/.rex/harness \\"
echo "    --env REX_WORKSPACE=/path/to/project \\"
echo "    --env REX_APPROVE_TASK_MUTATIONS=1 \\"
echo "    -- $DEST/rex-mcp"
echo
echo "Antigravity (~/.antigravity/mcp_config.json):"
printf '  {"mcpServers":{"rex":{"command":"%s","env":{"REX_STATE_DIR":"%s","REX_WORKSPACE":"/path/to/project","REX_APPROVE_TASK_MUTATIONS":"1"}}}}\n' \
  "$DEST/rex-mcp" "$HOME/.rex/harness"
echo
echo "Read docs/rex-mcp.md for the security and error contracts before"
echo "enabling REX_APPROVE_TASK_MUTATIONS=1."
