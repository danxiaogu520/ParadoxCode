#!/usr/bin/env node

/**
 * Run a whole-Current-Mod diagnostic pass through the real pdx-ls JSON-RPC transport.
 *
 * The language server remains the source of truth for parsing, first-party EU4 rules, Vanilla
 * resolution, and diagnostics. This script only opens each relevant file, collects the normal
 * publishDiagnostics notification, and writes a human-readable plus machine-readable report.
 */

import { spawn } from 'node:child_process';
import {
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  realpathSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { dirname, extname, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { TextDecoder } from 'node:util';

const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url));
const REPOSITORY_ROOT = resolve(SCRIPT_DIR, '..');
const DEFAULT_OUTPUT_DIR = join(REPOSITORY_ROOT, 'diagnostic-reports');
const DEFAULT_TIMEOUT_MS = 120_000;
const DEFAULT_FILE_TIMEOUT_MS = 60_000;
const DEFAULT_MAX_FILES = 100_000;
const MAX_SOURCE_BYTES = 16 * 1024 * 1024;
const MAX_SCAN_DEPTH = 64;
const MAX_REPORTED_SYMLINKS = 256;
const MAX_REPORTED_TOOL_ERRORS = 256;
const JSON_RPC_VERSION = '2.0';
const RELEVANT_EXTENSIONS = new Set(['.txt', '.gfx', '.yml', '.yaml']);
const UTF8_DECODER = new TextDecoder('utf-8', { fatal: true });
const WINDOWS_1252_DECODER = new TextDecoder('windows-1252');

const USAGE = `Usage: node scripts/diagnose-current-mod.mjs --mod PATH [options]

Diagnose every EU4 source file in a Current Mod using the embedded first-party rules and a local
Vanilla index cache. The default output is diagnostic-reports/current-mod-<timestamp>.{json,md}.

Required:
  --mod PATH                 Current Mod directory

Options:
  --vanilla-cache PATH       Vanilla .pdxindex (also PDX_DIAGNOSTIC_VANILLA_CACHE)
  --server PATH              pdx-ls executable (auto-detected from target/{debug,release})
  --workspace PATH           LSP workspace root (default: parent of --mod)
  --project-config PATH      optional .pdx/project.toml
  --output DIR               report directory (default: ${DEFAULT_OUTPUT_DIR})
  --timeout-ms N             overall server timeout (default: ${DEFAULT_TIMEOUT_MS})
  --file-timeout-ms N        timeout for one file (default: ${DEFAULT_FILE_TIMEOUT_MS})
  --max-files N              maximum files to inspect (default: ${DEFAULT_MAX_FILES})
  --fail-on LEVEL             error, warning, or none (default: error)
  --help                     show this help

Environment equivalents: PDX_DIAGNOSTIC_VANILLA_CACHE, PDX_DIAGNOSTIC_SERVER,
PDX_DIAGNOSTIC_WORKSPACE, PDX_DIAGNOSTIC_PROJECT_CONFIG, PDX_DIAGNOSTIC_OUTPUT,
PDX_DIAGNOSTIC_TIMEOUT_MS, PDX_DIAGNOSTIC_FILE_TIMEOUT_MS, PDX_DIAGNOSTIC_MAX_FILES,
PDX_DIAGNOSTIC_FAIL_ON.
`;

class CliUsageError extends Error {
  constructor(message) {
    super(`${message}\n\n${USAGE}`);
    this.name = 'CliUsageError';
  }
}

class ProtocolError extends Error {
  constructor(message) {
    super(message);
    this.name = 'ProtocolError';
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

function parseFailOn(value) {
  const normalized = String(value).toLowerCase();
  if (!['error', 'warning', 'none'].includes(normalized)) {
    throw new CliUsageError(`--fail-on must be error, warning, or none; got ${JSON.stringify(value)}`);
  }
  return normalized;
}

function parseArgs(argv) {
  const options = {
    mod: undefined,
    vanillaCache:
      envValue('PDX_DIAGNOSTIC_VANILLA_CACHE') || envValue('PDX_PERF_CACHE') || undefined,
    server: envValue('PDX_DIAGNOSTIC_SERVER'),
    workspace: envValue('PDX_DIAGNOSTIC_WORKSPACE'),
    projectConfig: envValue('PDX_DIAGNOSTIC_PROJECT_CONFIG'),
    output: envValue('PDX_DIAGNOSTIC_OUTPUT') || DEFAULT_OUTPUT_DIR,
    timeoutMs: parsePositiveInteger(
      envValue('PDX_DIAGNOSTIC_TIMEOUT_MS') || DEFAULT_TIMEOUT_MS,
      'timeout',
    ),
    fileTimeoutMs: parsePositiveInteger(
      envValue('PDX_DIAGNOSTIC_FILE_TIMEOUT_MS') || DEFAULT_FILE_TIMEOUT_MS,
      'file timeout',
    ),
    maxFiles: parsePositiveInteger(
      envValue('PDX_DIAGNOSTIC_MAX_FILES') || DEFAULT_MAX_FILES,
      'max files',
    ),
    failOn: parseFailOn(envValue('PDX_DIAGNOSTIC_FAIL_ON') || 'error'),
  };

  const valueOptions = new Map([
    ['--mod', 'mod'],
    ['--current-mod', 'mod'],
    ['--current-mode', 'mod'],
    ['--vanilla-cache', 'vanillaCache'],
    ['--vanilla', 'vanillaCache'],
    ['--server', 'server'],
    ['--workspace', 'workspace'],
    ['--project-config', 'projectConfig'],
    ['--output', 'output'],
    ['--timeout-ms', 'timeoutMs'],
    ['--file-timeout-ms', 'fileTimeoutMs'],
    ['--max-files', 'maxFiles'],
    ['--fail-on', 'failOn'],
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
    if (['timeoutMs', 'fileTimeoutMs', 'maxFiles'].includes(key)) {
      options[key] = parsePositiveInteger(value, argument);
    } else if (key === 'failOn') {
      options[key] = parseFailOn(value);
    } else {
      options[key] = value;
    }
    index += 1;
  }

  if (!options.mod) {
    throw new CliUsageError('--mod is required');
  }
  return options;
}

function canonicalDirectory(path, flag) {
  const candidate = resolve(path);
  if (!existsSync(candidate)) {
    throw new CliUsageError(`${flag} does not exist: ${candidate}`);
  }
  const real = realpathSync(candidate);
  if (!statSync(real).isDirectory()) {
    throw new CliUsageError(`${flag} is not a directory: ${real}`);
  }
  return real;
}

function canonicalFile(path, flag) {
  const candidate = resolve(path);
  if (!existsSync(candidate)) {
    throw new CliUsageError(`${flag} does not exist: ${candidate}`);
  }
  const real = realpathSync(candidate);
  if (!statSync(real).isFile()) {
    throw new CliUsageError(`${flag} is not a file: ${real}`);
  }
  return real;
}

function tomlQuotedValue(line, key) {
  const match = new RegExp(`^\\s*${key}\\s*=\\s*(["'])(.*?)\\1\\s*(?:#.*)?$`).exec(line);
  return match ? match[2] : undefined;
}

function configuredVanillaCache(projectConfig, workspace) {
  if (!projectConfig || !existsSync(projectConfig)) return undefined;
  const text = readFileSync(projectConfig, 'utf8');
  for (const line of text.split(/\r?\n/)) {
    const snake = tomlQuotedValue(line, 'vanilla_index_cache');
    if (snake) return resolve(workspace, snake);
    const alias = tomlQuotedValue(line, 'vanillaIndexCache');
    if (alias) return resolve(workspace, alias);
  }
  return undefined;
}

function userConfigCandidates() {
  const home = process.env.USERPROFILE || process.env.HOME;
  if (process.platform === 'win32') {
    const localAppData = process.env.LOCALAPPDATA || (home ? join(home, 'AppData', 'Local') : undefined);
    return localAppData ? [join(localAppData, 'ParadoxCode', 'config.toml')] : [];
  }
  if (process.platform === 'darwin') {
    return home ? [join(home, 'Library', 'Application Support', 'ParadoxCode', 'config.toml')] : [];
  }
  const configHome = process.env.XDG_CONFIG_HOME || (home ? join(home, '.config') : undefined);
  return configHome ? [join(configHome, 'paradoxcode', 'config.toml')] : [];
}

function userConfiguredVanillaCache() {
  for (const path of userConfigCandidates()) {
    if (!existsSync(path)) continue;
    const text = readFileSync(path, 'utf8');
    let section = '';
    for (const line of text.split(/\r?\n/)) {
      const sectionMatch = /^\s*\[([^\]]+)\]\s*$/.exec(line);
      if (sectionMatch) {
        section = sectionMatch[1].trim();
        continue;
      }
      if (section === 'games.eu4') {
        const value = tomlQuotedValue(line, 'vanilla_cache');
        if (value) return resolve(dirname(path), value);
      }
    }
  }
  return undefined;
}

function resolveOptions(raw) {
  const mod = canonicalDirectory(raw.mod, '--mod');
  const workspace = canonicalDirectory(raw.workspace || dirname(mod), '--workspace');
  let projectConfig = raw.projectConfig;
  if (!projectConfig) {
    const candidates = [join(workspace, '.pdx', 'project.toml'), join(mod, '.pdx', 'project.toml')];
    projectConfig = candidates.find((candidate) => existsSync(candidate));
  } else {
    projectConfig = canonicalFile(projectConfig, '--project-config');
  }

  let vanillaCache = raw.vanillaCache;
  if (!vanillaCache) vanillaCache = configuredVanillaCache(projectConfig, workspace);
  if (!vanillaCache) vanillaCache = userConfiguredVanillaCache();
  if (!vanillaCache) {
    throw new CliUsageError(
      'a Vanilla cache is required; pass --vanilla-cache PATH or configure vanilla_index_cache in .pdx/project.toml',
    );
  }
  vanillaCache = canonicalFile(vanillaCache, '--vanilla-cache');

  const server = resolveServer(raw.server);
  return { ...raw, mod, workspace, projectConfig, vanillaCache, server };
}

function resolveServer(explicit) {
  const candidates = [];
  if (explicit) candidates.push(resolve(explicit));
  const executableName = process.platform === 'win32' ? 'pdx-ls.exe' : 'pdx-ls';
  candidates.push(
    join(REPOSITORY_ROOT, 'target', 'debug', executableName),
    join(REPOSITORY_ROOT, 'target', 'release', executableName),
  );
  for (const candidate of candidates) {
    if (existsSync(candidate) && statSync(candidate).isFile()) return candidate;
  }
  if (explicit) {
    throw new CliUsageError(`pdx-ls executable was not found: ${explicit}`);
  }
  return 'pdx-ls';
}

function decodeSource(bytes) {
  try {
    return { text: UTF8_DECODER.decode(bytes), encoding: 'utf-8' };
  } catch {
    return { text: WINDOWS_1252_DECODER.decode(bytes), encoding: 'windows-1252' };
  }
}

function collectSourceFiles(root, maxFiles) {
  const files = [];
  const skippedSymlinks = [];
  let omittedSymlinks = 0;
  let depthLimitedDirectories = 0;

  function walk(directory, depth) {
    if (depth > MAX_SCAN_DEPTH) {
      depthLimitedDirectories += 1;
      return;
    }
    const entries = readdirSync(directory, { withFileTypes: true }).sort((left, right) =>
      left.name < right.name ? -1 : left.name > right.name ? 1 : 0,
    );
    for (const entry of entries) {
      const path = join(directory, entry.name);
      if (entry.isSymbolicLink()) {
        if (skippedSymlinks.length < MAX_REPORTED_SYMLINKS) {
          skippedSymlinks.push(relative(root, path).split(sep).join('/'));
        } else {
          omittedSymlinks += 1;
        }
        continue;
      }
      if (entry.isDirectory()) {
        walk(path, depth + 1);
        continue;
      }
      if (!entry.isFile() || !RELEVANT_EXTENSIONS.has(extname(entry.name).toLowerCase())) continue;
      files.push(path);
      if (files.length > maxFiles) {
        throw new CliUsageError(
          `Current Mod contains more than --max-files ${maxFiles} relevant files`,
        );
      }
    }
  }

  walk(root, 0);
  return { files, skippedSymlinks, omittedSymlinks, depthLimitedDirectories };
}

function fileUri(path) {
  return pathToFileURL(path).href;
}

function jsonRpcIdEquals(left, right) {
  return left !== undefined && right !== undefined && JSON.stringify(left) === JSON.stringify(right);
}

class LspClient {
  constructor(child) {
    this.child = child;
    this.buffer = Buffer.alloc(0);
    this.messages = [];
    this.waiters = [];
    this.closed = false;
    this.nextId = 1;
    this.serverMessages = [];
    this.progress = new Map();
    this.serverStderr = '';

    child.stdout.on('data', (chunk) => this.consume(chunk));
    child.stdout.on('end', () => this.failWaiters(new ProtocolError('pdx-ls closed stdout')));
    child.on('error', (error) => this.failWaiters(error));
    child.on('close', (code, signal) => {
      this.closed = true;
      if (code !== 0 || signal) {
        this.failWaiters(new ProtocolError(`pdx-ls exited with code ${code ?? 'unknown'}${signal ? ` (${signal})` : ''}`));
      } else {
        this.failWaiters(new ProtocolError('pdx-ls exited before the expected response'));
      }
    });
    child.stderr.on('data', (chunk) => {
      this.serverStderr += chunk.toString();
      if (this.serverStderr.length > 64 * 1024) {
        this.serverStderr = this.serverStderr.slice(-64 * 1024);
      }
    });
  }

  consume(chunk) {
    this.buffer = Buffer.concat([this.buffer, chunk]);
    const separator = Buffer.from('\r\n\r\n');
    while (true) {
      const headerEnd = this.buffer.indexOf(separator);
      if (headerEnd < 0) return;
      const header = this.buffer.subarray(0, headerEnd).toString('ascii');
      const lengthLine = header
        .split(/\r?\n/)
        .find((line) => line.toLowerCase().startsWith('content-length:'));
      const length = lengthLine ? Number(lengthLine.slice(lengthLine.indexOf(':') + 1).trim()) : NaN;
      if (!Number.isSafeInteger(length) || length < 0) {
        this.failWaiters(new ProtocolError(`invalid LSP Content-Length header: ${header}`));
        return;
      }
      const payloadStart = headerEnd + separator.length;
      if (this.buffer.length < payloadStart + length) return;
      const payload = this.buffer.subarray(payloadStart, payloadStart + length).toString('utf8');
      this.buffer = this.buffer.subarray(payloadStart + length);
      let message;
      try {
        message = JSON.parse(payload);
      } catch (error) {
        this.failWaiters(new ProtocolError(`invalid JSON from pdx-ls: ${error.message}`));
        return;
      }
      this.enqueue(message);
    }
  }

  enqueue(message) {
    const waiter = this.waiters.shift();
    if (waiter) {
      clearTimeout(waiter.timer);
      waiter.resolve(message);
    } else {
      this.messages.push(message);
    }
  }

  failWaiters(error) {
    for (const waiter of this.waiters.splice(0)) {
      clearTimeout(waiter.timer);
      waiter.reject(error);
    }
  }

  async next(timeoutMs) {
    if (this.messages.length) return this.messages.shift();
    if (this.closed) throw new ProtocolError('pdx-ls is no longer running');
    return new Promise((resolveMessage, reject) => {
      const timer = setTimeout(() => {
        const index = this.waiters.findIndex((candidate) => candidate.resolve === resolveMessage);
        if (index >= 0) this.waiters.splice(index, 1);
        reject(new ProtocolError(`timed out after ${timeoutMs} ms waiting for pdx-ls`));
      }, timeoutMs);
      this.waiters.push({ resolve: resolveMessage, reject, timer });
    });
  }

  send(message) {
    if (this.closed || this.child.stdin.destroyed) throw new ProtocolError('cannot write to stopped pdx-ls');
    const payload = Buffer.from(JSON.stringify(message), 'utf8');
    this.child.stdin.write(`Content-Length: ${payload.length}\r\n\r\n`);
    this.child.stdin.write(payload);
  }

  notify(method, params) {
    this.send({ jsonrpc: JSON_RPC_VERSION, method, ...(params === undefined ? {} : { params }) });
  }

  async request(method, params, timeoutMs) {
    const id = this.nextId;
    this.nextId += 1;
    this.send({ jsonrpc: JSON_RPC_VERSION, id, method, params });
    const deadline = Date.now() + timeoutMs;
    while (true) {
      const remaining = deadline - Date.now();
      if (remaining <= 0) throw new ProtocolError(`timed out waiting for ${method}`);
      const message = await this.next(remaining);
      this.handle(message);
      if (jsonRpcIdEquals(message.id, id) && !message.method) {
        if (message.error) throw new ProtocolError(`${method} failed: ${JSON.stringify(message.error)}`);
        return message.result;
      }
    }
  }

  handle(message) {
    if (message.method === '$/progress') {
      const token = message.params?.token;
      const value = message.params?.value;
      if (token !== undefined && value) {
        this.progress.set(String(token), value);
      }
    }
    if (message.method === 'window/showMessage' || message.method === 'window/logMessage') {
      const text = message.params?.message;
      if (text) this.serverMessages.push(String(text));
    }
    if (message.method && message.id !== undefined) {
      // The server sends these requests for dynamic watchers and work-done progress. The client
      // does not need either feature, but must acknowledge the request to keep the transport live.
      if (message.method === 'client/registerCapability' || message.method === 'window/workDoneProgress/create') {
        this.send({ jsonrpc: JSON_RPC_VERSION, id: message.id, result: null });
      } else {
        this.send({
          jsonrpc: JSON_RPC_VERSION,
          id: message.id,
          error: { code: -32601, message: `unsupported client request: ${message.method}` },
        });
      }
    }
  }

  async waitFor(predicate, timeoutMs, description) {
    const deadline = Date.now() + timeoutMs;
    while (true) {
      const remaining = deadline - Date.now();
      if (remaining <= 0) throw new ProtocolError(`timed out waiting for ${description}`);
      const message = await this.next(remaining);
      this.handle(message);
      if (predicate(message)) return message;
    }
  }
}

async function waitForVanillaReady(client, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let started = false;
  while (true) {
    const remaining = deadline - Date.now();
    if (remaining <= 0) throw new ProtocolError('timed out waiting for Vanilla cache loading');
    const message = await client.next(remaining);
    client.handle(message);
    if (message.method !== '$/progress') continue;
    const value = message.params?.value;
    const progressText = `${value?.title || ''} ${value?.message || ''}`.toLowerCase();
    if (value?.kind === 'begin' && progressText.includes('vanilla')) {
      started = true;
    }
    if (started && value?.kind === 'end') return value.message || 'Vanilla cache loading completed';
  }
}

function severityName(value) {
  switch (Number(value)) {
    case 1:
      return 'error';
    case 2:
      return 'warning';
    case 3:
      return 'info';
    case 4:
      return 'hint';
    default:
      return 'unknown';
  }
}

function diagnosticLocation(diagnostic) {
  const start = diagnostic.range?.start || { line: 0, character: 0 };
  const end = diagnostic.range?.end || start;
  return {
    line: Number(start.line) + 1,
    column: Number(start.character) + 1,
    end_line: Number(end.line) + 1,
    end_column: Number(end.character) + 1,
  };
}

function lineExcerpt(text, lineNumber) {
  const lines = text.split(/\r?\n/);
  return lines[lineNumber] ?? '';
}

function firstPartyRuleMetadata() {
  const manifestPath = join(REPOSITORY_ROOT, 'rules', 'manifest.json');
  try {
    const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
    return {
      manifest_path: manifestPath,
      source_format_version: manifest.source_format_version ?? null,
      target_game_version: manifest.target_game_version ?? null,
      manifest_rule_hash: manifest.rule_hash ?? null,
      semantic_rule_count: manifest.semantic_rule_count ?? null,
      file_category_count: manifest.file_category_count ?? null,
      symbol_descriptor_count: manifest.symbol_descriptor_count ?? null,
    };
  } catch (error) {
    return { manifest_path: manifestPath, read_error: error.message };
  }
}

function normalizeDiagnostic(diagnostic, text) {
  const location = diagnosticLocation(diagnostic);
  return {
    code: diagnostic.code === undefined ? 'unknown' : String(diagnostic.code),
    severity: Number(diagnostic.severity || 1),
    severity_name: severityName(diagnostic.severity),
    message: String(diagnostic.message || ''),
    source: diagnostic.source || 'pdx-ls',
    range: diagnostic.range || null,
    location,
    excerpt: lineExcerpt(text, location.line - 1),
  };
}

function baseReport(options, files, skippedSymlinks, omittedSymlinks, depthLimitedDirectories) {
  const cacheStat = statSync(options.vanillaCache);
  return {
    schema_version: 1,
    generated_at: new Date().toISOString(),
    status: 'running',
    inputs: {
      game: 'eu4',
      rules: {
        authority: 'embedded first-party rules/eu4 JSON source',
        external_source: false,
        ...firstPartyRuleMetadata(),
      },
      current_mod: options.mod,
      workspace: options.workspace,
      project_config: options.projectConfig || null,
      vanilla_cache: {
        path: options.vanillaCache,
        size_bytes: cacheStat.size,
        modified_at: cacheStat.mtime.toISOString(),
        loaded: false,
        status_message: null,
      },
      server: options.server,
      fail_on: options.failOn,
    },
    scan: {
      relevant_files_discovered: files.length,
      symlinks_skipped: skippedSymlinks,
      symlinks_skipped_omitted: omittedSymlinks,
      depth_limited_directories: depthLimitedDirectories,
      oversized_files_skipped: [],
      oversized_files_skipped_omitted: 0,
    },
    files: [],
    summary: {
      files_analyzed: 0,
      files_with_diagnostics: 0,
      total_diagnostics: 0,
      errors: 0,
      warnings: 0,
      infos: 0,
      hints: 0,
      by_code: {},
    },
    tool_errors: [],
    tool_errors_omitted: 0,
    server_messages: [],
  };
}

function addToolError(report, message) {
  if (report.tool_errors.length < MAX_REPORTED_TOOL_ERRORS) {
    report.tool_errors.push(String(message));
  } else {
    report.tool_errors_omitted += 1;
  }
}

function addFileResult(report, file, text, encoding, diagnostics, root) {
  const normalized = diagnostics.map((diagnostic) => normalizeDiagnostic(diagnostic, text));
  const relativePath = relative(root, file).split(sep).join('/');
  const result = {
    path: relativePath,
    physical_path: file,
    encoding,
    diagnostics: normalized,
  };
  report.files.push(result);
  report.summary.files_analyzed += 1;
  if (normalized.length) report.summary.files_with_diagnostics += 1;
  for (const diagnostic of normalized) {
    report.summary.total_diagnostics += 1;
    const severity = diagnostic.severity_name;
    if (severity === 'error') report.summary.errors += 1;
    else if (severity === 'warning') report.summary.warnings += 1;
    else if (severity === 'info') report.summary.infos += 1;
    else if (severity === 'hint') report.summary.hints += 1;
    const code = diagnostic.code;
    const byCode = report.summary.by_code[code] || { count: 0, errors: 0, warnings: 0, infos: 0, hints: 0 };
    byCode.count += 1;
    if (severity === 'error') byCode.errors += 1;
    else if (severity === 'warning') byCode.warnings += 1;
    else if (severity === 'info') byCode.infos += 1;
    else if (severity === 'hint') byCode.hints += 1;
    report.summary.by_code[code] = byCode;
  }
}

function shouldFail(report, failOn) {
  if (failOn === 'none') return false;
  if (failOn === 'warning') return report.summary.errors + report.summary.warnings > 0;
  return report.summary.errors > 0;
}

function markdownEscape(value) {
  return String(value).replaceAll('|', '\\|').replaceAll('`', '\\`');
}

function renderMarkdown(report) {
  const { summary } = report;
  const lines = [
    '# Current Mod diagnostic report',
    '',
    `- Status: **${report.status}**`,
    `- Generated: ${report.generated_at}`,
    `- Current Mod: \`${markdownEscape(report.inputs.current_mod)}\``,
    `- Vanilla cache: \`${markdownEscape(report.inputs.vanilla_cache.path)}\``,
    `- Rules: ${report.inputs.rules.authority}`,
    `- Failure threshold: \`${report.inputs.fail_on}\``,
    '',
    '## Summary',
    '',
    '| Metric | Count |',
    '| --- | ---: |',
    `| Relevant files discovered | ${report.scan.relevant_files_discovered} |`,
    `| Files analyzed | ${summary.files_analyzed} |`,
    `| Files with diagnostics | ${summary.files_with_diagnostics} |`,
    `| Total diagnostics | ${summary.total_diagnostics} |`,
    `| Errors | ${summary.errors} |`,
    `| Warnings | ${summary.warnings} |`,
    `| Info / hints | ${summary.infos + summary.hints} |`,
    '',
    '### Diagnostic codes',
    '',
    '| Code | Count | Errors | Warnings | Info | Hints |',
    '| --- | ---: | ---: | ---: | ---: | ---: |',
  ];
  if (report.inputs.rules.target_game_version) {
    lines.splice(7, 0, `- Rule target game version: ${markdownEscape(report.inputs.rules.target_game_version)}`);
  }
  if (report.inputs.rules.manifest_rule_hash) {
    lines.splice(8, 0, `- Rule manifest hash: \`${markdownEscape(report.inputs.rules.manifest_rule_hash)}\``);
  }
  const codes = Object.keys(summary.by_code).sort();
  if (!codes.length) lines.push('| _(none)_ | 0 | 0 | 0 | 0 | 0 |');
  for (const code of codes) {
    const value = summary.by_code[code];
    lines.push(`| ${markdownEscape(code)} | ${value.count} | ${value.errors} | ${value.warnings} | ${value.infos} | ${value.hints} |`);
  }

  lines.push('', '## Vanilla and scan status', '');
  const vanilla = report.inputs.vanilla_cache;
  lines.push(`- Cache loaded: **${vanilla.loaded ? 'yes' : 'no'}**`);
  if (vanilla.status_message) lines.push(`- Server status: ${markdownEscape(vanilla.status_message)}`);
  if (report.scan.symlinks_skipped.length || report.scan.symlinks_skipped_omitted) {
    const symlinkCount = report.scan.symlinks_skipped.length + report.scan.symlinks_skipped_omitted;
    lines.push(`- Symlinks skipped: ${symlinkCount}`);
    for (const path of report.scan.symlinks_skipped.slice(0, 20)) lines.push(`  - \`${markdownEscape(path)}\``);
    if (report.scan.symlinks_skipped.length > 20) lines.push(`  - … ${report.scan.symlinks_skipped.length - 20} more`);
    if (report.scan.symlinks_skipped_omitted) lines.push(`  - … ${report.scan.symlinks_skipped_omitted} more omitted by the report bound`);
  }
  if (report.scan.oversized_files_skipped.length || report.scan.oversized_files_skipped_omitted) {
    const oversizedCount =
      report.scan.oversized_files_skipped.length + report.scan.oversized_files_skipped_omitted;
    lines.push(`- Oversized files skipped: ${oversizedCount}`);
    for (const path of report.scan.oversized_files_skipped.slice(0, 20)) {
      lines.push(`  - \`${markdownEscape(path)}\``);
    }
    if (report.scan.oversized_files_skipped_omitted) {
      lines.push(`  - … ${report.scan.oversized_files_skipped_omitted} more omitted by the report bound`);
    }
  }
  if (report.scan.depth_limited_directories) {
    lines.push(`- Directories beyond the ${MAX_SCAN_DEPTH}-level scan bound skipped: ${report.scan.depth_limited_directories}`);
  }
  if (report.tool_errors.length) {
    lines.push('', '## Tool errors', '');
    for (const error of report.tool_errors) lines.push(`- ${markdownEscape(error)}`);
    if (report.tool_errors_omitted) lines.push(`- … ${report.tool_errors_omitted} more omitted by the report bound`);
  }
  if (report.server_messages.length) {
    lines.push('', '## Server messages', '');
    for (const message of report.server_messages) lines.push(`- ${markdownEscape(message)}`);
  }
  if (report.server_stderr) {
    lines.push('', '## Server stderr', '', '```text', report.server_stderr, '```');
  }

  lines.push('', '## Files with diagnostics', '');
  const affected = report.files.filter((file) => file.diagnostics.length);
  if (!affected.length) {
    lines.push('No diagnostics were reported.');
  } else {
    for (const file of affected) {
      lines.push('', `### \`${markdownEscape(file.path)}\` (${file.diagnostics.length})`, '');
      for (const diagnostic of file.diagnostics) {
        const location = diagnostic.location;
        lines.push(
          `- **${diagnostic.severity_name.toUpperCase()}** \`${markdownEscape(diagnostic.code)}\` at ${location.line}:${location.column} — ${markdownEscape(diagnostic.message)}`,
        );
        if (diagnostic.excerpt.trim()) {
          lines.push('', '  ```text', `  ${diagnostic.excerpt.replaceAll('`', '\\`')}`, '  ```');
        }
      }
    }
  }
  const clean = report.files.length - affected.length;
  if (clean > 0) lines.push('', `_${clean} analyzed file(s) had no diagnostics._`);
  return `${lines.join('\n')}\n`;
}

function writeReports(report, outputDir) {
  mkdirSync(outputDir, { recursive: true });
  const stamp = report.generated_at.replace(/[-:]/g, '').replace(/\.\d{3}Z$/, 'Z');
  const base = join(outputDir, `current-mod-${stamp}`);
  const jsonPath = `${base}.json`;
  const markdownPath = `${base}.md`;
  writeFileSync(jsonPath, `${JSON.stringify(report, null, 2)}\n`, 'utf8');
  writeFileSync(markdownPath, renderMarkdown(report), 'utf8');
  return { jsonPath, markdownPath };
}

function collectServerMessages(client, report) {
  for (const message of client?.serverMessages || []) report.server_messages.push(message);
  report.server_messages = [...new Set(report.server_messages)];
}

async function stopClient(client, timeoutMs) {
  if (!client || client.closed) return;
  try {
    await client.request('shutdown', null, Math.min(timeoutMs, 10_000));
    client.notify('exit');
  } catch {
    client.child.kill();
  }
}

async function run(rawOptions) {
  const options = resolveOptions(rawOptions);
  const collected = collectSourceFiles(options.mod, options.maxFiles);
  const report = baseReport(
    options,
    collected.files,
    collected.skippedSymlinks,
    collected.omittedSymlinks,
    collected.depthLimitedDirectories,
  );
  let client;
  try {
    const child = spawn(options.server, [], {
      cwd: REPOSITORY_ROOT,
      stdio: ['pipe', 'pipe', 'pipe'],
      windowsHide: true,
    });
    client = new LspClient(child);
    const initializeParams = {
      processId: process.pid,
      clientInfo: { name: 'paradoxcode-current-mod-diagnostics', version: '1' },
      rootUri: fileUri(options.workspace),
      workspaceFolders: [{ uri: fileUri(options.workspace), name: 'diagnostic-workspace' }],
      capabilities: {
        window: { workDoneProgress: true },
        workspace: { didChangeWatchedFiles: { dynamicRegistration: false } },
        textDocument: { completion: { completionItem: { snippetSupport: false } } },
      },
      initializationOptions: {
        ...(options.projectConfig ? { projectConfig: options.projectConfig } : {}),
        modDirectory: options.mod,
        vanillaIndexCache: options.vanillaCache,
      },
      trace: 'off',
    };
    await client.request('initialize', initializeParams, options.timeoutMs);
    client.notify('initialized', {});
    const vanillaMessage = await waitForVanillaReady(client, options.timeoutMs);
    const vanillaFailed = /could not|failed|without vanilla|error/i.test(vanillaMessage);
    report.inputs.vanilla_cache.loaded = !vanillaFailed;
    report.inputs.vanilla_cache.status_message = vanillaMessage;
    if (vanillaFailed) {
      addToolError(report, `Vanilla cache was not enabled: ${vanillaMessage}`);
    }

    for (const file of collected.files) {
      let uri;
      let opened = false;
      try {
        const bytes = readFileSync(file);
        if (bytes.length > MAX_SOURCE_BYTES) {
          const relativePath = relative(options.mod, file).split(sep).join('/');
          if (report.scan.oversized_files_skipped.length < MAX_REPORTED_TOOL_ERRORS) {
            report.scan.oversized_files_skipped.push(relativePath);
          } else {
            report.scan.oversized_files_skipped_omitted += 1;
          }
          addToolError(
            report,
            `${relativePath} exceeds the ${MAX_SOURCE_BYTES} byte source-file limit`,
          );
          continue;
        }
        const decoded = decodeSource(bytes);
        uri = fileUri(file);
        client.notify('textDocument/didOpen', {
          textDocument: { uri, languageId: 'eu4', version: 1, text: decoded.text },
        });
        opened = true;
        const diagnosticMessage = await client.waitFor(
          (message) =>
            message.method === 'textDocument/publishDiagnostics' && message.params?.uri === uri,
          options.fileTimeoutMs,
          `diagnostics for ${relative(options.mod, file)}`,
        );
        addFileResult(
          report,
          file,
          decoded.text,
          decoded.encoding,
          diagnosticMessage.params?.diagnostics || [],
          options.mod,
        );
      } catch (error) {
        addToolError(report, `${relative(options.mod, file)}: ${error.message}`);
      } finally {
        if (opened) client.notify('textDocument/didClose', { textDocument: { uri } });
      }
    }
  } catch (error) {
    addToolError(report, error instanceof Error ? error.message : String(error));
  } finally {
    collectServerMessages(client, report);
    await stopClient(client, options.timeoutMs);
    if (client?.serverStderr?.trim()) {
      report.server_stderr = client.serverStderr.trim();
    }
  }

  report.status = report.tool_errors.length
    ? 'incomplete'
    : shouldFail(report, options.failOn)
      ? 'failed'
      : 'passed';
  const output = writeReports(report, resolve(options.output));
  console.log(`Current Mod diagnostics: ${report.status}`);
  console.log(`Files analyzed: ${report.summary.files_analyzed}/${report.scan.relevant_files_discovered}`);
  console.log(`Diagnostics: ${report.summary.total_diagnostics} (errors ${report.summary.errors}, warnings ${report.summary.warnings})`);
  console.log(`JSON report: ${output.jsonPath}`);
  console.log(`Markdown report: ${output.markdownPath}`);
  if (report.tool_errors.length) {
    for (const error of report.tool_errors) console.error(`diagnostic tool: ${error}`);
  }
  return report.status === 'passed' ? 0 : 1;
}

async function main() {
  let rawOptions;
  try {
    rawOptions = parseArgs(process.argv.slice(2));
  } catch (error) {
    if (error instanceof CliUsageError) {
      console.error(error.message);
      return 2;
    }
    throw error;
  }
  if (rawOptions.help) {
    console.log(USAGE);
    return 0;
  }
  try {
    return await run(rawOptions);
  } catch (error) {
    console.error(`diagnose-current-mod: ${error instanceof Error ? error.message : String(error)}`);
    return error instanceof CliUsageError ? 2 : 1;
  }
}

process.exitCode = await main();
