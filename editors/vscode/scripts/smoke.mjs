// Smoke test for the VSCode mission-preview data contract: drives the real
// pdx-ls over stdio JSON-RPC and validates the `pdx/missionPreview` payload
// shape the webview renderer consumes. Exit code 1 on any mismatch.
//
// Usage: node scripts/smoke.mjs [path-to-pdx-ls-binary]
// Default: `cargo run --quiet -p pdx-lsp --bin pdx-ls` (repo checkout).

import { spawn } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { delimiter, dirname, join } from 'node:path';
import { parse } from 'smol-toml';
// The extension's PATH fallback must detect a missing `pdx-ls` before launch,
// so the user gets an actionable warning instead of a bare spawn ENOENT. The
// helper lives in the compiled `out/` tree (check runs compile first).
import { findExecutableOnPath } from '../out/serverPath.js';

const here = dirname(fileURLToPath(import.meta.url));
const workspaceRoot = join(here, '..', '..', '..');

const MISSION_TEXT = [
  'main_tree = {',
  '\tslot = 1',
  '\ta1 = { position = 1 icon = mission_alpha }',
  '\ta2 = { position = 2 required_missions = { a1 } }',
  '}',
  '',
  'branch_tree = {',
  '\tslot = 2',
  '\tb1 = { position = 1 required_missions = { external_id } }',
  '}',
  '',
].join('\n');

function encode(message) {
  const body = Buffer.from(JSON.stringify(message));
  const header = `Content-Length: ${body.length}\r\n\r\n`;
  return Buffer.concat([Buffer.from(header), body]);
}

function run(serverArgs) {
  return new Promise((resolve, reject) => {
    const child = spawn(serverArgs[0], serverArgs.slice(1), {
      stdio: ['pipe', 'pipe', 'inherit'],
      cwd: workspaceRoot,
    });
    const responses = [];
    let buffer = Buffer.alloc(0);

    child.stdout.on('data', (chunk) => {
      buffer = Buffer.concat([buffer, chunk]);
      while (true) {
        const headerEnd = buffer.indexOf('\r\n\r\n');
        if (headerEnd === -1) break;
        const header = buffer.subarray(0, headerEnd).toString();
        const match = /Content-Length: (\d+)/.exec(header);
        if (!match) break;
        const length = Number(match[1]);
        const bodyStart = headerEnd + 4;
        if (buffer.length < bodyStart + length) break;
        const body = buffer.subarray(bodyStart, bodyStart + length).toString();
        buffer = buffer.subarray(bodyStart + length);
        responses.push(JSON.parse(body));
      }
    });

    child.on('error', reject);
    child.on('exit', (code) => {
      if (code !== 0 && responses.length === 0) {
        reject(new Error(`pdx-ls exited with code ${code}`));
      } else {
        resolve(responses);
      }
    });

    const request = (id, method, params) => {
      child.stdin.write(encode({ jsonrpc: '2.0', id, method, params }));
    };
    request(1, 'initialize', {
      workspaceFolders: [{ uri: 'file:///tmp/paradoxcode-smoke', name: 'smoke' }],
      capabilities: {},
    });
    child.stdin.write(encode({ jsonrpc: '2.0', method: 'initialized', params: {} }));
    request(2, 'pdx/missionPreview', {
      path: 'missions/smoke.txt',
      text: MISSION_TEXT,
      uri: 'file:///tmp/paradoxcode-smoke/missions/smoke.txt',
      version: 7,
    });
    // The real client method for full-document semantic tokens is
    // `textDocument/semanticTokens/full`; a server that only knows the bare
    // `textDocument/semanticTokens` spelling answers -32601 and breaks theming.
    const scriptUri = 'file:///tmp/paradoxcode-smoke/events/smoke.txt';
    child.stdin.write(encode({
      jsonrpc: '2.0',
      method: 'textDocument/didOpen',
      params: {
        textDocument: { uri: scriptUri, languageId: 'eu4', version: 1, text: '# note\n@cost = 100\n' },
      },
    }));
    request(3, 'textDocument/semanticTokens/full', {
      textDocument: { uri: scriptUri },
    });
    request(5, 'pdx/workspaceFiles');
    request(4, 'shutdown', {});
    child.stdin.write(encode({ jsonrpc: '2.0', method: 'exit', params: {} }));
    child.stdin.end();
  });
}

function fail(message) {
  console.error(`FAIL: ${message}`);
  process.exitCode = 1;
}

const ARROW_GLYPHS = new Set([
  'verticalTile',
  'verticalSkipTier',
  'horizontalSkipSlot',
  'leftOut',
  'leftIn',
  'rightOut',
  'rightIn',
  'end',
]);

const serverArgs = process.argv.length > 2
  ? process.argv.slice(2)
  : ['cargo', 'run', '--quiet', '-p', 'pdx-lsp', '--bin', 'pdx-ls'];

const responses = await run(serverArgs);

// The `contributes.semanticTokenScopes` manifest entry must be an array of
// per-language `{ language, scopes }` objects; a plain object is rejected by
// VS Code's contribution schema validation. Scope keys must name token types
// the server actually advertises in its semantic-token legend.
const manifestPath = join(here, '..', 'package.json');
const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
const tokenScopes = manifest?.contributes?.semanticTokenScopes;
if (!Array.isArray(tokenScopes) || tokenScopes.length === 0) {
  fail('contributes.semanticTokenScopes must be a non-empty array');
}

// Semantic tokens remain enabled even with the local TextMate fallback grammar, so a
// theme can overlay rule-aware classifications once pdx-ls is ready.
const defaultConfig = manifest?.contributes?.configurationDefaults;
if (defaultConfig?.['[eu4]']?.['editor.semanticHighlighting.enabled'] !== true) {
  fail('configurationDefaults["[eu4]"].editor.semanticHighlighting.enabled must be true');
}

const initialize = responses.find((value) => value.id === 1);
const legendTypes = new Set(
  initialize?.result?.capabilities?.semanticTokensProvider?.legend?.tokenTypes ?? [],
);
for (const entry of tokenScopes) {
  if (typeof entry?.language !== 'string' || typeof entry?.scopes !== 'object') {
    fail('each semanticTokenScopes entry needs a language and a scopes map');
    continue;
  }
  for (const tokenType of Object.keys(entry.scopes)) {
    if (!legendTypes.has(tokenType)) {
      fail(`semanticTokenScopes key "${tokenType}" is not in the server legend`);
    }
  }
}

// The server must narrate its initialization over `window/logMessage`, so the
// VS Code output panel shows what pdx-ls is doing instead of staying empty.
// The client registers a handler that forwards these to the 'ParadoxCode'
// channel and mirrors the latest stage into the status-bar tooltip.
const serverLogs = responses.filter((value) => value.method === 'window/logMessage');
if (serverLogs.length === 0) {
  fail('no window/logMessage frames received during initialize');
} else {
  const messages = serverLogs
    .map((log) => log?.params?.message)
    .filter((message) => typeof message === 'string');
  if (!messages.some((message) => message.includes('pdx-ls initializing'))) {
    fail('initialize log trail must include the startup stage');
  }
  if (!messages.some((message) => message.includes('Initialization finished'))) {
    fail('initialize log trail must include the completion stage');
  }
}

const preview = responses.find((value) => value.id === 2);
if (!preview) {
  fail('no pdx/missionPreview response received');
} else if (preview.error) {
  fail(`pdx/missionPreview failed: ${JSON.stringify(preview.error)}`);
} else {
  const result = preview.result;
  const nodes = result?.nodes;
  const required = ['nodes', 'arrows', 'groups', 'external', 'diagnostics'];
  for (const field of required) {
    if (!Array.isArray(result?.[field])) {
      fail(`result.${field} must be an array`);
    }
  }
  const a1 = nodes?.find((node) => node.id === 'a1');
  if (!a1) {
    fail('node a1 missing');
  } else {
    for (const field of ['x', 'y', 'sourceRange', 'isRoot', 'hasError', 'hasWarning', 'icon', 'titleKey']) {
      if (!(field in a1)) {
        fail(`node a1 missing field ${field}`);
      }
    }
    if (a1.icon !== 'mission_alpha') {
      fail(`node a1 icon must be mission_alpha, got ${JSON.stringify(a1.icon)}`);
    }
    if (typeof a1.x !== 'number' || typeof a1.y !== 'number') {
      fail('node coordinates must be numbers');
    }
    if (
      !a1.sourceRange ||
      typeof a1.sourceRange.start?.line !== 'number' ||
      typeof a1.sourceRange.start?.character !== 'number' ||
      typeof a1.sourceRange.end?.line !== 'number' ||
      typeof a1.sourceRange.end?.character !== 'number'
    ) {
      fail('node sourceRange must be an LSP UTF-16 range');
    }
    if (!Array.isArray(a1.required)) {
      fail('node.required must be an array');
    }
    if (a1.titleKey !== 'a1_title') {
      fail(`node a1 titleKey must be a1_title, got ${JSON.stringify(a1.titleKey)}`);
    }
    if (
      a1.title !== null &&
      (typeof a1.title?.value !== 'string' ||
        (a1.title?.language !== null && typeof a1.title?.language !== 'string'))
    ) {
      fail('node a1 title must be null or { language, value }');
    }
  }
  const a2 = nodes?.find((node) => node.id === 'a2');
  if (a2 && !(a2.required ?? []).includes('a1')) {
    fail('a2 must require a1');
  }
  const b1 = nodes?.find((node) => node.id === 'b1');
  if (!b1?.hasError) {
    fail('b1 must carry the dangling-reference error');
  }
  if (!result.external.some((ext) => ext.label === 'external_id')) {
    fail('external stub for external_id missing');
  }
  if (!result.arrows.some((arrow) => arrow.glyph === 'end')) {
    fail('arrow glyphs must include an end marker');
  }
  for (const arrow of result.arrows) {
    if (!ARROW_GLYPHS.has(arrow?.glyph)) {
      fail(`unknown arrow glyph: ${JSON.stringify(arrow?.glyph)}`);
    }
    if (typeof arrow?.x !== 'number' || typeof arrow?.y !== 'number') {
      fail('arrow glyph coordinates must be numbers');
    }
    if (typeof arrow?.texture !== 'string') {
      fail(`arrow ${arrow?.glyph} must expose a sprite texture name`);
    }
  }
  // Without a configured game directory the texture table is empty but present.
  if (!result.textures || typeof result.textures !== 'object') {
    fail('result.textures must be an object');
  }
  if (result.groups.length !== 2) {
    fail(`expected 2 groups, got ${result.groups.length}`);
  }
  if (result.groups.some((group) => !group.sourceRange?.start || !group.sourceRange?.end)) {
    fail('mission groups must expose UTF-16 source ranges');
  }
  if (result.documentUri !== 'file:///tmp/paradoxcode-smoke/missions/smoke.txt' || result.documentVersion !== 7) {
    fail('mission preview must echo document URI and version');
  }
}

// Full-document semantic tokens must be served under the real protocol method
// name `textDocument/semanticTokens/full`; a -32601 here breaks VS Code theming.
const semanticTokens = responses.find((value) => value.id === 3);
if (!semanticTokens) {
  fail('no textDocument/semanticTokens/full response received');
} else if (semanticTokens.error) {
  fail(`textDocument/semanticTokens/full failed: ${JSON.stringify(semanticTokens.error)}`);
} else if (!Array.isArray(semanticTokens.result?.data) || semanticTokens.result.data.length < 1) {
  fail('semanticTokens/full must return relative token data');
}

const workspaceFiles = responses.find((value) => value.id === 5);
if (!workspaceFiles) {
  fail('no pdx/workspaceFiles response received');
} else if (workspaceFiles.error) {
  fail(`pdx/workspaceFiles failed: ${JSON.stringify(workspaceFiles.error)}`);
} else if (!Array.isArray(workspaceFiles.result?.roots) || !Array.isArray(workspaceFiles.result?.files)) {
  fail('pdx/workspaceFiles must return roots and files arrays');
}

// The shared config TOML that both editors read must parse and yield the
// `[server].binary`, and pdx-ls must tolerate the same file (server ignores
// the `[server]` table it does not consume).
const sharedConfig = `mod_directory = \"mod\"
[[dependencies]]\nid = \"Chinese Language Mod for 1.37\"\npath = \"deps/han\"\n
[server]\nbinary = \"C:/tools/pdx-ls.exe\"\n`;
try {
  const parsed = parse(sharedConfig);
  const binary = parsed?.server?.binary;
  if (binary !== 'C:/tools/pdx-ls.exe') {
    fail('shared config [server].binary must parse');
  }
} catch (error) {
  fail(`shared config failed to parse: ${String(error)}`);
}

const failures = process.exitCode === 1;
console.log(failures ? 'smoke FAILED' : 'smoke OK');

const fakeDir = mkdtempSync(join(tmpdir(), 'paradoxcode-path-'));
try {
  const fakeName = process.platform === 'win32' ? 'pdxls-fake.exe' : 'pdxls-fake';
  writeFileSync(join(fakeDir, fakeName), '');
  const previousPath = process.env.PATH;
  process.env.PATH = fakeDir + (previousPath ? delimiter + previousPath : '');
  const found = findExecutableOnPath('pdxls-fake');
  process.env.PATH = previousPath;
  if (found !== join(fakeDir, fakeName)) {
    fail(`findExecutableOnPath must resolve ${fakeName} from PATH`);
  }
  if (findExecutableOnPath('pdxls-definitely-missing') !== undefined) {
    fail('findExecutableOnPath must return undefined for a name not on PATH');
  }
} finally {
  rmSync(fakeDir, { recursive: true, force: true });
}

const pathFailures = process.exitCode === 1;
console.log(pathFailures ? 'serverPath FAILED' : 'serverPath OK');
