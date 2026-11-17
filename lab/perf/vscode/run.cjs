const fs = require('node:fs');
const path = require('node:path');
const { spawn } = require('node:child_process');
const { runTests } = require(path.resolve(__dirname, '../../../editors/vscode/node_modules/@vscode/test-electron/out/index.js'));
const [configArgument, variant = 'candidate', label = variant, outputArgument] = process.argv.slice(2);
if (!configArgument) throw new Error('Usage: node lab/perf/vscode/run.cjs CONFIG VARIANT LABEL [OUTPUT_DIRECTORY]');
const configPath = path.resolve(configArgument);
const output = path.resolve(outputArgument || path.join(__dirname, '../../../target/performance-results/vscode-completion'));
fs.mkdirSync(output, { recursive: true });
if (!/^[a-zA-Z0-9_.-]+$/.test(label)) throw new Error('Use a plain report label');
const config = JSON.parse(fs.readFileSync(configPath, 'utf8').replace(/^\uFEFF/, ''));
const server = config.servers[variant];
if (!server) throw new Error(`Unknown server variant ${variant}`);
const state = path.join(output, `host-${label}`);
const workspace = path.join(state, 'workspace');
fs.mkdirSync(path.join(workspace, '.vscode'), { recursive: true });
fs.writeFileSync(path.join(workspace, '.vscode', 'settings.json'), JSON.stringify({
  'paradoxcode.serverPath': server,
  'paradoxcode.modDirectory': config.workspace,
  'paradoxcode.vanilla.mode': 'cacheOnly',
  'paradoxcode.vanillaIndexCache': config.vanillaCache,
  'paradoxcode.dependencies': config.dependencies,
  'paradoxcode.workspaceWideDiagnostics': false,
  'paradoxcode.backgroundReindexIntervalMinutes': 0,
  'files.hotExit': 'off',
  'files.autoSave': 'off',
}));
const resources = { method: 'One persistent Windows process sampler; cumulative CPU, working set and private bytes at nominal 500 ms intervals. Peaks are sampled.', samples: [], errors: [] };
const sampler = spawn('powershell', ['-NoProfile', '-NonInteractive', '-Command', `
$perfTimer = [Diagnostics.Stopwatch]::StartNew()
while ($true) {
  foreach ($perfProcess in @(Get-Process -Name paradoxcode -ErrorAction SilentlyContinue)) {
    try {
      if ([string]::Equals($perfProcess.Path, $env:PDC_SAMPLED_SERVER, [StringComparison]::OrdinalIgnoreCase)) {
        [pscustomobject]@{pid=$perfProcess.Id;elapsedMs=$perfTimer.Elapsed.TotalMilliseconds;cpuSeconds=$perfProcess.CPU;workingSetBytes=$perfProcess.WorkingSet64;privateBytes=$perfProcess.PrivateMemorySize64} | ConvertTo-Json -Compress
      }
    } catch { [pscustomobject]@{error=$_.Exception.Message} | ConvertTo-Json -Compress }
  }
  Start-Sleep -Milliseconds 500
}`], { windowsHide: true, env: { ...process.env, PDC_SAMPLED_SERVER: server }, stdio: ['ignore', 'pipe', 'pipe'] });
let sampleBuffer = '';
sampler.stdout.on('data', chunk => {
  sampleBuffer += chunk.toString();
  let newline;
  while ((newline = sampleBuffer.indexOf('\n')) >= 0) {
    const line = sampleBuffer.slice(0, newline).trim(); sampleBuffer = sampleBuffer.slice(newline + 1);
    if (!line) continue;
    try { const value = JSON.parse(line); (value.error ? resources.errors : resources.samples).push(value); }
    catch (error) { resources.errors.push(String(error)); }
  }
});
sampler.stderr.on('data', data => resources.errors.push(data.toString()));
sampler.on('error', error => resources.errors.push(String(error)));
runTests({
  stdout: fs.createWriteStream(path.join(output, `host-${label}.stdout.log`), { flags: 'a' }),
  stderr: fs.createWriteStream(path.join(output, `host-${label}.stderr.log`), { flags: 'a' }),
  vscodeExecutablePath: config.vscode,
  extensionDevelopmentPath: config.extension,
  extensionTestsPath: path.join(__dirname, 'probe.cjs'),
  launchArgs: ['--disable-extensions', '--skip-welcome', '--skip-release-notes',
    '--user-data-dir', path.join(state, 'profile'), '--extensions-dir', path.join(state, 'extensions'), workspace],
  extensionTestsEnv: {
    PDC_PROBE_CONFIG: configPath,
    PDC_PROBE_SERVER: server,
    PDC_PROBE_REPORT: path.join(output, `host-${label}.json`),
    PDC_TRACE: path.join(output, `host-${label}.protocol.log`),
  },
}).catch(error => { console.error(error); process.exitCode = 1; }).finally(async () => {
  sampler.kill();
  await new Promise(resolve => { sampler.once('close', resolve); setTimeout(resolve, 1000); });
  resources.summary = {
    samples: resources.samples.length,
    lastCpuSeconds: resources.samples.at(-1)?.cpuSeconds ?? null,
    peakWorkingSetBytes: resources.samples.length ? Math.max(...resources.samples.map(s => s.workingSetBytes)) : null,
    peakPrivateBytes: resources.samples.length ? Math.max(...resources.samples.map(s => s.privateBytes)) : null,
  };
  fs.writeFileSync(path.join(output, `host-${label}.resources.json`), JSON.stringify(resources, null, 2));
});
