#!/usr/bin/env node
/**
 * Publish the npm package and its GitHub release assets.
 *
 *   node scripts/publish.js            # build, tag, publish, upload
 *   node scripts/publish.js --dry-run  # everything except the uploads
 *
 * The npm package and the release must carry the same version: postinstall
 * fetches `v<version>` from the release, so a mismatched pair installs a
 * package whose download 404s.
 */

import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const DIST = path.join(ROOT, 'dist');
const pkg = require('../package.json');

const dryRun = process.argv.includes('--dry-run');
const VERSION = pkg.version;
const TAG = `v${VERSION}`;

const log = (msg) => process.stderr.write(`${msg}\n`);

function run(command, args, options = {}) {
  log(`> ${command} ${args.join(' ')}`);
  execFileSync(command, args, { stdio: 'inherit', cwd: ROOT, ...options });
}

/** Run git and return its trimmed stdout. */
function git(...args) {
  return execFileSync('git', args, { encoding: 'utf8', cwd: ROOT }).trim();
}

function check() {
  const problems = [];

  if (!fs.existsSync(DIST)) {
    problems.push('dist/ is missing — run `node scripts/build-binaries.js` first');
  } else {
    const archives = fs.readdirSync(DIST).filter((f) => f.endsWith('.tar.gz'));
    if (archives.length < 6) {
      problems.push(`dist/ has ${archives.length} archives, expected 6`);
    }
  }

  const current = git('rev-parse', 'HEAD');
  const tagExists = git('tag', '--list', TAG);
  if (tagExists) {
    problems.push(`${TAG} already exists — bump the version or delete the tag`);
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
    log(`\ndry run: would publish ${pkg.name}@${VERSION} with tag ${TAG}`);
    for (const archive of fs.readdirSync(DIST)) {
      log(`  dist/${archive}`);
    }
    return;
  }

  // Tag first: the release and the npm tarball must agree on the version, and
  // the tag is what postinstall resolves `latest/download` against.
  run('git', ['tag', '-a', TAG, '-m', `Release ${TAG}`]);
  run('git', ['push', 'origin', TAG]);

  run('gh', ['release', 'create', TAG, ...fs.readdirSync(DIST).map((f) => path.join('dist', f)),
    '--title', TAG,
    '--generate-notes',
  ]);

  log(`\npublished ${pkg.name}@${VERSION}`);
  log('install with:');
  log(`  npm install -g ${pkg.name}`);
}

main();