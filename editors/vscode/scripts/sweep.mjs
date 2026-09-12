#!/usr/bin/env node

/**
 * Release sweep: the per-release performance and diagnostics gate.
 *
 * Cold-starts the real ParadoxCode server, lets it discover or rebuild the
 * Vanilla index cache, diagnoses the full Vanilla workspace through virtual
 * overlays, and records how long every phase took together with a stable
 * fingerprint of the diagnostic output. Each run appends one summary line to
 * `performance-results/history.jsonl` so release-to-release regressions are
 * a plain text diff.
 *
 * Cold protocol: this script never deletes caches itself (too easy to do by
 * accident while debugging). The release workflow deletes the cache files
 * before invoking it; for a local cold run remove the caches by hand first.
 *
 * Phases are wall-clock around the shared lib calls; `server_phases` adds
 * the fine-grained numbers the server logs on startup (rules load, source
 * roots) when it reports them.
 */

import { createHash } from 'node:crypto';
import { existsSync, appendFileSync, mkdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';

import { logicalRelative } from './lib/paths.mjs';
import { execFile } from 'node:child_process';
import { performance } from 'node:perf_hooks';
import { TextDecoder } from 'node:util';
import { promisify } from 'node:util';
import os from 'node:os';

import { ProcessSampler, formatBytes } from './lib/sampler.mjs';
import { CliUsageError, REPOSITORY_ROOT, parseArgs, resolveOptions } from './lib/options.mjs';
import { collectSourceFiles } from './lib/workspace.mjs';
import { fileUri, overlayPathFor } from './lib/overlay.mjs';
import { LspProtocolError } from './lib/client.mjs';
import {
  diagnoseTextFiles,
  handshake,
  selectDiagnosableFiles,
  spawnServer,
  stopClient,
} from './lib/diagnosis.mjs';
import {
  addFileResult,
  addToolError,
  baseReport,
  collectServerMessages,
  shouldFail,
  writeReports,
} from './lib/report.mjs';

const execFileAsync = promisify(execFile);

const DEFAULT_OUTPUT_DIR = join(REPOSITORY_ROOT, 'performance-results');
const DEFAULT_TIMEOUT_MS = 1_800_000;
const DEFAULT_FILE_TIMEOUT_MS = 120_000;
const DEFAULT_MEMORY_INTERVAL_MS = 500;
const DEFAULT_QUERY_SAMPLES = 40;
const QUERY_DIAGNOSTIC_WAIT_MS = 3_000;
const UTF8_DECODER = new TextDecoder('utf-8', { fatal: true });
const WINDOWS_1252_DECODER = new TextDecoder('windows-1252');
const VANILLA_SOURCE_CANDIDATES = [
  process.env.PDC_SWEEP_VANILLA_SOURCE,
  'C:/Program Files (x86)/Steam/steamapps/common/Europa Universalis IV',
].filter(Boolean);

const USAGE = `Usage: node editors/vscode/scripts/sweep.mjs [options]

Release sweep: cold-start the server, rebuild/verify the Vanilla cache, diagnose the
full Vanilla workspace, and record phase timings plus a diagnostics fingerprint.
Writes performance-results/sweep-<timestamp>.json and appends a summary line to
performance-results/history.jsonl. For a true cold run delete the Vanilla cache
first (the release workflow does this); this script never deletes caches itself.

Options:
  --vanilla-source PATH   Vanilla tree (default: PDC_SWEEP_VANILLA_SOURCE or the standard Steam path)
  --vanilla-cache PATH    Vanilla .pdcindex (default: user configuration)
  --server PATH           paradoxcode executable (default: target/release, then target/debug)
  --output DIR            report directory (default: ${DEFAULT_OUTPUT_DIR})
  --label NAME            release label recorded in the summary (default: git describe)
  --previous PATH         previous sweep summary to diff against
  --timeout-ms N          session timeout (default: ${DEFAULT_TIMEOUT_MS})
  --file-timeout-ms N     per-request timeout (default: ${DEFAULT_FILE_TIMEOUT_MS})
  --batch-size N          files per diagnostic request (default: 16)
  --concurrency N         concurrent workers (default: 8)
  --memory-interval-ms N  server resource sampling interval (default: ${DEFAULT_MEMORY_INTERVAL_MS})
  --query-samples N       files sampled for per-query latencies (default: ${DEFAULT_QUERY_SAMPLES})
  --fail-on LEVEL         error, warning, or none (default: none — the release
                          gate is the diagnostics fingerprint against the previous
                          sweep, not the absolute severity counts)
  --help                  show this help
`;

let activeServerChild;
function terminateActiveServer() {
  const child = activeServerChild;
  if (child && child.exitCode === null && child.signalCode === null) child.kill();
}
process.once('exit', terminateActiveServer);
process.once('SIGINT', () => {
  terminateActiveServer();
  process.exit(130);
});
process.once('SIGTERM', () => {
  terminateActiveServer();
  process.exit(143);
});

function parseSweepArgs(argv) {
  const options = {
    vanillaSource: process.env.PDC_SWEEP_VANILLA_SOURCE,
    vanillaCache: process.env.PDC_DIAGNOSTIC_VANILLA_CACHE,
    server: process.env.PDC_DIAGNOSTIC_SERVER,
    output: process.env.PDC_DIAGNOSTIC_OUTPUT || DEFAULT_OUTPUT_DIR,
    label: undefined,
    previous: undefined,
    timeoutMs: DEFAULT_TIMEOUT_MS,
    fileTimeoutMs: DEFAULT_FILE_TIMEOUT_MS,
    batchSize: 16,
    concurrency: 8,
    memoryIntervalMs: DEFAULT_MEMORY_INTERVAL_MS,
    querySamples: DEFAULT_QUERY_SAMPLES,
    failOn: 'none',
  };
  const numbers = new Map([
    ['--timeout-ms', 'timeoutMs'],
    ['--file-timeout-ms', 'fileTimeoutMs'],
    ['--batch-size', 'batchSize'],
    ['--concurrency', 'concurrency'],
    ['--memory-interval-ms', 'memoryIntervalMs'],
    ['--query-samples', 'querySamples'],
  ]);
  const values = new Map([
    ['--vanilla-source', 'vanillaSource'],
    ['--vanilla-cache', 'vanillaCache'],
    ['--server', 'server'],
    ['--output', 'output'],
    ['--label', 'label'],
    ['--previous', 'previous'],
    ['--fail-on', 'failOn'],
  ]);
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === '--help' || argument === '-h') return { help: true };
    const value = argv[index + 1];
    if (numbers.has(argument)) {
      if (!value || !/^[0-9]+$/.test(value)) throw new CliUsageError(`${argument} needs a number`);
      options[numbers.get(argument)] = Number(value);
      index += 1;
    } else if (values.has(argument)) {
      if (!value) throw new CliUsageError(`${argument} needs a value`);
      options[values.get(argument)] = value;
      index += 1;
    } else {
      throw new CliUsageError(`unknown option: ${argument}`);
    }
  }
  if (!['error', 'warning', 'none'].includes(options.failOn)) {
    throw new CliUsageError('--fail-on must be error, warning, or none');
  }
  return options;
}

function resolveSweepOptions(raw) {
  let vanillaSource = raw.vanillaSource;
  if (!vanillaSource) {
    vanillaSource = VANILLA_SOURCE_CANDIDATES.find((candidate) => existsSync(candidate));
  }
  if (!vanillaSource) {
    throw new CliUsageError(
      '--vanilla-source is required: pass the EU4 installation directory or set PDC_SWEEP_VANILLA_SOURCE',
    );
  }
  // Reuse the diagnose CLI resolution: canonical paths, user-configured cache
  // discovery, server auto-detection, and the overlay workspace setup.
  return resolveOptions({
    vanillaSource,
    vanillaCache: raw.vanillaCache,
    server: raw.server,
    output: raw.output,
    timeoutMs: raw.timeoutMs,
    fileTimeoutMs: raw.fileTimeoutMs,
    batchSize: raw.batchSize,
    concurrency: raw.concurrency,
    checkpointEvery: 128,
    shardCount: 1,
    shardIndex: 0,
    failOn: raw.failOn,
  });
}

/**
 * Extracts the server's own startup phase timings from the messages it logs;
 * absent lines simply stay out of the summary.
 */
function serverPhases(serverMessages) {
  const phases = {};
  for (const message of serverMessages) {
    let match = /first-party rules ready in ([\d.]+) ms/.exec(message);
    if (match) phases.rules_ready_ms = Number(match[1]);
    match = /Source roots resolved in ([\d.]+) ms/.exec(message);
    if (match) phases.source_roots_ms = Number(match[1]);
  }
  return phases;
}

/**
 * Stable fingerprint of the diagnostic output: one sorted line per finding,
 * hashed. Equal hashes across releases mean zero diagnostic drift regardless
 * of report wording or ordering quirks.
 */
function diagnosticsFingerprint(report) {
  const lines = [];
  for (const file of report.files) {
    for (const diagnostic of file.diagnostics) {
      lines.push(
        `${file.path}|${diagnostic.severity_name}|${diagnostic.code}|` +
          `${diagnostic.location.line}:${diagnostic.location.column}|${diagnostic.message}`,
      );
    }
  }
  lines.sort();
  return createHash('sha256').update(lines.join('\n'), 'utf8').digest('hex');
}

const QUERY_METHODS = [
  'textDocument/completion',
  'textDocument/hover',
  'textDocument/definition',
  'textDocument/references',
  'textDocument/documentSymbol',
];

function percentile(values, fraction) {
  if (!values.length) return undefined;
  const sorted = [...values].sort((left, right) => left - right);
  const index = Math.min(sorted.length - 1, Math.floor(sorted.length * fraction));
  return sorted[index];
}

/**
 * Interactive-query latency sampling over a spread of already-diagnosed
 * files: opens each sample as a real overlay document, waits for its
 * diagnostics (the signal that the file is analyzed), runs the common
 * query battery at a mid-file position, measures an incremental
 * `didChange -> publishDiagnostics` round trip, then closes it.
 *
 * Returns p50/p99/max per behavior. Sequential on purpose — concurrent
 * load would contaminate the latencies being measured.
 */
async function sampleQueryLatencies(client, options, files, sampleCount) {
  const samples = new Map();
  for (const method of QUERY_METHODS) {
    samples.set(method, []);
  }
  samples.set('incremental_diagnostics', []);

  const step = Math.max(1, Math.floor(files.length / Math.max(1, sampleCount)));
  const picked = [];
  for (let index = 0; index < files.length && picked.length < sampleCount; index += step) {
    picked.push(files[index]);
  }
  console.error(`Query latency sampling: ${picked.length} file(s)`);

  const timings = (method, params) => {
    const started = performance.now();
    return client
      .request(method, params, options.fileTimeoutMs)
      .then((result) => ({ elapsed: performance.now() - started, result }))
      .catch((error) => {
        throw new LspProtocolError(`${method} failed: ${error.message}`);
      });
  };

  for (const file of picked) {
    const relativePath = logicalRelative(options.source, file);
    let text;
    try {
      text = decodeVanillaSource(readFileSync(file));
    } catch (error) {
      console.error(`[query sample skipped] ${relativePath}: ${error.message}`);
      continue;
    }
    const lines = text.split(/\r?\n/);
    let line = lines.length - 1;
    while (line > 0 && lines[line].trim().length < 4) line -= 1;
    const position = { line, character: Math.min(2, lines[line]?.length ?? 0) };
    const uri = fileUri(
      options.virtualOverlayRoot
        ? overlayPathFor(options.virtualOverlayRoot, relativePath)
        : file,
    );

    client.notify('textDocument/didOpen', {
      textDocument: { uri, languageId: 'eu4', version: 1, text },
    });
    try {
      // A clean file may legitimately never publish diagnostics; the wait
      // only opportunistically confirms the file is analyzed before the
      // query battery, so its timeout is short and non-fatal.
      await client
        .waitFor(
          (message) =>
            message.method === 'textDocument/publishDiagnostics' &&
            message.params?.uri === uri,
          QUERY_DIAGNOSTIC_WAIT_MS,
          `first diagnostics for ${relativePath}`,
        )
        .catch(() => {});
      const queryParams = { textDocument: { uri }, position };
      for (const method of QUERY_METHODS) {
        const params =
          method === 'textDocument/documentSymbol'
            ? { textDocument: { uri } }
            : method === 'textDocument/references'
              ? { textDocument: { uri }, position, context: { includeDeclaration: true } }
              : queryParams;
        const { elapsed } = await timings(method, params);
        samples.get(method).push(elapsed);
      }

      // The probe key is guaranteed to be flagged as an unknown key, so the
      // incremental round trip always produces a publish to wait for; the
      // arrival stamp filters out the open-time publish.
      const changeStarted = performance.now();
      client.notify('textDocument/didChange', {
        textDocument: { uri, version: 2 },
        contentChanges: [{ text: `${text}\nedg_sweep_probe_unknown_key = yes` }],
      });
      const published = await client
        .waitFor(
          (message) =>
            message.method === 'textDocument/publishDiagnostics' &&
            message.params?.uri === uri &&
            message._at >= changeStarted,
          QUERY_DIAGNOSTIC_WAIT_MS,
          `incremental diagnostics for ${relativePath}`,
        )
        .then(() => true)
        .catch(() => false);
      if (published) {
        samples.get('incremental_diagnostics').push(performance.now() - changeStarted);
      }
    } catch (error) {
      console.error(`[query sample failed] ${relativePath}: ${error.message}`);
    } finally {
      client.notify('textDocument/didClose', { textDocument: { uri } });
    }
  }

  const summary = {};
  for (const [name, values] of samples) {
    if (!values.length) continue;
    summary[name] = {
      p50: Math.round(percentile(values, 0.5) * 10) / 10,
      p99: Math.round(percentile(values, 0.99) * 10) / 10,
      max: Math.round(Math.max(...values) * 10) / 10,
      samples: values.length,
    };
  }
  return summary;
}

function decodeVanillaSource(bytes) {
  try {
    return UTF8_DECODER.decode(bytes);
  } catch {
    return WINDOWS_1252_DECODER.decode(bytes);
  }
}

function vanillaBuildId(vanillaSource) {
  const facts = {};
  for (const [key, file] of [
    ['build_id', 'eu4_rev.txt'],
    ['branch', 'eu4_branch.txt'],
    ['clausewitz_rev', 'clausewitz_rev.txt'],
  ]) {
    const path = join(vanillaSource, file);
    if (existsSync(path)) facts[key] = readFileSync(path, 'utf8').trim();
  }
  return facts;
}

async function gitFacts() {
  try {
    const { stdout: commit } = await execFileAsync('git', ['rev-parse', 'HEAD'], { cwd: REPOSITORY_ROOT });
    let describe = undefined;
    try {
      const { stdout } = await execFileAsync('git', ['describe', '--tags', '--always'], { cwd: REPOSITORY_ROOT });
      describe = stdout.trim();
    } catch {
      // A repository without tags still records the commit.
    }
    return { commit: commit.trim(), describe };
  } catch {
    return {};
  }
}

function summarize(previous, current) {
  const phases = ['scan_ms', 'session_boot_ms', 'classify_ms', 'diagnose_ms', 'query_samples_ms', 'total_ms'];
  const lines = [];
  for (const phase of phases) {
    const before = previous?.phases?.[phase];
    const after = current.phases[phase];
    if (!before || !after) continue;
    const delta = ((after - before) / before) * 100;
    const flagged = delta > 25 && phase !== 'total_ms' ? '  << REGRESSION?' : '';
    lines.push(`  ${phase}: ${before.toFixed(0)} ms -> ${after.toFixed(0)} ms (${delta >= 0 ? '+' : ''}${delta.toFixed(1)}%)${flagged}`);
  }
  // Query latencies flag conservatively: sub-5 ms absolute movements are
  // scheduler noise even at large relative deltas, so only p99/max past the
  // floor are marked.
  for (const name of Object.keys(current.query_latencies || {})) {
    const before = previous?.query_latencies?.[name];
    const after = current.query_latencies[name];
    if (!before) continue;
    const parts = [];
    let flagged = false;
    for (const stat of ['p50', 'p99', 'max']) {
      const beforeMs = before[stat];
      const afterMs = after[stat];
      if (!Number.isFinite(beforeMs) || !Number.isFinite(afterMs)) continue;
      const delta = ((afterMs - beforeMs) / beforeMs) * 100;
      const mark = stat !== 'p50' && afterMs - beforeMs > 5 && delta > 25 ? ' <<' : '';
      if (mark) flagged = true;
      parts.push(`${stat} ${beforeMs} -> ${afterMs} ms (${delta >= 0 ? '+' : ''}${delta.toFixed(0)}%)${mark}`);
    }
    lines.push(`  query ${name}: ${parts.join(', ')}${flagged ? '  << REGRESSION?' : ''}`);
  }
  if (previous?.summary && current.summary) {
    lines.push(
      `  diagnostics: ${previous.summary.total_diagnostics} -> ${current.summary.total_diagnostics}`,
    );
    lines.push(
      `  fingerprint: ${previous.diagnostics_fingerprint === current.diagnostics_fingerprint ? 'unchanged' : 'CHANGED'}`,
    );
  }
  return lines.join('\n');
}

async function run(raw) {
  const options = resolveSweepOptions(raw);
  const git = await gitFacts();
  const started = Date.now();
  console.error(`Sweeping Vanilla at ${options.source}`);

  const scanStarted = Date.now();
  const collected = collectSourceFiles(options.source, Number.MAX_SAFE_INTEGER);
  const scanMs = Date.now() - scanStarted;
  console.error(`Discovered ${collected.files.length} Vanilla source files in ${scanMs} ms`);

  const report = baseReport(
    options,
    collected.files,
    collected.skippedSymlinks,
    collected.omittedSymlinks,
    collected.depthLimitedDirectories,
  );

  let client;
  let sampler;
  let queryLatencies = {};
  const phases = { scan_ms: scanMs };
  const sessionStarted = Date.now();
  try {
    // Sampling starts before the handshake so the resource numbers cover the
    // whole server lifetime: rules load, Vanilla cache discovery/build, and
    // every phase after.
    const boot = await spawnServer(options);
    client = boot.client;
    activeServerChild = boot.child;
    sampler = new ProcessSampler(boot.child.pid, raw.memoryIntervalMs);
    sampler.start();

    const vanillaMessage = await handshake(client, options);
    phases.session_boot_ms = Date.now() - sessionStarted;

    const vanillaFailed = /could not|failed|without vanilla|error/i.test(vanillaMessage);
    report.inputs.vanilla_cache.loaded = !vanillaFailed;
    report.inputs.vanilla_cache.status_message = vanillaMessage;
    if (vanillaFailed) {
      addToolError(report, `Vanilla cache was not enabled: ${vanillaMessage}`);
    }

    const classifyStarted = Date.now();
    const selected = await selectDiagnosableFiles(client, report, options, collected.files);
    phases.classify_ms = Date.now() - classifyStarted;

    const diagnoseStarted = Date.now();
    console.error(`Diagnosing ${selected.length} Vanilla files in bounded text batches`);
    await diagnoseTextFiles(client, report, options, selected);
    phases.diagnose_ms = Date.now() - diagnoseStarted;

    const queriesStarted = Date.now();
    queryLatencies = await sampleQueryLatencies(client, options, selected, raw.querySamples);
    phases.query_samples_ms = Date.now() - queriesStarted;
  } catch (error) {
    addToolError(report, error instanceof Error ? error.message : String(error));
  } finally {
    await sampler?.stop();
    collectServerMessages(client, report);
    await stopClient(client, options.timeoutMs);
    activeServerChild = undefined;
    if (client?.serverStderr?.trim()) {
      report.server_stderr = client.serverStderr.trim();
    }
  }
  phases.total_ms = Date.now() - started;

  report.status = report.tool_errors.length
    ? 'incomplete'
    : shouldFail(report, raw.failOn)
      ? 'failed'
      : 'passed';
  const outputs = writeReports(report, resolve(raw.output));

  const summary = {
    schema_version: 1,
    label: raw.label || git.describe,
    git,
    recorded_at: new Date().toISOString(),
    vanilla: vanillaBuildId(options.source),
    machine: {
      platform: os.platform(),
      release: os.release(),
      cpus: os.cpus().length,
    },
    rules_hash: report.inputs.rules.manifest_rule_hash,
    files_selected: report.scan.diagnosable_files_selected,
    query_latencies: queryLatencies,
    summary: report.summary,
    diagnostics_fingerprint: diagnosticsFingerprint(report),
    phases,
    server_phases: serverPhases(report.server_messages),
    resources: {
      // Whole server lifetime: the sampler starts at spawn, before the
      // handshake, so rules load and cache build are included.
      cpu_seconds: sampler?.cpuSeconds,
      peak_working_set_bytes: sampler?.peakWorkingSetBytes,
      samples: sampler?.samples,
      sample_interval_ms: raw.memoryIntervalMs,
    },
    full_report: outputs.jsonPath,
    status: report.status,
  };

  const outputDir = resolve(raw.output);
  mkdirSync(outputDir, { recursive: true });
  const summaryPath = join(outputDir, `sweep-${new Date().toISOString().replace(/[-:]/g, '').replace(/\.\d{3}Z$/, 'Z')}.json`);
  writeFileSync(summaryPath, `${JSON.stringify(summary, null, 2)}\n`, 'utf8');
  const historyPath = join(DEFAULT_OUTPUT_DIR, 'history.jsonl');
  mkdirSync(DEFAULT_OUTPUT_DIR, { recursive: true });
  appendFileSync(historyPath, `${JSON.stringify(summary)}\n`, 'utf8');

  console.log(`Release sweep: ${report.status}`);
  console.log(`Files analyzed: ${report.summary.files_analyzed}/${report.scan.diagnosable_files_selected ?? collected.files.length}`);
  console.log(
    `Diagnostics: ${report.summary.total_diagnostics} (errors ${report.summary.errors}, warnings ${report.summary.warnings})`,
  );
  console.log(`Fingerprint: ${summary.diagnostics_fingerprint}`);
  console.log(`Peak server memory: ${formatBytes(summary.resources.peak_working_set_bytes)}`);
  for (const [phase, ms] of Object.entries(phases)) console.log(`  ${phase}: ${ms} ms`);
  for (const [name, stats] of Object.entries(queryLatencies)) {
    console.log(
      `  query ${name}: p50 ${stats.p50} ms, p99 ${stats.p99} ms, max ${stats.max} ms (${stats.samples} samples)`,
    );
  }

  // Gate semantics: the Vanilla workspace carries a known nonzero error
  // baseline under the current rules, so absolute severities cannot gate a
  // release. With a baseline the gate is the diagnostics fingerprint — an
  // identical fingerprint means behavior did not drift, whatever the error
  // count; a drift means the release changed diagnostic output and must be
  // looked at. Without a baseline (first run) the gate falls back to the
  // plain fail-on status.
  let previousSummary;
  if (raw.previous) {
    if (!existsSync(raw.previous)) {
      console.error(`--previous file not found: ${raw.previous}`);
    } else {
      previousSummary = JSON.parse(readFileSync(raw.previous, 'utf8'));
      console.log(`Comparison against ${raw.previous}:`);
      console.log(summarize(previousSummary, summary));
    }
  }
  let gate;
  if (previousSummary) {
    const drifted = previousSummary.diagnostics_fingerprint !== summary.diagnostics_fingerprint;
    gate = !drifted && report.tool_errors.length === 0;
    console.log(
      drifted
        ? 'Gate: FAILED — diagnostics drifted from the previous sweep'
        : 'Gate: PASSED — diagnostics identical to the previous sweep',
    );
  } else {
    gate = report.status === 'passed' && report.tool_errors.length === 0;
    console.log(
      `Gate: ${gate ? 'PASSED' : 'FAILED'} (no --previous baseline; gate follows the fail-on status)`,
    );
  }
  summary.gate = {
    mode: previousSummary ? 'fingerprint' : 'status',
    passed: gate,
    previous_fingerprint: previousSummary?.diagnostics_fingerprint ?? null,
  };
  writeFileSync(summaryPath, `${JSON.stringify(summary, null, 2)}\n`, 'utf8');
  // Stable-named copy: the release workflow attaches this asset so the next
  // release's sweep can pass it as --previous without name bookkeeping.
  const stableSummaryPath = join(outputDir, 'sweep-summary.json');
  writeFileSync(stableSummaryPath, `${JSON.stringify(summary, null, 2)}\n`, 'utf8');
  console.log(`Full report: ${outputs.jsonPath}`);
  console.log(`Summary: ${summaryPath}`);
  console.log(`Stable summary: ${stableSummaryPath}`);
  console.log(`History: ${historyPath}`);
  if (report.tool_errors.length) {
    for (const error of report.tool_errors) console.error(`sweep: ${error}`);
  }
  return gate ? 0 : 1;
}

async function main() {
  let raw;
  try {
    raw = parseSweepArgs(process.argv.slice(2));
  } catch (error) {
    if (error instanceof CliUsageError) {
      console.error(error.message);
      return 2;
    }
    throw error;
  }
  if (raw.help) {
    console.log(USAGE);
    return 0;
  }
  try {
    return await run(raw);
  } catch (error) {
    console.error(`sweep: ${error instanceof Error ? error.message : String(error)}`);
    return error instanceof CliUsageError ? 2 : 1;
  }
}

process.exitCode = await main();
