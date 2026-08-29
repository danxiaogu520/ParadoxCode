#!/usr/bin/env node

/**
 * Head-to-head performance harness for pdx-ls against a real mod corpus.
 *
 * Unlike `lsp-e2e.mjs` (a synthetic single-file smoke), this drives the full
 * lifecycle on a real workspace: initialize (which today performs the whole
 * scan), `pdx/ready`, an idle window, then a sampled set of files measured
 * for open->diagnostics, hover, completion, and edit->diagnostics latency.
 * Child CPU time (user+kernel) and working set are sampled throughout so
 * results are comparable with the CWTools performance CLI's
 * wall-time + allocated-bytes report.
 *
 * Usage:
 *   node scripts/performance/head-to-head.mjs --workspace <mod> --cache <vanilla.pdxindex> \
 *       --label baseline --out performance-results/baseline.json
 *   node scripts/performance/head-to-head.mjs --compare performance-results/baseline.json \
 *       performance-results/candidate.json
 */

import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { basename, dirname, isAbsolute, join, relative, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

import { LspClient } from '../lib/lsp-client.mjs';
import { ProcessSampler, formatBytes } from '../lib/process-stats.mjs';

const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url));
const REPOSITORY_ROOT = resolve(SCRIPT_DIR, '..', '..');
const DEFAULT_TIMEOUT_MS = 300_000;
const REQUEST_TIMEOUT_MS = 120_000;
const DEFAULT_SAMPLE_INTERVAL_MS = 250;
const DEFAULT_SAMPLES = 6;
const DEFAULT_IDLE_WINDOW_MS = 20_000;
const DEFAULT_CLOSE_SETTLE_MS = 2_500;
const SCRIPT_DIRECTORIES = ['common', 'events', 'missions', 'decisions'];

const USAGE = `Usage: node scripts/performance/head-to-head.mjs [options]

Options:
  --server PATH              pdx-ls executable (default target/release/pdx-ls[.exe])
  --workspace DIR            mod/workspace root to measure (required unless --compare)
  --cache FILE               Vanilla .pdxindex cache (required unless --no-cache)
  --no-cache                 run without a Vanilla cache (explicit; numbers not comparable)
  --samples N                files to measure interactively (default ${DEFAULT_SAMPLES})
  --idle-window-ms N         post-ready idle observation window (default ${DEFAULT_IDLE_WINDOW_MS})
  --close-settle-ms N        wait after each didClose for background work (default ${DEFAULT_CLOSE_SETTLE_MS})
  --sample-interval-ms N     process sampling interval (default ${DEFAULT_SAMPLE_INTERVAL_MS})
  --timeout-ms N             initialize timeout (default ${DEFAULT_TIMEOUT_MS})
  --no-workspace-diagnostics disable workspaceWideDiagnostics (interactive-latency mode)
  --label LABEL              label stored in the result JSON (default: git describe)
  --out FILE                 write the result JSON here (default performance-results/<label>-<ts>.json)
  --compare A.json B.json    print a delta table between two results and exit
  --help                     show this help

Environment equivalents: PDX_PERF_SERVER, PDX_PERF_CACHE, PDX_PERF_SAMPLES.
`;

function parseArguments(argv) {
  const options = {
    server: process.env.PDX_PERF_SERVER,
    cache: process.env.PDX_PERF_CACHE,
    samples: process.env.PDX_PERF_SAMPLES ? Number(process.env.PDX_PERF_SAMPLES) : DEFAULT_SAMPLES,
    idleWindowMs: DEFAULT_IDLE_WINDOW_MS,
    closeSettleMs: DEFAULT_CLOSE_SETTLE_MS,
    sampleIntervalMs: DEFAULT_SAMPLE_INTERVAL_MS,
    timeoutMs: DEFAULT_TIMEOUT_MS,
    workspaceDiagnostics: true,
    help: false,
    compare: [],
    noCache: false,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    const next = () => {
      if (index + 1 >= argv.length) throw new Error(`${argument} requires a value`);
      return argv[(index += 1)];
    };
    switch (argument) {
      case '--server': options.server = next(); break;
      case '--workspace': options.workspace = next(); break;
      case '--cache': options.cache = next(); break;
      case '--no-cache': options.noCache = true; break;
      case '--samples': options.samples = Number(next()); break;
      case '--idle-window-ms': options.idleWindowMs = Number(next()); break;
      case '--close-settle-ms': options.closeSettleMs = Number(next()); break;
      case '--sample-interval-ms': options.sampleIntervalMs = Number(next()); break;
      case '--timeout-ms': options.timeoutMs = Number(next()); break;
      case '--no-workspace-diagnostics': options.workspaceDiagnostics = false; break;
      case '--label': options.label = next(); break;
      case '--out': options.out = next(); break;
      case '--compare': options.compare.push(next()); break;
      case '--help': options.help = true; break;
      default: throw new Error(`unknown option ${argument}\n\n${USAGE}`);
    }
  }
  if (options.compare.length === 1) throw new Error('--compare needs two result files');
  return options;
}

function inputPath(value, base = process.cwd()) {
  return isAbsolute(value) ? value : resolve(base, value);
}

function requireFile(path, label) {
  if (!path || !existsSync(path) || !statSync(path).isFile()) {
    throw new Error(`${label} not found: ${path}`);
  }
}

function requireDirectory(path, label) {
  if (!path || !existsSync(path) || !statSync(path).isDirectory()) {
    throw new Error(`${label} not found: ${path}`);
  }
}

/** Collects script files (sorted, deterministic) from the script-heavy roots. */
function collectScriptFiles(root) {
  const all = [];
  const walk = (directory, depth) => {
    if (depth > 12) return;
    let entries;
    try {
      entries = readdirSync(directory, { withFileTypes: true });
    } catch {
      return;
    }
    for (const entry of entries) {
      if (entry.isDirectory()) {
        walk(join(directory, entry.name), depth + 1);
        continue;
      }
      if (!entry.isFile() || !entry.name.endsWith('.txt')) continue;
      const full = join(directory, entry.name);
      let stats;
      try {
        stats = statSync(full);
      } catch {
        continue;
      }
      if (stats.size > 0) all.push(full);
    }
  };
  const roots = SCRIPT_DIRECTORIES.map((name) => join(root, name)).filter((path) => existsSync(path));
  if (roots.length === 0) roots.push(root);
  for (const directory of roots) walk(directory, 0);
  all.sort();
  return all;
}

/** Evenly spreads `count` picks over a sorted file list. */
function sampleFiles(files, count) {
  if (files.length === 0) throw new Error('no .txt script files found under the workspace');
  if (files.length <= count) return files;
  const picked = [];
  const step = (files.length - 1) / (count - 1);
  for (let index = 0; index < count; index += 1) {
    picked.push(files[Math.round(index * step)]);
  }
  return [...new Set(picked)];
}

/** Picks stable hover/completion positions from the first `=` line. */
function queryPositions(text) {
  const lines = text.split(/\r?\n/);
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index];
    const column = line.indexOf('=');
    if (column >= 0) {
      // Hover sits on the key's `=`; completion sits where a value is being
      // typed: right after `= ` when the separator exists, else line end.
      const afterEquals = column + 1 < line.length && line[column + 1] === ' ';
      return {
        hover: { line: index, character: column },
        completion: {
          line: index,
          character: afterEquals ? column + 2 : line.length,
        },
      };
    }
  }
  return {
    hover: { line: 0, character: 0 },
    completion: { line: 0, character: 0 },
  };
}

function percentile(values, fraction) {
  if (values.length === 0) return undefined;
  const sorted = [...values].sort((left, right) => left - right);
  const index = Math.min(sorted.length - 1, Math.ceil(fraction * sorted.length) - 1);
  return sorted[Math.max(0, index)];
}

function aggregate(values) {
  if (values.length === 0) return undefined;
  const sorted = [...values].sort((left, right) => left - right);
  return {
    count: values.length,
    p50: percentile(values, 0.5),
    p99: percentile(values, 0.99),
    max: sorted[sorted.length - 1],
  };
}

function median(values) {
  return percentile(values, 0.5);
}

async function timedRequest(client, method, params, timeoutMs) {
  const started = performance.now();
  const result = await client.request(method, params, timeoutMs);
  return { elapsed: performance.now() - started, result };
}

async function waitForDiagnostic(client, uri, timeoutMs) {
  const cutoff = performance.now();
  const message = await client.waitFor(
    (candidate) =>
      candidate.method === 'textDocument/publishDiagnostics' &&
      candidate.params?.uri === uri &&
      (candidate._at ?? performance.now()) >= cutoff,
    timeoutMs,
    `diagnostics for ${uri}`,
  );
  return message._at ?? performance.now();
}

async function waitForReady(client, timeoutMs) {
  const started = performance.now();
  await client.waitFor(
    (candidate) => candidate.method === 'pdx/ready',
    timeoutMs,
    'pdx/ready',
  );
  return performance.now() - started;
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function currentLabel() {
  try {
    const { execFileSync } = await import('node:child_process');
    const describe = execFileSync('git', ['describe', '--tags', '--always', '--dirty'], {
      cwd: REPOSITORY_ROOT,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
    });
    return describe.trim();
  } catch {
    return 'unlabeled';
  }
}

async function runMeasurement(options) {
  requireDirectory(options.workspace, 'Workspace');
  const serverPath = inputPath(
    options.server ??
      join('target', 'release', process.platform === 'win32' ? 'pdx-ls.exe' : 'pdx-ls'),
    REPOSITORY_ROOT,
  );
  requireFile(serverPath, 'pdx-ls executable');

  let cache;
  if (options.noCache) {
    cache = undefined;
  } else {
    cache = inputPath(options.cache, REPOSITORY_ROOT);
    requireFile(cache, 'Vanilla cache');
  }

  const label = options.label ?? (await currentLabel());
  const scriptFiles = collectScriptFiles(options.workspace);
  const files = sampleFiles(scriptFiles, options.samples);
  console.log(`workspace: ${options.workspace}`);
  console.log(`cache: ${cache ? basename(cache) : '(disabled)'}`);
  console.log(`server: ${serverPath}`);
  console.log(`files: ${files.length} sampled of ${scriptFiles.length}`);
  console.log(`label: ${label}`);

  const initializationOptions = {
    workspaceWideDiagnostics: options.workspaceDiagnostics,
  };
  if (cache) initializationOptions.vanillaIndexCache = cache;
  else initializationOptions.vanillaIndexCache = join(process.env.TMPDIR ?? '/tmp', `pdx-h2h-no-cache-${process.pid}.pdxindex`);

  const measurement = {
    label,
    timestamp: new Date().toISOString(),
    server: basename(serverPath),
    workspace: options.workspace,
    cache: cache ? basename(cache) : null,
    workspaceDiagnostics: options.workspaceDiagnostics,
    samples: files.length,
    startup: {},
    idle: {},
    perFile: [],
    aggregates: {},
  };

  const started = performance.now();
  const server = spawn(serverPath, [], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
  const client = new LspClient(server, { timeoutMs: options.timeoutMs });
  const sampler = new ProcessSampler(server.pid, options.sampleIntervalMs);
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    if (server.pid) resolve();
    else server.once('spawn', resolve);
  });
  sampler.start();

  try {
    const init = await timedRequest(
      client,
      'initialize',
      {
        processId: null,
        workspaceFolders: [{ uri: pathToFileURL(options.workspace).href, name: 'pdx-h2h' }],
        capabilities: {},
        initializationOptions,
      },
      options.timeoutMs,
    );
    const initCpu = sampler.cpuSeconds;
    measurement.startup = {
      initializeMs: init.elapsed,
      initializeCpuSeconds: initCpu,
    };
    console.log(`initialize: ${init.elapsed.toFixed(0)} ms (cpu ${initCpu?.toFixed(1) ?? 'n/a'} s)`);
    client.notify('initialized', {});

    const readyMs = await waitForReady(client, options.timeoutMs);
    const readyCpu = sampler.cpuSeconds;
    measurement.startup.readyMs = readyMs;
    measurement.startup.readyCpuSeconds = readyCpu;
    measurement.startup.readyTotalCpuSeconds = readyCpu;
    console.log(`pdx/ready: +${readyMs.toFixed(0)} ms (cpu ${readyCpu?.toFixed(1) ?? 'n/a'} s total)`);

    const idleStartCpu = sampler.cpuSeconds;
    await sleep(options.idleWindowMs);
    const idleEndCpu = sampler.cpuSeconds;
    const idleCpuDelta = sampler.cpuDelta(idleStartCpu, idleEndCpu);
    measurement.idle = {
      windowMs: options.idleWindowMs,
      cpuDeltaSeconds: idleCpuDelta,
      cpuRatePercent:
        idleCpuDelta === undefined
          ? undefined
          : Number(((idleCpuDelta / (options.idleWindowMs / 1000)) * 100).toFixed(1)),
    };
    console.log(
      `idle ${options.idleWindowMs} ms: cpu +${idleCpuDelta?.toFixed(1) ?? 'n/a'} s (${measurement.idle.cpuRatePercent ?? 'n/a'}% of one core)`,
    );

    for (const file of files) {
      const text = readFileSync(file, 'utf8');
      const uri = pathToFileURL(file).href;
      const positions = queryPositions(text);
      const entry = {
        file: relative(options.workspace, file) || basename(file),
        bytes: Buffer.byteLength(text, 'utf8'),
      };

      const tOpen = performance.now();
      client.notify('textDocument/didOpen', {
        textDocument: { uri, languageId: 'eu4', version: 1, text },
      });
      const openedAt = await waitForDiagnostic(client, uri, REQUEST_TIMEOUT_MS);
      entry.openToDiagnosticsMs = openedAt - tOpen;

      const hover = await timedRequest(
        client,
        'textDocument/hover',
        { textDocument: { uri }, position: positions.hover },
        REQUEST_TIMEOUT_MS,
      );
      entry.hoverMs = hover.elapsed;

      const completion = await timedRequest(
        client,
        'textDocument/completion',
        { textDocument: { uri }, position: positions.completion },
        REQUEST_TIMEOUT_MS,
      );
      entry.completionItems = Array.isArray(completion.result)
        ? completion.result.length
        : completion.result?.items?.length ?? null;
      entry.completionMs = completion.elapsed;

      const tChange = performance.now();
      client.notify('textDocument/didChange', {
        textDocument: { uri, version: 2 },
        contentChanges: [{ text: `${text}\n# perf-edit` }],
      });
      const changedAt = await waitForDiagnostic(client, uri, REQUEST_TIMEOUT_MS);
      entry.changeToDiagnosticsMs = changedAt - tChange;

      const tClose = performance.now();
      const closeStartCpu = sampler.cpuSeconds;
      client.notify('textDocument/didClose', { textDocument: { uri } });
      await sleep(options.closeSettleMs);
      entry.closeSettleCpuSeconds = sampler.cpuDelta(closeStartCpu, sampler.cpuSeconds);

      measurement.perFile.push(entry);
      console.log(
        `${entry.file}: open->diag ${entry.openToDiagnosticsMs.toFixed(0)} ms, ` +
          `hover ${entry.hoverMs.toFixed(0)} ms, completion ${entry.completionMs.toFixed(0)} ms ` +
          `(${entry.completionItems ?? '?'} items), edit->diag ${entry.changeToDiagnosticsMs.toFixed(0)} ms, ` +
          `close cpu +${entry.closeSettleCpuSeconds?.toFixed(1) ?? 'n/a'} s`,
      );
    }

    measurement.aggregates = {
      openToDiagnosticsMs: aggregate(measurement.perFile.map((entry) => entry.openToDiagnosticsMs)),
      hoverMs: aggregate(measurement.perFile.map((entry) => entry.hoverMs)),
      completionMs: aggregate(measurement.perFile.map((entry) => entry.completionMs)),
      changeToDiagnosticsMs: aggregate(measurement.perFile.map((entry) => entry.changeToDiagnosticsMs)),
      closeSettleCpuSeconds: aggregate(measurement.perFile.map((entry) => entry.closeSettleCpuSeconds)),
    };

    await client.request('shutdown', null, 10_000).catch(() => undefined);
    client.notify('exit', null);
    await new Promise((resolve) => {
      server.once('exit', resolve);
      setTimeout(resolve, 5_000).unref?.();
    });
  } finally {
    await sampler.stop();
    measurement.steady = {
      wallTotalMs: performance.now() - started,
      cpuTotalSeconds: sampler.cpuSeconds,
      workingSetBytesFinal: sampler.last?.workingSetBytes,
      workingSetBytesPeak: sampler.peakWorkingSetBytes,
    };
    if (!server.killed && server.exitCode === null) server.kill();
  }

  console.log('--- summary ---');
  console.log(`wall total: ${(measurement.steady.wallTotalMs / 1000).toFixed(1)} s`);
  console.log(`cpu total: ${measurement.steady.cpuTotalSeconds?.toFixed(1) ?? 'n/a'} s`);
  console.log(`working set: final ${formatBytes(measurement.steady.workingSetBytesFinal)}, peak ${formatBytes(measurement.steady.workingSetBytesPeak)}`);
  for (const [name, stats] of Object.entries(measurement.aggregates)) {
    if (!stats) continue;
    console.log(`${name}: p50 ${stats.p50?.toFixed(0)} ms, p99 ${stats.p99?.toFixed(0)} ms, max ${stats.max?.toFixed(0)} ms`);
  }
  return measurement;
}

function flatten(result) {
  const rows = [];
  const visit = (prefix, value) => {
    if (value === null || value === undefined) return;
    if (typeof value === 'number' || typeof value === 'boolean') {
      rows.push([prefix, value]);
      return;
    }
    if (Array.isArray(value)) return;
    for (const [key, child] of Object.entries(value)) visit(prefix ? `${prefix}.${ key}` : key, child);
  };
  visit('', result);
  return rows;
}

function compareResults(leftPath, rightPath) {
  const left = JSON.parse(readFileSync(leftPath, 'utf8'));
  const right = JSON.parse(readFileSync(rightPath, 'utf8'));
  const leftValues = new Map(flatten(left).filter(([name]) => typeof name === 'string'));
  const rightValues = new Map(flatten(right));
  const names = [...new Set([...leftValues.keys(), ...rightValues.keys()])]
    .filter((name) => name !== 'timestamp')
    .sort();
  console.log(`comparing ${leftPath} (${left.label}) -> ${rightPath} (${right.label})`);
  console.log('');
  const format = (value) =>
    value === undefined ? '-' : typeof value === 'boolean' ? String(value) : value.toFixed(1);
  for (const name of names) {
    const before = leftValues.get(name);
    const after = rightValues.get(name);
    if (typeof before !== 'number' && typeof after !== 'number') continue;
    const delta =
      typeof before === 'number' && typeof after === 'number'
        ? before === 0
          ? after === 0 ? 0 : Infinity
          : ((after - before) / before) * 100
        : undefined;
    const deltaText =
      delta === undefined ? '' : delta === Infinity ? ' (new)' : ` (${delta >= 0 ? '+' : ''}${delta.toFixed(1)}%)`;
    console.log(`  ${name}: ${format(before)} -> ${format(after)}${deltaText}`);
  }
}

async function main() {
  const options = parseArguments(process.argv.slice(2));
  if (options.help) {
    console.log(USAGE);
    return;
  }
  if (options.compare.length >= 2) {
    compareResults(options.compare[0], options.compare[1]);
    return;
  }
  if (!options.workspace) throw new Error(`--workspace is required\n\n${USAGE}`);
  if (!options.cache && !options.noCache) throw new Error(`--cache or --no-cache is required\n\n${USAGE}`);

  const measurement = await runMeasurement(options);
  const outPath =
    options.out ??
    join(
      REPOSITORY_ROOT,
      'performance-results',
      `${measurement.label.replace(/[^a-zA-Z0-9._-]+/g, '-')}-${Date.now()}.json`,
    );
  mkdirSync(dirname(outPath), { recursive: true });
  writeFileSync(outPath, `${JSON.stringify(measurement, null, 2)}\n`, 'utf8');
  console.log(`result written: ${outPath}`);
}

await main().catch((error) => {
  console.error(`HEAD-TO-HEAD FAILED: ${error instanceof Error ? error.stack : String(error)}`);
  process.exitCode = 1;
});
