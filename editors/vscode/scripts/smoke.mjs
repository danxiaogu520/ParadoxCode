// Smoke test for the VSCode mission-preview data contract: drives the real
// pdx-ls over stdio JSON-RPC and validates the `pdx/missionPreview` payload
// shape the webview renderer consumes. Exit code 1 on any mismatch.
//
// Usage: node scripts/smoke.mjs [path-to-pdx-ls-binary]
// Default: `cargo run --quiet -p pdx-lsp --bin pdx-ls` (repo checkout).

import { spawn } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { parse } from 'smol-toml';

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
      rootUri: 'file:///tmp/paradoxcode-smoke',
      capabilities: {},
    });
    child.stdin.write(encode({ jsonrpc: '2.0', method: 'initialized', params: {} }));
    request(2, 'pdx/missionPreview', {
      path: 'missions/smoke.txt',
      text: MISSION_TEXT,
    });
    request(3, 'shutdown', {});
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
    for (const field of ['x', 'y', 'start', 'end', 'isRoot', 'hasError', 'hasWarning', 'icon', 'titleKey']) {
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
