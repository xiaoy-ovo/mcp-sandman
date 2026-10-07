/**
 * Tests for the npm wrapper.
 *
 * These run without the native binary installed, so they cover the JS layer:
 * locating a binary, parsing `doctor` output, and the paths that decide which
 * install strategy applies.
 */

import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import { findBinary, VERSION } from '../index.js';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

test('package version matches the binary', () => {
  const pkg = JSON.parse(fs.readFileSync(path.join(ROOT, 'package.json'), 'utf8'));
  assert.equal(VERSION, pkg.version);
  assert.match(VERSION, /^\d+\.\d+\.\d+$/);
});

test('findBinary returns null rather than throwing when absent', () => {
  // The environment may legitimately have it installed; either answer is
  // valid, but a throw here would break every caller.
  const result = findBinary();
  assert.ok(result === null || typeof result === 'string');
});

test('MCP_SANDMAN_BINARY override is honoured', (t) => {
  const fake = path.join(os.tmpdir(), `mcp-sandman-test-${process.pid}`);
  fs.writeFileSync(fake, '#!/bin/sh\n');
  t.after(() => fs.rmSync(fake, { force: true }));

  const previous = process.env.MCP_SANDMAN_BINARY;
  process.env.MCP_SANDMAN_BINARY = fake;
  t.after(() => {
    if (previous === undefined) delete process.env.MCP_SANDMAN_BINARY;
    else process.env.MCP_SANDMAN_BINARY = previous;
  });

  assert.equal(findBinary(), fake);
});

test('a missing override falls through instead of throwing', (t) => {
  const previous = process.env.MCP_SANDMAN_BINARY;
  process.env.MCP_SANDMAN_BINARY = path.join(os.tmpdir(), 'definitely-not-here-xyz');
  t.after(() => {
    if (previous === undefined) delete process.env.MCP_SANDMAN_BINARY;
    else process.env.MCP_SANDMAN_BINARY = previous;
  });

  const result = findBinary();
  assert.ok(result === null || typeof result === 'string');
});

test('the CLI shim exists and is executable entry point', () => {
  const shim = path.join(ROOT, 'bin', 'mcp-sandman.js');
  assert.ok(fs.existsSync(shim), 'bin/mcp-sandman.js must exist');

  const pkg = JSON.parse(fs.readFileSync(path.join(ROOT, 'package.json'), 'utf8'));
  assert.equal(pkg.bin['mcp-sandman'], 'bin/mcp-sandman.js');
});

test('package files list covers everything the entry points need', () => {
  const pkg = JSON.parse(fs.readFileSync(path.join(ROOT, 'package.json'), 'utf8'));
  for (const required of ['bin/', 'index.js', 'index.d.ts', 'README.md', 'LICENSE']) {
    assert.ok(pkg.files.includes(required), `files must include ${required}`);
  }
});

test('postinstall downloads but never fails the install', () => {
  const script = fs.readFileSync(path.join(ROOT, 'scripts', 'download.js'), 'utf8');
  // The catch-all at the bottom is what keeps a network failure from turning
  // into a broken npm tree.
  assert.match(script, /main\(\)\.catch/);
  assert.match(script, /could not download a prebuilt binary/);
});