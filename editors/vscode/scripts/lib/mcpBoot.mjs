/**
 * MCP server lifecycle: argument parsing, workspace-root resolution
 * (--workspace > client roots > working directory), and the ParadoxCode
 * language-server session — spawn, LSP handshake, availability-gated
 * requests, and graceful shutdown. Mirrors the extension's agent layer
 * (src/agent/server.ts): the boot happens asynchronously after the MCP
 * handshake, and tool calls wait for readiness instead of racing it.
 */

import { spawn } from 'node:child_process';
import { existsSync, statSync } from 'node:fs';
import { homedir } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

import { LspClient } from './client.mjs';

const REPOSITORY_ROOT = resolve(
  dirname(fileURLToPath(import.meta.url)),
  '..', '..', '..', '..',
);

export const AVAILABILITY_WAIT_MS = 30_000;
export const REQUEST_TIMEOUT_MS = 120_000;
const AVAILABILITY_POLL_MS = 250;
const SHUTDOWN_REQUEST_MS = 5_000;
const SHUTDOWN_KILL_GRACE_MS = 3_000;

export const USAGE = `Usage: node editors/vscode/scripts/mcp.mjs [--server PATH] [--workspace PATH] [--timeout-ms N]

Stdio MCP server exposing ParadoxCode's read-only EU4 tools (rule search,
localisation search, symbol search, text validation, hover info) over the
ParadoxCode language server. Run it from an MCP client; stdout is protocol
traffic, logs go to stderr.

Options:
  --server PATH        paradoxcode executable (explicit path must exist; auto-detected
                       from target/{debug,release} only when omitted, then PATH lookup)
  --workspace PATH     workspace root to index (default: the MCP client's roots, then
                       the working directory)
  --timeout-ms N       per-request server timeout (default: ${REQUEST_TIMEOUT_MS})
  --help               show this help

Environment equivalents: PDC_MCP_SERVER, PDC_MCP_WORKSPACE, PDC_MCP_TIMEOUT_MS.
`;

export class CliUsageError extends Error {
  constructor(message) {
    super(`${message}\n\n${USAGE}`);
    this.name = 'CliUsageError';
    this.detail = message;
  }
}

function envValue(name) {
  const value = process.env[name];
  return value && value.trim() ? value.trim() : undefined;
}

function parsePositiveInteger(value, label) {
  if (!/^[0-9]+$/.test(String(value))) {
    throw new CliUsageError(`${label} must be a positive integer, got ${JSON.stringify(value)}`);
  }
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < 1) {
    throw new CliUsageError(`${label} is outside the supported range: ${value}`);
  }
  return parsed;
}

export function parseArgs(argv) {
  const options = {
    server: envValue('PDC_MCP_SERVER'),
    workspace: envValue('PDC_MCP_WORKSPACE'),
    timeoutMs: parsePositiveInteger(
      envValue('PDC_MCP_TIMEOUT_MS') || REQUEST_TIMEOUT_MS,
      'timeout',
    ),
  };
  const valueOptions = new Map([
    ['--server', 'server'],
    ['--workspace', 'workspace'],
    ['--timeout-ms', 'timeoutMs'],
  ]);
  const seen = new Set();
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === '--help' || argument === '-h') {
      return { help: true };
    }
    const key = valueOptions.get(argument);
    if (!key) {
      throw new CliUsageError(`unknown option: ${argument}`);
    }
    if (seen.has(key)) {
      throw new CliUsageError(`option supplied more than once: ${argument}`);
    }
    seen.add(key);
    const value = argv[index + 1];
    if (!value || value.startsWith('--')) {
      throw new CliUsageError(`missing value for ${argument}`);
    }
    options[key] = key === 'timeoutMs' ? parsePositiveInteger(value, argument) : value;
    index += 1;
  }
  return options;
}

/**
 * Resolves the paradoxcode executable. An explicit --server path pins the
 * binary and must exist; the auto-detect order is target/{debug,release}
 * and finally a bare name for PATH lookup.
 */
export function resolveServerBinary(explicit) {
  if (explicit) {
    const candidate = resolve(explicit);
    if (!existsSync(candidate) || !statSync(candidate).isFile()) {
      throw new CliUsageError(
        `--server executable was not found: ${candidate} `
          + '(pass an existing path, or omit --server to auto-detect target/{debug,release})',
      );
    }
    return candidate;
  }
  const executableName = process.platform === 'win32' ? 'paradoxcode.exe' : 'paradoxcode';
  const candidates = [
    join(REPOSITORY_ROOT, 'target', 'debug', executableName),
    join(REPOSITORY_ROOT, 'target', 'release', executableName),
  ];
  for (const candidate of candidates) {
    if (existsSync(candidate) && statSync(candidate).isFile()) return candidate;
  }
  return 'paradoxcode';
}

/** File-path roots only; other URI schemes are skipped. */
function rootPathsFromClientRoots(roots) {
  const paths = [];
  for (const root of roots ?? []) {
    const uri = root?.uri;
    if (typeof uri !== 'string' || !uri.startsWith('file:')) continue;
    try {
      paths.push(fileURLToPath(uri));
    } catch {
      // Skip unparsable file URIs.
    }
  }
  return paths;
}

function existingDirectory(path) {
  try {
    return statSync(path).isDirectory() ? path : undefined;
  } catch {
    return undefined;
  }
}

/**
 * Resolves the workspace root: explicit flag first, then the client's roots,
 * then the working directory (never the home directory itself — an MCP client
 * launched from home would otherwise index it). Returns undefined when
 * nothing resolvable is available; tool calls then explain how to fix it.
 */
export function resolveWorkspaceRoot(flagRoot, clientRoots, cwd = process.cwd()) {
  if (flagRoot) {
    const resolved = existingDirectory(resolve(flagRoot));
    if (!resolved) {
      throw new CliUsageError(`--workspace is not an existing directory: ${resolve(flagRoot)}`);
    }
    return { path: resolved, source: 'flag' };
  }
  for (const candidate of rootPathsFromClientRoots(clientRoots)) {
    const resolved = existingDirectory(candidate);
    if (resolved) return { path: resolved, source: 'roots' };
  }
  const cwdDirectory = existingDirectory(cwd);
  if (cwdDirectory && resolve(cwdDirectory) !== resolve(homedir())) {
    return { path: cwdDirectory, source: 'cwd' };
  }
  return undefined;
}

/** Races a request against a wall-clock timeout so one slow query cannot stall a tool call. */
export function withTimeout(work, timeoutMs, label) {
  let timer;
  return Promise.race([
    work,
    new Promise((_, reject) => {
      timer = setTimeout(
        () => reject(new Error(`${label} timed out after ${Math.round(timeoutMs / 1000)}s`)),
        timeoutMs,
      );
    }),
  ]).finally(() => clearTimeout(timer));
}

/**
 * One ParadoxCode language-server process. `boot()` is fire-and-forget from
 * the MCP entry; `request()` gates on readiness with a bounded wait so
 * early tool calls surface a readable state instead of racing the spawn.
 */
export class ServerSession {
  constructor({ server, workspace, timeoutMs = REQUEST_TIMEOUT_MS }) {
    this.server = server;
    this.workspace = workspace;
    this.timeoutMs = timeoutMs;
    this.state = 'idle'; // idle | booting | ready | failed | stopped
    this.bootError = undefined;
    this.bootPromise = undefined;
    this.client = undefined;
    this.child = undefined;
  }

  boot() {
    if (this.bootPromise) return this.bootPromise;
    this.state = 'booting';
    this.bootPromise = this.bootNow()
      .then(() => {
        this.state = 'ready';
      })
      .catch((error) => {
        this.state = 'failed';
        this.bootError = error;
        throw error;
      });
    return this.bootPromise;
  }

  async bootNow() {
    const child = spawn(this.server, [], {
      cwd: this.workspace,
      stdio: ['pipe', 'pipe', 'pipe'],
      windowsHide: true,
    });
    this.child = child;
    this.client = new LspClient(child, { timeoutMs: this.timeoutMs });
    const initializeParams = {
      processId: process.pid,
      clientInfo: { name: 'paradoxcode-mcp', version: '1' },
      workspaceFolders: [
        { uri: pathToFileURL(this.workspace).href, name: basename(this.workspace) },
      ],
      capabilities: {
        window: { workDoneProgress: true },
        workspace: { didChangeWatchedFiles: { dynamicRegistration: false } },
      },
      initializationOptions: {},
      trace: 'off',
    };
    try {
      await this.client.request('initialize', initializeParams, this.timeoutMs);
    } catch (error) {
      // Surface spawn/handshake failures with the server's own stderr when
      // captured; the raw exit story otherwise hides the actual cause.
      const detail = this.client.serverStderr
        ? ` (${this.client.serverStderr.slice(-400).trim()})`
        : '';
      throw new Error(
        `The ParadoxCode server failed to start: ${error.message}${detail}`,
      );
    }
    this.client.notify('initialized', {});
  }

  /** Waits until the session is ready, mirroring acquireAgentClient's contract. */
  async ready() {
    const deadline = Date.now() + AVAILABILITY_WAIT_MS;
    for (;;) {
      if (this.state === 'ready') return;
      if (this.state === 'failed') {
        throw new Error(`The ParadoxCode server failed to start: ${this.bootError?.message ?? 'unknown error'}`);
      }
      if (this.state === 'stopped') {
        throw new Error(
          'The ParadoxCode server is no longer running; retry the tool call shortly.',
        );
      }
      if (Date.now() >= deadline) {
        throw new Error(
          this.state === 'booting'
            ? 'The ParadoxCode server is still starting (first boot may build the Vanilla cache); retry shortly.'
            : 'The ParadoxCode server is not available. Check the MCP server logs and retry.',
        );
      }
      await new Promise((resolve) => setTimeout(resolve, AVAILABILITY_POLL_MS));
    }
  }

  async request(method, params, label = method) {
    await this.ready();
    return withTimeout(
      this.client.request(method, params, this.timeoutMs),
      this.timeoutMs,
      label,
    );
  }

  async shutdown() {
    const client = this.client;
    const child = this.child;
    this.state = 'stopped';
    if (!client || !child) return;
    try {
      await withTimeout(
        Promise.resolve(client.request('shutdown', {}, SHUTDOWN_REQUEST_MS)),
        SHUTDOWN_REQUEST_MS,
        'Shutdown',
      );
    } catch {
      // A server that never answers shutdown still gets the exit + kill path.
    }
    try {
      client.notify('exit', {});
    } catch {
      // Transport may already be gone.
    }
    const exited = new Promise((resolveExit) => {
      if (child.exitCode !== null) {
        resolveExit();
      } else {
        child.once('exit', () => resolveExit());
        setTimeout(() => {
          child.kill('SIGTERM');
          setTimeout(() => child.kill('SIGKILL'), SHUTDOWN_KILL_GRACE_MS);
        }, SHUTDOWN_KILL_GRACE_MS).unref?.();
      }
    });
    await Promise.race([exited, new Promise((resolve) => setTimeout(resolve, 8_000))]);
  }
}
