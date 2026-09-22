'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');

const root = path.resolve(__dirname, '..');
test('package exposes the npx launcher', () => {
  const manifest = JSON.parse(fs.readFileSync(path.join(root, 'package.json'), 'utf8'));
  assert.equal(manifest.bin['rex-mcp'], 'bin/rex-mcp.cjs');
  assert.equal(manifest.engines.node, '>=18');
  assert.equal(manifest.license, 'UNLICENSED');
});

test('launcher covers the release build matrix', () => {
  const source = fs.readFileSync(path.join(root, 'bin', 'rex-mcp.cjs'), 'utf8');
  for (const target of ['linux-x64', 'linux-arm64', 'darwin-x64', 'darwin-arm64', 'win32-x64']) {
    assert.match(source, new RegExp(target));
  }
});
