#!/usr/bin/env node
/**
 * Build release binaries for every platform we publish and pack them into
 * archives the npm postinstall step knows how to fetch.
 *
 *   node scripts/build-binaries.js
 *
 * Run from a machine with the Rust toolchain and `cross` installed:
 *
 *   cargo install cross
 *
 * Writes to dist/. Publishing uploads those archives to a GitHub release; the
 * npm package itself stays small and fetches the one it needs at install time.
 */

import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const DIST = path.join(ROOT, 'dist');
const { version } = require('../package.json');

/** Each target, the Rust triple `cross` expects, and the archive it packs into. */
const TARGETS = [
  { name: 'x86_64-unknown-linux-gnu', archive: 'mcp-sandman-x86_64-unknown-linux-gnu.tar.gz', bin: 'mcp-sandman' },
  { name: 'aarch64-unknown-linux-gnu', archive: 'mcp-sandman-aarch64-unknown-linux-gnu.tar.gz', bin: 'mcp-sandman' },
  { name: 'x86_64-apple-darwin', archive: 'mcp-sandman-x86_64-apple-darwin.tar.gz', bin: 'mcp-sandman' },
  { name: 'aarch64-apple-darwin', archive: 'mcp-sandman-aarch64-apple-darwin.tar.gz', bin: 'mcp-sandman' },
  { name: 'x86_64-pc-windows-msvc', archive: 'mcp-sandman-x86_64-pc-windows-msvc.tar.gz', bin: 'mcp-sandman.exe' },
  { name: 'aarch64-pc-windows-msvc', archive: 'mcp-sandman-aarch64-pc-windows-msvc.tar.gz', bin: 'mcp-sandman.exe' },
];

const FEATURES = '--features http';

function run(command, args, options = {}) {
  process.stderr.write(`> ${command} ${args.join(' ')}\n`);
  execFileSync(command, args, { stdio: 'inherit', cwd: ROOT, ...options });
}

function has(command) {
  try {
    execFileSync(process.platform === 'win32' ? 'where' : 'which', [command], { stdio: 'pipe' });
    return true;
  } catch {
    return false;
  }
}

function main() {
  if (!has('cargo')) {
    process.stderr.write('cargo not found; install Rust from https://rustup.rs\n');
    process.exit(1);
  }

  const useCross = has('cross');
  if (!useCross) {
    process.stderr.write(
      'note: `cross` not found, falling back to plain cargo. ' +
        'Targets needing a cross toolchain will fail; install it with ' +
        '`cargo install cross`.\n',
    );
  }

  fs.mkdirSync(DIST, { recursive: true });

  const failures = [];
  for (const target of TARGETS) {
    const builder = useCross ? 'cross' : 'cargo';
    try {
      run(builder, ['build', '--release', FEATURES, '--target', target.name]);

      const binary = path.join(ROOT, 'target', target.name, 'release', target.bin);
      if (!fs.existsSync(binary)) {
        throw new Error(`expected binary at ${binary}`);
      }

      // Pack it flat: the installer expects the binary at the archive root.
      const archive = path.join(DIST, target.archive);
      fs.rmSync(archive, { force: true });
      run('tar', ['-czf', archive, '-C', path.dirname(binary), target.bin]);

      const size = (fs.statSync(archive).size / 1024 / 1024).toFixed(2);
      process.stderr.write(`  ${target.archive} (${size} MB)\n`);
    } catch (error) {
      process.stderr.write(`  ${target.name}: FAILED — ${error.message}\n`);
      failures.push(target.name);
    }
  }

  if (failures.length > 0) {
    process.stderr.write(`\n${failures.length} target(s) failed: ${failures.join(', ')}\n`);
    process.stderr.write('The npm package cannot install on those platforms until these succeed.\n');
    process.exit(1);
  }

  process.stderr.write(`\nAll binaries built into dist/. Publish them as release assets for v${version}:\n`);
  for (const target of TARGETS) {
    process.stderr.write(`  dist/${target.archive}\n`);
  }
}

main();