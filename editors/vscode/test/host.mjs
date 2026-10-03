import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

if (process.env.RUN_VSCODE_HOST_TESTS !== '1') {
  console.log('VS Code Extension Host tests skipped (set RUN_VSCODE_HOST_TESTS=1 to download/use Electron).');
  process.exit(0);
}

const here = dirname(fileURLToPath(import.meta.url));
const extensionDevelopmentPath = join(here, '..');
// The host opens a throwaway folder so tests have a real workspace root
// without mutating the window's workspace at runtime (a first-folder add can
// stall behind workspace trust, and leftover state leaks into later runs).
const workspaceRoot = mkdtempSync(join(tmpdir(), 'paradoxcode-host-'));
const { runTests } = await import('@vscode/test-electron');
try {
  await runTests({
    extensionDevelopmentPath,
    extensionTestsPath: join(extensionDevelopmentPath, 'test', 'suite'),
    launchArgs: ['--disable-extensions', workspaceRoot],
    extensionTestsEnv: { PDCLOC_HOST_WORKSPACE: workspaceRoot },
  });
} finally {
  rmSync(workspaceRoot, { recursive: true, force: true });
}
