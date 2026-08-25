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
const localisationGrammar = readJson('syntaxes/localisation.tmLanguage.json');
const localisationConfiguration = readJson('localisation-language-configuration.json');

if (!manifest.files?.includes('node_modules/**')) {
  fail('production node_modules must be included in the VSIX file allowlist');
}
for (const dependency of ['smol-toml', 'vscode-languageclient']) {
  if (typeof manifest.dependencies?.[dependency] !== 'string') {
    fail(`runtime dependency ${dependency} must remain in dependencies`);
  }
}

if (manifest.contributes.languages?.length !== 2) {
  fail('the explicit VS Code language scope must contain EU4 and Localisation');
}
const eu4Language = manifest.contributes.languages.find((entry) => entry.id === 'eu4');
const localisationLanguage = manifest.contributes.languages.find((entry) => entry.id === 'localisation');
if (!eu4Language || !localisationLanguage) {
  fail('both eu4 and localisation language contributions are required');
}
const eu4Patterns = eu4Language.filenamePatterns ?? [];
const expectedEu4Patterns = [
  '**/common/achievements.txt',
  '**/common/alerts.txt',
  '**/common/graphicalculturetype.txt',
  '**/common/historial_lucky.txt',
  '**/common/technology.txt',
  '**/common/advisortypes/*.txt',
  '**/common/ages/*.txt',
  '**/common/ai_army/*.txt',
  '**/common/ai_attitudes/*.txt',
  '**/common/ai_personalities/*.txt',
  '**/common/ancestor_personalities/*.txt',
  '**/common/bookmarks/*.txt',
  '**/common/buildings/*.txt',
  '**/common/cb_types/*.txt',
  '**/common/centers_of_trade/*.txt',
  '**/common/church_aspects/*.txt',
  '**/common/client_states/*.txt',
  '**/common/colonial_regions/*.txt',
  '**/common/countries/*.txt',
  '**/common/country_colors/*.txt',
  '**/common/country_tags/*.txt',
  '**/common/cultures/*.txt',
  '**/common/custom_country_colors/*.txt',
  '**/common/custom_gui/*.txt',
  '**/common/custom_ideas/*.txt',
  '**/common/decrees/*.txt',
  '**/common/defender_of_faith/*.txt',
  '**/common/defines/*.txt',
  '**/common/diplomatic_actions/*.txt',
  '**/common/disasters/*.txt',
  '**/common/dynasty_colors/*.txt',
  '**/common/estate_agendas/*.txt',
  '**/common/estate_crown_land/*.txt',
  '**/common/estate_privileges/*.txt',
  '**/common/estates/*.txt',
  '**/common/estates_preload/*.txt',
  '**/common/event_modifiers/*.txt',
  '**/common/factions/*.txt',
  '**/common/federation_advancements/*.txt',
  '**/common/fervor/*.txt',
  '**/common/fetishist_cults/*.txt',
  '**/common/flagship_modifications/*.txt',
  '**/common/golden_bulls/*.txt',
  '**/common/government_mechanics/*.txt',
  '**/common/government_names/*.txt',
  '**/common/government_ranks/*.txt',
  '**/common/government_reforms/*.txt',
  '**/common/governments/*.txt',
  '**/common/great_projects/*.txt',
  '**/common/hegemons/*.txt',
  '**/common/holy_orders/*.txt',
  '**/common/ideas/*.txt',
  '**/common/imperial_incidents/*.txt',
  '**/common/imperial_reforms/*.txt',
  '**/common/incidents/*.txt',
  '**/common/institutions/*.txt',
  '**/common/insults/*.txt',
  '**/common/isolationism/*.txt',
  '**/common/leader_personalities/*.txt',
  '**/common/mercenary_companies/*.txt',
  '**/common/natives/*.txt',
  '**/common/naval_doctrines/*.txt',
  '**/common/new_diplomatic_actions/*.txt',
  '**/common/on_actions/*.txt',
  '**/common/opinion_modifiers/*.txt',
  '**/common/parliament_bribes/*.txt',
  '**/common/parliament_issues/*.txt',
  '**/common/peace_treaties/*.txt',
  '**/common/personal_deities/*.txt',
  '**/common/policies/*.txt',
  '**/common/powerprojection/*.txt',
  '**/common/prices/*.txt',
  '**/common/professionalism/*.txt',
  '**/common/province_names/*.txt',
  '**/common/province_triggered_modifiers/*.txt',
  '**/common/rebel_types/*.txt',
  '**/common/region_colors/*.txt',
  '**/common/religions/*.txt',
  '**/common/religious_conversions/*.txt',
  '**/common/religious_reforms/*.txt',
  '**/common/revolt_triggers/*.txt',
  '**/common/revolution/*.txt',
  '**/common/ruler_personalities/*.txt',
  '**/common/scripted_effects/*.txt',
  '**/common/scripted_functions/*.txt',
  '**/common/scripted_triggers/*.txt',
  '**/common/state_edicts/*.txt',
  '**/common/static_modifiers/*.txt',
  '**/common/subject_type_upgrades/*.txt',
  '**/common/subject_types/*.txt',
  '**/common/technologies/*.txt',
  '**/common/timed_modifiers/*.txt',
  '**/common/trade_companies/*.txt',
  '**/common/tradecompany_investments/*.txt',
  '**/common/tradegoods/*.txt',
  '**/common/tradenodes/*.txt',
  '**/common/trading_policies/*.txt',
  '**/common/triggered_modifiers/*.txt',
  '**/common/units/*.txt',
  '**/common/units_display/*.txt',
  '**/common/wargoal_types/*.txt',
  '**/customizable_localization/*.txt',
  '**/decisions/*.txt',
  '**/events/*.txt',
  '**/hints/*.txt',
  '**/history/advisors/*.txt',
  '**/history/countries/*.txt',
  '**/history/diplomacy/*.txt',
  '**/history/provinces/*.txt',
  '**/history/wars/*.txt',
  '**/map/ambient_object.txt',
  '**/map/area.txt',
  '**/map/climate.txt',
  '**/map/continent.txt',
  '**/map/lakes/00_lakes.txt',
  '**/map/positions.txt',
  '**/map/provincegroup.txt',
  '**/map/random/RNWScenarios.txt',
  '**/map/random/RandomLakeNames.txt',
  '**/map/random/RandomLandNames.txt',
  '**/map/random/RandomSeaNames.txt',
  '**/map/region.txt',
  '**/map/seasons.txt',
  '**/map/superregion.txt',
  '**/map/terrain.txt',
  '**/map/trade_winds.txt',
  '**/music/*.txt',
  '**/missions/*.txt',
  '**/sound/*.txt',
  '**/sound/amb/*.txt',
  '**/sound/battle/*.txt',
  '**/sound/battle/naval/*.txt',
  '**/tutorial/*.txt',
  '**/gfx/*.txt',
  '**/gfx/combat_result/*.txt',
  '**/gfx/sprite_packs/*.txt',
  '**/gfx/sprite_packs_order/*.txt',
  '**/interface/*.txt',
  '**/interface/*.gui',
  '**/interface/*.gfx',
  '**/interface/assets/*.gfx',
  '**/interface/government_mechanics/*.txt',
  '**/interface/government_mechanics/*.gui',
  '**/interface/government_mechanics/*.gfx',
  '**/interface/state_view/*.txt',
];
const missingEu4Patterns = expectedEu4Patterns.filter((pattern) => !eu4Patterns.includes(pattern));
if (missingEu4Patterns.length > 0) {
  fail(`EU4 language contribution is missing profile directory patterns: ${missingEu4Patterns.join(', ')}`);
}
if (eu4Patterns.some((pattern) => pattern === '**/common/*.txt' || pattern === '**/common/**/*.txt')) {
  fail('EU4 language contribution must use the fixed common file whitelist');
}
if (eu4Patterns.some((pattern) => pattern.includes('/**'))) {
  fail('EU4 language contribution must not recursively match script directories');
}
const unexpectedCommonPatterns = eu4Patterns.filter(
  (pattern) => pattern.startsWith('**/common/') && !expectedEu4Patterns.includes(pattern),
);
if (unexpectedCommonPatterns.length > 0) {
  fail('EU4 language contribution has unexpected common file patterns: ' + unexpectedCommonPatterns.join(', '));
}
if (eu4Patterns.some((pattern) => /\.ya?ml|\.asset|\.sfx/i.test(pattern))) {
  fail('EU4 script language must not claim localisation YAML or asset/sfx files');
}
if (!localisationLanguage.filenamePatterns?.includes('**/localisation/**/*')) {
  fail('Localisation must claim all files recursively below localisation/');
}
if (manifest.activationEvents?.includes('onStartupFinished')) {
  fail('the extension must not activate and download pdx-ls in unrelated workspaces');
}
if (!manifest.activationEvents?.includes('onLanguage:eu4')) {
  fail('opening an EU4 document must activate the zero-configuration startup path');
}
if (!manifest.activationEvents?.includes('onLanguage:localisation')) {
  fail('opening a Localisation document must activate the zero-configuration startup path');
}
if (!manifest.activationEvents?.includes('workspaceContains:**/localisation/**/*')) {
  fail('a workspace containing localisation files must activate the extension');
}
if (manifest.activationEvents?.includes('workspaceContains:**/common/*.txt')) {
  fail('common activation must use the fixed bare-file whitelist');
}
if ((manifest.activationEvents ?? []).some(
  (event) => event.includes('/**') && !event.endsWith('/localisation/**/*'),
)) {
  fail('script activation must not recursively match script directories');
}
const unexpectedCommonActivation = (manifest.activationEvents ?? []).filter(
  (event) =>
    event.startsWith('workspaceContains:**/common/') &&
    !expectedEu4Patterns.some((pattern) => 'workspaceContains:' + pattern === event),
);
if (unexpectedCommonActivation.length > 0) {
  fail('common activation has unexpected file patterns: ' + unexpectedCommonActivation.join(', '));
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
if (!manifest.contributes.grammars?.some((entry) => entry.language === 'localisation'
  && entry.scopeName === 'source.localisation'
  && entry.path === './syntaxes/localisation.tmLanguage.json')) {
  fail('Localisation TextMate fallback grammar is missing');
}
if (manifest.contributes.configurationDefaults?.['[eu4]']?.['editor.semanticHighlighting.enabled'] !== true) {
  fail('semantic highlighting must remain enabled for EU4');
}
if (manifest.contributes.configurationDefaults?.['[localisation]']?.['editor.semanticHighlighting.enabled'] !== true) {
  fail('semantic highlighting must remain enabled for Localisation');
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
if (localisationGrammar.scopeName !== 'source.localisation'
  || !Array.isArray(localisationGrammar.patterns)
  || localisationGrammar.patterns.length < 5
  || !localisationGrammar.fileTypes?.includes('yml')
  || !localisationGrammar.fileTypes?.includes('yaml')) {
  fail('Localisation fallback grammar has an unexpected shape');
}
if (localisationConfiguration.comments?.lineComment !== '#') {
  fail('Localisation language configuration must recognize # comments');
}

const previewSource = readFileSync(join(root, 'src', 'previewPanel.ts'), 'utf8');
for (const marker of ['show(extensionUri: vscode.Uri, client?: LanguageClient)', 'version: documentVersion', 'sourceRange', 'requestSequence']) {
  if (!previewSource.includes(marker)) {
    fail(`Preview reliability marker missing: ${marker}`);
  }
}
const extensionSource = readFileSync(join(root, 'src', 'extension.ts'), 'utf8');
if (
  extensionSource.includes("{ pattern: '**/common/*.txt' }") ||
  extensionSource.includes("{ pattern: '**/common/**/*.txt' }")
) {
  fail('the language client must use the fixed common file whitelist');
}
const extensionCommonPatterns = [...extensionSource.matchAll(/\{ pattern: '(\*\*\/common\/[^']+)' \}/g)].map(
  ([, pattern]) => pattern,
);
const unexpectedCommonSelectors = extensionCommonPatterns.filter(
  (pattern) => !expectedEu4Patterns.includes(pattern),
);
if (unexpectedCommonSelectors.length > 0) {
  fail('the language client has unexpected common file patterns: ' + unexpectedCommonSelectors.join(', '));
}
for (const marker of [
  'context.extension.packageJSON.version',
  'context.extensionMode !== vscode.ExtensionMode.Production',
  'installPdxLs(context, options, progress)',
  'automatic checksum-verified ParadoxCode installation',
  'paradoxcodeVanillaReady',
  "const LOCALISATION_LANGUAGE_ID = 'localisation';",
  '{ language: LOCALISATION_LANGUAGE_ID }',
  "{ pattern: '**/map/area.txt' }",
  "{ pattern: '**/localisation/**/*' }",
]) {
  if (!extensionSource.includes(marker)) {
    fail(`Automatic server setup marker missing: ${marker}`);
  }
}
const installerSource = readFileSync(join(root, 'src', 'serverInstaller.ts'), 'utf8');
const timeoutMatch = /const DOWNLOAD_TIMEOUT_MS = ([0-9_]+)/.exec(installerSource);
if (!timeoutMatch || Number(timeoutMatch[1].replaceAll('_', '')) < 60_000) {
  fail('server installer must allow at least 60 seconds for a release download');
}
const attemptsMatch = /const MAX_DOWNLOAD_ATTEMPTS = ([0-9_]+)/.exec(installerSource);
if (!attemptsMatch || Number(attemptsMatch[1].replaceAll('_', '')) < 2) {
  fail('server installer must retry transient release download failures');
}
for (const marker of ['fetchBytesOnce', 'ECONNRESET', 'Timed out downloading ']) {
  if (!installerSource.includes(marker)) {
    fail(`server installer resilience marker missing: ${marker}`);
  }
}
const rendererSource = readFileSync(join(root, 'media', 'renderer.js'), 'utf8');
for (const marker of [
  'exportPng',
  'exportSvg',
  'exportJson',
  'addEventListener(\'keydown\'',
  'renderNodeList',
  'readColors',
  'requestAnimationFrame',
  'replaceChildren',
  'worldRectVisible',
]) {
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
