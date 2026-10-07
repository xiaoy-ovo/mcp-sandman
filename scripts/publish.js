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
/**
 * Run a command, capturing output so a failure can be explained.
 *
 * npm prints the reason to stderr and `inherit` would swallow it from the
 * error object, so publish captures instead of piping through and echoes on
 * success.
 */
function run(command, args, options = {}) {
  log(`> ${command} ${args.join(' ')}`);
  return execFileSync(command, args, {
    encoding: 'utf8',
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

/**
 * Run npm.
 *
 * npm's own errors are the useful part here — a 401 or a 403 names the exact
 * problem. Wrapping them in a raw stack trace buries that, so translate the
 * common ones and print the guidance inline.
 */
function runNpm(args) {
  try {
    const output = run('npm', args);
    if (output) log(output);
  } catch (error) {
    const output = `${error.stdout ?? ''}${error.stderr ?? ''}`;
    if (output.trim()) log(output.trim());

    if (error.status === 401 || /E401|not logged in/i.test(output)) {
      log('');
      log('npm rejected the credentials. Log in again:');
      log('');
      log('  npm login');
      log('');
      log('If you use two-factor auth, a plain login token cannot publish.');
      log('Create one at https://www.npmjs.com/settings#access-tokens');
      log('with Read and Write scope and Bypass 2FA enabled, then:');
      log('');
      log('  npm login --auth-type=legacy');
      log('');
      process.exit(1);
    }

    if (error.status === 403 || /E403|two-factor|2fa/i.test(output)) {
      log('');
      log('npm refused the publish. This is almost always the 2FA policy:');
      log('');
      log('  npm requires a granular access token with Bypass 2FA enabled.');
      log('  A normal login token is not enough.');
      log('');
      log('Create one at https://www.npmjs.com/settings#access-tokens');
      log('  Token type:      Automation (or Granular Access Token)');
      log('  Permissions:    Read and Write');
      log('  Packages:       Only select packages -> mcp-sandman');
      log('  Bypass 2FA:     enabled');
      log('');
      log('Then:');
      log('  npm login --auth-type=legacy');
      log('');
      process.exit(1);
    }

    throw error;
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
    log(
      `  bundled binary: ${
        fs.existsSync(binary)
          ? `${binary} (${fs.statSync(binary).size} bytes)`
          : 'none for this platform'
      }`,
    );
    log('  run: npm publish --access public');
    return;
  }

  // `prepublishOnly` builds and verifies the binary, so this is the last step.
  runNpm(['publish', '--access', 'public']);

  log(`\npublished ${pkg.name}@${VERSION}`);
  log('install with:');
  log(`  npm install -g ${pkg.name}`);
}

main();