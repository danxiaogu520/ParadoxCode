// Smoke test for the VSCode mission-preview data contract: drives the real
// ParadoxCode server over stdio JSON-RPC and validates the `pdc/missionPreview` payload
// shape the webview renderer consumes. Exit code 1 on any mismatch.
//
// Usage: node scripts/smoke.mjs [path-to-paradoxcode-binary]
// Default: `cargo run --quiet -p pdc --bin paradoxcode` (repo checkout).

import { spawn } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { delimiter, dirname, join } from 'node:path';
// The extension's PATH fallback must detect a missing `paradoxcode` before launch,
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
        reject(new Error(`paradoxcode exited with code ${code}`));
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
    request(2, 'pdc/missionPreview', {
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
    request(5, 'pdc/workspaceFiles');
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
  : ['cargo', 'run', '--quiet', '-p', 'pdc', '--bin', 'paradoxcode'];

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
// theme can overlay rule-aware classifications once pdc is ready.
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
// VS Code output panel shows what pdc is doing instead of staying empty.
// The client registers a handler that forwards these to the 'ParadoxCode'
// channel and mirrors the latest stage into the status-bar tooltip.
const serverLogs = responses.filter((value) => value.method === 'window/logMessage');
if (serverLogs.length === 0) {
  fail('no window/logMessage frames received during initialize');
} else {
  const messages = serverLogs
    .map((log) => log?.params?.message)
    .filter((message) => typeof message === 'string');
  if (!messages.some((message) => message.includes('pdc initializing'))) {
    fail('initialize log trail must include the startup stage');
  }
  if (!messages.some((message) => message.includes('Initialization finished'))) {
    fail('initialize log trail must include the completion stage');
  }
}

const preview = responses.find((value) => value.id === 2);
if (!preview) {
  fail('no pdc/missionPreview response received');
} else if (preview.error) {
  fail(`pdc/missionPreview failed: ${JSON.stringify(preview.error)}`);
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
    for (const field of ['x', 'y', 'sourceRange', 'hasError', 'hasWarning', 'icon', 'titleKey']) {
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
    if (!Number.isInteger(arrow?.tree) || !Number.isInteger(arrow?.from)) {
      fail(`arrow ${arrow?.glyph} must name its endpoint series (tree/from)`);
    }
  }
  // The preview payload is pure text — pixel data ships through the
  // client-side asset pipeline, so the wire must not carry a textures table.
  if (result.textures !== undefined) {
    fail('result.textures must be absent from the missionPreview payload');
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
  fail('no pdc/workspaceFiles response received');
} else if (workspaceFiles.error) {
  fail(`pdc/workspaceFiles failed: ${JSON.stringify(workspaceFiles.error)}`);
} else if (!Array.isArray(workspaceFiles.result?.roots) || !Array.isArray(workspaceFiles.result?.files)) {
  fail('pdc/workspaceFiles must return roots and files arrays');
}

const failures = process.exitCode === 1;
console.log(failures ? 'smoke FAILED' : 'smoke OK');

const fakeDir = mkdtempSync(join(tmpdir(), 'paradoxcode-path-'));
try {
  const fakeName = process.platform === 'win32' ? 'pdc-fake.exe' : 'pdc-fake';
  writeFileSync(join(fakeDir, fakeName), '');
  const previousPath = process.env.PATH;
  process.env.PATH = fakeDir + (previousPath ? delimiter + previousPath : '');
  const found = findExecutableOnPath('pdc-fake');
  process.env.PATH = previousPath;
  if (found !== join(fakeDir, fakeName)) {
    fail(`findExecutableOnPath must resolve ${fakeName} from PATH`);
  }
  if (findExecutableOnPath('pdc-definitely-missing') !== undefined) {
    fail('findExecutableOnPath must return undefined for a name not on PATH');
  }
} finally {
  rmSync(fakeDir, { recursive: true, force: true });
}

const pathFailures = process.exitCode === 1;
console.log(pathFailures ? 'serverPath FAILED' : 'serverPath OK');

// ---------------------------------------------------------------------------
// MCP server contract: boot scripts/mcp.mjs over stdio, verify the handshake,
// the tool manifest mirroring package.json, and one real tool round trip.
// ---------------------------------------------------------------------------

async function runMcpSession(serverBinary) {
  const fixtureDir = mkdtempSync(join(tmpdir(), 'paradoxcode-mcp-'));
  let child;
  try {
    child = spawn(process.execPath, [
      join(here, 'mcp.mjs'),
      '--server', serverBinary,
      '--workspace', fixtureDir,
      '--timeout-ms', '120000',
    ], { stdio: ['pipe', 'pipe', 'inherit'], cwd: workspaceRoot });

    const waiters = [];
    let buffer = '';
    child.stdout.setEncoding('utf8');
    child.stdout.on('data', (chunk) => {
      buffer += chunk;
      let newline;
      while ((newline = buffer.indexOf('\n')) >= 0) {
        const line = buffer.slice(0, newline).trim();
        buffer = buffer.slice(newline + 1);
        if (!line) continue;
        let message;
        try {
          message = JSON.parse(line);
        } catch {
          fail(`mcp output is not newline-delimited JSON: ${line.slice(0, 120)}`);
          continue;
        }
        const index = message.id === undefined
          ? -1
          : waiters.findIndex((waiter) => waiter.id === message.id);
        if (index >= 0) {
          const [waiter] = waiters.splice(index, 1);
          clearTimeout(waiter.timer);
          waiter.resolve(message);
        }
      }
    });
    const exited = new Promise((resolve) => child.on('exit', (code) => resolve(code)));

    const request = (id, method, params, timeoutMs = 120_000) => {
      child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`);
      return new Promise((resolve, reject) => {
        const timer = setTimeout(() => {
          const index = waiters.findIndex((waiter) => waiter.id === id);
          if (index >= 0) waiters.splice(index, 1);
          reject(new Error(`timed out waiting for the mcp response to ${method}`));
        }, timeoutMs);
        waiters.push({ id, resolve, reject, timer });
      });
    };
    const notify = (method, params) => {
      child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', method, params })}\n`);
    };

    const init = await request(1, 'initialize', {
      protocolVersion: '2025-06-18',
      capabilities: {},
      clientInfo: { name: 'smoke', version: '0' },
    });
    if (init.error) {
      fail(`mcp initialize failed: ${JSON.stringify(init.error)}`);
    } else {
      const result = init.result ?? {};
      if (result.serverInfo?.name !== 'paradoxcode-mcp') {
        fail(`mcp serverInfo.name must be paradoxcode-mcp, got ${JSON.stringify(result.serverInfo)}`);
      }
      if (result.protocolVersion !== '2025-06-18') {
        fail(`mcp must negotiate protocolVersion 2025-06-18, got ${JSON.stringify(result.protocolVersion)}`);
      }
      if (typeof result.capabilities?.tools !== 'object') {
        fail('mcp initialize must advertise the tools capability');
      }
      if (typeof result.instructions !== 'string' || !result.instructions.includes('paradoxcode_validate_text')) {
        fail('mcp instructions must be a string carrying the tool discipline');
      }
    }

    notify('notifications/initialized', {});

    const list = await request(2, 'tools/list', {});
    if (list.error) {
      fail(`mcp tools/list failed: ${JSON.stringify(list.error)}`);
    } else {
      const tools = list.result?.tools;
      if (!Array.isArray(tools) || tools.length === 0) {
        fail('mcp tools/list must return a non-empty tools array');
      } else {
        const expected = manifest.contributes.languageModelTools.map((tool) => tool.name).sort();
        const actual = tools.map((tool) => tool.name).sort();
        if (JSON.stringify(actual) !== JSON.stringify(expected)) {
          fail(`mcp tools/list must mirror languageModelTools: ${JSON.stringify({ actual, expected })}`);
        }
        for (const tool of tools) {
          if (typeof tool.description !== 'string' || tool.description.length === 0) {
            fail(`mcp tool ${tool.name} needs a description`);
          }
          if (tool.inputSchema?.type !== 'object') {
            fail(`mcp tool ${tool.name} needs an object inputSchema`);
          }
        }
      }
    }

    // First tool call also covers the async language-server boot path.
    const rules = await request(3, 'tools/call', {
      name: 'paradoxcode_rules',
      arguments: { key: 'add_army_tradition' },
    }, 180_000);
    if (rules.error) {
      fail(`mcp rules failed: ${JSON.stringify(rules.error)}`);
    } else if (rules.result?.isError) {
      fail(`mcp rules returned an error result: ${JSON.stringify(rules.result)}`);
    } else {
      const text = rules.result?.content?.[0]?.text ?? '';
      if (rules.result?.content?.[0]?.type !== 'text' || !text.includes('add_army_tradition')) {
        fail(`mcp rules must surface the add_army_tradition rule, got: ${text.slice(0, 200)}`);
      }
    }

    const guidance = await request(4, 'tools/call', {
      name: 'paradoxcode_rules',
      arguments: {},
    });
    if (guidance.error || guidance.result?.isError) {
      fail(`mcp empty rules must return guidance text, got: ${JSON.stringify(guidance)}`);
    } else if (!guidance.result?.content?.[0]?.text.includes('at least one')) {
      fail('mcp empty rules must explain the required filters');
    }

    const validation = await request(5, 'tools/call', {
      name: 'paradoxcode_validate_text',
      arguments: {
        files: [{ path: 'events/mcp_smoke.txt', text: 'bad_key = yes\n' }],
      },
    }, 180_000);
    if (validation.error || validation.result?.isError) {
      fail(`mcp validate_text must succeed, got: ${JSON.stringify(validation)}`);
    } else {
      const text = validation.result?.content?.[0]?.text ?? '';
      if (!text.includes('mcp_smoke.txt') || !text.includes('diagnostic')) {
        fail(`mcp validate_text must report the file's diagnostics, got: ${text.slice(0, 200)}`);
      }
    }

    const workspace = await request(6, 'tools/call', {
      name: 'paradoxcode_workspace',
      arguments: {},
    }, 180_000);
    if (workspace.error || workspace.result?.isError) {
      fail(`mcp workspace must succeed, got: ${JSON.stringify(workspace)}`);
    } else {
      const text = workspace.result?.content?.[0]?.text ?? '';
      if (!text.includes('Game: eu4')) {
        fail(`mcp workspace must report the game identity, got: ${text.slice(0, 200)}`);
      }
    }

    const locGet = await request(7, 'tools/call', {
      name: 'paradoxcode_loc_get',
      arguments: { key: 'definitely_missing_localisation_key_xyz' },
    }, 180_000);
    if (locGet.error || locGet.result?.isError) {
      fail(`mcp loc_get must succeed, got: ${JSON.stringify(locGet)}`);
    } else {
      const text = locGet.result?.content?.[0]?.text ?? '';
      if (!text.includes('No localisation key')) {
        fail(`mcp loc_get must report the miss with guidance, got: ${text.slice(0, 200)}`);
      }
    }

    const ping = await request(8, 'ping', {});
    if (ping.error || JSON.stringify(ping.result) !== '{}') {
      fail(`mcp ping must answer an empty result, got: ${JSON.stringify(ping)}`);
    }

    child.stdin.end();
    const code = await exited;
    if (code !== 0) {
      fail(`mcp server must exit cleanly after stdin closes, got code ${code}`);
    }
  } finally {
    child?.kill('SIGKILL');
    rmSync(fixtureDir, { recursive: true, force: true });
  }
}

const explicitServer = serverArgs.length === 1 ? serverArgs[0] : undefined;
let mcpServerBinary = explicitServer;
if (!mcpServerBinary) {
  const executableName = process.platform === 'win32' ? 'paradoxcode.exe' : 'paradoxcode';
  for (const build of ['debug', 'release']) {
    const candidate = join(workspaceRoot, 'target', build, executableName);
    if (existsSync(candidate)) {
      mcpServerBinary = candidate;
      break;
    }
  }
}
if (!mcpServerBinary) {
  fail('mcp smoke requires a built paradoxcode binary (pass one explicitly or build target/debug)');
} else {
  await runMcpSession(mcpServerBinary);
}

const mcpFailures = process.exitCode === 1;
console.log(mcpFailures ? 'mcp FAILED' : 'mcp OK');
