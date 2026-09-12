#!/usr/bin/env node

/**
 * Run a whole-Current-Mod diagnostic pass through the real ParadoxCode server JSON-RPC transport.
 *
 * The language server remains the source of truth for parsing, first-party EU4 rules, Vanilla
 * resolution, and diagnostics. This script only opens each relevant file, collects the normal
 * publishDiagnostics notification, and writes a human-readable plus machine-readable report.
 *
 * The CLI contract lives in `lib/options.mjs`; discovery in `lib/workspace.mjs`; the overlay
 * mapping in `lib/overlay.mjs`; the LSP session and diagnosis loops in `lib/diagnosis.mjs`; and
 * the report model in `lib/report.mjs`. This entry only composes them.
 */

import { resolve } from 'node:path';

import {
  CliUsageError,
  USAGE,
  parseArgs,
  resolveOptions,
} from './lib/options.mjs';
import { collectSourceFiles, filterFiles } from './lib/workspace.mjs';
import { pathKey } from './lib/overlay.mjs';
import {
  connectClient,
  diagnoseExplicitFiles,
  diagnoseIndexedWorkspace,
  diagnoseTextFiles,
  selectDiagnosableFiles,
  stopClient,
} from './lib/diagnosis.mjs';
import {
  addToolError,
  baseReport,
  collectServerMessages,
  shouldFail,
  writeReports,
} from './lib/report.mjs';

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

async function run(rawOptions) {
  const options = resolveOptions(rawOptions);
  const collected = collectSourceFiles(options.source, options.maxFiles);
  const files = filterFiles(
    collected.files,
    options.source,
    options.pathPrefix,
    options.shardCount,
    options.shardIndex,
  );
  console.error(
    `Discovered ${files.length} relevant source files${
      options.pathPrefix ? ` below ${options.pathPrefix}` : ''
    }${options.shardCount > 1 ? ` in shard ${options.shardIndex}/${options.shardCount}` : ''}`,
  );
  const report = baseReport(
    options,
    files,
    collected.skippedSymlinks,
    collected.omittedSymlinks,
    collected.depthLimitedDirectories,
  );
  let client;
  try {
    const { client: connected, child, vanillaMessage } = await connectClient(options);
    client = connected;
    activeServerChild = child;
    const vanillaFailed = /could not|failed|without vanilla|error/i.test(vanillaMessage);
    report.inputs.vanilla_cache.loaded = !vanillaFailed;
    report.inputs.vanilla_cache.status_message = vanillaMessage;
    if (vanillaFailed) {
      addToolError(report, `Vanilla cache was not enabled: ${vanillaMessage}`);
    }

    const selectedFiles = options.vanillaSource
      ? await selectDiagnosableFiles(client, report, options, files)
      : files;
    if (options.vanillaSource) {
      console.error(`Diagnosing ${selectedFiles.length} Vanilla files in bounded text batches`);
      await diagnoseTextFiles(client, report, options, selectedFiles);
    } else {
      const indexedFiles = await diagnoseIndexedWorkspace(client, report, options);
      const explicitOnlyFiles = selectedFiles.filter((file) => !indexedFiles.has(pathKey(file)));
      if (explicitOnlyFiles.length) {
        console.error(
          `Opening ${explicitOnlyFiles.length} file(s) not present in the indexed workspace`,
        );
      }
      await diagnoseExplicitFiles(
        client,
        report,
        options,
        explicitOnlyFiles,
        selectedFiles.length,
      );
    }
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
    console.error(`diagnose: ${error instanceof Error ? error.message : String(error)}`);
    return error instanceof CliUsageError ? 2 : 1;
  }
}

process.exitCode = await main();
