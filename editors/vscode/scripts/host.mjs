import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

if (process.env.RUN_VSCODE_HOST_TESTS !== '1') {
  console.log('VS Code Extension Host tests skipped (set RUN_VSCODE_HOST_TESTS=1 to download/use Electron).');
  process.exit(0);
}

const here = dirname(fileURLToPath(import.meta.url));
const extensionDevelopmentPath = join(here, '..');
const { runTests } = await import('@vscode/test-electron');
await runTests({
  extensionDevelopmentPath,
  extensionTestsPath: join(extensionDevelopmentPath, 'test', 'suite'),
  launchArgs: ['--disable-extensions'],
});
