#!/usr/bin/env node
/**
 * The `mcp-sandman` command installed by npm.
 *
 * This is a thin shim: it locates the native binary and execs it, passing
 * through every argument and both stdio streams untouched. The binary speaks
 * JSON-RPC on stdin/stdout, so anything this process prints to stdout would
 * corrupt the protocol — which is why nothing here writes to stdout but the
 * child itself.
 */

import { spawn, execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const binary = path.join(here, process.platform === 'win32' ? 'mcp-sandman.exe' : 'mcp-sandman');

const missing = `
mcp-sandman: the native binary is missing from ${binary}

The npm package normally downloads it during install. To fix:

  npm rebuild mcp-sandman

Or build it yourself and point the package at it:

  export MCP_SANDMAN_BINARY=/path/to/mcp-sandman

Or install the Rust toolchain once and use it directly:

  cargo install --git https://github.com/xiaoy-ovo/mcp-sandman
`.trim();

/**
 * Find a usable binary.
 *
 * MCP_SANDMAN_BINARY wins so a developer can run a locally built binary
 * against this package without reinstalling.
 */
function locate() {
  if (process.env.MCP_SANDMAN_BINARY) {
    const override = process.env.MCP_SANDMAN_BINARY;
    if (fs.existsSync(override)) return override;
    process.stderr.write(`mcp-sandman: MCP_SANDMAN_BINARY=${override} does not exist\n`);
  }

  if (fs.existsSync(binary)) {
    // npm does not preserve the executable bit on every platform/filesystem,
    // so set it here rather than failing with EACCES at spawn time.
    if (process.platform !== 'win32') {
      try {
        fs.chmodSync(binary, 0o755);
      } catch {
        // Read-only install dirs are fine as long as the bit was already set.
      }
    }
    return binary;
  }

  return null;
}

const found = locate();
if (!found) {
  process.stderr.write(`${missing}\n`);
  process.exit(1);
}

const child = spawn(found, process.argv.slice(2), {
  stdio: 'inherit',
  // Detach from this process's controlling terminal so Ctrl-C reaches the
  // sandbox, which forwards it upstream and shuts the child server down.
  windowsHide: true,
});

child.on('error', (error) => {
  process.stderr.write(`mcp-sandman: could not start ${found}: ${error.message}\n`);
  process.exit(1);
});

child.on('exit', (code, signal) => {
  if (signal) {
    // Reproduce shell convention: dying by signal is 128 + signal number.
    process.exit(128 + (signal === 'SIGINT' ? 2 : 15));
  }
  process.exit(code ?? 0);
});

// Forward the signals that mean "stop" so the sandbox can tear the upstream
// down cleanly rather than leaving an orphaned child process behind.
for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP']) {
  process.on(signal, () => {
    try {
      child.kill(signal);
    } catch {
      // Already gone.
    }
  });
}