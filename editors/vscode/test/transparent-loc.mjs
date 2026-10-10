// Exercise the compiled extension with controlled VS Code events and codec responses.
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync, writeFileSync, mkdirSync } from 'node:fs';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
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
        settings, active: undefined,
        emit(name, value) { for (const callback of callbacks[name] ?? []) callback(value); },
        show(document) { this.active = document ? { document } : undefined; this.emit('active', this.active); },
    };
    const vscode = {
        Uri,
        Disposable: class { constructor(dispose) { this.dispose = dispose; } static from() { return { dispose() {} }; } },
        EventEmitter: class { event = () => ({ dispose() {} }); fire() {} dispose() {} },
        StatusBarAlignment: { Left: 1 },
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
            async showTextDocument(document) { state.opens.push(document); return { document }; },
            showInformationMessage(message) { state.messages.push(message); },
            showErrorMessage(message) { state.messages.push(message); },
        },
        workspace: {
            textDocuments: [],
            getConfiguration: () => ({ get: (key, fallback) => settings[key] ?? fallback }),
            getWorkspaceFolder: (uri) => uri.fsPath.startsWith(`${root}/`) ? { uri: Uri.file(root) } : undefined,
            registerFileSystemProvider(scheme, provider) {
                state.providers.push({ scheme, provider }); return { dispose() {} };
            },
            async openTextDocument(uri) { return { uri }; },
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
            if (id === './agent/server') return {
                acquireAgentClient: async () => ({
                    async sendRequest(method, params) {
                        state.requests.push({ method, params });
                        return state.respond(method, params);
                    },
                }),
                withTimeout: (promise) => promise,
            };
            return require(id);
        },
    }, { filename: compiled });
    state.navigate = uri => exports.openTextDocumentForNavigation(uri);
    state.activate = () => exports.activateTransparentLocalisation({ subscriptions: [] }, { appendLine() {} });
    state.document = (relative) => ({
        uri: Uri.file(join(root, relative)), isDirty: false, getText: () => 'name = "x"\n',
    });
    state.vscode = vscode;
    return state;
}

function visible(menu, command, state) {
    const item = packageJson.contributes.menus[menu].find((entry) => entry.command === command);
    assert.ok(item, `${menu} must contribute ${command}`);
    const context = {
        'config.paradoxcode.localisation.transparentEncoding': state.settings.transparentEncoding ?? false,
        resourceScheme: state.active?.document.uri.scheme,
        'paradoxcode.transcodeEligible': state.contexts.get('paradoxcode.transcodeEligible'),
    };
    return item.when.split('&&').every((condition) => {
        const [key, expected] = condition.trim().split(/\s*==\s*/);
        return expected === undefined ? !!context[key]
            : context[key] === (expected === 'true' ? true : expected === 'false' ? false : expected);
    });
}

const root = mkdtempSync(join(tmpdir(), 'pdc-transparent-loc-'));
try {
    await test('default activation exposes manual buttons without registering or opening pdcloc', async () => {
        const state = host(root);
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
        const state = host(root);
        await state.activate();
        state.show(state.document('events/demo.txt'));
        assert.equal(state.contexts.get('paradoxcode.transcodeEligible'), true);
        state.emit('open', state.document('docs/readme.md'));
        assert.equal(state.contexts.get('paradoxcode.transcodeEligible'), true);
        state.settings.transparentScriptGlobs = [];
        state.emit('configuration', { affectsConfiguration: (key) => key.endsWith('transparentScriptGlobs') });
        assert.equal(state.contexts.get('paradoxcode.transcodeEligible'), false);
        state.show(state.document('localisation/demo.yml'));
        assert.equal(state.contexts.get('paradoxcode.transcodeEligible'), true);
        const twin = state.document('localisation/demo.yml');
        twin.uri = twin.uri.with({ scheme: 'pdcloc' });
        state.show(twin);
        assert.equal(visible('editor/title', 'paradoxcode.localisation.decodeFile', state), false);
        state.show(undefined);
        assert.equal(state.contexts.get('paradoxcode.transcodeEligible'), false);
    });

    await test('explicit opt-in keeps experimental commands in the palette and hides manual buttons', async () => {
        const state = host(root, { transparentEncoding: true });
        state.show(state.document('localisation/demo.yml'));
        await state.activate();
        assert.equal(state.providers.length, 1);
        assert.equal(state.providers[0].scheme, 'pdcloc');
        assert.ok(state.commands.has('paradoxcode.localisation.openDecoded'));
        assert.equal(visible('commandPalette', 'paradoxcode.localisation.openDecoded', state), true);
        const twin = state.document('localisation/demo.yml');
        twin.uri = twin.uri.with({ scheme: 'pdcloc' });
        state.active = { document: twin };
        assert.ok(state.commands.has('paradoxcode.localisation.peekOriginal'));
        assert.equal(visible('commandPalette', 'paradoxcode.localisation.peekOriginal', state), true);
        assert.equal(visible('editor/title', 'paradoxcode.localisation.decodeFile', state), false);
        assert.equal(visible('editor/title', 'paradoxcode.localisation.encodeFile', state), false);
    });

    await test('search navigation preserves a raw file even when decoded auto-open is enabled', async () => {
        const state = host(root, { transparentEncoding: true, autoOpen: 'always' });
        await state.activate();
        const document = state.document('localisation/navigation.yml');
        await state.navigate(document.uri);
        state.show(document); state.emit('open', document);
        await Promise.resolve(); await Promise.resolve();
        assert.equal(state.requests.length, 0);
        assert.equal(state.opens.length, 0);
    });

    await test('manual decode and encode update ordinary files and preserve the input backup', async () => {
        const state = host(root);
        const document = state.document('localisation/manual.yml');
        const readable = Buffer.from('l_english:\n KEY:0 "中文"\n');
        const escaped = Buffer.from('l_english:\n KEY:0 "\x10-N\x10‡e"\n');
        mkdirSync(dirname(document.uri.fsPath), { recursive: true });
        writeFileSync(document.uri.fsPath, escaped);
        state.show(document);
        await state.activate();
        state.respond = () => ({ eligible: true, form: 'whole', bytes: readable.toString('hex'), broken: 0 });
        await state.commands.get('paradoxcode.localisation.decodeFile')(document.uri);
        assert.deepEqual(readFileSync(document.uri.fsPath), readable);
        assert.deepEqual(readFileSync(`${document.uri.fsPath}.pre-transcode.bak`), escaped);
        state.respond = (method) => method === 'pdc/transcodeDecode'
            ? { eligible: true, form: 'plain', bytes: readable.toString('hex'), quotedCjk: true }
            : { bytes: escaped.toString('hex') };
        await state.commands.get('paradoxcode.localisation.encodeFile')(document.uri);
        assert.deepEqual(readFileSync(document.uri.fsPath), escaped);
        assert.deepEqual(readFileSync(`${document.uri.fsPath}.pre-transcode.bak`), readable);
        assert.equal(state.opens.length, 0);
        assert.equal(document.uri.scheme, 'file');
    });

    await test('manual conversion refuses unsaved edits before requesting a codec or writing', async () => {
        const state = host(root);
        const document = state.document('localisation/dirty.yml');
        document.isDirty = true;
        state.vscode.workspace.textDocuments.push(document);
        state.show(document);
        await state.activate();
        await state.commands.get('paradoxcode.localisation.decodeFile')(document.uri);
        await state.commands.get('paradoxcode.localisation.encodeFile')(document.uri);
        assert.equal(state.requests.length, 0);
        assert.equal(state.messages.length, 2);
        assert.match(state.messages[0], /save or close/i);
    });
} finally {
    rmSync(root, { recursive: true, force: true });
}
