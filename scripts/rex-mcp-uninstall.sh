#!/usr/bin/env bash
# Remove the rex-mcp binary. Client config and state are yours to remove:
# `claude mcp remove rex`, delete the rex entry from Antigravity's
# mcp_config.json, then optionally `rm -rf ~/.rex/harness` (irreversible:
# that directory is the only durable record of tasks, custody and audits).
set -euo pipefail
DEST="${REX_INSTALL_DIR:-$HOME/.rex/bin}"
rm -f "$DEST/rex-mcp"
echo "Removed $DEST/rex-mcp"
echo "State kept at ${REX_STATE_DIR:-$HOME/.rex/harness} (remove manually if unwanted)."
