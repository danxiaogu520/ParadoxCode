import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

const root = new URL('..', import.meta.url).pathname.replace(/^\/(\w):/, '$1:');
const readJson = (relative) => JSON.parse(readFileSync(join(root, relative), 'utf8'));
const fail = (message) => {
  throw new Error(`extension contract: ${message}`);
};

const manifest = readJson('package.json');
const nls = readJson('package.nls.json');
const zh = readJson('package.nls.zh-cn.json');
const grammar = readJson('syntaxes/eu4.tmLanguage.json');

if (!manifest.files?.includes('node_modules/**')) {
  fail('production node_modules must be included in the VSIX file allowlist');
}
for (const dependency of ['smol-toml', 'vscode-languageclient']) {
  if (typeof manifest.dependencies?.[dependency] !== 'string') {
    fail(`runtime dependency ${dependency} must remain in dependencies`);
  }
}

if (manifest.contributes.languages?.length !== 1 || manifest.contributes.languages[0].id !== 'eu4') {
  fail('the explicit VS Code language scope must remain EU4 only');
}
const eu4Patterns = manifest.contributes.languages[0].filenamePatterns ?? [];
if (eu4Patterns.some((pattern) => /\.ya?ml|\.asset|\.sfx/i.test(pattern))) {
  fail('P0-4/P0-5 exclusions must not be reintroduced into the language contribution');
}
if (manifest.activationEvents?.includes('onStartupFinished')) {
  fail('the extension must not activate and download pdx-ls in unrelated workspaces');
}
if (!manifest.activationEvents?.includes('onLanguage:eu4')) {
  fail('opening an EU4 document must activate the zero-configuration startup path');
}
if (manifest.capabilities?.untrustedWorkspaces?.supported !== false) {
  fail('automatic server download and execution must be disabled in untrusted workspaces');
}
for (const setting of ['paradoxcode.serverVersion', 'paradoxcode.serverRepository']) {
  if (Object.hasOwn(manifest.contributes.configuration?.properties ?? {}, setting)) {
    fail(`${setting} must not let a workspace redirect automatic executable downloads`);
  }
}
for (const command of [
  'paradoxcode.showMissionPreview',
  'paradoxcode.installServer',
  'paradoxcode.selectServer',
  'paradoxcode.selectGameDirectory',
  'paradoxcode.reloadServer',
  'paradoxcode.openOutput',
  'paradoxcode.exportDiagnostics',
]) {
  if (!manifest.contributes.commands.some((entry) => entry.command === command)) {
    fail(`missing command ${command}`);
  }
}
if (!manifest.contributes.views?.explorer?.some((entry) => entry.id === 'paradoxcode.loadedFiles')) {
  fail('loaded files Explorer view is missing');
}
const walkthrough = manifest.contributes.walkthroughs?.find((entry) => entry.id === 'paradoxcode.gettingStarted');
if (!walkthrough || !Array.isArray(walkthrough.steps) || walkthrough.steps.length < 6) {
  fail('the detailed Getting Started walkthrough is missing or incomplete');
}
if (walkthrough.steps.some((step) => step.media?.markdown !== 'media/getting-started.md')) {
  fail('every Getting Started step must reference the packaged onboarding media');
}
if (!manifest.contributes.grammars?.some((entry) => entry.path === './syntaxes/eu4.tmLanguage.json')) {
  fail('EU4 TextMate fallback grammar is missing');
}
if (manifest.contributes.configurationDefaults?.['[eu4]']?.['editor.semanticHighlighting.enabled'] !== true) {
  fail('semantic highlighting must remain enabled for EU4');
}
for (const key of [
  'extension.displayName',
  'walkthrough.gettingStarted.title',
  'walkthrough.vanillaData.description',
  'commands.showMissionPreview.title',
  'configuration.pdxLsPath.description',
]) {
  if (typeof nls[key] !== 'string' || typeof zh[key] !== 'string') {
    fail(`English and Chinese NLS entries are required for ${key}`);
  }
}
if (grammar.scopeName !== 'source.eu4' || !Array.isArray(grammar.patterns) || grammar.patterns.length < 5) {
  fail('fallback grammar has an unexpected shape');
}

const previewSource = readFileSync(join(root, 'src', 'previewPanel.ts'), 'utf8');
for (const marker of ['show(extensionUri: vscode.Uri, client?: LanguageClient)', 'version: documentVersion', 'sourceRange', 'requestSequence']) {
  if (!previewSource.includes(marker)) {
    fail(`Preview reliability marker missing: ${marker}`);
  }
}
const extensionSource = readFileSync(join(root, 'src', 'extension.ts'), 'utf8');
for (const marker of [
  'context.extension.packageJSON.version',
  'context.extensionMode !== vscode.ExtensionMode.Production',
  'installPdxLs(context, options, progress)',
  'automatic checksum-verified ParadoxCode installation',
  'paradoxcodeVanillaReady',
]) {
  if (!extensionSource.includes(marker)) {
    fail(`Automatic server setup marker missing: ${marker}`);
  }
}
const rendererSource = readFileSync(join(root, 'media', 'renderer.js'), 'utf8');
for (const marker of ['exportPng', 'exportSvg', 'exportJson', 'addEventListener(\'keydown\'', 'renderNodeList', 'readColors']) {
  if (!rendererSource.includes(marker)) {
    fail(`Preview UX marker missing: ${marker}`);
  }
}
for (const relative of ['README.md', 'package.nls.json', 'package.nls.zh-cn.json', 'media/getting-started.md', 'LICENSE']) {
  if (!existsSync(join(root, relative))) {
    fail(`packaged asset missing: ${relative}`);
  }
}

console.log('extension contract OK');
