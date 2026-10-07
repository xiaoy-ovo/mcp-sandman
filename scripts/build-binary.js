#!/usr/bin/env node
/**
 * Build the release binary and put it where the npm wrapper expects it.
 *
 *   npm run build:binary
 *
 * Runs automatically on `npm publish` via prepublishOnly.
 *
 * The Windows build is bundled into the tarball, so `npm install -g
 * mcp-sandman` on Windows works with no download and no Rust toolchain. On
 * other platforms the binary cannot be shipped this way — it has to be the
 * right architecture — so those users get the postinstall download or a
 * source build.
 */

import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const IS_WINDOWS = process.platform === 'win32';
const BINARY_NAME = IS_WINDOWS ? 'mcp-sandman.exe' : 'mcp-sandman';

const log = (msg) => process.stderr.write(`mcp-sandman: ${msg}\n`);

function buildWithCargo() {
  const output = path.join(ROOT, 'target', 'release', BINARY_NAME);
  log(`cargo build --release --features http`);
  execFileSync('cargo', ['build', '--release', '--features', 'http'], {
    stdio: 'inherit',
    cwd: ROOT,
  });
  if (!fs.existsSync(output)) {
    throw new Error(`cargo reported success but ${output} does not exist`);
  }
  return output;
}

/** Copy the built binary next to the shim, where the wrapper looks for it. */
function stage(binary) {
  const staged = path.join(ROOT, 'bin', BINARY_NAME);
  fs.copyFileSync(binary, staged);
  if (!IS_WINDOWS) fs.chmodSync(staged, 0o755);
  return staged;
}

/** Sanity-check the binary before it goes into a published tarball. */
function verify(staged) {
  const version = execFileSync(staged, ['--version'], { encoding: 'utf8' }).trim();
  if (!version.startsWith('mcp-sandman')) {
    throw new Error(`binary reported an unexpected version: ${version}`);
  }
  return version;
}

function main() {
  if (!IS_WINDOWS) {
    // Shipping this machine's binary would be wrong: the npm tarball carries
    // one file, and it has to match the platform it was built for.
    log('not Windows — skipping the bundled binary.');
    log('Other platforms resolve their binary at install time or build from source.');
    return;
  }

  try {
    const binary = buildWithCargo();
    const staged = stage(binary);
    const version = verify(staged);
    const size = (fs.statSync(staged).size / 1024 / 1024).toFixed(2);
    log(`bundled ${BINARY_NAME} (${size} MB) — ${version}`);
    log('Windows users will get this with no download step.');
  } catch (error) {
    log(`could not build the Windows binary: ${error.message}`);
    log('');
    log('Publishing without it would leave Windows users with a package that');
    log('cannot run. Fix the build, or publish without a bundled binary only if');
    log('you intend to attach release assets for every platform.');
    process.exit(1);
  }
}

main();