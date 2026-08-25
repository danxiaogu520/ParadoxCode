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
        'paradoxcode.reloadServer',
        'paradoxcode.openOutput',
      ]) {
        assert.ok(available.includes(command), `missing command ${command}`);
      }
      const config = vscode.workspace.getConfiguration('paradoxcode');
      assert.equal(typeof config.get('diagnosticIgnoreCodes'), 'object');
      assert.equal(typeof config.get('preview.zoomSensitivity'), 'number');
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
});
