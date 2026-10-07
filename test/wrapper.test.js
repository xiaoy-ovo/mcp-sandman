/**
 * Tests for the npm wrapper.
 *
 * These run without the native binary installed, so they cover the JS layer:
 * locating a binary, parsing `doctor` output, and the paths that decide which
 * install strategy applies.
 */

import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
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
  for (const required of ['bin/mcp-sandman.js', 'index.js', 'index.d.ts', 'README.md', 'LICENSE']) {
    assert.ok(pkg.files.includes(required), `files must include ${required}`);
  }
});

test('the Windows binary ships in the tarball', () => {
  // Windows users should never have to install Rust. The binary is listed
  // explicitly and prepublishOnly builds it, so `npm i -g` on Windows works
  // with no download step.
  const pkg = JSON.parse(fs.readFileSync(path.join(ROOT, 'package.json'), 'utf8'));
  assert.ok(
    pkg.files.includes('bin/mcp-sandman.exe'),
    'the Windows binary must be listed in files',
  );
  assert.equal(pkg.scripts.prepublishOnly, 'npm run build:binary');
  assert.equal(pkg.scripts['build:binary'], 'node scripts/build-binary.js');
});

test('both READMEs ship and cross-link', () => {
  const pkg = JSON.parse(fs.readFileSync(path.join(ROOT, 'package.json'), 'utf8'));
  assert.ok(pkg.files.includes('README.en.md'), 'the English README must ship too');

  const chinese = fs.readFileSync(path.join(ROOT, 'README.md'), 'utf8');
  const english = fs.readFileSync(path.join(ROOT, 'README.en.md'), 'utf8');

  assert.match(chinese, /\[English\]\(README\.en\.md\)/, 'the Chinese README links to the English one');
  assert.match(english, /\[中文文档\]\(README\.md\)/, 'the English README links to the Chinese one');

  // A stray U+FFFD means a multibyte character got mangled somewhere in the
  // toolchain, and it is invisible in a diff.
  for (const [name, text] of [['README.md', chinese], ['README.en.md', english]]) {
    assert.ok(!text.includes('�'), `${name} contains a replacement character`);
  }
});

test('the postinstall script ships inside the tarball', () => {
  // The `files` whitelist is applied at pack time, and postinstall runs from
  // the installed tree. If the downloader is not listed, every install fails
  // with "Cannot find module" before it can warn about anything.
  const pkg = JSON.parse(fs.readFileSync(path.join(ROOT, 'package.json'), 'utf8'));
  const script = pkg.scripts.postinstall.replace(/^node\s+/, '').trim();
  assert.ok(script, 'postinstall should name a script');
  assert.ok(
    pkg.files.some((entry) => script.startsWith(entry)),
    `postinstall runs \`${script}\`, which no entry in files covers`,
  );
  assert.ok(
    fs.existsSync(path.join(ROOT, script)),
    `${script} does not exist in the repo`,
  );
});

test('the files list never names the bin directory itself', () => {
  // `files: ["bin/"]` ships whatever the developer happens to have built
  // there, including a Windows .exe in a package meant for every platform.
  // The postinstall step places the binary after extraction, so the tarball
  // must contain the shim and nothing else from that directory.
  const pkg = JSON.parse(fs.readFileSync(path.join(ROOT, 'package.json'), 'utf8'));
  assert.ok(
    !pkg.files.includes('bin/'),
    'listing `bin/` wholesale would publish a stray local build',
  );
  assert.ok(
    pkg.files.includes('bin/mcp-sandman.js'),
    'the shim itself must still be listed',
  );
});

test('postinstall downloads but never fails the install', () => {
  const script = fs.readFileSync(path.join(ROOT, 'scripts', 'download.js'), 'utf8');
  // The catch-all at the bottom is what keeps a network failure from turning
  // into a broken npm tree.
  assert.match(script, /main\(\)\.catch/);
  assert.match(script, /No prebuilt binary is available for this platform/);
  // And it must tell the truth about why: no binaries are published yet.
  assert.match(script, /None are published yet/);
  assert.doesNotMatch(
    script,
    /could not download a prebuilt binary/,
    'the old wording implied a transient network failure rather than an absent release',
  );
});

test('every publish script parses as an ES module', () => {
  // A syntax error in a publishing script only surfaces at publish time, which
  // is the worst possible moment to find one. Parse them here instead.
  const scripts = ['download.js', 'build-binary.js', 'publish.js', 'verify-install.js'];
  for (const name of scripts) {
    const file = path.join(ROOT, 'scripts', name);
    // `node --check` parses without executing, so this stays offline and safe.
    const result = spawnSync(process.execPath, ['--check', file], { encoding: 'utf8' });
    assert.equal(result.status, 0, `${name} failed to parse:\n${result.stderr}`);
  }
});

test('scripts do not mix Python-style rest parameters into JavaScript', () => {
  // Regression guard: publish.js shipped with `function git(*args)` once, which
  // is a syntax error that only appeared when the script was actually run.
  for (const name of ['download.js', 'build-binary.js', 'publish.js', 'verify-install.js']) {
    const file = path.join(ROOT, 'scripts', name);
    const source = fs.readFileSync(file, 'utf8');
    assert.doesNotMatch(source, /function \w+\(\*/, `${name} looks like it has a stray *`);
  }
});

test('scripts do not reference the removed dist/ build', () => {
  // scripts/build-binaries.js produced six cross-compiled archives for the
  // postinstall downloader. It was never run in release mode, so there are no
  // release assets, and shipping a Windows-only npm package means dist/ is
  // now dead weight. Nothing should still point at it.
  for (const name of ['publish.js', 'download.js', 'build-binary.js']) {
    const source = fs.readFileSync(path.join(ROOT, 'scripts', name), 'utf8');
    assert.doesNotMatch(source, /dist\/|build-binaries/, `${name} still references dist/`);
  }
  assert.ok(
    !fs.existsSync(path.join(ROOT, 'scripts', 'build-binaries.js')),
    'build-binaries.js should be removed',
  );
});