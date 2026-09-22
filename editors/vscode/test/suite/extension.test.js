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
      assert.equal(config.get('vanilla.mode'), 'auto');
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
      assert.equal(config.get('localisation.transparentEncoding'), true);
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

  test('agent budget helpers bound lists and text', () => {
    const { capList, capText, collapseWhitespace } = require('../../out/agent/budget.js');
    assert.deepEqual(capList([1, 2, 3], 5), { items: [1, 2, 3], omitted: 0 });
    assert.deepEqual(capList([1, 2, 3, 4], 2), { items: [1, 2], omitted: 2 });
    assert.equal(capText('short', 10), 'short');
    const capped = capText('x'.repeat(50), 10);
    assert.ok(capped.startsWith('x'.repeat(10)));
    assert.ok(capped.includes('(+40 more characters)'));
    assert.equal(collapseWhitespace(' a \n\t b  c '), 'a b c');
  });

  test('agent server accessor reports an unavailable client as a retryable error', async () => {
    const { acquireAgentClient, setAgentClient } = require('../../out/agent/server.js');
    setAgentClient(undefined);
    await assert.rejects(
      acquireAgentClient(10),
      (error) => error instanceof Error && error.name === 'AgentServerUnavailableError'
        && /not running/.test(error.message),
    );
  });

  test('agent tools register on hosts with the Language Model Tools API', () => {
    const { registerAgentTools } = require('../../out/agent/register.js');
    const disposables = registerAgentTools();
    if ('lm' in vscode && typeof vscode.lm?.registerTool === 'function') {
      assert.equal(disposables.length, 11, 'all eleven agent tools must register');
      for (const disposable of disposables) {
        disposable.dispose();
      }
    } else {
      assert.equal(disposables.length, 0);
    }
  });

  test('agent tools are prompt-referenceable and carry unique reference names', async () => {
    const extension = vscode.extensions.getExtension('paradoxcode.paradoxcode-vscode');
    const contributed = extension.packageJSON.contributes?.languageModelTools ?? [];
    assert.equal(contributed.length, 11, 'all eleven agent tools must be contributed');
    const referenceNames = contributed.map((tool) => tool.toolReferenceName);
    for (const tool of contributed) {
      assert.equal(tool.canBeReferencedInPrompt, true, `${tool.name} must be prompt-referenceable`);
      assert.match(tool.toolReferenceName, /^paradox[A-Z]/, `${tool.name} needs a paradox-prefixed reference name`);
    }
    assert.equal(new Set(referenceNames).size, referenceNames.length, 'reference names must be unique');
    const chatParticipants = extension.packageJSON.contributes?.chatParticipants ?? [];
    assert.equal(chatParticipants.length, 0, 'the @paradox chat participant must stay removed');
  });

  test('mission preview and diagnostic paths resolve pdcloc decoded views', async () => {
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
      const document = await vscode.workspace.openTextDocument(decoded);
      assert.equal(
        document.languageId,
        'eu4',
        'decoded mission views must keep the EU4 language (gates the preview context and edit refresh)',
      );
      assert.equal(
        document.uri.scheme,
        'pdcloc',
        'the missions file must open through its decoded twin',
      );
      const { logicalPath } = require('../../out/previewPanel.js');
      assert.equal(
        logicalPath(document),
        'missions/EDG_FDMMissions.txt',
        'decoded mission views must resolve to the workspace-relative logical path',
      );
      const { relativeDiagnosticPath } = require('../../out/extension.js');
      assert.equal(
        relativeDiagnosticPath(decoded),
        'missions/EDG_FDMMissions.txt',
        'diagnostic ignore patterns must match decoded views by logical path',
      );
      const { openDocumentUriFor } = require('../../out/agent/tools.js');
      assert.equal(
        openDocumentUriFor(vscode.Uri.file(file)).toString(),
        decoded.toString(),
        'agent position tools must target the open decoded twin (it carries the live text)',
      );
      const closedFile = path.join(root, 'missions', 'ClosedMissions.txt');
      fs.writeFileSync(closedFile, 'closed = {}\n', 'utf8');
      assert.equal(
        openDocumentUriFor(vscode.Uri.file(closedFile)).toString(),
        vscode.Uri.file(closedFile).toString(),
        'closed files must fall back to their on-disk file URI',
      );
    } finally {
      fs.rmSync(path.join(root, 'missions'), { recursive: true, force: true });
    }
  });
});
