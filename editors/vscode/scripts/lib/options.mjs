/**
 * CLI contract for the Current Mod diagnostic tool: argument parsing, path
 * validation, user-configuration discovery, and the effective-options
 * resolution that turns raw flags into the session inputs every other module
 * consumes. Owns the usage text so `--help` and `CliUsageError` render the
 * same contract.
 */

import { existsSync, readFileSync, realpathSync, statSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createOverlayWorkspace } from './overlay.mjs';

// lib/ sits four levels below the repository root (lib → scripts → vscode → editors).
export const REPOSITORY_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..', '..', '..');

export const DEFAULT_OUTPUT_DIR = join(REPOSITORY_ROOT, 'diagnostic-reports');
export const DEFAULT_TIMEOUT_MS = 120_000;
export const DEFAULT_FILE_TIMEOUT_MS = 60_000;
export const DEFAULT_MAX_FILES = 100_000;
export const DEFAULT_WORKSPACE_DIAGNOSTIC_BATCH_SIZE = 16;
export const CHECKPOINT_FILE_INTERVAL = 128;

export const USAGE = `Usage: node editors/vscode/scripts/diagnose.mjs (--mod PATH | --vanilla-source PATH) [options]

Diagnose every EU4 source file in a Current Mod using the embedded first-party rules and a local
Vanilla index cache. The default output is diagnostic-reports/current-mod-<timestamp>.{json,md}.

Required:
  --mod PATH                 Current Mod directory
  --vanilla-source PATH      Vanilla source tree; diagnose through virtual overlays backed by its cache

Options:
  --vanilla-cache PATH       Vanilla .pdcindex (also PDC_DIAGNOSTIC_VANILLA_CACHE)
  --server PATH              paradoxcode executable (explicit path must exist; auto-detected from target/{debug,release} only when omitted)
  --workspace PATH           LSP workspace root (default: parent of --mod)
  --output DIR               report directory (default: ${DEFAULT_OUTPUT_DIR})
  --timeout-ms N             overall server timeout (default: ${DEFAULT_TIMEOUT_MS})
  --file-timeout-ms N        timeout for one file (default: ${DEFAULT_FILE_TIMEOUT_MS})
  --max-files N              maximum files to inspect (default: ${DEFAULT_MAX_FILES})
  --path-prefix PATH         inspect only files below this source-relative path
  --shard-count N            split selected files into N stable round-robin shards (default: 1)
  --shard-index N            zero-based shard to inspect (default: 0)
  --batch-size N             indexed files per diagnostic request (default: ${DEFAULT_WORKSPACE_DIAGNOSTIC_BATCH_SIZE})
  --concurrency N            simultaneous virtual/explicit overlays (default: 8)
  --checkpoint-every N       completed files between partial reports (default: ${CHECKPOINT_FILE_INTERVAL})
  --fail-on LEVEL             error, warning, or none (default: error)
  --help                     show this help

Environment equivalents: PDC_DIAGNOSTIC_VANILLA_CACHE, PDC_DIAGNOSTIC_SERVER,
PDC_DIAGNOSTIC_WORKSPACE, PDC_DIAGNOSTIC_OUTPUT,
PDC_DIAGNOSTIC_TIMEOUT_MS, PDC_DIAGNOSTIC_FILE_TIMEOUT_MS, PDC_DIAGNOSTIC_MAX_FILES,
PDC_DIAGNOSTIC_PATH_PREFIX, PDC_DIAGNOSTIC_SHARD_COUNT, PDC_DIAGNOSTIC_SHARD_INDEX,
PDC_DIAGNOSTIC_BATCH_SIZE, PDC_DIAGNOSTIC_CONCURRENCY, PDC_DIAGNOSTIC_CHECKPOINT_EVERY,
PDC_DIAGNOSTIC_FAIL_ON.
`;

export class CliUsageError extends Error {
  constructor(message) {
    super(`${message}\n\n${USAGE}`);
    this.name = 'CliUsageError';
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

function parseNonnegativeInteger(value, label) {
  if (!/^[0-9]+$/.test(String(value))) {
    throw new CliUsageError(`${label} must be a nonnegative integer, got ${JSON.stringify(value)}`);
  }
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed)) {
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

export function parseArgs(argv) {
  const options = {
    mod: undefined,
    vanillaSource: undefined,
    vanillaCache:
      envValue('PDC_DIAGNOSTIC_VANILLA_CACHE') || envValue('PDC_PERF_CACHE') || undefined,
    server: envValue('PDC_DIAGNOSTIC_SERVER'),
    workspace: envValue('PDC_DIAGNOSTIC_WORKSPACE'),
    output: envValue('PDC_DIAGNOSTIC_OUTPUT') || DEFAULT_OUTPUT_DIR,
    timeoutMs: parsePositiveInteger(
      envValue('PDC_DIAGNOSTIC_TIMEOUT_MS') || DEFAULT_TIMEOUT_MS,
      'timeout',
    ),
    fileTimeoutMs: parsePositiveInteger(
      envValue('PDC_DIAGNOSTIC_FILE_TIMEOUT_MS') || DEFAULT_FILE_TIMEOUT_MS,
      'file timeout',
    ),
    maxFiles: parsePositiveInteger(
      envValue('PDC_DIAGNOSTIC_MAX_FILES') || DEFAULT_MAX_FILES,
      'max files',
    ),
    pathPrefix: envValue('PDC_DIAGNOSTIC_PATH_PREFIX'),
    shardCount: parsePositiveInteger(envValue('PDC_DIAGNOSTIC_SHARD_COUNT') || 1, 'shard count'),
    shardIndex: parseNonnegativeInteger(envValue('PDC_DIAGNOSTIC_SHARD_INDEX') || 0, 'shard index'),
    batchSize: parsePositiveInteger(
      envValue('PDC_DIAGNOSTIC_BATCH_SIZE') || DEFAULT_WORKSPACE_DIAGNOSTIC_BATCH_SIZE,
      'batch size',
    ),
    concurrency: parsePositiveInteger(
      envValue('PDC_DIAGNOSTIC_CONCURRENCY') || 8,
      'concurrency',
    ),
    checkpointEvery: parsePositiveInteger(
      envValue('PDC_DIAGNOSTIC_CHECKPOINT_EVERY') || CHECKPOINT_FILE_INTERVAL,
      'checkpoint interval',
    ),
    failOn: parseFailOn(envValue('PDC_DIAGNOSTIC_FAIL_ON') || 'error'),
  };

  const valueOptions = new Map([
    ['--mod', 'mod'],
    ['--current-mod', 'mod'],
    ['--current-mode', 'mod'],
    ['--vanilla-source', 'vanillaSource'],
    ['--vanilla-cache', 'vanillaCache'],
    ['--vanilla', 'vanillaCache'],
    ['--server', 'server'],
    ['--workspace', 'workspace'],
    ['--output', 'output'],
    ['--timeout-ms', 'timeoutMs'],
    ['--file-timeout-ms', 'fileTimeoutMs'],
    ['--max-files', 'maxFiles'],
    ['--path-prefix', 'pathPrefix'],
    ['--shard-count', 'shardCount'],
    ['--shard-index', 'shardIndex'],
    ['--batch-size', 'batchSize'],
    ['--concurrency', 'concurrency'],
    ['--checkpoint-every', 'checkpointEvery'],
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
    if (
      ['timeoutMs', 'fileTimeoutMs', 'maxFiles', 'batchSize', 'concurrency', 'checkpointEvery', 'shardCount'].includes(
        key,
      )
    ) {
      options[key] = parsePositiveInteger(value, argument);
    } else if (key === 'shardIndex') {
      options[key] = parseNonnegativeInteger(value, argument);
    } else if (key === 'failOn') {
      options[key] = parseFailOn(value);
    } else {
      options[key] = value;
    }
    index += 1;
  }

  if (Boolean(options.mod) === Boolean(options.vanillaSource)) {
    throw new CliUsageError('exactly one of --mod or --vanilla-source is required');
  }
  if (options.shardIndex >= options.shardCount) {
    throw new CliUsageError('--shard-index must be smaller than --shard-count');
  }
  return options;
}

export function canonicalDirectory(path, flag) {
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

export function canonicalFile(path, flag) {
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

function userConfigCandidates() {
  const home = process.env.USERPROFILE || process.env.HOME;
  if (process.platform === 'win32') {
    // The server resolves its config from %APPDATA% (Roaming); LOCALAPPDATA
    // holds only the cache root.
    const roaming = process.env.APPDATA || (home ? join(home, 'AppData', 'Roaming') : undefined);
    return roaming ? [join(roaming, 'ParadoxCode', 'config.toml')] : [];
  }
  if (process.platform === 'darwin') {
    return home ? [join(home, 'Library', 'Application Support', 'ParadoxCode', 'config.toml')] : [];
  }
  const configHome = process.env.XDG_CONFIG_HOME || (home ? join(home, '.config') : undefined);
  return configHome ? [join(configHome, 'paradoxcode', 'config.toml')] : [];
}

export function userConfiguredVanillaCache() {
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

export function resolveOptions(raw) {
  const vanillaSource = raw.vanillaSource
    ? canonicalDirectory(raw.vanillaSource, '--vanilla-source')
    : undefined;
  const source = vanillaSource || canonicalDirectory(raw.mod, '--mod');
  let virtualOverlayRoot;
  let mod = source;
  if (vanillaSource) {
    const overlay = createOverlayWorkspace(raw.output);
    virtualOverlayRoot = overlay.root;
    mod = overlay.mod;
  }
  const workspace = canonicalDirectory(raw.workspace || (vanillaSource ? mod : dirname(mod)), '--workspace');
  let vanillaCache = raw.vanillaCache;
  if (!vanillaCache) vanillaCache = userConfiguredVanillaCache();
  if (!vanillaCache) {
    throw new CliUsageError(
      'a Vanilla cache is required; pass --vanilla-cache PATH or launch `pdc` once against the game so it is discovered and built automatically',
    );
  }
  // The cache file may legitimately not exist yet: the server rebuilds a
  // missing or unloadable cache in place from the discovered installation,
  // and the release sweep's cold protocol deletes the file before every run.
  // Only an existing non-file is a usage error.
  const cacheCandidate = resolve(vanillaCache);
  if (existsSync(cacheCandidate)) {
    vanillaCache = canonicalFile(cacheCandidate, '--vanilla-cache');
  } else {
    vanillaCache = cacheCandidate;
  }

  const server = resolveServer(raw.server);
  const pathPrefix = raw.pathPrefix
    ? raw.pathPrefix.replaceAll('\\', '/').replace(/^\/+|\/+$/g, '')
    : undefined;
  if (pathPrefix && pathPrefix.split('/').some((component) => component === '..')) {
    throw new CliUsageError('--path-prefix must remain within the diagnosed source tree');
  }
  return {
    ...raw,
    mod,
    source,
    vanillaSource,
    virtualOverlayRoot,
    workspace,
    vanillaCache,
    server,
    pathPrefix,
  };
}

function resolveServer(explicit) {
  // An explicit --server path pins the binary under test; a typo'd or
  // unresolvable path must fail loudly instead of silently measuring a
  // stale target/{debug,release} build (cost a full evening of phantom
  // baseline diffs on 2026-09-10).
  if (explicit) {
    const candidate = resolve(explicit);
    if (!existsSync(candidate) || !statSync(candidate).isFile()) {
      throw new CliUsageError(
        `--server executable was not found: ${candidate} ` +
          '(pass an existing path, or omit --server to auto-detect target/{debug,release})',
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
