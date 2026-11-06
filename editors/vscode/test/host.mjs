import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
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
const hostState = mkdtempSync(join(tmpdir(), 'paradoxcode-host-state-'));
if (process.env.PDC_COMPLETION_SERVER) {
  mkdirSync(join(workspaceRoot, '.vscode'));
  writeFileSync(join(workspaceRoot, '.vscode', 'settings.json'), JSON.stringify({
    'paradoxcode.serverPath': process.env.PDC_COMPLETION_SERVER,
    'paradoxcode.vanilla.mode': 'disabled',
  }));
}
const { runTests } = await import('@vscode/test-electron');
try {
  await runTests({
    ...(process.env.PDC_VSCODE_EXECUTABLE ? { vscodeExecutablePath: process.env.PDC_VSCODE_EXECUTABLE } : {}),
    extensionDevelopmentPath,
    extensionTestsPath: join(extensionDevelopmentPath, 'test', 'suite'),
    launchArgs: ['--disable-extensions', '--disable-workspace-trust', '--skip-welcome', '--skip-release-notes', '--no-sandbox', '--user-data-dir', join(hostState, 'profile'), '--extensions-dir', join(hostState, 'extensions'), workspaceRoot],
    extensionTestsEnv: {
      PDCLOC_HOST_WORKSPACE: workspaceRoot,
      ...(process.env.PDC_COMPLETION_SERVER ? { PDC_COMPLETION_SERVER: process.env.PDC_COMPLETION_SERVER } : {}),
      ...(process.env.PDC_COMPLETION_REPORT ? { PDC_COMPLETION_REPORT: process.env.PDC_COMPLETION_REPORT } : {}),
    },
  });
} finally {
  rmSync(workspaceRoot, { recursive: true, force: true });
  rmSync(hostState, { recursive: true, force: true });
}
