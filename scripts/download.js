#!/usr/bin/env node
/**
 * Fetch the mcp-sandman binary for this machine.
 *
 * Runs on `npm install`. Three ways to supply the binary, in priority order:
 *
 *   1. MCP_SANDMAN_BINARY — an absolute path to one you already have
 *   2. an optional platform package (@mcp-sandman/darwin-arm64, etc.)
 *   3. a GitHub release download
 *
 * If none of those work — offline, unsupported platform, corporate proxy —
 * this exits 0 with a warning rather than failing the install. A failed
 * `postinstall` would leave the user with a broken tree and no way to fix it;
 * a warning plus a clear message at run time is recoverable.
 */

import { createRequire } from 'node:module';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const BIN_DIR = path.join(ROOT, 'bin');
const VERSION = require('../package.json').version;
const REPO = 'xiaoy-ovo/mcp-sandman';

/** Map (process.platform, process.arch) to the name we publish releases under. */
const TARGETS = {
  'darwin-arm64': 'aarch64-apple-darwin',
  'darwin-x64': 'x86_64-apple-darwin',
  'linux-x64': 'x86_64-unknown-linux-gnu',
  'linux-arm64': 'aarch64-unknown-linux-gnu',
  'win32-x64': 'x86_64-pc-windows-msvc',
  'win32-arm64': 'aarch64-pc-windows-msvc',
};

const log = (msg) => process.stderr.write(`mcp-sandman: ${msg}\n`);

function targetTriple() {
  const key = `${process.platform}-${process.arch}`;
  const triple = TARGETS[key];
  if (!triple) {
    log(`unsupported platform ${key}`);
  }
  return triple;
}

function binaryName() {
  return process.platform === 'win32' ? 'mcp-sandman.exe' : 'mcp-sandman';
}

/** Where the binary must end up for `bin/mcp-sandman.js` to find it. */
function destination() {
  return path.join(BIN_DIR, binaryName());
}

/** Skip if a usable binary is already present, e.g. from a cached install. */
function alreadyInstalled() {
  const dest = destination();
  if (!fs.existsSync(dest)) return false;
  try {
    const stat = fs.statSync(dest);
    // A zero-byte file means a previous download failed partway through.
    return stat.size > 0;
  } catch {
    return false;
  }
}

/** Honour an explicit override before touching the network. */
function fromEnv() {
  const override = process.env.MCP_SANDMAN_BINARY;
  if (!override) return null;
  if (!fs.existsSync(override)) {
    log(`MCP_SANDMAN_BINARY points at ${override}, which does not exist`);
    return null;
  }
  return override;
}

/**
 * Look for a platform-specific optional dependency.
 *
 * This is the npm-native path and needs no network access of our own, but it
 * only works if the matching package was published alongside this one.
 */
function fromOptionalPackage() {
  const scope = `mcp-sandman-${process.platform}-${process.arch}`;
  try {
    const pkg = require.resolve(`${scope}/package.json`);
    const dir = path.dirname(pkg);
    const candidate = path.join(dir, binaryName());
    return fs.existsSync(candidate) ? candidate : null;
  } catch {
    return null;
  }
}

/** Pull the release tarball for this platform out of GitHub. */
function fromGitHub(triple) {
  const url = `https://github.com/${REPO}/releases/download/v${VERSION}/mcp-sandman-${triple}${archiveSuffix()}`;
  log(`looking for a prebuilt binary: ${url}`);

  const tmp = path.join(os.tmpdir(), `mcp-sandman-${triple}${archiveSuffix()}`);
  const fetch = download(url, tmp);
  if (!fetch) {
    // No release assets are published yet. Saying so plainly is better than
    // letting this look like a network problem the user should retry.
    return null;
  }

  const extracted = path.join(os.tmpdir(), `mcp-sandman-extract-${process.pid}`);
  fs.rmSync(extracted, { recursive: true, force: true });
  fs.mkdirSync(extracted, { recursive: true });

  if (!unpack(tmp, extracted)) return null;

  // The archive holds the binary at its root.
  const unpacked = path.join(extracted, binaryName());
  if (!fs.existsSync(unpacked)) {
    log(`archive did not contain ${binaryName()}`);
    return null;
  }
  return unpacked;
}

function archiveSuffix() {
  return '.tar.gz';
}

/** Download with curl or Node's fetch, whichever is available. */
function download(url, dest) {
  if (tryCurl(url, dest)) return true;
  return tryFetch(url, dest);
}

function tryCurl(url, dest) {
  try {
    execFileSync('curl', ['-fsSL', url, '-o', dest], { stdio: 'pipe' });
    return fs.existsSync(dest) && fs.statSync(dest).size > 0;
  } catch {
    return false;
  }
}

async function tryFetch(url, dest) {
  try {
    const response = await fetch(url, { redirect: 'follow' });
    if (!response.ok) return false;
    const buffer = Buffer.from(await response.arrayBuffer());
    fs.writeFileSync(dest, buffer);
    return buffer.length > 0;
  } catch {
    return false;
  }
}

/** Extract a .tar.gz. Uses the system tar, which is present everywhere we ship. */
function unpack(archive, into) {
  try {
    execFileSync('tar', ['-xzf', archive, '-C', into], { stdio: 'pipe' });
    return true;
  } catch {
    // Windows 10+ ships bsdtar, which handles .tar.gz under the same name; the
    // failure above may mean something else, so try PowerShell as a last resort.
    if (process.platform === 'win32') {
      try {
        execFileSync(
          'powershell',
          ['-NoProfile', '-Command', `tar -xzf '${archive}' -C '${into}'`],
          { stdio: 'pipe' },
        );
        return true;
      } catch {
        return false;
      }
    }
    return false;
  }
}

async function main() {
  fs.mkdirSync(BIN_DIR, { recursive: true });

  if (alreadyInstalled()) {
    log('binary already present, skipping download');
    return;
  }

  const dest = destination();

  const sources = [fromEnv(), fromOptionalPackage()];
  if (sources.every((s) => s === null)) {
    const triple = targetTriple();
    if (triple) sources.push(fromGitHub(triple));
  }

  const source = sources.find(Boolean);
  if (!source) {
    log('');
    log('No prebuilt binary is available for this platform.');
    log('None are published yet — build it from source instead:');
    log('');
    log('  git clone https://github.com/' + REPO);
    log('  cd mcp-sandman && cargo install --path .');
    log('');
    log('Already have a binary? Point MCP_SANDMAN_BINARY at it and skip this step.');
    log('');
    return;
  }

  fs.copyFileSync(source, dest);
  if (process.platform !== 'win32') fs.chmodSync(dest, 0o755);

  const size = (fs.statSync(dest).size / 1024 / 1024).toFixed(2);
  log(`installed ${dest} (${size} MB)`);
}

main().catch((error) => {
  // Never fail the install. See the note at the top of this file.
  log(`install step skipped: ${error.message}`);
});