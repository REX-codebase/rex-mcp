'use strict';

const fs = require('node:fs');
const path = require('node:path');

const root = path.resolve(__dirname, '..');
const source = process.env.REX_MCP_BINARIES_DIR;
if (!source) {
  console.error('REX_MCP_BINARIES_DIR must point to a directory containing platform folders.');
  process.exit(1);
}
const targets = [
  ['linux-x64', 'rex-mcp'],
  ['linux-arm64', 'rex-mcp'],
  ['darwin-x64', 'rex-mcp'],
  ['darwin-arm64', 'rex-mcp'],
  ['win32-x64', 'rex-mcp.exe']
];
const vendor = path.join(root, 'vendor');
fs.rmSync(vendor, { recursive: true, force: true });
let copied = 0;
for (const [platform, name] of targets) {
  const from = path.join(source, platform, name);
  if (!fs.existsSync(from)) continue;
  const to = path.join(vendor, platform, name);
  fs.mkdirSync(path.dirname(to), { recursive: true });
  fs.copyFileSync(from, to);
  if (name !== 'rex-mcp.exe') fs.chmodSync(to, 0o755);
  copied += 1;
}
if (copied === 0) {
  console.error(`No REX MCP binaries found under ${source}.`);
  process.exit(1);
}
console.log(`Staged ${copied} platform binary/binaries.`);
