import { execSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');

let listing;
try {
  listing = execSync('npm exec -- vsce ls', {
    cwd: root,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
    shell: true,
  });
} catch (error) {
  const message = error instanceof Error ? error.message : String(error);
  throw new Error(`package contract: unable to list VSIX files: ${message}`);
}

const files = new Set(listing.split(/\r?\n/).map((value) => value.trim()).filter(Boolean));
for (const required of [
  'node_modules/smol-toml/dist/index.cjs',
  'node_modules/vscode-languageclient/lib/node/main.js',
  'node_modules/vscode-jsonrpc/lib/node/main.js',
  'node_modules/vscode-languageserver-protocol/lib/common/api.js',
]) {
  if (!files.has(required)) {
    throw new Error(`package contract: VSIX is missing ${required}`);
  }
}
if ([...files].some((file) => file.startsWith('node_modules/typescript/'))) {
  throw new Error('package contract: VSIX must not include the TypeScript development dependency');
}

console.log('package contract OK');
