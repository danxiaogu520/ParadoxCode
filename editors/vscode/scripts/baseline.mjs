#!/usr/bin/env node

/**
 * Local rules-redesign baseline export (docs/rules-redesign.md, phase 0, item 5).
 *
 * Cold-starts the real ParadoxCode server exactly like `sweep.mjs`, then exports
 * the three comparison dimensions the phase-5 switch-over is checked against:
 *
 *   1. `diagnostics` — every analyzed Vanilla file with its diagnostics
 *      (code, range, message), files in stable path order;
 *   2. `symbols`     — per-kind definition and reference counts from the
 *      `pdc/workspaceSummary` request;
 *   3. `completions` — completion candidates at fixed positions on the golden
 *      test fixtures (crates/ide/src/tests/golden/*.txt).
 *
 * Two baselines must diff down to semantic differences only: the comparison
 * dimensions carry no volatile fields, all provenance (git describe/label,
 * rule hashes, generation timestamp, machine facts) lives under the single
 * `metadata` key, so `diff <(jq 'del(.metadata)' a.json) <(jq 'del(.metadata)' b.json)`
 * shows exactly what changed.
 *
 * DATA CONSTRAINT (same as `sweep.mjs`): a baseline is derived from the
 * licensed Vanilla corpus. It is a local, machine-held artifact — it must stay
 * inside the gitignored output tree (`performance-results/` and below) and must
 * never be committed, attached to a PR, or shipped in a release.
 *
 * Cold protocol: this script never deletes caches itself. For a cold run,
 * remove the dedicated Vanilla cache by hand first.
 */

import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import { join, resolve } from 'node:path';
import { promisify } from 'node:util';

import { CliUsageError, REPOSITORY_ROOT, resolveOptions } from './lib/options.mjs';
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
  addToolError,
  baseReport,
  collectServerMessages,
  shouldFail,
  writeReports,
} from './lib/report.mjs';

const execFileAsync = promisify(execFile);

// The output default stays under the repository-root `performance-results/`
// tree that .gitignore already covers with `/performance-results/*`; nothing
// this script writes is ever tracked (see the data constraint above).
const DEFAULT_OUTPUT_DIR = join(REPOSITORY_ROOT, 'performance-results', 'baselines');
const DEFAULT_TIMEOUT_MS = 1_800_000;
const DEFAULT_FILE_TIMEOUT_MS = 120_000;
const PROBE_DIAGNOSTIC_WAIT_MS = 3_000;
const GOLDEN_DIR = join(REPOSITORY_ROOT, 'crates', 'ide', 'src', 'tests', 'golden');
const VANILLA_SOURCE_CANDIDATES = [
  process.env.PDC_SWEEP_VANILLA_SOURCE,
  'C:/Program Files (x86)/Steam/steamapps/common/Europa Universalis IV',
].filter(Boolean);

const USAGE = `Usage: node editors/vscode/scripts/baseline.mjs [options]

Local rules-redesign baseline export: cold-start an explicitly selected server, diagnose
the full Vanilla workspace, record per-kind symbol definition/reference counts, and record
completion candidates at fixed golden-fixture positions. The output is one deterministic
JSON document whose only volatile block is the top-level "metadata" key; diff two baselines
with that key excluded. Baselines contain licensed Vanilla-derived data and stay in the
gitignored performance-results/baselines/ tree. For a true cold run delete the Vanilla
cache first; this script never deletes caches itself.

Options:
  --vanilla-source PATH   Vanilla tree (default: PDC_SWEEP_VANILLA_SOURCE or the standard Steam path)
  --vanilla-cache PATH    Vanilla .pdcindex (default: user configuration)
  --server PATH           paradoxcode executable to baseline (required; no auto-detection)
  --output DIR            baseline directory (default: ${DEFAULT_OUTPUT_DIR})
  --label NAME            local run label recorded in the baseline metadata (default: git describe)
  --timeout-ms N          session timeout (default: ${DEFAULT_TIMEOUT_MS})
  --file-timeout-ms N     per-request timeout (default: ${DEFAULT_FILE_TIMEOUT_MS})
  --batch-size N          files per diagnostic request (default: 16)
  --concurrency N         concurrent diagnostic workers (default: 8)
  --fail-on LEVEL         error, warning, or none (default: none)
  --help                  show this help
`;

/**
 * Fixed completion probes over the golden test fixtures.
 *
 * Every probe addresses one (line, character) position inside the embedded
 * `// input:` text of crates/ide/src/tests/golden/<goldenFile>.txt, opened as a
 * virtual overlay document at the same logical path the golden test uses so
 * file-zone semantics match the fixture's scenario. Positions are hardcoded on
 * purpose (they are the comparison contract); `expectLine` pins the exact
 * input line so a drifted fixture fails the probe loudly instead of silently
 * sampling the wrong spot.
 *
 * Position semantics (crates/ide/src/completion/mod.rs + support.rs): the
 * completion prefix is the *whole word* at the cursor (word characters are
 * `[A-Za-z0-9_\-.@$]`, scanned in both directions) and candidates match by
 * case-insensitive prefix or substring. A cursor on an existing token
 * therefore filters by that full token; a cursor in whitespace yields an
 * empty prefix and the full candidate inventory (bounded by the server's
 * completion cap). The probes deliberately mix both flavors across the
 * typical surfaces from the design doc: values of known rule keys, key
 * positions in effect/trigger blocks, an empty block body, scope
 * expressions, and reference-name blocks.
 */
const GOLDEN_COMPLETION_PROBES = [
  {
    id: 'rws-option-name-value',
    goldenFile: 'rule_wrong_scope.txt',
    documentPath: 'events/golden_rws.txt',
    line: 0,
    character: 65,
    expectLine: 'province_event = { id = rws.1 title = rws.1.t option = { name = a add_prestige = 1 } }',
    // Value position of a known key: `name` of an event `option` holds a
    // localisation key. The cursor ends the value word `a`, so the candidate
    // set is the localisation-key inventory filtered by substring `a`.
    reason: 'value of known key `name` in an event option (localisation-key value, word `a`)',
  },
  {
    id: 'rws-option-effect-key',
    goldenFile: 'rule_wrong_scope.txt',
    documentPath: 'events/golden_rws.txt',
    line: 0,
    character: 70,
    expectLine: 'province_event = { id = rws.1 title = rws.1.t option = { name = a add_prestige = 1 } }',
    // Key position inside an effect block, word-filtered by the full key
    // `add_prestige`. The fixture is a province_event, so the option body is
    // province scope and the country-scoped `add_prestige` family is filtered
    // out: a scope-filter regression check (currently no candidates).
    reason: 'effect-key position in a province-scope option block (scope filter, word `add_prestige`)',
  },
  {
    id: 'mle-empty-if-body',
    goldenFile: 'missing_limit_and_empty_block.txt',
    documentPath: 'events/golden_mle.txt',
    line: 3,
    character: 8,
    expectLine: '  if = { }',
    // Empty spot in a block: the cursor sits in the whitespace of an empty
    // `if = { }` body (empty word), so this records the full effect-key
    // inventory available in an event-option effect block.
    reason: 'empty effect-block body (empty word in `if = { }`)',
  },
  {
    id: 'mle-if-effect-key',
    goldenFile: 'missing_limit_and_empty_block.txt',
    documentPath: 'events/golden_mle.txt',
    line: 1,
    character: 15,
    expectLine: '  if = { add_prestige = 1 }',
    // Same effect block, but word-filtered: the cursor is inside the key
    // `add_prestige`, so the candidates are the effect keys matching that
    // full word (here a country scope where the key is valid).
    reason: 'effect-key position inside an `if` body (word `add_prestige`)',
  },
  {
    id: 'mle-limit-trigger-open',
    goldenFile: 'missing_limit_and_empty_block.txt',
    documentPath: 'events/golden_mle.txt',
    line: 2,
    character: 18,
    expectLine: '  if = { limit = { always = yes } }',
    // Empty spot in a trigger block: whitespace right after `limit = {`
    // (empty word), so this records the full trigger-key inventory.
    reason: 'empty position at the start of a `limit` block (trigger-key inventory)',
  },
  {
    id: 'mle-limit-trigger-key',
    goldenFile: 'missing_limit_and_empty_block.txt',
    documentPath: 'events/golden_mle.txt',
    line: 2,
    character: 23,
    expectLine: '  if = { limit = { always = yes } }',
    // Same trigger block, word-filtered by the full key `always`.
    reason: 'trigger-key position inside a `limit` block (word `always`)',
  },
  {
    id: 'msm-modifier-name-value',
    goldenFile: 'modifier_scope_mismatch.txt',
    documentPath: 'events/golden_events.txt',
    line: 4,
    character: 48,
    expectLine: '        add_province_modifier = { name = country_mod duration = 100 }',
    // Value position of a known key with a name-reference matcher: `name` of
    // `add_province_modifier` takes an event/static modifier name; the cursor
    // is inside the value word `country_mod`.
    reason: 'value of known key `name` in `add_province_modifier` (modifier-name reference, word `country_mod`)',
  },
  {
    id: 'dsc-scope-target-open',
    goldenFile: 'dynamic_scope_contracts.txt',
    documentPath: 'common/scripted_effects/00_contracts.txt',
    line: 4,
    character: 39,
    expectLine: 'dual_ok = { add_prestige = 1 add_core = FRA }',
    // Scope expression position, unfiltered: whitespace right after `add_core =`
    // (empty word) before the scope target `FRA`, i.e. the full scope-target
    // candidate inventory (tags, scope chains, links).
    reason: 'scope-target value of known key `add_core` (empty word before `FRA`)',
  },
  {
    id: 'dsc-scope-chain-keyword',
    goldenFile: 'dynamic_scope_contracts.txt',
    documentPath: 'common/scripted_effects/00_contracts.txt',
    line: 8,
    character: 35,
    expectLine: 'root_opaque = { add_prestige = 1 ROOT = { change_province_name = "Z" } }',
    // Scope expression position, word-filtered: the cursor is inside the
    // `ROOT` scope-chain keyword, so candidates are everything matching
    // `root` — scope keywords plus workspace definitions (e.g. the fixture's
    // own `root_opaque` scripted effect).
    reason: 'scope-chain keyword position inside an effect body (word `ROOT`)',
  },
  {
    id: 'mst-required-missions-open',
    goldenFile: 'mission_trees.txt',
    documentPath: 'missions/golden_main.txt',
    line: 14,
    character: 23,
    expectLine: '\t\trequired_missions = { gm_root }',
    // Reference-name block, unfiltered: whitespace right after
    // `required_missions = {` (empty word), i.e. the full mission-id candidate
    // inventory from the workspace plus the open fixture.
    reason: 'mission-id reference block `required_missions` (empty word before `gm_root`)',
  },
  {
    id: 'mst-required-missions-ref',
    goldenFile: 'mission_trees.txt',
    documentPath: 'missions/golden_main.txt',
    line: 14,
    character: 27,
    expectLine: '\t\trequired_missions = { gm_root }',
    // Reference-name block, word-filtered: the cursor is inside the mission id
    // `gm_root`.
    reason: 'mission-id reference position inside `required_missions` (word `gm_root`)',
  },
];

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

function parseBaselineArgs(argv) {
  const options = {
    vanillaSource: process.env.PDC_SWEEP_VANILLA_SOURCE,
    vanillaCache: process.env.PDC_DIAGNOSTIC_VANILLA_CACHE,
    server: process.env.PDC_DIAGNOSTIC_SERVER,
    output: process.env.PDC_DIAGNOSTIC_OUTPUT || DEFAULT_OUTPUT_DIR,
    label: undefined,
    timeoutMs: DEFAULT_TIMEOUT_MS,
    fileTimeoutMs: DEFAULT_FILE_TIMEOUT_MS,
    batchSize: 16,
    concurrency: 8,
    failOn: 'none',
  };
  const numbers = new Map([
    ['--timeout-ms', 'timeoutMs'],
    ['--file-timeout-ms', 'fileTimeoutMs'],
    ['--batch-size', 'batchSize'],
    ['--concurrency', 'concurrency'],
  ]);
  const values = new Map([
    ['--vanilla-source', 'vanillaSource'],
    ['--vanilla-cache', 'vanillaCache'],
    ['--server', 'server'],
    ['--output', 'output'],
    ['--label', 'label'],
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

function resolveBaselineOptions(raw) {
  if (!raw.server) {
    throw new CliUsageError(
      '--server is required for a baseline export; build the intended binary and pass its explicit path',
    );
  }
  let vanillaSource = raw.vanillaSource;
  if (!vanillaSource) {
    vanillaSource = VANILLA_SOURCE_CANDIDATES.find((candidate) => existsSync(candidate));
  }
  if (!vanillaSource) {
    throw new CliUsageError(
      '--vanilla-source is required: pass the EU4 installation directory or set PDC_SWEEP_VANILLA_SOURCE',
    );
  }
  // Reuse the diagnose CLI resolution for canonical paths, user-configured
  // cache discovery, and the virtual overlay workspace setup. Requiring
  // --server above prevents a stale target/debug binary from being selected
  // implicitly (same contract as sweep.mjs).
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
 * Provenance helpers, kept in the same shape as sweep.mjs's summary facts:
 * everything here lands under the baseline's `metadata` key so it never mixes
 * into the diffable comparison dimensions.
 */
async function gitFacts() {
  try {
    const { stdout: commit } = await execFileAsync('git', ['rev-parse', 'HEAD'], { cwd: REPOSITORY_ROOT });
    const { stdout: status } = await execFileAsync('git', ['status', '--porcelain'], { cwd: REPOSITORY_ROOT });
    let describe = undefined;
    try {
      const { stdout } = await execFileAsync('git', ['describe', '--tags', '--always'], { cwd: REPOSITORY_ROOT });
      describe = stdout.trim();
    } catch {
      // A repository without tags still records the commit.
    }
    return { commit: commit.trim(), describe, dirty: status.trim().length > 0 };
  } catch {
    return {};
  }
}

async function serverFacts(server) {
  const bytes = readFileSync(server);
  let version;
  try {
    const { stdout } = await execFileAsync(server, ['--version']);
    version = stdout.trim();
  } catch {
    version = undefined;
  }
  return {
    path: server,
    sha256: createHash('sha256').update(bytes).digest('hex'),
    version,
  };
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

function activeRulesHash(serverMessages) {
  for (const message of serverMessages) {
    const match = /first-party rules ready.*\(hash ([0-9a-f]{64})\)/.exec(message);
    if (match) return match[1];
  }
  return undefined;
}

// ---------------------------------------------------------------------------
// symbols: per-kind definition/reference counts from pdc/workspaceSummary
// ---------------------------------------------------------------------------

/**
 * Normalizes a kind -> count map into a plain object with keys in code-unit
 * order so serialization is byte-stable across runs and platforms.
 */
function sortedCountMap(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return undefined;
  const out = {};
  for (const key of Object.keys(value).sort()) {
    const count = value[key];
    if (!Number.isSafeInteger(count) || count < 0) return undefined;
    out[key] = count;
  }
  return out;
}

async function requestWorkspaceSummary(client, options, report) {
  // pdc/workspaceSummary is parameterless: passing params at all is rejected
  // by the server, and the shared LspClient drops an undefined `params` key.
  let result;
  try {
    result = await client.request('pdc/workspaceSummary', undefined, options.timeoutMs);
  } catch (error) {
    addToolError(report, `pdc/workspaceSummary failed: ${error.message}`);
    return undefined;
  }
  const definitions = sortedCountMap(result?.symbols?.definitions);
  const references = sortedCountMap(result?.symbols?.references);
  if (!definitions || !references) {
    // The request exists but the payload predates (or lost) the per-kind
    // symbol counts. Do not synthesize them: keep the baseline honest, record
    // what the request did return, and flag the hole as a tool error.
    addToolError(
      report,
      'pdc/workspaceSummary did not return symbols.definitions/symbols.references as kind -> count maps; ' +
        'the symbols section of this baseline is incomplete',
    );
  }
  return {
    definitions,
    references,
    gameId: result?.gameId ?? null,
    ruleHash: result?.ruleHash ?? null,
    revision: Number.isSafeInteger(result?.revision) ? result.revision : null,
    fileCounts: result?.fileCounts ?? null,
    scan: result?.scan ?? null,
  };
}

// ---------------------------------------------------------------------------
// completions: fixed golden-fixture probes
// ---------------------------------------------------------------------------

/**
 * Reconstructs the fixture's embedded input text: the lines between the
 * `// input:` marker and the `diagnostics:` section, each stored by
 * crates/ide/src/tests/golden.rs as `//   {line}`. The join reproduces the
 * original text exactly, trailing newline included.
 */
function extractGoldenInput(path) {
  const lines = readFileSync(path, 'utf8').replace(/\r\n/g, '\n').split('\n');
  const marker = lines.indexOf('// input:');
  if (marker < 0) throw new Error('no `// input:` section found');
  const input = [];
  for (let index = marker + 1; index < lines.length; index += 1) {
    const line = lines[index];
    if (!line.startsWith('//   ')) break;
    input.push(line.slice(5));
  }
  return input.join('\n');
}

function compareStrings(left, right) {
  return left < right ? -1 : left > right ? 1 : 0;
}

function compareCandidates(left, right) {
  return (
    compareStrings(left.label, right.label) ||
    compareStrings(String(left.kind ?? ''), String(right.kind ?? '')) ||
    compareStrings(left.detail ?? '', right.detail ?? '')
  );
}

/**
 * Opens each golden fixture once as a virtual overlay document (same logical
 * path as its golden test, so file-zone semantics match the scenario), runs
 * `textDocument/completion` at every fixed probe position, and records the
 * sorted candidate labels plus their kind/detail. Probes sharing a fixture
 * share one didOpen/didClose pair; a drifted fixture line or a failed request
 * is reported as a tool error and skipped, never silently mis-sampled.
 */
async function collectGoldenCompletions(client, options, report) {
  const results = [];
  const fixtures = new Map();
  for (const probe of GOLDEN_COMPLETION_PROBES) {
    const key = `${probe.goldenFile}\u0000${probe.documentPath}`;
    if (!fixtures.has(key)) {
      fixtures.set(key, { goldenFile: probe.goldenFile, documentPath: probe.documentPath, probes: [] });
    }
    fixtures.get(key).probes.push(probe);
  }

  for (const fixture of fixtures.values()) {
    let text;
    try {
      text = extractGoldenInput(join(GOLDEN_DIR, fixture.goldenFile));
    } catch (error) {
      addToolError(report, `golden fixture ${fixture.goldenFile}: ${error.message}`);
      continue;
    }
    const lines = text.split('\n');
    const probes = fixture.probes.filter((probe) => {
      if (lines[probe.line] !== probe.expectLine) {
        addToolError(
          report,
          `golden probe ${probe.id}: ${fixture.goldenFile} input line ${probe.line} drifted ` +
            `(expected ${JSON.stringify(probe.expectLine)}); update the probe constants`,
        );
        return false;
      }
      return true;
    });
    if (!probes.length) continue;

    const uri = fileUri(overlayPathFor(options.virtualOverlayRoot, fixture.documentPath));
    client.notify('textDocument/didOpen', {
      textDocument: { uri, languageId: 'eu4', version: 1, text },
    });
    try {
      // Best-effort readiness signal only: a clean fixture may legitimately
      // never publish diagnostics (same non-fatal wait as sweep.mjs).
      await client
        .waitFor(
          (message) =>
            message.method === 'textDocument/publishDiagnostics' &&
            message.params?.uri === uri,
          PROBE_DIAGNOSTIC_WAIT_MS,
          `first diagnostics for ${fixture.documentPath}`,
        )
        .catch(() => {});
      for (const probe of probes) {
        try {
          const response = await client.request(
            'textDocument/completion',
            { textDocument: { uri }, position: { line: probe.line, character: probe.character } },
            options.fileTimeoutMs,
          );
          const items = Array.isArray(response)
            ? response
            : Array.isArray(response?.items)
              ? response.items
              : undefined;
          if (!items) throw new LspProtocolError('textDocument/completion returned an invalid result');
          const candidates = items
            .map((item) => ({
              label: String(item?.label ?? ''),
              kind: Number.isSafeInteger(item?.kind) ? item.kind : null,
              detail: item?.detail == null ? null : String(item.detail),
            }))
            .sort(compareCandidates);
          results.push({
            id: probe.id,
            golden_file: probe.goldenFile,
            document_path: probe.documentPath,
            reason: probe.reason,
            position: { line: probe.line, character: probe.character },
            labels: [...new Set(candidates.map((candidate) => candidate.label))].sort(compareStrings),
            candidates,
            is_incomplete: Array.isArray(response?.items) ? Boolean(response.isIncomplete) : false,
          });
          console.error(
            `[probe ${probe.id}] ${fixture.documentPath}:${probe.line}:${probe.character} -> ${candidates.length} candidate(s)`,
          );
        } catch (error) {
          addToolError(report, `golden probe ${probe.id}: ${error.message}`);
        }
      }
    } finally {
      client.notify('textDocument/didClose', { textDocument: { uri } });
    }
  }
  return results;
}

// ---------------------------------------------------------------------------
// baseline document
// ---------------------------------------------------------------------------

function normalizeRange(range) {
  if (!range) return null;
  const position = (value) => ({
    line: Number(value?.line ?? 0),
    character: Number(value?.character ?? 0),
  });
  return { start: position(range.start), end: position(range.end ?? range.start) };
}

function compareDiagnostics(left, right) {
  return (
    left.range.start.line - right.range.start.line ||
    left.range.start.character - right.range.start.character ||
    left.range.end.line - right.range.end.line ||
    left.range.end.character - right.range.end.character ||
    compareStrings(left.code, right.code) ||
    compareStrings(left.message, right.message)
  );
}

/**
 * Assembles the baseline document. Field order is fixed (JSON.stringify keeps
 * insertion order) and every list is sorted with locale-independent string
 * comparisons, so two runs over identical inputs serialize byte-identically
 * outside `metadata`.
 */
function buildBaseline({ label, git, server, report, summary, completions, vanilla, startedAt }) {
  const diagnostics = report.files
    .map((file) => ({
      path: file.path,
      diagnostics: file.diagnostics
        .map((diagnostic) => ({
          code: diagnostic.code,
          range: normalizeRange(diagnostic.range),
          message: diagnostic.message,
        }))
        .sort(compareDiagnostics),
    }))
    .sort((left, right) => compareStrings(left.path, right.path));
  return {
    schema_version: 1,
    kind: 'paradoxcode-rules-baseline',
    metadata: {
      label: label || git.describe || null,
      // The only wall-clock field in the whole document; exclude `metadata`
      // when diffing two baselines.
      generated_at: new Date(startedAt).toISOString(),
      git,
      server,
      machine: {
        platform: os.platform(),
        release: os.release(),
        cpus: os.cpus().length,
      },
      rules: {
        manifest_rule_hash: report.inputs.rules.manifest_rule_hash ?? null,
        server_log_rule_hash: activeRulesHash(report.server_messages) ?? null,
        workspace_summary_rule_hash: summary?.ruleHash ?? null,
      },
      vanilla,
      vanilla_cache: {
        path: report.inputs.vanilla_cache.path,
        loaded: report.inputs.vanilla_cache.loaded,
        status_message: report.inputs.vanilla_cache.status_message,
      },
      workspace_summary: {
        game_id: summary?.gameId ?? null,
        revision: summary?.revision ?? null,
        file_counts: summary?.fileCounts ?? null,
        scan: summary?.scan ?? null,
      },
      totals: {
        files: report.summary.files_analyzed,
        files_with_diagnostics: report.summary.files_with_diagnostics,
        diagnostics: report.summary.total_diagnostics,
        errors: report.summary.errors,
        warnings: report.summary.warnings,
        infos: report.summary.infos,
        hints: report.summary.hints,
      },
      tool_errors: report.tool_errors,
      status: report.status,
    },
    diagnostics,
    symbols: {
      definitions: summary?.definitions ?? {},
      references: summary?.references ?? {},
    },
    completions: [...completions].sort(
      (left, right) =>
        compareStrings(left.document_path, right.document_path) ||
        left.position.line - right.position.line ||
        left.position.character - right.position.character ||
        compareStrings(left.id, right.id),
    ),
  };
}

async function run(raw) {
  const options = resolveBaselineOptions(raw);
  const startedAt = Date.now();
  const git = await gitFacts();
  const server = await serverFacts(options.server);
  console.error(`Baselining Vanilla at ${options.source}`);

  const collected = collectSourceFiles(options.source, Number.MAX_SAFE_INTEGER);
  console.error(`Discovered ${collected.files.length} Vanilla source files`);
  const report = baseReport(
    options,
    collected.files,
    collected.skippedSymlinks,
    collected.omittedSymlinks,
    collected.depthLimitedDirectories,
  );

  let client;
  let summary;
  let completions = [];
  try {
    const boot = await spawnServer(options);
    client = boot.client;
    activeServerChild = boot.child;

    const vanillaMessage = await handshake(client, options);
    const vanillaFailed = /could not|failed|without vanilla|error/i.test(vanillaMessage);
    report.inputs.vanilla_cache.loaded = !vanillaFailed;
    report.inputs.vanilla_cache.status_message = vanillaMessage;
    if (vanillaFailed) {
      addToolError(report, `Vanilla cache was not enabled: ${vanillaMessage}`);
    }

    const selected = await selectDiagnosableFiles(client, report, options, collected.files);
    console.error(`Diagnosing ${selected.length} Vanilla files in bounded text batches`);
    await diagnoseTextFiles(client, report, options, selected);

    // Symbol counts come from the workspace index as it stands after the
    // diagnosis pass, before the golden probe documents are opened (an open
    // overlay can contribute definitions; probes must not feed each other).
    summary = await requestWorkspaceSummary(client, options, report);
    completions = await collectGoldenCompletions(client, options, report);
  } catch (error) {
    addToolError(report, error instanceof Error ? error.message : String(error));
  } finally {
    collectServerMessages(client, report);
    await stopClient(client, options.timeoutMs);
    activeServerChild = undefined;
    if (client?.serverStderr?.trim()) {
      report.server_stderr = client.serverStderr.trim();
    }
  }

  const sourceRulesHash = report.inputs.rules.manifest_rule_hash;
  const loadedRulesHash = activeRulesHash(report.server_messages);
  if (!loadedRulesHash) {
    addToolError(report, 'the server did not report its active first-party rules hash');
  } else if (loadedRulesHash !== sourceRulesHash) {
    addToolError(
      report,
      `server rules hash ${loadedRulesHash} does not match the checkout manifest ${sourceRulesHash}; rebuild the selected binary`,
    );
  }
  if (summary?.ruleHash && sourceRulesHash && summary.ruleHash !== sourceRulesHash) {
    addToolError(
      report,
      `workspace summary rule hash ${summary.ruleHash} does not match the checkout manifest ${sourceRulesHash}`,
    );
  }

  report.status = report.tool_errors.length
    ? 'incomplete'
    : shouldFail(report, raw.failOn)
      ? 'failed'
      : 'passed';
  // Full diagnostic report in the shared sweep/diagnose shape, next to the
  // baseline itself (all of it under the gitignored output tree).
  const outputs = writeReports(report, resolve(raw.output));

  const baseline = buildBaseline({
    label: raw.label,
    git,
    server,
    report,
    summary,
    completions,
    vanilla: vanillaBuildId(options.source),
    startedAt,
  });
  const outputDir = resolve(raw.output);
  mkdirSync(outputDir, { recursive: true });
  const stamp = baseline.metadata.generated_at.replace(/[-:]/g, '').replace(/\.\d{3}Z$/, 'Z');
  const baselinePath = join(outputDir, `baseline-${stamp}.json`);
  writeFileSync(baselinePath, `${JSON.stringify(baseline, null, 2)}\n`, 'utf8');
  // Stable local copy so a later comparison can reference one known path.
  const latestPath = join(outputDir, 'baseline-latest.json');
  writeFileSync(latestPath, `${JSON.stringify(baseline, null, 2)}\n`, 'utf8');

  console.log(`Local rules-redesign baseline: ${report.status}`);
  console.log(
    `Files: ${report.summary.files_analyzed} analyzed, ${report.summary.files_with_diagnostics} with diagnostics`,
  );
  console.log(
    `Diagnostics: ${report.summary.total_diagnostics} (errors ${report.summary.errors}, warnings ${report.summary.warnings})`,
  );
  if (summary?.definitions && summary?.references) {
    const definitions = Object.values(summary.definitions).reduce((total, count) => total + count, 0);
    const references = Object.values(summary.references).reduce((total, count) => total + count, 0);
    console.log(
      `Symbols: ${definitions} definitions across ${Object.keys(summary.definitions).length} kinds, ` +
        `${references} references across ${Object.keys(summary.references).length} kinds`,
    );
  } else {
    console.log('Symbols: unavailable (see tool errors)');
  }
  console.log(`Completion probes: ${completions.length}/${GOLDEN_COMPLETION_PROBES.length} recorded`);
  console.log(`Full report: ${outputs.jsonPath}`);
  console.log(`Baseline: ${baselinePath}`);
  console.log(`Stable baseline: ${latestPath}`);
  if (report.tool_errors.length) {
    for (const error of report.tool_errors) console.error(`baseline: ${error}`);
  }
  return report.status === 'passed' ? 0 : 1;
}

async function main() {
  let raw;
  try {
    raw = parseBaselineArgs(process.argv.slice(2));
  } catch (error) {
    if (error instanceof CliUsageError) {
      console.error(`${error.detail}\n\n${USAGE}`);
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
    const message = error instanceof CliUsageError
      ? `${error.detail}\n\n${USAGE}`
      : error instanceof Error
        ? error.message
        : String(error);
    console.error(`baseline: ${message}`);
    return error instanceof CliUsageError ? 2 : 1;
  }
}

process.exitCode = await main();
