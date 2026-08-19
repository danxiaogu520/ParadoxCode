const assert = require('node:assert/strict');
const vscode = require('vscode');

suite('ParadoxCode VS Code extension host', () => {
  test('activates and contributes the EU4 language', async () => {
    const extension = vscode.extensions.getExtension('paradoxcode.paradoxcode-vscode');
    assert.ok(extension, 'development extension must be discoverable');
    await extension.activate();
    assert.equal(extension.isActive, true);
    const eu4 = vscode.languages.getLanguages
      ? (await vscode.languages.getLanguages()).includes('eu4')
      : false;
    assert.equal(eu4, true, 'eu4 language contribution must be registered');
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
