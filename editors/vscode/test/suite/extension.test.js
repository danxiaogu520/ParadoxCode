const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const vscode = require('vscode');

suite('ParadoxCode VS Code extension host', () => {
  test('activates and contributes the EU4 and Localisation languages', async () => {
    const extension = vscode.extensions.getExtension('paradoxcode.paradoxcode-vscode');
    assert.ok(extension, 'development extension must be discoverable');
    await extension.activate();
    assert.equal(extension.isActive, true);
    const languages = vscode.languages.getLanguages
      ? await vscode.languages.getLanguages()
      : [];
    const eu4 = languages.includes('eu4');
    const localisation = languages.includes('localisation');
    assert.equal(eu4, true, 'eu4 language contribution must be registered');
    assert.equal(localisation, true, 'localisation language contribution must be registered');
  });

  test('assigns nested localisation files to the Localisation language', async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'paradoxcode-localisation-'));
    const file = path.join(root, 'localisation', 'nested', 'test.yml');
    const scriptFile = path.join(root, 'map', 'area.txt');
    const nestedMapScriptFile = path.join(root, 'map', 'lakes', '00_lakes.txt');
    const unknownMapFile = path.join(root, 'map', 'nested', 'test.txt');
    const commonScriptFile = path.join(root, 'common', 'ai_army', 'test.txt');
    const commonBareFile = path.join(root, 'common', 'technology.txt');
    const unknownCommonBareFile = path.join(root, 'common', 'unknown.txt');
    const nestedCommonFile = path.join(root, 'common', 'ai_army', 'nested', 'test.txt');
    const unlistedCommonFile = path.join(root, 'common', 'not_a_script_folder', 'test.txt');
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.mkdirSync(path.dirname(scriptFile), { recursive: true });
    fs.mkdirSync(path.dirname(nestedMapScriptFile), { recursive: true });
    fs.mkdirSync(path.dirname(unknownMapFile), { recursive: true });
    fs.mkdirSync(path.dirname(commonScriptFile), { recursive: true });
    fs.mkdirSync(path.dirname(commonBareFile), { recursive: true });
    fs.mkdirSync(path.dirname(nestedCommonFile), { recursive: true });
    fs.mkdirSync(path.dirname(unlistedCommonFile), { recursive: true });
    fs.writeFileSync(file, 'l_english:\n  test_key:0 "Test"\n', 'utf8');
    fs.writeFileSync(scriptFile, 'test = { }\n', 'utf8');
    fs.writeFileSync(nestedMapScriptFile, 'test = { }\n', 'utf8');
    fs.writeFileSync(unknownMapFile, 'test = { }\n', 'utf8');
    fs.writeFileSync(commonScriptFile, 'test = { }\n', 'utf8');
    fs.writeFileSync(commonBareFile, 'test = { }\n', 'utf8');
    fs.writeFileSync(unknownCommonBareFile, 'test = { }\n', 'utf8');
    fs.writeFileSync(nestedCommonFile, 'test = { }\n', 'utf8');
    fs.writeFileSync(unlistedCommonFile, 'test = { }\n', 'utf8');
    try {
      const document = await vscode.workspace.openTextDocument(vscode.Uri.file(file));
      assert.equal(document.languageId, 'localisation');
      const scriptDocument = await vscode.workspace.openTextDocument(vscode.Uri.file(scriptFile));
      assert.equal(scriptDocument.languageId, 'eu4');
      const nestedMapDocument = await vscode.workspace.openTextDocument(
        vscode.Uri.file(nestedMapScriptFile),
      );
      assert.equal(nestedMapDocument.languageId, 'eu4');
      const unknownMapDocument = await vscode.workspace.openTextDocument(vscode.Uri.file(unknownMapFile));
      assert.notEqual(unknownMapDocument.languageId, 'eu4');
      const commonScriptDocument = await vscode.workspace.openTextDocument(vscode.Uri.file(commonScriptFile));
      assert.equal(commonScriptDocument.languageId, 'eu4');
      const commonBareDocument = await vscode.workspace.openTextDocument(vscode.Uri.file(commonBareFile));
      assert.equal(commonBareDocument.languageId, 'eu4');
      const unknownCommonBareDocument = await vscode.workspace.openTextDocument(
        vscode.Uri.file(unknownCommonBareFile),
      );
      assert.notEqual(unknownCommonBareDocument.languageId, 'eu4');
      const nestedCommonDocument = await vscode.workspace.openTextDocument(vscode.Uri.file(nestedCommonFile));
      assert.notEqual(nestedCommonDocument.languageId, 'eu4');
      const unlistedCommonDocument = await vscode.workspace.openTextDocument(vscode.Uri.file(unlistedCommonFile));
      assert.notEqual(unlistedCommonDocument.languageId, 'eu4');
    } finally {
      fs.rmSync(root, { recursive: true, force: true });
    }
  });

  test('exposes the user-facing commands and settings', () => {
    const commands = vscode.commands.getCommands(true);
    return commands.then((available) => {
      for (const command of [
        'paradoxcode.showMissionPreview',
        'paradoxcode.installServer',
        'paradoxcode.selectServer',
        'paradoxcode.selectGameDirectory',
        'paradoxcode.addDependency',
        'paradoxcode.removeDependency',
        'paradoxcode.openDependencySettings',
        'paradoxcode.updateIndexCaches',
        'paradoxcode.reloadServer',
        'paradoxcode.openOutput',
        'paradoxcode.localisation.decodeFile',
        'paradoxcode.localisation.encodeFile',
      ]) {
        assert.ok(available.includes(command), `missing command ${command}`);
      }
      const config = vscode.workspace.getConfiguration('paradoxcode');
      assert.equal(typeof config.get('diagnosticIgnoreCodes'), 'object');
      assert.equal(typeof config.get('preview.zoomSensitivity'), 'number');
      assert.equal(config.get('workspaceWideDiagnostics'), false);
      assert.equal(config.get('backgroundReindexIntervalMinutes'), 0);
      assert.equal(config.get('backgroundReindexIdleSeconds'), 15);
      assert.deepEqual(config.get('ignoreFilePatterns'), []);
      assert.deepEqual(config.get('ignoreDirectories'), []);
      assert.equal(config.get('server.installPolicy'), 'auto');
      assert.equal(config.inspect('vanilla.mode').defaultValue, 'auto');
      assert.equal(config.get('preview.refreshMode'), 'always');
      assert.equal(config.get('preview.showExternalPrerequisites'), true);
      assert.equal(config.get('preview.showDiagnostics'), true);
      assert.equal(config.get('preview.gameFonts'), true);
      assert.equal(config.get('preview.chineseFontMod'), '');
      assert.equal(config.get('hover.texturePreview'), true);
      assert.equal(config.get('hover.missionCard'), true);
      assert.equal(config.get('hover.eventCard'), true);
      assert.deepEqual(config.get('diagnostics.severityOverrides'), {});
      assert.deepEqual(config.get('localisation.preferredLanguages'), []);
      assert.equal(config.get('localisation.encodingMode'), 'automatic');
      assert.ok(available.includes('paradoxcode.localisation.openDecoded'));
      assert.deepEqual(config.get('completion.sourceLayers'), [
        'project',
        'dependencies',
        'vanilla',
      ]);
      assert.equal(config.get('performance.profile'), 'balanced');
    });
  });

  test('marks assignment and empty-block completions for a follow-up retrigger', () => {
    const {
      attachFollowupCompletionTrigger,
      shouldTriggerFollowupCompletion,
      FOLLOWUP_COMPLETION_TRIGGER_COMMAND,
    } = require('../../out/completionMiddleware.js');
    const item = { insertText: 'national_focus = ' };
    assert.equal(shouldTriggerFollowupCompletion(item), true);
    attachFollowupCompletionTrigger(item, 'file:///tmp/events/test.txt');
    assert.deepEqual(item.command, {
      title: 'Trigger follow-up completion',
      command: FOLLOWUP_COMPLETION_TRIGGER_COMMAND,
      arguments: [{ uri: 'file:///tmp/events/test.txt' }],
    });

    const block = { insertText: { value: 'country_event = {\n\t$0\n}' } };
    assert.equal(shouldTriggerFollowupCompletion(block), true);
    attachFollowupCompletionTrigger(block);
    assert.equal(block.command.command, FOLLOWUP_COMPLETION_TRIGGER_COMMAND);

    const parameterSnippet = { insertText: { value: 'apply = {\n\tamount = $1\n\t$0\n}' } };
    assert.equal(shouldTriggerFollowupCompletion(parameterSnippet), false);
    attachFollowupCompletionTrigger(parameterSnippet);
    assert.equal(parameterSnippet.command, undefined);

    const existing = { insertText: 'foo = ', command: { title: 'keep', command: 'keep' } };
    attachFollowupCompletionTrigger(existing);
    assert.deepEqual(existing.command, { title: 'keep', command: 'keep' });
  });

  test('contributes the detailed Getting Started walkthrough', async () => {
    const extension = vscode.extensions.getExtension('paradoxcode.paradoxcode-vscode');
    assert.ok(extension, 'development extension must be discoverable');
    const walkthrough = extension.packageJSON.contributes?.walkthroughs?.find(
      (entry) => entry.id === 'paradoxcode.gettingStarted',
    );
    assert.ok(walkthrough, 'Getting Started walkthrough must be contributed');
    assert.equal(walkthrough.steps.length, 6);
    assert.equal(walkthrough.steps[3].id, 'vanillaData');
    assert.deepEqual(walkthrough.steps[3].completionEvents, ['onContext:paradoxcodeVanillaReady']);
  });

  test('server accessor reports an unavailable client', async () => {
    const { acquireServerClient, serverExecutable, setServerClient } = require('../../out/serverClient.js');
    const previous = await acquireServerClient(5_000).catch(() => undefined);
    const executable = serverExecutable();
    setServerClient(undefined);
    try { await assert.rejects(
      acquireServerClient(10),
      (error) => error instanceof Error && error.name === 'ServerUnavailableError'
        && /not running/.test(error.message),
    ); } finally { setServerClient(previous, executable); }
  });

  test('default mission files keep normal views and decoded-path helpers remain compatible', async () => {
    const root = process.env.PDCLOC_HOST_WORKSPACE;
    assert.ok(root, 'the host runner must open the fixture workspace (npm run test:host)');
    const file = path.join(root, 'missions', 'EDG_FDMMissions.txt');
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, 'missions = {}\n', 'utf8');
    try {
      const extension = vscode.extensions.getExtension('paradoxcode.paradoxcode-vscode');
      assert.ok(extension, 'development extension must be discoverable');
      await extension.activate();
      // The bare-path launch arg opens the folder asynchronously; wait for it
      // so the eligibility check (getWorkspaceFolder) cannot race window startup.
      const fixtureUri = vscode.Uri.file(file);
      const deadline = Date.now() + 10_000;
      while (Date.now() < deadline && !vscode.workspace.getWorkspaceFolder(fixtureUri)) {
        await new Promise((resolve) => setTimeout(resolve, 250));
      }
      assert.ok(
        vscode.workspace.getWorkspaceFolder(fixtureUri),
        'the fixture workspace folder must be open before the pdcloc eligibility check',
      );
      const decoded = vscode.Uri.file(file).with({ scheme: 'pdcloc' });
      const document = await vscode.workspace.openTextDocument(fixtureUri);
      assert.equal(
        document.languageId,
        'eu4',
        'ordinary mission files must keep the EU4 language',
      );
      assert.equal(
        document.uri.scheme,
        'file',
        'the default workflow must use the ordinary file URI',
      );
      const { logicalPath } = require('../../out/previewPanel.js');
      assert.equal(
        logicalPath({ uri: decoded }),
        'missions/EDG_FDMMissions.txt',
        'decoded mission views must resolve to the workspace-relative logical path',
      );
      const { relativeDiagnosticPath } = require('../../out/extension.js');
      assert.equal(
        relativeDiagnosticPath(decoded),
        'missions/EDG_FDMMissions.txt',
        'diagnostic ignore patterns must match decoded views by logical path',
      );
    } finally {
      fs.rmSync(path.join(root, 'missions'), { recursive: true, force: true });
    }
  });
  test('YML modes switch live and preserve game bytes across automatic and manual saves', async function () {
    if (!process.env.PDC_COMPLETION_SERVER) this.skip();
    const folder = path.join(process.env.PDCLOC_HOST_WORKSPACE, 'localisation', 'replace');
    fs.mkdirSync(folder, { recursive: true });
    const file = path.join(folder, 'mode_l_english.yml');
    const plain = 'l_english:\r\n key:0 "';
    const original = Buffer.from(plain + '"\r\n');
    const game = Buffer.from('\uFEFF' + plain + '\x10-N\x10ýV"\r\n');
    fs.writeFileSync(file, original);
    const uri = vscode.Uri.file(file);
    const config = vscode.workspace.getConfiguration('paradoxcode.localisation');
    try {
      await vscode.window.showTextDocument(await vscode.workspace.openTextDocument(uri));
      const until = Date.now() + 10_000;
      while (vscode.window.activeTextEditor?.document.uri.scheme !== 'pdcloc' && Date.now() < until) {
        await new Promise(resolve => setTimeout(resolve, 50));
      }
      const editor = vscode.window.activeTextEditor;
      assert.equal(editor.document.uri.scheme, 'pdcloc');
      assert.deepEqual(fs.readFileSync(file), original);
      assert.ok(await editor.edit(edit => edit.insert(editor.document.positionAt(plain.length), '中国')));
      assert.deepEqual(fs.readFileSync(file), original);
      assert.ok(await editor.document.save());
      assert.deepEqual(fs.readFileSync(file), game);
      const script = path.join(process.env.PDCLOC_HOST_WORKSPACE, 'ordinary.txt');
      fs.writeFileSync(script, '# script');
      const txt = await vscode.window.showTextDocument(vscode.Uri.file(script));
      assert.ok(await txt.edit(edit => edit.insert(new vscode.Position(0, 8), ' 中文')));
      await new Promise(resolve => setTimeout(resolve, 900));
      assert.equal(vscode.window.activeTextEditor.document.uri.scheme, 'file');
      assert.equal(txt.document.isDirty, true);
      assert.equal(fs.readFileSync(script, 'utf8'), '# script');
      assert.ok(await txt.document.save());
      assert.equal(fs.readFileSync(script, 'utf8'), '# script 中文');
      await config.update('encodingMode', 'manual', vscode.ConfigurationTarget.Workspace);
      await vscode.commands.executeCommand('paradoxcode.localisation.decodeFile', uri);
      const raw = vscode.window.activeTextEditor;
      const readable = plain + '中国"\r\n';
      assert.equal(raw.document.uri.toString(), uri.toString());
      assert.equal(raw.document.getText(), readable);
      assert.ok(raw.document.isDirty);
      assert.deepEqual(fs.readFileSync(file), game);
      assert.equal(fs.existsSync(file + '.pre-transcode.bak'), false);
      await vscode.commands.executeCommand('undo');
      assert.equal(raw.document.getText(), game.toString().slice(1));
      await vscode.commands.executeCommand('redo');
      assert.equal(raw.document.getText(), readable);
      assert.ok(await raw.document.save());
      const savedReadable = Buffer.from('\uFEFF' + readable);
      assert.deepEqual(fs.readFileSync(file), savedReadable);
      assert.ok(await raw.edit(edit => edit.insert(raw.document.positionAt(readable.length), '# 中')));
      await vscode.commands.executeCommand('paradoxcode.localisation.encodeFile', uri);
      assert.equal(raw.document.getText(), game.toString().slice(1) + '# \x10-N');
      assert.deepEqual(fs.readFileSync(file), savedReadable);
      assert.ok(await raw.document.save());
      assert.deepEqual(fs.readFileSync(file), Buffer.concat([game, Buffer.from('# \x10-N')]));
      assert.equal(fs.existsSync(file + '.pre-transcode.bak'), false);

      // A plain UTF-8 buffer supplies the game BOM as part of its undoable edit.
      const noBom = path.join(folder, 'utf8_l_english.yml');
      fs.writeFileSync(noBom, readable);
      await vscode.commands.executeCommand('paradoxcode.localisation.encodeFile', vscode.Uri.file(noBom));
      const encodedDoc = vscode.window.activeTextEditor.document;
      assert.ok(encodedDoc.isDirty);
      assert.equal(fs.readFileSync(noBom, 'utf8'), readable);
      assert.ok(await encodedDoc.save());
      assert.deepEqual(fs.readFileSync(noBom), game);
      assert.equal(fs.existsSync(noBom + '.pre-transcode.bak'), false);
    } finally {
      await config.update('encodingMode', undefined, vscode.ConfigurationTarget.Workspace);
    }
  });

  test('TXT context commands save exact game bytes and recover readable UTF-8', async function () {
    if (!process.env.PDC_COMPLETION_SERVER) this.skip();
    const file = path.join(process.env.PDCLOC_HOST_WORKSPACE, 'selected.txt');
    const uri = vscode.Uri.file(file);
    const prefix = '# café 😀\r\nname = "';
    const suffix = '" # untouched 中文\r\n';
    const readable = Buffer.from(prefix + '中国' + suffix);
    const game = Buffer.concat([Buffer.from(prefix), Buffer.from([0x10, 0x2d, 0x4e, 0x10, 0xfd, 0x56]), Buffer.from(suffix)]);
    fs.writeFileSync(file, readable);
    const edit = await vscode.window.showTextDocument(await vscode.workspace.openTextDocument(uri, { encoding: 'utf8' }));
    const start = edit.document.positionAt(prefix.length);
    edit.selection = new vscode.Selection(start, edit.document.positionAt(prefix.length + 2));
    await vscode.commands.executeCommand('paradoxcode.localisation.encodeSelection');
    const encoded = vscode.window.activeTextEditor.document;
    assert.ok(encoded.isDirty, 'a clean source converts as an undoable unsaved edit');
    assert.deepEqual(fs.readFileSync(file), readable);
    await vscode.commands.executeCommand('undo');
    assert.deepEqual(fs.readFileSync(file), readable);
    await vscode.commands.executeCommand('redo');
    assert.ok(await vscode.window.activeTextEditor.document.save());
    assert.deepEqual(fs.readFileSync(file), game);

    const byteEditor = vscode.window.activeTextEditor;
    const rawStart = byteEditor.document.positionAt(Buffer.byteLength(prefix));
    byteEditor.selection = new vscode.Selection(rawStart, rawStart.translate(0, 6));
    await vscode.commands.executeCommand('paradoxcode.localisation.decodeSelection');
    const decoded = vscode.window.activeTextEditor.document;
    assert.equal(decoded.getText(), readable.toString());
    assert.ok(await decoded.save());
    assert.deepEqual(fs.readFileSync(file), readable);

    // Reopen the game bytes in an ordinary UTF-8 editor. U+FFFD on screen must not
    // replace 0xFD in the conversion input. All malformed bytes are inside the selection.
    const rawCopy = path.join(process.env.PDCLOC_HOST_WORKSPACE, 'selected-raw.txt');
    fs.writeFileSync(rawCopy, game);
    const doc = await vscode.workspace.openTextDocument(vscode.Uri.file(rawCopy), { encoding: 'utf8' });
    const editor = await vscode.window.showTextDocument(doc);
    editor.selection = new vscode.Selection(start, doc.positionAt(prefix.length + 6));
    await vscode.commands.executeCommand('paradoxcode.localisation.decodeSelection');
    assert.equal(doc.getText(), readable.toString());
    assert.ok(await doc.save());
    assert.deepEqual(fs.readFileSync(rawCopy), readable);
  });

  test('replays completion through the editor after Unicode and CRLF edits', async function () {
    this.timeout(120_000);
    const server = process.env.PDC_COMPLETION_SERVER;
    if (!server) { this.skip(); return; }
    const root = process.env.PDCLOC_HOST_WORKSPACE;
    const samples = [];
    try {
      const { acquireServerClient } = require('../../out/serverClient.js');
      await acquireServerClient(30_000);
      const definition = path.join(root, 'common', 'scripted_effects', 'completion_host.txt');
      const file = path.join(root, 'events', 'completion_host.txt');
      fs.mkdirSync(path.dirname(definition), { recursive: true });
      fs.mkdirSync(path.dirname(file), { recursive: true });
      fs.writeFileSync(definition, 'host_embed = { $BODY$ }\n');
      const source = '# 中文😀\r\ncountry_event = { immediate = { host_embed = { BODY = "add_pre" } } }\r\n'
        + 'country_event = { immediate = { add_prestige = 1 } }\r\n'.repeat(2000);
      fs.writeFileSync(file, source);
      const definitionDocument = await vscode.workspace.openTextDocument(definition);
      await vscode.window.showTextDocument(definitionDocument);
      const document = await vscode.workspace.openTextDocument(file);
      await vscode.window.showTextDocument(document);
      const offset = source.indexOf('add_pre');
      let width = 'add_pre'.length;
      const request = async () => vscode.commands.executeCommand(
        'vscode.executeCompletionItemProvider', document.uri, document.positionAt(offset + width),
      );
      const deadline = Date.now() + 30_000;
      let initial;
      do {
        initial = await request();
        if (initial?.items.some((item) => (typeof item.label === 'string' ? item.label : item.label.label) === 'add_prestige')) break;
        await new Promise((resolve) => setTimeout(resolve, 100));
      } while (Date.now() < deadline);
      assert.ok(initial?.items.some((item) => (typeof item.label === 'string' ? item.label : item.label.label) === 'add_prestige'));
      for (let index = 0; index < 30; index++) {
        const prefix = 'add_prestige'.slice(0, 1 + index % 11);
        const edit = new vscode.WorkspaceEdit();
        edit.replace(document.uri, new vscode.Range(document.positionAt(offset), document.positionAt(offset + width)), prefix);
        const started = performance.now();
        assert.equal(await vscode.workspace.applyEdit(edit), true);
        width = prefix.length;
        const result = await request();
        const elapsedMs = performance.now() - started;
        assert.ok(result?.items.some((item) => (typeof item.label === 'string' ? item.label : item.label.label) === 'add_prestige'), `missing completion after edit ${index}`);
        samples.push({ prefix, elapsedMs, items: result.items.length, version: document.version });
      }
      if (process.env.PDC_COMPLETION_REPORT) {
        fs.mkdirSync(path.dirname(process.env.PDC_COMPLETION_REPORT), { recursive: true });
        fs.writeFileSync(process.env.PDC_COMPLETION_REPORT, JSON.stringify({ samples, platform: process.platform, vscode: vscode.version, limits: 'Editor command through middleware and LSP; excludes suggest-widget painting.' }, null, 2));
      }
    } finally {
      fs.rmSync(path.join(root, 'events', 'completion_host.txt'), { force: true });
      fs.rmSync(path.join(root, 'common', 'scripted_effects', 'completion_host.txt'), { force: true });
    }
  });

});
