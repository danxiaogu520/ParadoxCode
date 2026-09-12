/**
 * LSP diagnosis session: boots the ParadoxCode server over stdio, waits for
 * the Vanilla cache, and drives the three diagnosis paths — indexed
 * workspace batches, explicit didOpen overlays, and bounded text batches —
 * feeding every result into the shared report model.
 */

import { readFileSync } from 'node:fs';
import { relative, resolve, sep } from 'node:path';
import { spawn } from 'node:child_process';
import { TextDecoder } from 'node:util';

import { LspClient, LspProtocolError } from './client.mjs';
import { CHECKPOINT_FILE_INTERVAL, REPOSITORY_ROOT } from './options.mjs';
import { addFileResult, addToolError, writeReports, MAX_REPORTED_TOOL_ERRORS } from './report.mjs';
import { diagnosticItemPath, fileUri, overlayPathFor, pathKey } from './overlay.mjs';

const MAX_SOURCE_BYTES = 16 * 1024 * 1024;
const UTF8_DECODER = new TextDecoder('utf-8', { fatal: true });
const WINDOWS_1252_DECODER = new TextDecoder('windows-1252');

function decodeSource(bytes) {
  try {
    return { text: UTF8_DECODER.decode(bytes), encoding: 'utf-8' };
  } catch {
    return { text: WINDOWS_1252_DECODER.decode(bytes), encoding: 'windows-1252' };
  }
}

/**
 * Spawns the server, completes the LSP handshake, and waits for the Vanilla
 * cache progress token. Returns the live client, the child process (so the
 * caller can hard-kill it on signals), and the Vanilla readiness message.
 */
export async function connectClient(options) {
  const child = spawn(options.server, [], {
    cwd: REPOSITORY_ROOT,
    stdio: ['pipe', 'pipe', 'pipe'],
    windowsHide: true,
  });
  const client = new LspClient(child);
  const initializeParams = {
    processId: process.pid,
    clientInfo: { name: 'paradoxcode-current-mod-diagnostics', version: '1' },
    workspaceFolders: [{ uri: fileUri(options.workspace), name: 'diagnostic-workspace' }],
    capabilities: {
      window: { workDoneProgress: true },
      workspace: { didChangeWatchedFiles: { dynamicRegistration: false } },
      textDocument: { completion: { completionItem: { snippetSupport: false } } },
    },
    initializationOptions: {
      modDirectory: options.mod,
      vanillaIndexCache: options.vanillaCache,
    },
    trace: 'off',
  };
  const initializeStarted = Date.now();
  await client.request('initialize', initializeParams, options.timeoutMs);
  console.error(`Workspace initialized in ${Date.now() - initializeStarted} ms`);
  client.notify('initialized', {});
  const vanillaStarted = Date.now();
  const vanillaMessage = await waitForVanillaReady(client, options.timeoutMs);
  console.error(`Vanilla cache ready in ${Date.now() - vanillaStarted} ms`);
  return { client, child, vanillaMessage };
}

async function waitForVanillaReady(client, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let started = false;
  let lastReportedPercentage = -10;
  while (true) {
    const remaining = deadline - Date.now();
    if (remaining <= 0) throw new LspProtocolError('timed out waiting for Vanilla cache loading');
    const message = await client.next(remaining);
    client.handle(message);
    if (message.method !== '$/progress') continue;
    const value = message.params?.value;
    const progressText = `${value?.title || ''} ${value?.message || ''}`.toLowerCase();
    if (value?.kind === 'begin' && progressText.includes('vanilla')) {
      started = true;
      console.error(value.message || 'Vanilla cache loading started');
    }
    if (
      started &&
      value?.kind === 'report' &&
      Number.isFinite(value.percentage) &&
      value.percentage >= lastReportedPercentage + 10
    ) {
      lastReportedPercentage = value.percentage;
      console.error(value.message || `Vanilla cache loading: ${value.percentage}%`);
    }
    if (started && value?.kind === 'end') return value.message || 'Vanilla cache loading completed';
  }
}

export async function diagnoseIndexedWorkspace(client, report, options) {
  const analyzed = new Set();
  let offset = 0;
  let nextCheckpoint = CHECKPOINT_FILE_INTERVAL;
  while (true) {
    const batch = await client.request(
      'pdc/workspaceDiagnostics',
      { offset, limit: options.batchSize },
      Math.max(options.fileTimeoutMs * options.batchSize, 120_000),
    );
    if (!batch || !Array.isArray(batch.items) || !Number.isSafeInteger(batch.total)) {
      throw new LspProtocolError('pdc/workspaceDiagnostics returned an invalid batch');
    }
    report.scan.workspace_diagnostic_files = batch.total;
    for (const item of batch.items) {
      if (typeof item?.uri !== 'string' || !Array.isArray(item.diagnostics)) {
        throw new LspProtocolError('pdc/workspaceDiagnostics returned an invalid file result');
      }
      const file = diagnosticItemPath(item, options.mod);
      const bytes = readFileSync(file);
      const decoded = decodeSource(bytes);
      addFileResult(report, file, decoded.text, decoded.encoding, item.diagnostics, options.mod);
      analyzed.add(pathKey(file));
    }
    const nextOffset = batch.nextOffset;
    report.scan.next_workspace_diagnostic_offset = nextOffset ?? batch.total;
    const completed = report.scan.next_workspace_diagnostic_offset;
    console.error(`Workspace diagnostics: ${completed}/${batch.total} indexed files`);
    if (completed >= nextCheckpoint || nextOffset === null) {
      writeReports(report, resolve(options.output));
      console.error(`Checkpoint written after ${completed} indexed files`);
      nextCheckpoint = completed + CHECKPOINT_FILE_INTERVAL;
    }
    if (nextOffset === null) break;
    if (!Number.isSafeInteger(nextOffset) || nextOffset <= offset || nextOffset > batch.total) {
      throw new LspProtocolError('pdc/workspaceDiagnostics returned a non-advancing offset');
    }
    offset = nextOffset;
  }
  return analyzed;
}

export async function selectDiagnosableFiles(client, report, options, files) {
  const accepted = new Set();
  for (let offset = 0; offset < files.length; offset += 4_096) {
    const batch = files.slice(offset, offset + 4_096);
    const logicalPaths = batch.map((file) =>
      relative(options.source, file).split(sep).join('/'),
    );
    const result = await client.request(
      'pdc/classifyPaths',
      { paths: logicalPaths },
      options.fileTimeoutMs,
    );
    if (!Array.isArray(result) || result.some((path) => typeof path !== 'string')) {
      throw new LspProtocolError('pdc/classifyPaths returned an invalid result');
    }
    for (const path of result) accepted.add(path);
  }
  const selected = files.filter((file) =>
    accepted.has(relative(options.source, file).split(sep).join('/')),
  );
  report.scan.diagnosable_files_selected = selected.length;
  console.error(
    `Selected ${selected.length}/${files.length} profile-supported Script/Localisation files`,
  );
  return selected;
}

export async function diagnoseExplicitFiles(client, report, options, files, totalFiles) {
  const pending = new Map();
  let cursor = 0;

  function openUntilFull() {
    while (pending.size < options.concurrency && cursor < files.length) {
      const file = files[cursor];
      cursor += 1;
      const relativePath = relative(options.source, file).split(sep).join('/');
      try {
        const bytes = readFileSync(file);
        if (bytes.length > MAX_SOURCE_BYTES) {
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
        const overlayPath = options.virtualOverlayRoot
          ? overlayPathFor(options.virtualOverlayRoot, relativePath)
          : file;
        const uri = fileUri(overlayPath);
        client.notify('textDocument/didOpen', {
          textDocument: { uri, languageId: 'eu4', version: 1, text: decoded.text },
        });
        pending.set(uri, { file, relativePath, decoded });
      } catch (error) {
        addToolError(report, `${relativePath}: ${error.message}`);
      }
    }
  }

  openUntilFull();
  while (pending.size) {
    let diagnosticMessage;
    try {
      diagnosticMessage = await client.waitFor(
        (message) =>
          message.method === 'textDocument/publishDiagnostics' &&
          pending.has(message.params?.uri),
        options.fileTimeoutMs,
        'diagnostics for an explicit file batch',
      );
    } catch (error) {
      const stalled = [...pending.entries()];
      for (const [uri, value] of stalled) {
        client.notify('textDocument/didClose', { textDocument: { uri } });
        addToolError(report, `${value.relativePath}: ${error.message}`);
        console.error(`[timeout] ${value.relativePath}`);
      }
      pending.clear();
      openUntilFull();
      continue;
    }
    const uri = diagnosticMessage.params.uri;
    const current = pending.get(uri);
    pending.delete(uri);
    client.notify('textDocument/didClose', { textDocument: { uri } });
    addFileResult(
      report,
      current.file,
      current.decoded.text,
      current.decoded.encoding,
      diagnosticMessage.params?.diagnostics || [],
      options.source,
    );
    console.error(
      `[${report.summary.files_analyzed}/${totalFiles}] ${current.relativePath}: ${diagnosticMessage.params?.diagnostics?.length || 0} diagnostics`,
    );
    if (report.summary.files_analyzed % options.checkpointEvery === 0) {
      writeReports(report, resolve(options.output));
      console.error(`Checkpoint written after ${report.summary.files_analyzed} files`);
    }
    openUntilFull();
  }
}

export async function diagnoseTextFiles(client, report, options, files) {
  const requestBatchSize = Math.min(options.batchSize, 16);
  let cursor = 0;
  let completed = 0;
  let nextCheckpoint = options.checkpointEvery;

  async function worker() {
    while (true) {
      const offset = cursor;
      cursor += requestBatchSize;
      if (offset >= files.length) return;
      const batch = files.slice(offset, offset + requestBatchSize);
      const inputs = [];
      const sources = new Map();
      for (const file of batch) {
        const relativePath = relative(options.source, file).split(sep).join('/');
        try {
          const bytes = readFileSync(file);
          if (bytes.length > MAX_SOURCE_BYTES) {
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
          inputs.push({ path: relativePath, text: decoded.text });
          sources.set(relativePath, { file, decoded });
        } catch (error) {
          addToolError(report, `${relativePath}: ${error.message}`);
        }
      }

      if (inputs.length) {
        try {
          const results = await client.request(
            'pdc/textDiagnostics',
            { files: inputs },
            Math.max(options.fileTimeoutMs, options.fileTimeoutMs * inputs.length),
          );
          if (!Array.isArray(results) || results.length !== inputs.length) {
            throw new LspProtocolError('pdc/textDiagnostics returned an invalid batch');
          }
          const returned = new Set();
          for (const result of results) {
            if (
              typeof result?.path !== 'string' ||
              !Array.isArray(result.diagnostics) ||
              !sources.has(result.path) ||
              returned.has(result.path)
            ) {
              throw new LspProtocolError('pdc/textDiagnostics returned an invalid file result');
            }
            returned.add(result.path);
            const source = sources.get(result.path);
            addFileResult(
              report,
              source.file,
              source.decoded.text,
              source.decoded.encoding,
              result.diagnostics,
              options.source,
            );
            console.error(
              `[${report.summary.files_analyzed}/${files.length}] ${result.path}: ${result.diagnostics.length} diagnostics`,
            );
          }
        } catch (error) {
          for (const input of inputs) {
            addToolError(report, `${input.path}: ${error.message}`);
            console.error(`[batch failed] ${input.path}: ${error.message}`);
          }
        }
      }

      completed += batch.length;
      if (completed >= nextCheckpoint || completed === files.length) {
        writeReports(report, resolve(options.output));
        console.error(`Checkpoint written after ${completed}/${files.length} attempted files`);
        while (nextCheckpoint <= completed) nextCheckpoint += options.checkpointEvery;
      }
    }
  }

  const workerCount = Math.min(options.concurrency, Math.ceil(files.length / requestBatchSize));
  console.error(`Running ${workerCount} concurrent text diagnostic worker(s)`);
  await Promise.all(Array.from({ length: workerCount }, () => worker()));
}

export async function stopClient(client, timeoutMs) {
  if (!client) return;
  if (!client.closed) {
    try {
      await client.request('shutdown', null, Math.min(timeoutMs, 10_000));
      client.notify('exit');
    } catch {
      client.child.kill();
    }
  }
  if (!client.closed) {
    await new Promise((resolveExit) => {
      const timer = setTimeout(() => {
        client.child.kill();
        resolveExit();
      }, 2_000);
      client.child.once('close', () => {
        clearTimeout(timer);
        resolveExit();
      });
    });
  }
}
