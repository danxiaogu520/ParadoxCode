// i18n contract test: keeps extension UI localisation at full coverage.
//
// Checks (exit code 1 on any failure):
//   1. Every `vscode.l10n.t(<string literal>)` source string in src/ has an
//      identity entry in l10n/bundle.l10n.json and a translation in
//      l10n/bundle.l10n.zh-cn.json (exact key-set parity both ways).
//   2. User-visible call sites (notifications, status bar, dialogs, quick
//      picks, progress, webview titles, thrown errors that surface as
//      notifications, progress.report messages) pass strings through
//      `vscode.l10n.t(...)`; only brand/codicon-only status text is exempt.
//      src/agent/ is excluded: its strings are LLM-facing, not UI.
//   3. The webview string tables in src/webviewI18n.ts stay key- and
//      value-identical to the DEFAULT_STRINGS fallback tables in
//      media/renderer.js / media/icon-picker.js, and every data-i18n*
//      attribute in the webview HTML names an existing key.
//
// Usage: node scripts/i18n-test.mjs   (wired into npm run check / test:contract)

import { readFileSync, readdirSync, statSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const extRoot = join(here, '..');

function fail(message) {
  console.error(`FAIL: ${message}`);
  process.exitCode = 1;
}

function listFiles(directory, extension, skip, accumulator = []) {
  for (const name of readdirSync(directory)) {
    const candidate = join(directory, name);
    if (statSync(candidate).isDirectory()) {
      if (!skip.some((part) => candidate.endsWith(part))) {
        listFiles(candidate, extension, skip, accumulator);
      }
    } else if (candidate.endsWith(extension)) {
      accumulator.push(candidate);
    }
  }
  return accumulator;
}

const LITERAL = "(?:'(?:[^'\\\\]|\\\\.)*'|\"(?:[^\"\\\\]|\\\\.)*\"|`(?:[^`\\\\]|\\\\.)*`)";
const T_CALL = new RegExp(
  String.raw`vscode\.l10n\.t\(\s*((${LITERAL})(?:\s*\+\s*${LITERAL})*)`,
  'gs',
);

function unquote(literal) {
  const body = literal.slice(1, -1);
  return body.replace(/\\(u\{?[0-9a-fA-F]{1,6}\}|.)/g, (match, group) => {
    if (group.startsWith('u')) {
      return String.fromCodePoint(parseInt(group.replace(/[u{}]/g, ''), 16));
    }
    switch (group) {
      case 'n': return '\n';
      case 't': return '\t';
      case 'r': return '\r';
      default: return group;
    }
  });
}

function evaluateLiteralChain(chain) {
  const part = new RegExp(LITERAL, 'gs');
  let value = '';
  for (const match of chain.matchAll(part)) {
    value += unquote(match[0]);
  }
  return value;
}

// --- 1. bundle parity --------------------------------------------------------

const sourceFiles = listFiles(join(extRoot, 'src'), '.ts', [join('src', 'agent')]);
const tStrings = new Map();
for (const file of sourceFiles) {
  const text = readFileSync(file, 'utf8');
  for (const match of text.matchAll(T_CALL)) {
    tStrings.set(evaluateLiteralChain(match[1]), file);
  }
}

const enBundle = JSON.parse(readFileSync(join(extRoot, 'l10n', 'bundle.l10n.json'), 'utf8'));
const zhBundle = JSON.parse(readFileSync(join(extRoot, 'l10n', 'bundle.l10n.zh-cn.json'), 'utf8'));

for (const key of tStrings.keys()) {
  if (!Object.hasOwn(enBundle, key)) {
    fail(`l10n/bundle.l10n.json is missing the source string: ${JSON.stringify(key)}`);
  }
}
for (const [key, value] of Object.entries(enBundle)) {
  if (value !== key) {
    fail(`l10n/bundle.l10n.json must map every key to itself (identity), got: ${JSON.stringify(key)}`);
  }
  if (!tStrings.has(key)) {
    fail(`l10n/bundle.l10n.json has a stale entry no longer present in src/: ${JSON.stringify(key)}`);
  }
  if (!Object.hasOwn(zhBundle, key)) {
    fail(`l10n/bundle.l10n.zh-cn.json is missing a translation for: ${JSON.stringify(key)}`);
  }
}
for (const key of Object.keys(zhBundle)) {
  if (!Object.hasOwn(enBundle, key)) {
    fail(`l10n/bundle.l10n.zh-cn.json has an entry without an English source: ${JSON.stringify(key)}`);
  }
}

// --- 2. user-visible call sites ----------------------------------------------

// A violation is a string literal rendered straight to the user. Values that
// start with an identifier (a variable, a constructor call, a type shape) are
// left alone: the bundle-parity checks above still guarantee every translated
// string exists, this layer guards against raw-literal regressions.
function isRawLiteral(prefix) {
  return /^['"`]/.test(prefix.trim());
}

function isQuotedLiteral(prefix) {
  return /^['"]/.test(prefix.trim());
}

// Template literals whose non-interpolated remainder carries no letters are
// pure data joins (paths, ids, escape sequences) and need no translation.
function templateIsDataOnly(template) {
  return template.replace(/\$\{[^}]*\}/g, '').replace(/\\[nrt'"`\\]/g, '').match(/[A-Za-z]/) === null;
}

const SINKS = [
  {
    name: 'show*Message',
    pattern: /vscode\.window\.show(?:Information|Warning|Error)Message\(\s*([\s\S]{0,200}?)[,)]/g,
  },
  {
    name: 'setStatusBarMessage',
    pattern: /vscode\.window\.setStatusBarMessage\(\s*([\s\S]{0,200}?)[,)]/g,
  },
  {
    name: 'createWebviewPanel title',
    pattern: /createWebviewPanel\(\s*[^,]+,\s*([\s\S]{0,120}?)[,)]/g,
  },
  {
    name: 'dialog/pick option property',
    pattern: /\b(?:title|placeHolder|openLabel|saveLabel|prompt|label|detail|description)\s*:\s*([\s\S]{0,80}?)[,}\n]/g,
  },
  {
    name: 'status item .tooltip assignment',
    pattern: /\.tooltip\s*=\s*([\s\S]{0,120}?)[;)\n]/g,
    // Templates get the dedicated data-only / l10n.t analysis below.
    allowTemplates: true,
  },
  {
    name: 'thrown Error',
    pattern: /throw new Error\(\s*([\s\S]{0,200}?)[,)]/g,
    // Internal invariant guards that never surface as notifications.
    skipFiles: [join('src', 'transcode.ts')],
  },
  {
    name: 'FileSystemError',
    pattern: /vscode\.FileSystemError\.[A-Za-z]+\(\s*([\s\S]{0,200}?)[,)]/g,
  },
  {
    name: 'progress.report message',
    pattern: /report\(\{\s*message\s*:\s*([\s\S]{0,80}?)[,}\n]/g,
  },
];

const BRAND_AND_CODICON = /^ParadoxCode(?: \$\([-\w~]+\))?$/;

for (const file of sourceFiles) {
  const relative = file.slice(extRoot.length + 1);
  const text = readFileSync(file, 'utf8');
  for (const sink of SINKS) {
    if (sink.skipFiles?.some((skip) => relative === skip)) {
      continue;
    }
    for (const match of text.matchAll(sink.pattern)) {
      const value = match[1].trim();
      if (isRawLiteral(value) && !(sink.allowTemplates && value.startsWith('`'))) {
        fail(`${relative}: ${sink.name} renders a raw string; wrap it in vscode.l10n.t() (near "${value.slice(0, 60)}")`);
      }
    }
  }
  // Status text: quoted literals must be brand+codicon only; templates must
  // carry a translation call or join pure data.
  for (const match of text.matchAll(/\.text\s*=\s*([\s\S]{0,120}?)[;\n]/g)) {
    const value = match[1].trim();
    if (isQuotedLiteral(value) && !BRAND_AND_CODICON.test(value.replaceAll("'", ''))) {
      fail(`${relative}: status item .text literal is not brand/codicon-only: ${value.slice(0, 60)}`);
    }
    if (value.startsWith('`')
      && !value.includes('vscode.l10n.t(')
      && !templateIsDataOnly(value)) {
      fail(`${relative}: status item .text template has no vscode.l10n.t() call: ${value.slice(0, 60)}`);
    }
  }
  // Tooltips assembled as templates: translated call or pure data join.
  for (const match of text.matchAll(/\.tooltip\s*=\s*`([\s\S]{0,120}?)[;]/g)) {
    if (!match[0].includes('vscode.l10n.t(') && !templateIsDataOnly(match[0].slice('.tooltip = '.length + 1))) {
      fail(`${relative}: status item .tooltip template has no vscode.l10n.t() call: ${match[0].slice(0, 60)}`);
    }
  }
}

// --- 3. webview table sync -----------------------------------------------------

function extractWebviewTables(sourcePath) {
  const text = readFileSync(sourcePath, 'utf8');
  const tables = new Map();
  const functionRe = /export function (\w+)\(\): Record<string, string> \{\s*return \{([\s\S]*?)\n    \};/g;
  for (const match of text.matchAll(functionRe)) {
    const entries = new Map();
    const entryRe = new RegExp(`(\\w+):\\s*vscode\\.l10n\\.t\\(\\s*((${LITERAL})(?:\\s*\\+\\s*${LITERAL})*)`, 'gs');
    for (const entry of match[2].matchAll(entryRe)) {
      entries.set(entry[1], evaluateLiteralChain(entry[2]));
    }
    tables.set(match[1], entries);
  }
  return tables;
}

function extractMediaDefaults(mediaPath) {
  const text = readFileSync(mediaPath, 'utf8');
  const block = text.match(/const DEFAULT_STRINGS = \{([\s\S]*?)\n    \};/);
  const entries = new Map();
  if (block) {
    for (const match of block[1].matchAll(/(\w+):\s*'((?:[^'\\]|\\.)*)'/g)) {
      entries.set(match[1], match[2].replace(/\\n/g, '\n'));
    }
  }
  return entries;
}

const tables = extractWebviewTables(join(extRoot, 'src', 'webviewI18n.ts'));
const previewTable = tables.get('missionPreviewStrings');
const pickerTable = tables.get('missionIconPickerStrings');
if (!previewTable || previewTable.size === 0 || !pickerTable || pickerTable.size === 0) {
  fail('src/webviewI18n.ts must define non-empty missionPreviewStrings and missionIconPickerStrings tables');
}

const webviewPairs = [
  ['missionPreviewStrings', previewTable, 'renderer.js'],
  ['missionIconPickerStrings', pickerTable, 'icon-picker.js'],
];
for (const [tableName, srcTable, mediaFile] of webviewPairs) {
  const mediaTable = extractMediaDefaults(join(extRoot, 'media', mediaFile));
  if (mediaTable.size === 0) {
    fail(`media/${mediaFile} is missing its DEFAULT_STRINGS fallback table`);
  }
  for (const [key, value] of srcTable) {
    if (!mediaTable.has(key)) {
      fail(`media/${mediaFile} DEFAULT_STRINGS is missing key "${key}" from ${tableName}`);
    } else if (mediaTable.get(key) !== value) {
      fail(
        `media/${mediaFile} DEFAULT_STRINGS["${key}"] (${JSON.stringify(mediaTable.get(key))}) `
        + `does not match the English source in ${tableName} (${JSON.stringify(value)})`,
      );
    }
  }
  for (const key of mediaTable.keys()) {
    if (!srcTable.has(key)) {
      fail(`src/webviewI18n.ts ${tableName} is missing key "${key}" present in media/${mediaFile}`);
    }
  }
}

for (const [tableName, table, htmlFile] of [
  ['missionPreviewStrings', previewTable, 'index.html'],
  ['missionIconPickerStrings', pickerTable, 'icon-picker.html'],
]) {
  const html = readFileSync(join(extRoot, 'media', htmlFile), 'utf8');
  for (const match of html.matchAll(/data-i18n(?:-title|-placeholder|-aria-label)?="([^"]+)"/g)) {
    if (!table.has(match[1])) {
      fail(`media/${htmlFile} references i18n key "${match[1]}" missing from ${tableName}`);
    }
  }
}

// --- summary -------------------------------------------------------------------

const total = process.exitCode === 1 ? 'FAILED' : 'OK';
console.log(`i18n contract ${total} (${tStrings.size} source strings, `
  + `${Object.keys(zhBundle).length} zh-cn translations, `
  + `${previewTable.size + pickerTable.size} webview keys)`);
