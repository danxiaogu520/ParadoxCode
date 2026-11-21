// Exercise the compiled extension with controlled VS Code events and codec responses.
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync, writeFileSync, mkdirSync } from 'node:fs';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { dirname, join, sep } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { test } from 'node:test';
import { runInNewContext } from 'node:vm';

const extension = join(dirname(fileURLToPath(import.meta.url)), '..');
const packageJson = JSON.parse(readFileSync(join(extension, 'package.json'), 'utf8'));
const compiled = join(extension, 'out/transparentLoc.js');

class Uri {
    constructor(fsPath, scheme = 'file') { this.fsPath = fsPath; this.scheme = scheme; }
    static file(fsPath) { return new Uri(fsPath); }
    with({ scheme }) { return new Uri(this.fsPath, scheme); }
    toString() { return pathToFileURL(this.fsPath).href.replace(/^file:/, `${this.scheme}:`); }
}

function host(root, settings = {}) {
    const callbacks = {};
    const event = (name) => (callback) => {
        (callbacks[name] ??= []).push(callback);
        return { dispose() {} };
    };
    const state = {
        contexts: new Map(), commands: new Map(), providers: [], requests: [], messages: [], opens: [],
        settings, active: undefined, wholeFileTranscode: true,
        emit(name, value) { for (const callback of callbacks[name] ?? []) callback(value); },
        show(document) {
            this.active = document ? { document, async edit(build) {
                build({ replace: (range, text) => {
                    const [start, end] = range.args;
                    state.lastEdit = { start: start.character, end: end.character };
                    document.text = document.text.slice(0, start.character) + text + document.text.slice(end.character);
                    document.isDirty = true;
                    document.version++;
                } });
                return true;
            } } : undefined;
            this.emit('active', this.active);
        },
    };
    const vscode = {
        Uri,
        Disposable: class { constructor(dispose) { this.dispose = dispose; } static from() { return { dispose() {} }; } },
        EventEmitter: class { event = () => ({ dispose() {} }); fire() {} dispose() {} },
        StatusBarAlignment: { Left: 1 },
        Diagnostic: class { constructor(range, message, severity) { Object.assign(this, { range, message, severity }); } },
        Range: class { constructor(...args) { this.args = args; } },
        DiagnosticSeverity: { Error: 0, Warning: 1, Information: 2 },
        FileSystemError: { NoPermissions: message => new Error(message), Unavailable: message => new Error(message) },
        l10n: { t: (message, ...args) => message.replace(/\{(\d+)\}/g, (_, i) => args[i]) },
        commands: {
            registerCommand(name, callback) { state.commands.set(name, callback); return { dispose() {} }; },
            async executeCommand(name, key, value) {
                assert.equal(name, 'setContext'); state.contexts.set(key, value);
            },
        },
        languages: { createDiagnosticCollection: () => ({ dispose() {}, delete() {}, set() {} }) },
        window: {
            get activeTextEditor() { return state.active; },
            onDidChangeActiveTextEditor: event('active'),
            createStatusBarItem: () => ({ show() {}, hide() {}, dispose() {} }),
            tabGroups: { all: [], onDidChangeTabs: event('tabs') },
            async showTextDocument(document) { state.opens.push(document); state.show(document); return state.active; },
            showInformationMessage(message) { state.messages.push(message); },
            showErrorMessage(message) { state.messages.push(message); },
        },
        workspace: {
            textDocuments: [],
            getConfiguration: () => ({
                get: (key, fallback) => settings[key] ?? fallback,
                inspect: key => ({
                    workspaceValue: settings[key],
                    defaultValue: packageJson.contributes.configuration.properties[`paradoxcode.localisation.${key}`]?.default,
                }),
            }),
            getWorkspaceFolder: (uri) => uri.fsPath.startsWith(`${root}${sep}`) ? { uri: Uri.file(root) } : undefined,
            registerFileSystemProvider(scheme, provider) {
                state.providers.push({ scheme, provider }); return { dispose() {} };
            },
            async openTextDocument(uri) {
                return this.textDocuments.find(doc => doc.uri.toString() === uri.toString()) ?? { uri };
            },
            onDidChangeWorkspaceFolders: event('folders'),
            onDidChangeConfiguration: event('configuration'),
            onDidOpenTextDocument: event('open'),
            onDidChangeTextDocument: event('change'),
            onDidSaveTextDocument: event('save'),
            onDidCloseTextDocument: event('close'),
        },
    };
    const require = createRequire(compiled);
    const exports = {};
    runInNewContext(readFileSync(compiled, 'utf8'), {
        exports, Buffer, setTimeout, clearTimeout,
        require(id) {
            if (id === 'vscode') return vscode;
            if (id === './scriptSelection') return { convertSelection() {} };
            if (id === './byteCodec') return { ConversionRefused: Error, async convertBytes(profile, direction, bytes) {
                const method = direction === 'encode' ? 'pdc/transcodeEncode' : 'pdc/transcodeDecode';
                const params = {bytes: Buffer.from(bytes).toString('hex'), profile};
                state.requests.push({method, params});
                if (!state.wholeFileTranscode) throw new Error('updated ParadoxCode server required');
                const response = state.respond(method, params);
                if (response.refused === 'brokenEscape') throw new Error('Damaged escape sequence at byte ' + response.offsets[0]);
                if (response.refused) throw new Error('cannot be round-tripped');
                return Buffer.from(response.bytes, 'hex');
            } };
            return require(id);
        },
    }, { filename: compiled });
    state.navigate = uri => exports.openTextDocumentForNavigation(uri);
    state.activate = () => exports.activateTransparentLocalisation({ subscriptions: [] }, { appendLine() {} });
    state.document = (relative, text = 'name = "x"\n') => {
        const doc = { uri: Uri.file(join(root, relative)), text, isDirty: false, version: 1, encoding: 'utf8',
            getText() { return this.text; }, positionAt(offset) { return { line: 0, character: offset }; } };
        vscode.workspace.textDocuments.push(doc);
        return doc;
    };
    state.vscode = vscode;
    return state;
}

function visible(menu, command, state) {
    const item = packageJson.contributes.menus[menu].find((entry) => entry.command === command);
    assert.ok(item, `${menu} must contribute ${command}`);
    const context = {
        'paradoxcode.automaticLocalisation': state.contexts.get('paradoxcode.automaticLocalisation'),
        resourceScheme: state.active?.document.uri.scheme,
        'paradoxcode.transcodeEligible': state.contexts.get('paradoxcode.transcodeEligible'),
    };
    return item.when.split('&&').every((condition) => {
        const [key, expected] = condition.trim().split(/\s*==\s*/);
        return expected === undefined ? (key.startsWith('!') ? !context[key.slice(1)] : !!context[key])
            : context[key] === (expected === 'true' ? true : expected === 'false' ? false : expected);
    });
}

const root = mkdtempSync(join(tmpdir(), 'pdc-transparent-loc-'));
try {
    await test('Windows save retries revalidate the destination before publishing the staged bytes', async () => {
        const compiledReplace = join(extension, 'out/replaceFile.js');
        const require = createRequire(compiledReplace);
        const fs = require('node:fs/promises');
        let attempts = 0;
        const exports = {};
        runInNewContext(readFileSync(compiledReplace, 'utf8'), {
            exports, process: { platform: 'win32' },
            require(id) {
                if (id === 'node:timers/promises') return { setTimeout: async () => {} };
                if (id === 'node:fs/promises') return { ...fs, async rename(from, to) {
                    if (++attempts === 1) throw Object.assign(new Error('sharing violation'), { code: 'EPERM' });
                    return fs.rename(from, to);
                } };
                return require(id);
            },
        });
        const file = join(root, 'retry.yml');
        for (const changed of [false, true]) {
            attempts = 0;
            let checks = 0;
            writeFileSync(file, 'original');
            const work = exports.replaceFile(file, Buffer.from('converted'), async () => {
                if (++checks === 2 && changed) throw new Error('destination changed');
            });
            if (changed) await assert.rejects(work, /destination changed/);
            else await work;
            assert.equal(checks, 2);
            assert.equal(readFileSync(file, 'utf8'), changed ? 'original' : 'converted');
        }
    });

    await test('explicit manual mode exposes manual buttons without registering or opening pdcloc', async () => {
        const state = host(root, { encodingMode: 'manual' });
        state.show(state.document('localisation/demo.yml'));
        await state.activate();
        assert.equal(packageJson.contributes.configuration.properties['paradoxcode.localisation.transparentEncoding'].default, false);
        assert.deepEqual(packageJson.contributes.configuration.properties['paradoxcode.localisation.transparentEncoding'].tags, ['experimental']);
        assert.equal(state.providers.length, 0);
        assert.equal(state.opens.length, 0);
        for (const command of ['decodeFile', 'encodeFile']) {
            const name = `paradoxcode.localisation.${command}`;
            assert.ok(state.commands.has(name));
            assert.equal(visible('editor/title', name, state), true);
        }
        for (const command of ['openDecoded', 'revealOriginal', 'peekOriginal', 'endPeek']) {
            const name = `paradoxcode.localisation.${command}`;
            assert.equal(state.commands.has(name), false);
            assert.equal(visible('commandPalette', name, state), false);
        }
    });

    await test('button eligibility follows the active editor and configuration while disabled', async () => {
        const state = host(root, { encodingMode: 'manual' });
        await state.activate();
        state.show(state.document('events/demo.txt'));
        assert.equal(state.contexts.get('paradoxcode.transcodeEligible'), false);
        for (const direction of ['encodeFile', 'decodeFile']) {
            await state.commands.get('paradoxcode.localisation.' + direction)(state.active.document.uri);
        }
        assert.equal(state.requests.length, 0);
        state.show(state.document('localisation/demo.yml'));
        assert.equal(state.contexts.get('paradoxcode.transcodeEligible'), true);
        const twin = state.document('localisation/demo.yml');
        twin.uri = twin.uri.with({ scheme: 'pdcloc' });
        state.show(twin);
        assert.equal(visible('editor/title', 'paradoxcode.localisation.decodeFile', state), false);
        state.show(undefined);
        assert.equal(state.contexts.get('paradoxcode.transcodeEligible'), false);
    });

    await test('legacy opt-outs still use manual management', async () => {
        for (const settings of [{ transparentEncoding: false }, { autoOpen: 'off' }]) {
            const state = host(root, settings);
            await state.activate();
            assert.equal(state.providers.length, 0);
            assert.equal((await state.navigate(state.document('localisation/demo.yml').uri)).uri.scheme, 'file');
        }
    });

    await test('the legacy opt-in enables the managed view and hides manual buttons', async () => {
        const state = host(root, { transparentEncoding: true });
        await state.activate();
        state.show(state.document('localisation/demo.yml'));
        assert.equal(state.providers.length, 1);
        assert.equal(state.providers[0].scheme, 'pdcloc');
        assert.ok(state.commands.has('paradoxcode.localisation.openDecoded'));
        assert.equal(visible('commandPalette', 'paradoxcode.localisation.openDecoded', state), true);
        await new Promise(resolve => setImmediate(resolve));
        assert.equal(state.active.document.uri.scheme, 'pdcloc');
        assert.ok(state.commands.has('paradoxcode.localisation.peekOriginal'));
        assert.equal(visible('commandPalette', 'paradoxcode.localisation.peekOriginal', state), true);
        assert.equal(visible('editor/title', 'paradoxcode.localisation.decodeFile', state), false);
        assert.equal(visible('editor/title', 'paradoxcode.localisation.encodeFile', state), false);
    });

    await test('the default mode navigates to readable YML and leaves TXT on its original URI', async () => {
        const state = host(root);
        await state.activate();
        assert.equal((await state.navigate(state.document('localisation/navigation.yml').uri)).uri.scheme, 'pdcloc');
        assert.equal((await state.navigate(state.document('events/example.txt').uri)).uri.scheme, 'file');
        state.settings.encodingMode = 'manual';
        state.emit('configuration', { affectsConfiguration: () => true });
        assert.equal((await state.navigate(state.document('localisation/navigation.yml').uri)).uri.scheme, 'file');
    });

    await test('manual commands replace the target buffer, including unsaved edits, without writing or backing up the file', async () => {
        const state = host(root, { encodingMode: 'manual' });
        const document = state.document('localisation/manual.yml');
        const readable = Buffer.from('l_english:\n # 中文注释\n 中文键:0 "中文"\n');
        const escaped = Buffer.from('\uFEFFl_english:\n KEY:0 "\x10-N\x10‡e"\n');
        mkdirSync(dirname(document.uri.fsPath), { recursive: true });
        writeFileSync(document.uri.fsPath, escaped);
        document.text = escaped.toString() + '# pending';
        document.isDirty = true;
        await state.activate();
        state.respond = () => ({ eligible: true, form: 'whole', bytes: readable.toString('hex'), broken: 0 });
        await state.commands.get('paradoxcode.localisation.decodeFile')(document.uri);
        assert.equal(Buffer.from(state.requests[0].params.bytes, 'hex').toString(), escaped.toString() + '# pending');
        assert.equal(document.text, readable.toString());
        assert.deepEqual(readFileSync(document.uri.fsPath), escaped);
        state.respond = (method) => method === 'pdc/transcodeDecode'
            ? { eligible: true, form: 'plain', bytes: readable.toString('hex'), needsEncoding: true }
            : { bytes: escaped.toString('hex') };
        await state.commands.get('paradoxcode.localisation.encodeFile')(document.uri);
        assert.equal(document.text, escaped.toString());
        assert.equal(document.isDirty, true);
        assert.deepEqual(readFileSync(document.uri.fsPath), escaped);
        assert.throws(() => readFileSync(`${document.uri.fsPath}.pre-transcode.bak`), /ENOENT/);
        assert.deepEqual(state.opens, [document]);
        assert.equal(document.uri.scheme, 'file');
        for (const { params } of state.requests) assert.deepEqual(Object.keys(params).sort(), ['bytes', 'profile']);
    });

    await test('manual conversion preserves the file on codec failure', async () => {
        const state = host(root, { encodingMode: 'manual' });
        state.wholeFileTranscode = false;
        const document = state.document('localisation/old-server.yml');
        mkdirSync(dirname(document.uri.fsPath), { recursive: true });
        writeFileSync(document.uri.fsPath, '# 中文注释\n中文 = "x"\n');
        const original = readFileSync(document.uri.fsPath);
        state.show(document);
        await state.activate();
        await state.commands.get('paradoxcode.localisation.encodeFile')(document.uri);
        assert.equal(state.requests.length, 1);
        assert.deepEqual(readFileSync(document.uri.fsPath), original);
        assert.match(state.messages.at(-1), /updated ParadoxCode server/i);
        assert.throws(() => readFileSync(`${document.uri.fsPath}.pre-transcode.bak`), /ENOENT/);
    });

    await test('damaged comment refuses manual conversion and preserves file and backup', async () => {
        const state = host(root, { encodingMode: 'manual' });
        const document = state.document('localisation/damaged-comment.yml');
        mkdirSync(dirname(document.uri.fsPath), { recursive: true });
        const original = Buffer.from('# \x10\nname = "x"\n');
        document.text = original.toString();
        writeFileSync(document.uri.fsPath, original);
        state.show(document);
        await state.activate();
        state.respond = () => ({ refused: 'brokenEscape', offsets: [2], bytes: original.toString('hex') });
        await state.commands.get('paradoxcode.localisation.decodeFile')(document.uri);
        assert.deepEqual(readFileSync(document.uri.fsPath), original);
        assert.match(state.messages.at(-1), /byte 2/i);
        assert.throws(() => readFileSync(`${document.uri.fsPath}.pre-transcode.bak`), /ENOENT/);
    });

    await test('automatic saves write a BOM and preserve the file on refusal', async () => {
        const state = host(root, { encodingMode: 'automatic' });
        await state.activate();
        const real = state.document('localisation/transparent.yml').uri;
        mkdirSync(dirname(real.fsPath), { recursive: true });
        const readable = Buffer.from('# 中文注释\n中文 = "中文"\n');
        const encoded = Buffer.from('\uFEFF# \x10-N\nkey = "\x10-N"\n');
        writeFileSync(real.fsPath, readable);
        state.respond = () => ({ bytes: encoded.subarray(3).toString('hex') });
        await state.providers[0].provider.writeFile(real.with({ scheme: 'pdcloc' }), readable);
        assert.deepEqual(readFileSync(real.fsPath), encoded);
        assert.equal(state.requests.at(-1).method, 'pdc/transcodeEncode');
        assert.deepEqual(Object.keys(state.requests.at(-1).params).sort(), ['bytes', 'profile']);
        state.respond = () => ({ refused: 'unencodable', points: [{ codePoint: 0x1f600 }] });
        await assert.rejects(() => state.providers[0].provider.writeFile(real.with({ scheme: 'pdcloc' }), Buffer.from('# 😀\n')), /cannot be round-tripped/);
        assert.deepEqual(readFileSync(real.fsPath), encoded);
    });

    await test('manual conversion ignores an obsolete result after another edit', async () => {
        const state = host(root, { encodingMode: 'manual' });
        const document = state.document('localisation/dirty.yml', 'name = "\x10-N"');
        document.isDirty = true;
        state.show(document);
        await state.activate();
        state.respond = () => {
            document.text = 'newer draft';
            document.version++;
            return { bytes: Buffer.from('outdated result').toString('hex') };
        };
        await state.commands.get('paradoxcode.localisation.decodeFile')(document.uri);
        assert.equal(document.text, 'newer draft');
        assert.equal(state.requests.length, 1);
        assert.match(state.messages[0], /changed during conversion/i);
    });

    await test('manual conversion replaces just the changed line between identical CRLF and Unicode lines', async () => {
        const state = host(root, { encodingMode: 'manual' });
        const prefix = 'l_english:\r\n# 😀\r\n';
        const changed = ' key:0 "\x10-N"\r\n';
        const suffix = ' tail:0 "plain"\r\n';
        const doc = state.document('localisation/sparse.yml', prefix + changed + suffix);
        const expected = prefix + ' key:0 "中"\r\n' + suffix;
        state.respond = () => ({ bytes: Buffer.from(expected).toString('hex') });
        await state.activate();
        await state.commands.get('paradoxcode.localisation.decodeFile')(doc.uri);
        assert.deepEqual(state.lastEdit, { start: prefix.length, end: prefix.length + changed.length });
        assert.equal(doc.text, expected);
        await state.commands.get('paradoxcode.localisation.decodeFile')(doc.uri);
        assert.equal(state.requests.length, 1, 'decoding a readable buffer incurs no additional codec request');
    });
} finally {
    rmSync(root, { recursive: true, force: true });
}
