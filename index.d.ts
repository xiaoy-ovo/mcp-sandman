/**
 * Type definitions for mcp-sandman.
 */

import type { ChildProcess } from 'node:child_process';

export interface ServeOptions {
  /** Path to a sandman.toml policy file. */
  config?: string;
  /** Extra command-line arguments passed to the binary. */
  args?: string[];
  /** Extra environment variables for the sandbox process. */
  env?: NodeJS.ProcessEnv;
}

/**
 * Spawn the sandbox over stdio. The child speaks JSON-RPC on stdin/stdout,
 * like any other MCP server.
 */
export function serve(options?: ServeOptions): ChildProcess;

/** Run a subcommand (`check`, `doctor`, `init`) and return its stdout. */
export function run(args?: string[]): string;

/** List the tool names a policy exposes. */
export function exposedTools(config: string): string[];

/** Print a starter policy for an upstream command. */
export function init(command: string): string;

/** Absolute path to the native binary, or null when it is not installed. */
export function findBinary(): string | null;

export const VERSION: string;

declare const _default: {
  findBinary: typeof findBinary;
  serve: typeof serve;
  run: typeof run;
  exposedTools: typeof exposedTools;
  init: typeof init;
  VERSION: typeof VERSION;
};

export default _default;