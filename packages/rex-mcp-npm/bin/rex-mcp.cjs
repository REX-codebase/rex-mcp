#!/usr/bin/env node
'use strict';

const { spawn } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');

const platform = process.platform;
const arch = process.arch;
const key = `${platform}-${arch}`;
const supported = new Set([
  'linux-x64',
  'linux-arm64',
  'darwin-x64',
  'darwin-arm64',
  'win32-x64'
]);

if (!supported.has(key)) {
  console.error(`rex-mcp: unsupported platform ${key}`);
  console.error('Supported: Linux x64/arm64, macOS x64/arm64, Windows x64.');
  process.exit(1);
}

const executable = platform === 'win32' ? 'rex-mcp.exe' : 'rex-mcp';
const binary = path.join(__dirname, '..', 'vendor', key, executable);
if (!fs.existsSync(binary)) {
  console.error(`rex-mcp: package is missing its ${key} binary.`);
  console.error('This package was assembled incorrectly. Reinstall or report the package version.');
  process.exit(1);
}

if (platform !== 'win32') {
  try {
    fs.chmodSync(binary, 0o755);
  } catch (error) {
    console.error(`rex-mcp: cannot make the bundled binary executable: ${error.message}`);
    process.exit(1);
  }
}

const child = spawn(binary, process.argv.slice(2), {
  stdio: 'inherit',
  env: process.env,
  windowsHide: true
});
child.on('error', (error) => {
  console.error(`rex-mcp: failed to start bundled server: ${error.message}`);
  process.exit(1);
});
for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP']) {
  process.on(signal, () => {
    if (!child.killed) child.kill(signal);
  });
}
child.on('exit', (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
  } else {
    process.exit(code === null ? 1 : code);
  }
});
