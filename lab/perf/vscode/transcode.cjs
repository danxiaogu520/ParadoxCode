// PDC_VSCODE_EXECUTABLE and PDC_COMPLETION_SERVER identify the two executables.
// node lab/perf/vscode/transcode.cjs path/to/source.yml 1 4 19
const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');
const { setTimeout: delay } = require('node:timers/promises');

const root = path.resolve(__dirname, '../../..');
const sourceExtension = path.join(root, 'editors/vscode');
const extension = path.resolve(process.env.PDC_TRANSCODE_EXTENSION || sourceExtension);
const digest = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
const normalize = text => text.replace(/^\uFEFF/, '').replace(/\r\n/g, '\n');

function activeModule(vscode, name) {
    const folder = vscode.extensions.getExtension('paradoxcode.paradoxcode-vscode').extensionPath;
    const file = path.join(folder, 'out', name + '.js');
    // VS Code and the launcher can spell the same Windows drive letter differently.
    const key = Object.keys(require.cache).find(key => path.relative(key, file) === '');
    assert.ok(key, 'extension did not load ' + file);
    return require.cache[key].exports;
}

async function measureYml(vscode, file) {
    const original = fs.readFileSync(file);
    const start = performance.now();
    await vscode.window.showTextDocument(await vscode.workspace.openTextDocument(vscode.Uri.file(file)));
    let document;
    while (performance.now() - start < 60_000) {
        const active = vscode.window.activeTextEditor?.document;
        if (active?.uri.scheme === 'pdcloc' && active.uri.fsPath === file) {
            document = active;
            break;
        }
        await delay(25);
    }
    assert.ok(document, 'automatic YML view did not open within 60 seconds');
    const openMs = Math.round(performance.now() - start);
    assert.ok(fs.readFileSync(file).equals(original), 'opening a file changed its bytes');
    const saveMs = [];
    for (let editNumber = 0; editNumber < 5; editNumber++) {
        const edit = new vscode.WorkspaceEdit();
        edit.insert(document.uri, document.lineAt(document.lineCount - 1).range.end, '\n# ' + editNumber + '中');
        assert.ok(await vscode.workspace.applyEdit(edit));
        const started = performance.now();
        assert.ok(await document.save());
        saveMs.push(Math.round(performance.now() - started));
        assert.ok(fs.readFileSync(file).includes(Buffer.from('# ' + editNumber + '\x10-N')), 'expected EU4 bytes for 中');
    }
    const decoded = await vscode.workspace.fs.readFile(document.uri);
    assert.ok(normalize(Buffer.from(decoded).toString()) === normalize(document.getText()), 'complete YML round trip');
    await vscode.commands.executeCommand('workbench.action.closeActiveEditor');
    const result = { inputBytes: original.length, openMs, saveMs, outputSha256: digest(fs.readFileSync(file)) };
    const samples = Number(process.env.PDC_TRANSCODE_MANUAL_SAMPLES || 0);
    if (samples > 0) {
        assert.ok(Number.isSafeInteger(samples));
        result.manual = await measureManual(vscode, file, decoded, samples);
    }
    return result;
}

async function measureManual(vscode, file, decoded, samples) {
    const config = vscode.workspace.getConfiguration('paradoxcode.localisation');
    const previous = config.inspect('encodingMode').workspaceValue;
    const module = activeModule(vscode, 'byteCodec');
    const convert = module.convertBytes;
    const results = [];
    try {
        await config.update('encodingMode', 'manual', vscode.ConfigurationTarget.Workspace);
        const editor = await vscode.window.showTextDocument(vscode.Uri.file(file));
        const doc = editor.document;
        const disk = fs.readFileSync(file);
        const raw = doc.getText();
        const readable = normalize(Buffer.from(decoded).toString());
        let codecMs;
        module.convertBytes = async (...args) => {
            const t = performance.now();
            const bytes = await convert(...args);
            codecMs = performance.now() - t;
            return bytes;
        };
        // The automatic saves above produced canonical bytes. Both commands should change
        // the buffer, and two undos should restore the exact starting state for every pair.
        for (let i = 0; i <= samples; i++) {
            for (const direction of ['decode', 'encode']) {
                codecMs = undefined;
                const start = performance.now();
                await vscode.commands.executeCommand('paradoxcode.localisation.' + direction + 'File', doc.uri);
                const totalMs = performance.now() - start;
                assert.ok(Number.isFinite(codecMs), 'expected an actual codec invocation');
                assert.ok(direction === 'decode' ? normalize(doc.getText()) === readable : doc.getText() === raw);
                if (i > 0) results.push({ direction, totalMs, codecMs });
            }
            await vscode.commands.executeCommand('undo');
            await vscode.commands.executeCommand('undo');
            assert.ok(doc.getText() === raw);
        }
        assert.ok(fs.readFileSync(file).equals(disk), 'manual conversion must only edit the buffer');
        await vscode.commands.executeCommand('workbench.action.closeActiveEditor');
    } finally {
        module.convertBytes = convert;
        await config.update('encodingMode', previous, vscode.ConfigurationTarget.Workspace);
    }
    return results;
}

async function measureTxt(vscode, file) {
    const initial = fs.readFileSync(file);
    const document = await vscode.workspace.openTextDocument(vscode.Uri.file(file), { encoding: 'iso88591' });
    const editor = await vscode.window.showTextDocument(document);
    const anchor = document.lineAt(document.lineCount - 1).range.end;
    assert.ok(await editor.edit(edit => edit.insert(anchor, '中国')));
    editor.selection = new vscode.Selection(anchor, anchor.translate(0, 2));
    const started = performance.now();
    await vscode.commands.executeCommand('paradoxcode.localisation.encodeSelection');
    const selectionMs = Math.round(performance.now() - started);
    assert.ok(fs.readFileSync(file).equals(initial), 'selection edit should remain unsaved');
    const saving = performance.now();
    assert.ok(await document.save());
    const saveMs = Math.round(performance.now() - saving);
    const result = fs.readFileSync(file);
    assert.ok(result.equals(Buffer.concat([initial, Buffer.from([16, 45, 78, 16, 253, 86])])), 'unexpected script bytes');
    return { inputBytes: initial.length, selectionMs, saveMs, outputSha256: digest(result) };
}

// The input is already game-encoded except for one readable character on the last line.
async function measureSparse(vscode, file, samples) {
    const disk = fs.readFileSync(file);
    const editor = await vscode.window.showTextDocument(vscode.Uri.file(file));
    const doc = editor.document;
    const raw = doc.getText();
    assert.ok(raw.endsWith('# 中'));
    const expected = raw.slice(0, -1) + '\x10-N';
    const results = [];
    let changes = [];
    const listener = vscode.workspace.onDidChangeTextDocument(event => {
        if (event.document === doc) changes.push(...event.contentChanges);
    });
    try {
        for (let i = 0; i <= samples; i++) {
            changes = [];
            const start = performance.now();
            await vscode.commands.executeCommand('paradoxcode.localisation.encodeFile', doc.uri);
            const totalMs = performance.now() - start;
            assert.ok(doc.getText() === expected);
            assert.equal(changes.length, 1);
            if (i > 0) results.push({ totalMs, replacedUnits: changes[0].rangeLength });
            await vscode.commands.executeCommand('undo');
            // The host may deliver the clean-state notification after the workbench command resolves.
            const deadline = performance.now() + 30_000;
            while (doc.isDirty && performance.now() < deadline) await delay(25);
            assert.ok(!doc.isDirty, 'undo should restore the saved state');
            assert.ok(doc.getText() === raw, 'undo should restore every original character');
        }
        assert.ok(fs.readFileSync(file).equals(disk));
        return { inputBytes: disk.length, outputSha256: digest(Buffer.from(expected)), samples: results };
    } finally {
        listener.dispose();
        await vscode.commands.executeCommand('workbench.action.closeActiveEditor');
    }
}

exports.run = async () => {
    const vscode = require('vscode');
    const installed = vscode.extensions.getExtension('paradoxcode.paradoxcode-vscode');
    await installed.activate();
    const client = await activeModule(vscode, 'serverClient').acquireServerClient(30_000);
    const transitions = [];
    const listener = client.onDidChangeState(e => transitions.push(e.newState));
    const workspace = vscode.workspace.workspaceFolders[0].uri.fsPath;
    const loc = path.join(workspace, 'localisation');
    const results = { vscode: vscode.version, yml: [] };
    const sparse = Number(process.env.PDC_TRANSCODE_SPARSE_SAMPLES || 0);
    for (const name of fs.readdirSync(loc).sort()) {
        const file = path.join(loc, name);
        results.yml.push(sparse ? await measureSparse(vscode, file, sparse) : await measureYml(vscode, file));
    }
    const script = path.join(workspace, 'sample.txt');
    if (fs.existsSync(script)) results.txt = await measureTxt(vscode, script);
    listener.dispose();
    assert.deepEqual(transitions, [], 'benchmark requires one uninterrupted language-server session');
    fs.writeFileSync(process.env.PDC_TRANSCODE_REPORT, JSON.stringify(results, null, 2));
};

async function main() {
    const [source, ...sizes] = process.argv.slice(2);
    const { PDC_VSCODE_EXECUTABLE, PDC_COMPLETION_SERVER, PDC_SCRIPT_SAMPLE } = process.env;
    if (!source || !PDC_VSCODE_EXECUTABLE || !PDC_COMPLETION_SERVER) throw new Error('Supply a YML path and both executable paths.');
    const output = path.join(root, 'target/performance-results');
    fs.mkdirSync(output, { recursive: true });
    const run = fs.mkdtempSync(path.join(output, 'transcode-'));
    const workspace = path.join(run, 'workspace');
    const report = path.join(run, 'report.json');
    const loc = path.join(workspace, 'localisation');
    fs.mkdirSync(loc, { recursive: true });
    fs.mkdirSync(path.join(workspace, '.vscode'));
    const original = fs.readFileSync(source);
    const sparse = Number(process.env.PDC_TRANSCODE_SPARSE_SAMPLES || 0);
    assert.ok(Number.isSafeInteger(sparse) && sparse >= 0);
    const encoded = sparse ? require('node:child_process').execFileSync(PDC_COMPLETION_SERVER,
        ['--transcode-stdio', 'localisation', 'encode'], { input: original, maxBuffer: 4 * original.length + 32 }) : original;
    for (const [index, value] of (sizes.length ? sizes : ['1']).entries()) {
        const times = Number(value);
        assert.ok(Number.isSafeInteger(times) && times > 0);
        const parts = Array(times).fill(encoded);
        if (sparse) parts.push(Buffer.from('\r\n# 中'));
        fs.writeFileSync(path.join(loc, 'sample' + index + '_l_english.yml'), Buffer.concat(parts));
    }
    if (PDC_SCRIPT_SAMPLE) fs.copyFileSync(PDC_SCRIPT_SAMPLE, path.join(workspace, 'sample.txt'));
    fs.writeFileSync(path.join(workspace, '.vscode/settings.json'), JSON.stringify({
        'paradoxcode.serverPath': path.resolve(PDC_COMPLETION_SERVER), 'paradoxcode.vanilla.mode': 'disabled',
        'paradoxcode.localisation.encodingMode': sparse ? 'manual' : 'automatic', 'paradoxcode.localisation.transparentEncoding': true,
        'files.autoSave': 'off',
    }));
    console.log(report);
    const { runTests } = require(path.join(sourceExtension, 'node_modules/@vscode/test-electron'));
    await runTests({
        vscodeExecutablePath: PDC_VSCODE_EXECUTABLE, extensionDevelopmentPath: extension, extensionTestsPath: __filename,
        launchArgs: ['--disable-extensions', '--user-data-dir', path.join(run, 'profile'), '--extensions-dir', path.join(run, 'extensions'), workspace],
        extensionTestsEnv: { PDC_TRANSCODE_REPORT: report },
        stdout: fs.createWriteStream(path.join(run, 'stdout.log')), stderr: fs.createWriteStream(path.join(run, 'stderr.log')),
    });
    console.log(fs.readFileSync(report, 'utf8'));
}

if (require.main === module) main().catch(error => { console.error(error); process.exitCode = 1; });
