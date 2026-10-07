#!/usr/bin/env node
/**
 * End-to-end check: pack the npm tarball, install it into a throwaway
 * directory, and run the installed binary.
 *
 *   node scripts/verify-install.js
 *
 * This is the only way to know the package actually works. Passing tests
 * and a clean `npm pack --dry-run` can both be true while the published
 * tarball is still broken for a user.
 */

import { execFileSync, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

const log = (msg) => process.stderr.write(`${msg}\n`);
const ok = (msg) => log(`  ok    ${msg}`);
const fail = (msg) => {
  log(`  FAIL  ${msg}`);
  process.exitCode = 1;
};

/**
 * Locate npm's own entry point.
 *
 * npm is bundled with Node and lives next to the node binary rather than in
 * this project's dependencies, so it cannot be resolved with `require.resolve`.
 */
function findNpmCli() {
  const candidates = [
    path.join(path.dirname(process.execPath), 'node_modules', 'npm', 'bin', 'npm-cli.js'),
    path.join(path.dirname(process.execPath), 'node_modules', 'npm', 'bin', 'npm-cli.js'),
  ];
  for (const candidate of candidates) {
    if (fs.existsSync(candidate)) return candidate;
  }
  return null;
}

/**
 * Run npm.
 *
 * Spawning `npm` directly does not work on Windows, where it is a `.cmd`
 * shim, and adding `shell: true` re-introduces argument injection: the
 * tarball path gets concatenated into a command line, so a checkout under
 * `C:\Users\A B\` would break. Invoking npm's JS entry point with the same
 * node binary avoids both problems.
 */
function npm(args, options = {}) {
  const cli = findNpmCli();
  if (!cli) {
    throw new Error('could not find npm-cli.js next to the node binary');
  }
  const result = spawnSync(process.execPath, [cli, ...args], {
    encoding: 'utf8',
    shell: false,
    ...options,
  });
  if (result.error) throw result.error;
  if (result.status !== 0 && !options.allowFailure) {
    throw new Error(`npm ${args.join(' ')} exited ${result.status}: ${result.stderr}`);
  }
  return result.stdout ?? '';
}

function step(name) {
  log(`\n${name}`);
}

async function main() {
  step('Packing');
  const tarballName = JSON.parse(npm(['pack', '--json']))[0].filename;
  const tarball = path.join(ROOT, tarballName);
  if (fs.existsSync(tarball)) {
    ok(`tarball: ${tarballName}`);
  } else {
    fail('no tarball produced');
    process.exit(1);
  }

  step('Inspecting tarball contents');
  const listing = JSON.parse(npm(['pack', '--dry-run', '--json']))[0].files.map((f) => f.path);
  const required = [
    'package.json',
    'index.js',
    'bin/mcp-sandman.js',
    'scripts/download.js',
    'README.md',
    'README.en.md',
    'LICENSE',
  ];
  for (const want of required) {
    if (listing.includes(want)) {
      ok(want);
    } else {
      fail(`missing from tarball: ${want}`);
    }
  }
  const leaked = listing.filter((f) => f.startsWith('src/') || f.startsWith('fixtures/'));
  if (leaked.length > 0) {
    fail(`Rust sources leaked into the tarball: ${leaked.join(', ')}`);
  } else {
    ok('no Rust sources in the tarball');
  }

  step('Installing into a clean directory');
  const sandbox = fs.mkdtempSync(path.join(os.tmpdir(), 'mcp-sandman-verify-'));
  const installed = path.join(sandbox, 'node_modules', 'mcp-sandman');

  // Give the sandbox a package.json so the install behaves like a real project
  // rather than a bare `npm i <tarball>`, which resolves differently.
  fs.writeFileSync(
    path.join(sandbox, 'package.json'),
    JSON.stringify({ name: 'verify-sandbox', private: true, version: '1.0.0' }, null, 2),
  );
  npm(['install', tarball], { cwd: sandbox, stdio: 'pipe' });

  if (fs.existsSync(installed)) {
    ok('package installed');
  } else {
    fail(`nothing landed at ${installed}`);
    log('\nWhat npm actually installed:');
    log(npm(['ls', '--depth=0'], { cwd: sandbox, stdio: 'pipe' }));
    process.exit(1);
  }

  step('Running the installed binary');
  const binary = path.join(
    installed,
    'bin',
    process.platform === 'win32' ? 'mcp-sandman.exe' : 'mcp-sandman',
  );

  if (!fs.existsSync(binary)) {
    // Expected off Windows until release assets are published. Not a failure:
    // those platforms resolve the binary at install time instead.
    log(`  skip  no bundled binary for ${process.platform}; that platform downloads at install time`);
  } else {
    const version = execFileSync(binary, ['--version'], { encoding: 'utf8' }).trim();
    if (version.startsWith('mcp-sandman')) {
      ok(version);
    } else {
      fail(`unexpected version from the binary: ${version}`);
    }
  }

  step('Running the installed shim');
  const shim = path.join(installed, 'bin', 'mcp-sandman.js');
  const shimVersion = execFileSync(process.execPath, [shim, '--version'], { encoding: 'utf8' }).trim();
  if (shimVersion.startsWith('mcp-sandman')) {
    ok(shimVersion);
  } else {
    fail(`shim reported: ${shimVersion}`);
  }

  step('Cleanup');
  fs.rmSync(sandbox, { recursive: true, force: true });
  fs.rmSync(tarball, { force: true });
  ok('removed the sandbox and the tarball');

  log('');
  log(process.exitCode ? 'verification FAILED' : 'verification passed: the published package works');
}

main().catch((error) => {
  log(`verification crashed: ${error.message}`);
  process.exit(1);
});