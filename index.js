/**
 * mcp-sandman as a Node library.
 *
 * Most people will just run the CLI, but the pieces are exported so the proxy
 * can be embedded in a Node agent harness — spawn it, supervise it, or read
 * its config without shelling out.
 */

import { spawn, execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const HERE = path.dirname(fileURLToPath(import.meta.url));
const { version } = require('./package.json');

/**
 * Locate the native binary.
 *
 * @returns {string|null} an absolute path, or null when it is not installed
 */
export function findBinary() {
  if (process.env.MCP_SANDMAN_BINARY && fs.existsSync(process.env.MCP_SANDMAN_BINARY)) {
    return process.env.MCP_SANDMAN_BINARY;
  }

  const local = path.join(HERE, 'bin', process.platform === 'win32' ? 'mcp-sandman.exe' : 'mcp-sandman');
  if (fs.existsSync(local)) return local;

  // Fall back to a copy on PATH, which is where a source install puts it.
  try {
    const found = execFileSync(process.platform === 'win32' ? 'where' : 'which', ['mcp-sandman'], {
      encoding: 'utf8',
      stdio: 'pipe',
    })
      .split(/\r?\n/)[0]
      .trim();
    return found || null;
  } catch {
    return null;
  }
}

/**
 * Spawn the sandbox over stdio.
 *
 * The returned child speaks JSON-RPC on its stdin/stdout, exactly like any
 * other MCP server, so it drops straight into an MCP client that spawns
 * servers from a config.
 *
 * @param {object} options
 * @param {string} [options.config] path to a sandman.toml
 * @param {string[]} [options.args] extra CLI arguments
 * @param {object} [options.env] extra environment for the sandbox
 * @returns {import('node:child_process').ChildProcess}
 */
export function serve({ config, args = [], env } = {}) {
  const binary = findBinary();
  if (!binary) {
    throw new Error(
      'mcp-sandman binary not found. Run `npm rebuild mcp-sandman`, or set ' +
        'MCP_SANDMAN_BINARY to an existing binary.',
    );
  }

  const argv = [];
  if (config) argv.push('--config', config);
  argv.push(...args);

  return spawn(binary, argv, {
    stdio: 'inherit',
    env: { ...process.env, ...env },
    windowsHide: true,
  });
}

/**
 * Run a subcommand and return its stdout. For `check`, `doctor` and `--version`,
 * where the output is a report rather than a JSON-RPC stream.
 *
 * @param {string[]} args
 * @returns {string}
 */
export function run(args = []) {
  const binary = findBinary();
  if (!binary) {
    throw new Error(
      'mcp-sandman binary not found. Run `npm rebuild mcp-sandman`, or set ' +
        'MCP_SANDMAN_BINARY to an existing binary.',
    );
  }
  return execFileSync(binary, args, { encoding: 'utf8' });
}

/**
 * List the tools a policy exposes, by running `doctor` and parsing the result.
 *
 * @param {string} config path to a sandman.toml
 * @returns {string[]} the exposed tool names
 */
export function exposedTools(config) {
  const output = run(['--config', config, 'doctor']);
  const tools = [];
  for (const line of output.split(/\r?\n/)) {
    const match = line.match(/^\s{2}(\S+)\s*$/);
    if (match) tools.push(match[1]);
  }
  return tools;
}

/** Print a starter policy for a command. */
export function init(command) {
  return run(['init', command]);
}

export const VERSION = version;

export default { findBinary, serve, run, exposedTools, init, VERSION };