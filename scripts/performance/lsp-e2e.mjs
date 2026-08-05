#!/usr/bin/env node

/**
 * Measure the pdx-ls stdio JSON-RPC path without depending on a particular
 * user's checkout or home directory.  The default workspace is a temporary
 * one-file EU4 fixture; pass --workspace/--document to measure another tree.
 * Vanilla cache resolution is explicit first, then PDX_PERF_CACHE, then the
 * platform user configuration written by `pdx setup vanilla`.
 */

import { execFile, spawn } from 'node:child_process';
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, isAbsolute, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { promisify } from 'node:util';

const execFileAsync = promisify(execFile);
const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url));
const REPOSITORY_ROOT = resolve(SCRIPT_DIR, '..', '..');
const DEFAULT_TIMEOUT_MS = 60_000;
const DEFAULT_MEMORY_INTERVAL_MS = 250;
const FIXTURE_TEXT = 'country_event = {\n    id = my_perf_event\n}\n';
const NO_CACHE_NAME = `pdx-perf-no-cache-${process.pid}.pdxindex`;

const USAGE = `Usage: node scripts/performance/lsp-e2e.mjs [options]

Options:
  --server PATH             pdx-ls executable (default target/release/pdx-ls[.exe])
  --workspace DIR           workspace root (default: temporary one-file fixture)
  --document FILE            document to open (relative to workspace or absolute)
  --cache FILE               Vanilla cache (overrides user config)
  --no-cache                 explicitly disable Vanilla cache loading
  --project-config FILE      optional .pdx/project.toml passed in initializationOptions
  --line N                   zero-based query line (default: fixture's id value)
  --character N              zero-based UTF-16 query character
  --timeout-ms N             request timeout (default: ${DEFAULT_TIMEOUT_MS})
  --memory-interval-ms N     working-set sampling interval (default: ${DEFAULT_MEMORY_INTERVAL_MS})
  --keep-workspace            keep an automatically-created fixture for inspection
  --help                      show this help

Environment equivalents: PDX_PERF_SERVER, PDX_PERF_WORKSPACE, PDX_PERF_DOCUMENT,
PDX_PERF_CACHE, PDX_PERF_PROJECT_CONFIG, PDX_PERF_LINE, PDX_PERF_CHARACTER,
PDX_PERF_TIMEOUT_MS, PDX_PERF_MEMORY_INTERVAL_MS.
`;

function envValue(name) {
  const value = process.env[name];
  return value && value.trim() ? value.trim() : undefined;
}

function parseNonNegativeInteger(value, label) {
  if (!/^[0-9]+$/.test(String(value))) {
    throw new Error(`${label} must be a non-negative integer, got ${JSON.stringify(value)}`);
  }
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed)) {
    throw new Error(`${label} is too large: ${value}`);
  }
  return parsed;
}

function parsePositiveInteger(value, label) {
  const parsed = parseNonNegativeInteger(value, label);
  if (parsed < 1) {
    throw new Error(`${label} must be greater than zero`);
  }
  return parsed;
}

function takeOption(argv, index, option) {
  const value = argv[index + 1];
  if (!value || value.startsWith('--')) {
    throw new Error(`${option} requires a value\n\n${USAGE}`);
  }
  return [value, index + 1];
}

function parseArguments(argv) {
  const options = {
    server: undefined,
    workspace: undefined,
    document: undefined,
    cache: undefined,
    projectConfig: undefined,
    line: undefined,
    character: undefined,
    timeoutMs: DEFAULT_TIMEOUT_MS,
    memoryIntervalMs: DEFAULT_MEMORY_INTERVAL_MS,
    noCache: false,
    keepWorkspace: false,
    help: false,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === '--help' || argument === '-h') {
      options.help = true;
      continue;
    }
    if (argument === '--keep-workspace') {
      options.keepWorkspace = true;
      continue;
    }
    if (argument === '--no-cache') {
      if (options.cache !== undefined) {
        throw new Error('--no-cache cannot be combined with --cache');
      }
      options.noCache = true;
      continue;
    }
    const [name, inlineValue] = argument.split('=', 2);
    let value = inlineValue;
    if (value === undefined) {
      const needsValue = new Set([
        '--server',
        '--workspace',
        '--document',
        '--cache',
        '--project-config',
        '--line',
        '--character',
        '--timeout-ms',
        '--memory-interval-ms',
      ]);
      if (!needsValue.has(name)) {
        throw new Error(`unknown option ${argument}\n\n${USAGE}`);
      }
      [value, index] = takeOption(argv, index, name);
    }
    switch (name) {
      case '--server':
        options.server = value;
        break;
      case '--workspace':
        options.workspace = value;
        break;
      case '--document':
        options.document = value;
        break;
      case '--cache':
        if (options.noCache) {
          throw new Error('--cache cannot be combined with --no-cache');
        }
        options.cache = value;
        break;
      case '--project-config':
        options.projectConfig = value;
        break;
      case '--line':
        options.line = parseNonNegativeInteger(value, '--line');
        break;
      case '--character':
        options.character = parseNonNegativeInteger(value, '--character');
        break;
      case '--timeout-ms':
        options.timeoutMs = parsePositiveInteger(value, '--timeout-ms');
        break;
      case '--memory-interval-ms':
        options.memoryIntervalMs = parsePositiveInteger(value, '--memory-interval-ms');
        break;
      default:
        throw new Error(`unknown option ${argument}\n\n${USAGE}`);
    }
  }
  return options;
}

function applyEnvironment(options) {
  const fromEnvironment = {
    server: envValue('PDX_PERF_SERVER'),
    workspace: envValue('PDX_PERF_WORKSPACE'),
    document: envValue('PDX_PERF_DOCUMENT'),
    cache: envValue('PDX_PERF_CACHE'),
    projectConfig: envValue('PDX_PERF_PROJECT_CONFIG'),
  };
  for (const [key, value] of Object.entries(fromEnvironment)) {
    if (options[key] === undefined && value !== undefined) options[key] = value;
  }
  if (options.line === undefined && envValue('PDX_PERF_LINE') !== undefined) {
    options.line = parseNonNegativeInteger(envValue('PDX_PERF_LINE'), 'PDX_PERF_LINE');
  }
  if (options.character === undefined && envValue('PDX_PERF_CHARACTER') !== undefined) {
    options.character = parseNonNegativeInteger(
      envValue('PDX_PERF_CHARACTER'),
      'PDX_PERF_CHARACTER',
    );
  }
  if (envValue('PDX_PERF_TIMEOUT_MS') !== undefined && options.timeoutMs === DEFAULT_TIMEOUT_MS) {
    options.timeoutMs = parsePositiveInteger(
      envValue('PDX_PERF_TIMEOUT_MS'),
      'PDX_PERF_TIMEOUT_MS',
    );
  }
  if (
    envValue('PDX_PERF_MEMORY_INTERVAL_MS') !== undefined &&
    options.memoryIntervalMs === DEFAULT_MEMORY_INTERVAL_MS
  ) {
    options.memoryIntervalMs = parsePositiveInteger(
      envValue('PDX_PERF_MEMORY_INTERVAL_MS'),
      'PDX_PERF_MEMORY_INTERVAL_MS',
    );
  }
  if (options.noCache && options.cache !== undefined) {
    throw new Error('--no-cache cannot be combined with PDX_PERF_CACHE/--cache');
  }
  const noCacheEnvironment = envValue('PDX_PERF_NO_CACHE');
  if (noCacheEnvironment && /^(1|true|yes)$/i.test(noCacheEnvironment)) {
    if (options.cache !== undefined) {
      throw new Error('--no-cache cannot be combined with PDX_PERF_CACHE');
    }
    options.noCache = true;
  }
  return options;
}

function inputPath(value, base = process.cwd()) {
  return isAbsolute(value) ? value : resolve(base, value);
}

function requireFile(path, label) {
  let stats;
  try {
    stats = statSync(path);
  } catch (error) {
    throw new Error(`${label} does not exist or is not readable: ${path} (${error.message})`);
  }
  if (!stats.isFile()) throw new Error(`${label} is not a file: ${path}`);
}

function requireDirectory(path, label) {
  let stats;
  try {
    stats = statSync(path);
  } catch (error) {
    throw new Error(`${label} does not exist or is not readable: ${path} (${error.message})`);
  }
  if (!stats.isDirectory()) throw new Error(`${label} is not a directory: ${path}`);
}

function userConfigurationPath() {
  const platform = process.platform;
  if (platform === 'win32') {
    const appData = envValue('APPDATA');
    return appData ? join(appData, 'ParadoxCode', 'config.toml') : undefined;
  }
  const home = envValue('HOME');
  if (!home) return undefined;
  if (platform === 'darwin') return join(home, 'Library', 'Application Support', 'ParadoxCode', 'config.toml');
  return join(envValue('XDG_CONFIG_HOME') ?? join(home, '.config'), 'paradoxcode', 'config.toml');
}

function conventionalCachePath() {
  if (process.platform === 'win32') {
    const localAppData = envValue('LOCALAPPDATA');
    return localAppData
      ? join(localAppData, 'ParadoxCode', 'cache', 'eu4', 'vanilla.pdxindex')
      : undefined;
  }
  const home = envValue('HOME');
  if (!home) return undefined;
  if (process.platform === 'darwin') {
    return join(home, 'Library', 'Caches', 'ParadoxCode', 'eu4', 'vanilla.pdxindex');
  }
  return join(
    envValue('XDG_CACHE_HOME') ?? join(home, '.cache'),
    'paradoxcode',
    'eu4',
    'vanilla.pdxindex',
  );
}

function decodeTomlString(token) {
  if (token.startsWith("'") && token.endsWith("'")) return token.slice(1, -1);
  if (token.startsWith('"') && token.endsWith('"')) {
    try {
      return JSON.parse(token);
    } catch (error) {
      throw new Error(`cannot decode vanilla_cache in ${userConfigurationPath()}: ${error.message}`);
    }
  }
  throw new Error(`vanilla_cache in ${userConfigurationPath()} must be a TOML quoted string`);
}

function cacheFromUserConfiguration(configPath) {
  let text;
  try {
    text = readFileSync(configPath, 'utf8');
  } catch (error) {
    throw new Error(`cannot read user configuration ${configPath}: ${error.message}`);
  }
  let section = '';
  for (const rawLine of text.split(/\r?\n/)) {
    const sectionMatch = /^\s*\[([^\]]+)\]\s*(?:#.*)?$/.exec(rawLine);
    if (sectionMatch) {
      section = sectionMatch[1].trim();
      continue;
    }
    if (section !== 'games.eu4') continue;
    const valueMatch = /^\s*vanilla_cache\s*=\s*("(?:\\.|[^"])*"|'[^']*')\s*(?:#.*)?$/.exec(
      rawLine,
    );
    if (valueMatch) return decodeTomlString(valueMatch[1]);
  }
  return undefined;
}

function resolveVanillaCache(options) {
  if (options.noCache) return undefined;
  let configuredPath = options.cache;
  let sourceDescription = options.cache ? 'the --cache/PDX_PERF_CACHE option' : undefined;
  const configPath = userConfigurationPath();
  if (configuredPath === undefined && configPath && existsSync(configPath)) {
    configuredPath = cacheFromUserConfiguration(configPath);
    if (configuredPath !== undefined) sourceDescription = `user configuration ${configPath}`;
  }
  if (configuredPath === undefined) {
    const conventional = conventionalCachePath();
    if (conventional && existsSync(conventional)) {
      configuredPath = conventional;
      sourceDescription = 'the platform cache location';
    }
  }
  if (configuredPath === undefined) {
    throw new Error(
      'Vanilla cache was not found. Pass --cache PATH (or PDX_PERF_CACHE), configure [games.eu4].vanilla_cache in the user config, or use --no-cache. The reported cache-backed numbers are not comparable without this precondition.',
    );
  }
  const base = sourceDescription?.startsWith('user configuration') ? dirname(configPath) : process.cwd();
  const cachePath = inputPath(configuredPath, base);
  requireFile(cachePath, 'Vanilla cache');
  return cachePath;
}

function createFixture() {
  const root = mkdtempSync(join(tmpdir(), 'pdx-perf-root-'));
  const events = join(root, 'events');
  mkdirSync(events, { recursive: true });
  const document = join(events, 'perf_event.txt');
  writeFileSync(document, FIXTURE_TEXT, 'utf8');
  return { root, document, text: FIXTURE_TEXT, generated: true };
}

function resolveWorkspaceAndDocument(options) {
  if (options.workspace === undefined) return createFixture();
  const root = inputPath(options.workspace);
  requireDirectory(root, 'Workspace');
  const documentName = options.document ?? join('events', 'perf_event.txt');
  const document = inputPath(documentName, root);
  requireFile(document, 'Document');
  let text;
  try {
    text = readFileSync(document, 'utf8');
  } catch (error) {
    throw new Error(`cannot read document ${document}: ${error.message}`);
  }
  return { root, document, text, generated: false };
}

function offsetToPosition(text, offset) {
  const before = text.slice(0, offset);
  const lines = before.split('\n');
  return { line: lines.length - 1, character: lines[lines.length - 1].length };
}

function defaultQueryPosition(text) {
  const preferred = text.indexOf('my_perf_event');
  if (preferred >= 0) return offsetToPosition(text, preferred);
  const token = /[A-Za-z_][A-Za-z0-9_.-]*/.exec(text);
  return token ? offsetToPosition(text, token.index) : { line: 0, character: 0 };
}

function validatePosition(text, position) {
  const lines = text.split('\n');
  if (position.line >= lines.length) {
    throw new Error(`query line ${position.line} is outside the document (${lines.length} lines)`);
  }
  if (position.character > lines[position.line].length) {
    throw new Error(
      `query character ${position.character} is outside line ${position.line} (${lines[position.line].length} UTF-16 code units)`,
    );
  }
}

function changedText(text) {
  if (text.includes('my_perf_event')) return text.replace('my_perf_event', 'my_perf_event_renamed');
  return `${text}${text.endsWith('\n') ? '' : '\n'}# pdx-perf didChange\n`;
}

function withTimeout(promise, timeoutMs, label) {
  let timer;
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(`timeout waiting for ${label} (${timeoutMs} ms)`)), timeoutMs);
  });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
}

function waitForSpawn(child, timeoutMs) {
  return withTimeout(
    new Promise((resolvePromise, rejectPromise) => {
      const onSpawn = () => {
        cleanup();
        resolvePromise();
      };
      const onError = (error) => {
        cleanup();
        rejectPromise(error);
      };
      const cleanup = () => {
        child.off('spawn', onSpawn);
        child.off('error', onError);
      };
      child.once('spawn', onSpawn);
      child.once('error', onError);
    }),
    timeoutMs,
    'pdx-ls spawn',
  );
}

function waitForExit(child, timeoutMs) {
  if (child.exitCode !== null || child.signalCode !== null) return Promise.resolve();
  return withTimeout(
    new Promise((resolvePromise) => child.once('exit', () => resolvePromise())),
    timeoutMs,
    'pdx-ls exit',
  );
}

class JsonRpcClient {
  constructor(child) {
    this.child = child;
    this.buffer = Buffer.alloc(0);
    this.nextId = 1;
    this.pending = new Map();
    this.stderr = '';
    this.diagnostics = [];
    this.protocolError = undefined;
    child.stdout.on('data', (chunk) => this.consume(chunk));
    child.stderr.on('data', (chunk) => {
      this.stderr = `${this.stderr}${chunk.toString('utf8')}`.slice(-16_384);
    });
    child.on('error', (error) => this.failPending(error));
    child.on('exit', (code, signal) => {
      if (code === 0 || signal === 'SIGTERM' || signal === 'SIGKILL') return;
      this.failPending(new Error(`pdx-ls exited before completing a request (code=${code}, signal=${signal})`));
    });
  }

  failPending(error) {
    for (const pending of this.pending.values()) pending.reject(error);
    this.pending.clear();
  }

  consume(chunk) {
    this.buffer = Buffer.concat([this.buffer, chunk]);
    for (;;) {
      const headerEnd = this.buffer.indexOf('\r\n\r\n');
      if (headerEnd < 0) return;
      const header = this.buffer.subarray(0, headerEnd).toString('ascii');
      const lengthMatch = /^Content-Length:\s*(\d+)\s*$/im.exec(header);
      if (!lengthMatch) {
        this.protocolError = new Error('pdx-ls sent a response without a valid Content-Length header');
        this.failPending(this.protocolError);
        return;
      }
      const bodyLength = Number(lengthMatch[1]);
      const bodyStart = headerEnd + 4;
      if (this.buffer.length < bodyStart + bodyLength) return;
      const body = this.buffer.subarray(bodyStart, bodyStart + bodyLength);
      this.buffer = this.buffer.subarray(bodyStart + bodyLength);
      let message;
      try {
        message = JSON.parse(body.toString('utf8'));
      } catch (error) {
        this.protocolError = new Error(`pdx-ls sent invalid JSON: ${error.message}`);
        this.failPending(this.protocolError);
        return;
      }
      this.handleMessage(message);
    }
  }

  handleMessage(message) {
    if (message.method === 'textDocument/publishDiagnostics') {
      this.diagnostics.push({
        at: performance.now(),
        count: Array.isArray(message.params?.diagnostics) ? message.params.diagnostics.length : 0,
        uri: message.params?.uri,
      });
    }
    if (message.id === undefined || message.id === null) return;
    const pending = this.pending.get(message.id);
    if (!pending) return;
    this.pending.delete(message.id);
    const elapsed = performance.now() - pending.started;
    if (message.error) {
      pending.reject(
        new Error(`${pending.method} failed (${message.error.code}): ${message.error.message ?? 'unknown error'}`),
      );
      return;
    }
    pending.resolve({ elapsed, result: message.result });
  }

  send(message) {
    if (!this.child.stdin.writable) throw new Error('pdx-ls stdin is not writable');
    const body = Buffer.from(JSON.stringify(message), 'utf8');
    const header = Buffer.from(`Content-Length: ${body.length}\r\n\r\n`, 'ascii');
    this.child.stdin.write(Buffer.concat([header, body]));
  }

  notify(method, params) {
    this.send({ jsonrpc: '2.0', method, params });
  }

  request(method, params) {
    const id = this.nextId;
    this.nextId += 1;
    return new Promise((resolvePromise, rejectPromise) => {
      this.pending.set(id, {
        method,
        started: performance.now(),
        resolve: resolvePromise,
        reject: rejectPromise,
      });
      try {
        this.send({ jsonrpc: '2.0', id, method, params });
      } catch (error) {
        this.pending.delete(id);
        rejectPromise(error);
      }
    });
  }
}

async function sampleWorkingSet(pid) {
  if (!pid) return undefined;
  try {
    if (process.platform === 'win32') {
      const { stdout } = await execFileAsync(
        'tasklist',
        ['/FI', `PID eq ${pid}`, '/FO', 'CSV', '/NH'],
        { windowsHide: true, maxBuffer: 1_048_576 },
      );
      const row = stdout
        .split(/\r?\n/)
        .find((line) => line.includes(`"${pid}"`));
      if (!row) return undefined;
      const fields = row.match(/"(?:[^"]|"")*"/g);
      const memory = fields?.at(-1)?.replace(/[^0-9]/g, '');
      const kilobytes = memory ? Number(memory) : Number.NaN;
      return Number.isFinite(kilobytes) ? kilobytes * 1024 : undefined;
    }
    const { stdout } = await execFileAsync('ps', ['-o', 'rss=', '-p', String(pid)], {
      maxBuffer: 1_048_576,
    });
    const kilobytes = Number(stdout.trim().split(/\s+/)[0]);
    return Number.isFinite(kilobytes) && kilobytes >= 0 ? kilobytes * 1024 : undefined;
  } catch {
    return undefined;
  }
}

class WorkingSetSampler {
  constructor(pid, intervalMs) {
    this.pid = pid;
    this.intervalMs = intervalMs;
    this.timer = undefined;
    this.inFlight = undefined;
    this.peakBytes = undefined;
    this.lastBytes = undefined;
  }

  start() {
    this.timer = setInterval(() => {
      void this.sample();
    }, this.intervalMs);
    void this.sample();
  }

  async sample() {
    if (this.inFlight) return this.inFlight;
    this.inFlight = sampleWorkingSet(this.pid)
      .then((bytes) => {
        if (bytes === undefined) return;
        this.lastBytes = bytes;
        this.peakBytes = this.peakBytes === undefined ? bytes : Math.max(this.peakBytes, bytes);
      })
      .finally(() => {
        this.inFlight = undefined;
      });
    return this.inFlight;
  }

  async stop() {
    if (this.timer) clearInterval(this.timer);
    this.timer = undefined;
    await this.sample();
  }
}

function formatBytes(bytes) {
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GiB`;
}

async function waitForDiagnostic(client, uri, startIndex, timeoutMs, label) {
  const started = performance.now();
  for (;;) {
    const event = client.diagnostics.slice(startIndex).find((candidate) => candidate.uri === uri);
    if (event) return event;
    if (performance.now() - started >= timeoutMs) {
      throw new Error(`timeout waiting for ${label} (${timeoutMs} ms)`);
    }
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 10));
  }
}

function initializeParameters(root, initializationOptions) {
  const rootUri = pathToFileURL(root).href;
  return {
    processId: null,
    rootUri,
    workspaceFolders: [{ uri: rootUri, name: 'pdx-perf-workspace' }],
    capabilities: {},
    initializationOptions,
  };
}

async function runMeasurement(options, workspace) {
  const cache = resolveVanillaCache(options);
  const serverPath = inputPath(
    options.server ?? join('target', 'release', process.platform === 'win32' ? 'pdx-ls.exe' : 'pdx-ls'),
    REPOSITORY_ROOT,
  );
  requireFile(serverPath, 'pdx-ls executable');
  const projectConfig = options.projectConfig
    ? inputPath(options.projectConfig, workspace.root)
    : undefined;
  if (projectConfig) requireFile(projectConfig, 'project config');

  const initializationOptions = {};
  if (cache) initializationOptions.vanillaIndexCache = cache;
  else initializationOptions.vanillaIndexCache = join(tmpdir(), NO_CACHE_NAME);
  if (projectConfig) initializationOptions.projectConfig = projectConfig;

  const inferredPosition = defaultQueryPosition(workspace.text);
  const position = {
    line: options.line ?? inferredPosition.line,
    character: options.character ?? inferredPosition.character,
  };
  validatePosition(workspace.text, position);
  const uri = pathToFileURL(workspace.document).href;
  const changed = changedText(workspace.text);
  const tSpawn = performance.now();
  const server = spawn(serverPath, [], {
    stdio: ['pipe', 'pipe', 'pipe'],
    windowsHide: true,
  });
  const client = new JsonRpcClient(server);
  const sampler = new WorkingSetSampler(server.pid, options.memoryIntervalMs);
  sampler.start();
  try {
    await waitForSpawn(server, options.timeoutMs);
    const init = await withTimeout(
      client.request('initialize', initializeParameters(workspace.root, initializationOptions)),
      options.timeoutMs,
      'initialize response',
    );
    console.log(`cold start (spawn -> initialize response): ${(performance.now() - tSpawn).toFixed(2)} ms`);
    console.log(`initialize (request -> response): ${init.elapsed.toFixed(2)} ms`);
    client.notify('initialized', {});

    const tOpen = performance.now();
    const beforeOpenDiagnostics = client.diagnostics.length;
    client.notify('textDocument/didOpen', {
      textDocument: { uri, languageId: 'eu4', version: 1, text: workspace.text },
    });
    const firstDiagnostics = await waitForDiagnostic(
      client,
      uri,
      beforeOpenDiagnostics,
      options.timeoutMs,
      'first diagnostics',
    );
    console.log(
      `didOpen -> first publishDiagnostics: ${(firstDiagnostics.at - tOpen).toFixed(2)} ms (${firstDiagnostics.count} diagnostics)`,
    );

    const queryParams = { textDocument: { uri }, position };
    const hover = await withTimeout(
      client.request('textDocument/hover', queryParams),
      options.timeoutMs,
      'hover',
    );
    console.log(`textDocument/hover: ${hover.elapsed.toFixed(2)} ms`);
    const symbols = await withTimeout(
      client.request('textDocument/documentSymbol', { textDocument: { uri } }),
      options.timeoutMs,
      'documentSymbol',
    );
    const symbolCount = Array.isArray(symbols.result) ? symbols.result.length : 'n/a';
    console.log(`textDocument/documentSymbol: ${symbols.elapsed.toFixed(2)} ms (${symbolCount} symbols)`);
    const definition = await withTimeout(
      client.request('textDocument/definition', queryParams),
      options.timeoutMs,
      'definition',
    );
    console.log(`textDocument/definition: ${definition.elapsed.toFixed(2)} ms`);

    const steadyHover = await withTimeout(
      client.request('textDocument/hover', queryParams),
      options.timeoutMs,
      'steady hover',
    );
    console.log(`textDocument/hover (steady): ${steadyHover.elapsed.toFixed(2)} ms`);
    const steadyDefinition = await withTimeout(
      client.request('textDocument/definition', queryParams),
      options.timeoutMs,
      'steady definition',
    );
    console.log(`textDocument/definition (steady): ${steadyDefinition.elapsed.toFixed(2)} ms`);

    const tChange = performance.now();
    const beforeChangeDiagnostics = client.diagnostics.length;
    client.notify('textDocument/didChange', {
      textDocument: { uri, version: 2 },
      contentChanges: [{ text: changed }],
    });
    const changedDiagnostics = await waitForDiagnostic(
      client,
      uri,
      beforeChangeDiagnostics,
      options.timeoutMs,
      'changed diagnostics',
    );
    console.log(
      `didChange -> publishDiagnostics: ${(changedDiagnostics.at - tChange).toFixed(2)} ms (${changedDiagnostics.count} diagnostics)`,
    );

    await withTimeout(client.request('shutdown', null), 5_000, 'shutdown response').catch((error) => {
      console.error(`shutdown warning: ${error.message}`);
    });
    client.notify('exit', null);
    await waitForExit(server, 5_000).catch(() => {
      if (!server.killed) server.kill();
    });
  } finally {
    await sampler.stop();
    if (!server.killed && server.exitCode === null) server.kill();
    if (sampler.peakBytes !== undefined) {
      const suffix = sampler.lastBytes === undefined ? '' : `; final ${formatBytes(sampler.lastBytes)}`;
      console.log(`working set peak: ${formatBytes(sampler.peakBytes)}${suffix}`);
    } else {
      console.log(`working set: unavailable on ${process.platform}; continuing without a child-process sample`);
    }
    if (client.stderr.trim()) console.log(`stderr (truncated): ${client.stderr.slice(-2_000)}`);
  }
}

async function run(options) {
  const workspace = resolveWorkspaceAndDocument(options);
  try {
    await runMeasurement(options, workspace);
  } finally {
    if (workspace.generated && !options.keepWorkspace) {
      rmSync(workspace.root, { recursive: true, force: true });
    }
  }
}

async function main() {
  const options = applyEnvironment(parseArguments(process.argv.slice(2)));
  if (options.help) {
    console.log(USAGE);
    return;
  }
  try {
    await run(options);
  } catch (error) {
    console.error(`MEASUREMENT FAILED: ${error instanceof Error ? error.message : String(error)}`);
    process.exitCode = 1;
  }
}

await main();
