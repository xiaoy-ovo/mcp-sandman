#!/usr/bin/env node
/**
 * Publish the npm package.
 *
 *   node scripts/publish.js --dry-run   # check, publish nothing
 *   node scripts/publish.js             # check, then npm publish
 *
 * `prepublishOnly` runs the build first, so the bundled Windows binary is
 * verified to exist and to answer `--version` before anything ships.
 */

import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const pkg = require('../package.json');

const dryRun = process.argv.includes('--dry-run');
const VERSION = pkg.version;

const log = (msg) => process.stderr.write(`${msg}\n`);

/** npm is a .cmd shim on Windows and needs a shell to spawn. */
function run(command, args, options = {}) {
  log(`> ${command} ${args.join(' ')}`);
  execFileSync(command, args, {
    stdio: 'inherit',
    cwd: ROOT,
    ...(process.platform === 'win32' ? { shell: true } : {}),
    ...options,
  });
}

/** Run git and return its trimmed stdout. */
function git(...args) {
  return execFileSync('git', args, { encoding: 'utf8', cwd: ROOT }).trim();
}

function check() {
  const problems = [];

  // The Windows binary must be present and runnable before publishing: it is
  // what makes `npm i -g` work without a Rust toolchain.
  const binary = path.join(
    ROOT,
    'bin',
    process.platform === 'win32' ? 'mcp-sandman.exe' : 'mcp-sandman',
  );
  if (fs.existsSync(binary)) {
    const size = fs.statSync(binary).size;
    if (size < 1024) {
      problems.push(`${path.basename(binary)} is only ${size} bytes; the build failed partway through`);
    }
  } else if (process.platform === 'win32') {
    problems.push('bin/mcp-sandman.exe is missing — run `npm run build:binary`');
  } else {
    log('note: no bundled binary for this platform; Windows users get one from the tarball, others build from source');
  }

  const status = git('status', '--porcelain');
  if (status) {
    problems.push('working tree is dirty; commit before publishing');
    log(status);
  }

  if (problems.length > 0) {
    log('\nnot ready to publish:');
    for (const problem of problems) log(`  - ${problem}`);
    process.exit(1);
  }
}

function main() {
  check();

  if (dryRun) {
    const binary = path.join(
      ROOT,
      'bin',
      process.platform === 'win32' ? 'mcp-sandman.exe' : 'mcp-sandman',
    );
    log(`\ndry run: would publish ${pkg.name}@${VERSION}`);
    log(`  bundled binary: ${fs.existsSync(binary) ? `${binary} (${fs.statSync(binary).size} bytes)` : 'none'}`);
    log(`  run: npm publish --access public`);
    return;
  }

  // `prepublishOnly` builds and verifies the binary, so this is the last step.
  run('npm', ['publish', '--access', 'public']);

  log(`\npublished ${pkg.name}@${VERSION}`);
  log('install with:');
  log(`  npm install -g ${pkg.name}`);
}

main();