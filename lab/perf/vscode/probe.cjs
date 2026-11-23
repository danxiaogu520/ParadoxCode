const fs = require('node:fs');
const path = require('node:path');
const { performance } = require('node:perf_hooks');
const crypto = require('node:crypto');
const vscode = require('vscode');

exports.run = async function () {
  const config = JSON.parse(fs.readFileSync(process.env.PDC_PROBE_CONFIG, 'utf8').replace(/^\uFEFF/, ''));
  const reportPath = process.env.PDC_PROBE_REPORT;
  const responsesPath = reportPath.replace(/\.json$/, '.responses.json');
  const responses = { raw: [], provider: [] };
  const pendingRaw = [];
  const canonical = value => Array.isArray(value) ? value.map(canonical)
    : value && typeof value === 'object' ? Object.fromEntries(Object.keys(value).sort().map(key => [key, canonical(value[key])])) : value;
  const normalized = value => {
    const copy = JSON.parse(JSON.stringify(value));
    for (const item of Array.isArray(copy) ? copy : copy?.items ?? [])
      if (item.data?.template) delete item.data.template.revision;
    return canonical(copy);
  };
  const digest = value => crypto.createHash('sha256').update(JSON.stringify(value)).digest('hex');
  const start = performance.now();
  const report = { status: 'running', vscode: vscode.version, platform: process.platform,
    readinessMeasurement: 'indexed-file-count-v2',
    server: process.env.PDC_PROBE_SERVER, serverSha256: crypto.createHash('sha256').update(fs.readFileSync(process.env.PDC_PROBE_SERVER)).digest('hex'),
    configuration: config, samples: [], requests: [], requestStarts: [], decisions: [], resultDigests: [], sourceSha256: {} };
  const save = () => {
    for (const { phase, value } of pendingRaw.splice(0)) {
      const result = normalized(value);
      responses.raw.push({ phase, result });
      report.resultDigests.push({ phase, kind: 'raw', sha256: digest(result), items: (Array.isArray(result) ? result : result?.items)?.length });
    }
    fs.writeFileSync(reportPath, JSON.stringify(report, null, 2));
    fs.writeFileSync(responsesPath, JSON.stringify(responses, null, 2));
  };
  const measured = async (phase, action) => {
    report.pending = { phase, atMs: performance.now() - start }; save();
    const started = performance.now();
    let timer;
    try {
      const value = await Promise.race([action(), new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error(`${phase}: 90 second timeout`)), 90000);
      })]);
      const items = Array.isArray(value) ? value : value?.items;
      const sample = { phase, elapsedMs: performance.now() - started,
        items: items?.length, atMs: performance.now() - start };
      report.samples.push(sample); delete report.pending; save();
      if (phase.includes(':provider-') || phase.includes(':edit-to-provider:')) {
        const result = normalized(value);
        responses.provider.push({ phase, result });
        report.resultDigests.push({ phase, kind: 'provider', sha256: digest(result), items: result?.items?.length });
        save();
      }
      console.log('PDC_PROBE', JSON.stringify(sample));
      return value;
    } finally { clearTimeout(timer); }
  };
  save();
  try {
    const ext = vscode.extensions.getExtension('paradoxcode.paradoxcode-vscode');
    if (!ext) throw new Error('Installed extension was not discovered');
    await measured('extension.activate', () => ext.activate());
    const existingClientModule = Object.values(require.cache).find(entry =>
      /[\\/]out[\\/]serverClient\.js$/i.test(entry.filename)
      && entry.filename.toLowerCase().startsWith(ext.extensionPath.toLowerCase()));
    report.extensionPath = ext.extensionPath;
    const { acquireServerClient } = existingClientModule?.exports
      ?? require(path.join(ext.extensionPath, 'out', 'serverClient.js'));
    const client = await measured('client.available', () => acquireServerClient(90000));
    client.onNotification('pdc/trace', params => {
      report.decisions.push({ atMs: performance.now() - start, ...params });
    });
    await client.sendNotification('$/setTrace', { value: 'verbose' });
    await measured('workspace.ready-and-summary', async () => {
      const expected = config.expectedIndexedFiles ?? 10000;
      for (let i = 0; i < 120; i++) {
        const summary = await client.sendRequest('pdc/workspaceSummary');
        report.workspaceSummary = summary;
        const files = summary.fileCounts?.total;
        if (config.expectedIndexedFiles === undefined ? files >= expected : files === expected) return summary;
        await new Promise(resolve => setTimeout(resolve, 500));
      }
      throw new Error(`Workspace readiness timed out: expected ${expected} files, got ${report.workspaceSummary?.fileCounts?.total}`);
    });
    save();
    const originalRequest = client.sendRequest.bind(client);
    client.sendRequest = function (...args) {
      const method = typeof args[0] === 'string' ? args[0] : args[0]?.method;
      if (method !== 'textDocument/completion') return originalRequest(...args);
      const at = performance.now();
      const phase = report.pending?.phase;
      report.requestStarts.push({ phase, atMs: at - start }); // Persist after the timed request, outside the measured path.
      return originalRequest(...args).then(value => {
        report.requests.push({ phase, elapsedMs: performance.now() - at,
          items: (Array.isArray(value) ? value : value?.items)?.length });
        pendingRaw.push({ phase, value });
        return value;
      });
    };
    let randomState = (config.seed ?? 1701) >>> 0;
    const random = () => { randomState = (Math.imul(randomState, 1664525) + 1013904223) >>> 0; return randomState / 4294967296; };
    for (const fixture of config.cases) {
      const file = path.join(config.workspace, fixture.path);
      report.sourceSha256[fixture.path] = crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
      let document;
      await measured(`${fixture.path}:open`, async () => {
        document = await vscode.workspace.openTextDocument(file);
        if (document.languageId !== 'eu4') document = await vscode.languages.setTextDocumentLanguage(document, 'eu4');
        await vscode.window.showTextDocument(document);
      });
      const source = document.getText();
      const offset = source.indexOf(fixture.needle);
      if (offset < 0) throw new Error(`Needle missing in ${fixture.path}: ${fixture.needle}`);
      let width = fixture.needle.length;
      report.editorSettings = {
        quickSuggestions: vscode.workspace.getConfiguration('editor', document).get('quickSuggestions'),
        quickSuggestionsDelay: vscode.workspace.getConfiguration('editor', document).get('quickSuggestionsDelay'),
        suggestOnTriggerCharacters: vscode.workspace.getConfiguration('editor', document).get('suggestOnTriggerCharacters'),
        language: document.languageId,
      }; save();
      const provider = () => vscode.commands.executeCommand('vscode.executeCompletionItemProvider',
        document.uri, document.positionAt(offset + width));
      const direct = () => client.sendRequest('textDocument/completion', {
        textDocument: { uri: document.uri.toString() }, position: document.positionAt(offset + width),
        context: { triggerKind: 1 },
      });
      await measured(`${fixture.path}:provider-first`, provider);
      await measured(`${fixture.path}:raw-stable`, direct);
      await measured(`${fixture.path}:provider-stable`, provider);
      const replace = async prefix => {
        const edit = new vscode.WorkspaceEdit();
        edit.replace(document.uri, new vscode.Range(document.positionAt(offset), document.positionAt(offset + width)), prefix);
        if (!await vscode.workspace.applyEdit(edit)) throw new Error('Workspace edit rejected');
        width = prefix.length;
      };
      const repetitions = config.editRepetitions ?? 1;
      const prefixes = [fixture.needle.slice(0, 4), '', fixture.needle.slice(0, 2)];
      for (let round = 0; round < repetitions; round++) {
        const order = [...prefixes];
        if (round > 0) {
          for (let i = order.length - 1; i > 0; i--) { const j = Math.floor(random() * (i + 1)); [order[i], order[j]] = [order[j], order[i]]; }
          if (order[0] === document.getText().slice(offset, offset + width)) order.push(order.shift());
        }
        const roundLabel = repetitions > 1 ? `:round-${round + 1}` : '';
        for (const prefix of order) {
        await measured(`${fixture.path}:edit-to-provider:${JSON.stringify(prefix)}${roundLabel}`, async () => {
          await replace(prefix); return provider();
        });
        await measured(`${fixture.path}:raw-after-edit:${JSON.stringify(prefix)}${roundLabel}`, direct);
        }
      }
      {
        const editor = await vscode.window.showTextDocument(document);
        editor.selection = new vscode.Selection(document.positionAt(offset), document.positionAt(offset + width));
        report.pending = { phase: `${fixture.path}:automatic-typing`, atMs: performance.now() - start };
        const count = report.requestStarts.length;
        const typed = performance.now();
        await vscode.commands.executeCommand('type', { text: fixture.needle.slice(0, 3) });
        width = 3;
        while (report.requestStarts.length === count && performance.now() - typed < 5000)
          await new Promise(resolve => setTimeout(resolve, 25));
        const firstRequestAfterMs = report.requestStarts[count]?.atMs - (typed - start);
        const automaticPhase = report.pending.phase;
        while (report.requestStarts.length > count
          && !report.requests.some(request => request.phase === automaticPhase)
          && performance.now() - typed < 90000)
          await new Promise(resolve => setTimeout(resolve, 25));
        const response = report.requests.find(request => request.phase === automaticPhase);
        report.automaticTyping ??= {};
        report.automaticTyping[fixture.path] = { requestObserved: report.requestStarts.length > count,
          firstRequestAfterMs, responseObserved: Boolean(response),
          completionAfterMs: response ? firstRequestAfterMs + response.elapsedMs : null,
          observedWindowMs: performance.now() - typed };
        await new Promise(resolve => setTimeout(resolve, 1500));
        delete report.pending; save();
      }
      await replace(fixture.needle);
      // Every edit remains an unsaved overlay; the original bytes are never written.
      await vscode.commands.executeCommand('workbench.action.revertAndCloseActiveEditor');
      report.sourceSha256After ??= {};
      report.sourceSha256After[fixture.path] = crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
      if (report.sourceSha256After[fixture.path] !== report.sourceSha256[fixture.path])
        throw new Error(`Source bytes changed on disk: ${fixture.path}`);
    }
    report.status = 'completed';
  } catch (error) {
    report.status = 'failed'; report.error = String(error?.stack ?? error); throw error;
  } finally { report.totalMs = performance.now() - start; save(); }
};
