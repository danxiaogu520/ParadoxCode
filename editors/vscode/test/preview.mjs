import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { runInNewContext } from 'node:vm';

const extension = join(dirname(fileURLToPath(import.meta.url)), '..');
const realRequire = createRequire(import.meta.url);
let now = 0;
let discoveries = 0;
let fontDiscoveries = 0;
const settings = {};
const document = {
    uri: { fsPath: '/mod/missions/tree.txt', toString: () => 'file:///mod/missions/tree.txt' },
    version: 1, getText: () => 'tree = { slot = 1 }',
};
const messages = [];
class TokenSource {
    constructor() { this.token = { isCancellationRequested: false }; }
    cancel() { this.token.isCancellationRequested = true; }
    dispose() {}
}
const vscode = {
    CancellationTokenSource: TokenSource,
    workspace: {
        getConfiguration: () => ({ get: (key, fallback) => settings[key] ?? fallback }),
        workspaceFolders: [{ uri: { fsPath: '/mod' } }], textDocuments: [document],
        getWorkspaceFolder: () => ({ uri: { fsPath: '/mod' } }),
    },
    window: { activeTextEditor: { document } }, l10n: { t: value => value },
};
class Store {
    async spriteUrls() { return {}; }
    async flagUrls(tags) { return tags.has('FRA') ? new Map([['FRA', flagUrl]]) : new Map(); }
    async loadFonts() { return {}; }
}
const flagUrl = 'data:image/png;base64,flag';
const module = { exports: {} };
const imports = id => {
    if (id === 'vscode') return vscode;
    if (id === './gameAssets') return {
        FRAME_SPRITE: 'frame', GameAssetStore: Store,
        findGameDirectory: () => { discoveries++; return '/game'; },
        findChineseFontMod: () => { fontDiscoveries++; return '/font'; },
    };
    if (id === './transparentLoc') return { realUriOf: doc => doc.uri };
    if (id === './hoverCards') return realRequire(join(extension, 'out/hoverCards.js'));
    if (id === './webviewI18n' || id === 'vscode-languageclient/node') return {};
    return realRequire(id);
};
runInNewContext(`(function(require, module, exports) { ${readFileSync(join(extension, 'out/previewPanel.js'), 'utf8')}\n})`,
    { Date: { now: () => now } })(imports, module, module.exports);
const panelClass = module.exports.MissionPreviewPanel;
const initialStore = panelClass.store();
for (let i = 0; i < 100; i++) assert.equal(panelClass.store(), initialStore);
assert.equal(discoveries, 1);
assert.equal(fontDiscoveries, 1);
settings['preview.chineseFontMod'] = '/new-font';
panelClass.store();
assert.equal(discoveries, 2, 'settings immediately invalidate discovery');
now = 30_000;
panelClass.store();
assert.equal(discoveries, 3, 'new installations are discovered periodically');

panelClass.panel = { webview: { postMessage: message => messages.push(message) } };
const pending = [];
const client = { sendRequest: (_method, _params, token) => new Promise((resolve, reject) => pending.push({ resolve, reject, token })) };
const first = panelClass.refresh(client);
const second = panelClass.refresh(client);
assert.equal(pending[0].token.isCancellationRequested, true);
pending[0].reject(new Error('obsolete response'));
await first;
assert.equal(messages.some(message => message.type === 'error'), false, 'cancelled failures cannot replace a newer preview');
const payload = { documentUri: document.uri.toString(), documentVersion: 1, nodes: [], arrows: [], groups: [], external: [], diagnostics: [] };
pending[1].resolve(payload);
await second;
assert.equal(messages.filter(message => message.type === 'preview').length, 1);
const third = panelClass.refresh(client);
document.version++;
pending[2].resolve(payload);
await third;
assert.equal(messages.filter(message => message.type === 'preview').length, 1, 'edited documents reject stale previews');
console.log('mission preview discovery and cancellation checks passed');

export function createRenderer(script) {
    let measures = 0;
    const painted = [];
    const images = [];
    const handlers = new Map();
    const frames = [];
    const canvasContext = new Proxy({
        measureText: text => { measures++; return { width: text.length * 7 }; },
        fillText: (...args) => painted.push(args),
        drawImage: (...args) => images.push(args),
    }, { get: (target, name) => target[name] ?? (() => {}) });
    class Element {
        constructor() {
            this.dataset = {}; this.style = {}; this.value = ''; this.children = []; this.events = new Map();
            this.clientWidth = 1600; this.clientHeight = 1600;
            this.classList = { add() {}, remove() {}, toggle() {} };
        }
        addEventListener(name, fn) { this.events.set(name, fn); }
        setAttribute() {}
        append(...children) { this.children.push(...children); }
        appendChild(child) { this.append(child); }
        replaceChildren(...children) { this.children = children; }
        getContext() { return canvasContext; }
        getBoundingClientRect() { return { left: 0, top: 0 }; }
        querySelectorAll() { return []; }
    }
    const elements = new Map();
    const doc = {
        body: {}, documentElement: {}, querySelectorAll: () => [],
        getElementById: id => {
            if (!elements.has(id)) elements.set(id, new Element());
            return elements.get(id);
        },
        createElement: () => new Element(), createDocumentFragment: () => new Element(),
    };
    const window = {
        devicePixelRatio: 1, LocFormat: realRequire(join(extension, 'media/loc-format.js')),
        requestAnimationFrame: callback => frames.push(callback),
        addEventListener: (name, callback) => handlers.set(name, callback),
    };
    runInNewContext(script, { window, document: doc,
        acquireVsCodeApi: () => ({ postMessage() {}, getState: () => ({}), setState() {} }),
        getComputedStyle: () => ({ fontFamily: 'sans-serif', color: '#ffffff', getPropertyValue: () => '' }),
        Image: class {
            complete = true;
            naturalWidth = 32;
            naturalHeight = 32;
        },
    });
    const flush = () => { while (frames.length) frames.shift()(); };
    flush();
    return {
        click: id => { elements.get(id).events.get('click')({}); flush(); },
        message: data => { handlers.get('message')({ data }); flush(); },
        resize: () => { handlers.get('resize')(); flush(); },
        painted, images, get measures() { return measures; },
    };
}

const view = createRenderer(readFileSync(join(extension, 'media/renderer.js'), 'utf8'));
const nodes = Array.from({ length: 80 }, (_, index) => ({
    tree: 0, mission: index, id: `mission_${index}`, x: index % 10 * 132, y: Math.floor(index / 10) * 152,
    title: { language: 'english', value: `§YMission ${index}§! long title` },
}));
view.message({ type: 'preview', payload: { ...payload, nodes } });
const firstMeasurements = view.measures;
assert.ok(firstMeasurements > 0);
const firstPaint = JSON.stringify(view.painted);
view.painted.length = 0;
view.message({ type: 'options', gameFonts: false });
assert.ok(view.measures - firstMeasurements < firstMeasurements / 5,
    'unchanged frames only measure drawn text runs, reusing wrapping and centering');
assert.equal(JSON.stringify(view.painted), firstPaint, 'cached layouts preserve the drawn text and positions');
const beforeResize = view.measures;
view.resize();
assert.ok(view.measures - beforeResize >= firstMeasurements, 'theme/viewport changes invalidate title layouts');
console.log('mission preview title layout checks passed');

const focusPayload = { ...payload, nodes: [{ ...nodes[0], x: 3000, title: { value: 'Zed' } }],
    groups: [{ tree: 0, label: 'branch', x: 0, y: 0 }] };
view.message({ type: 'preview', payload: focusPayload });
view.painted.length = 0;
view.click('series-none');
assert.ok(!view.painted.some(([text]) => text === 'Zed'));
view.message({ type: 'preview', payload: focusPayload, focusMission: 0 });
assert.ok(view.painted.some(([text, x, y]) => text === 'Zed' && Math.abs(x - 800) < 100 && Math.abs(y - 800) < 100),
    'locating a node reveals its hidden series and centers it on the canvas');

const formatted = { ...focusPayload, nodes: [{ ...nodes[0], title: { value: 'First\\n@FRA' } }] };
const formattedView = createRenderer(readFileSync(join(extension, 'media/renderer.js'), 'utf8'));
formattedView.message({ type: 'preview', payload: formatted });
assert.ok(formattedView.painted.find(([text]) => text === 'First')[2] <
    formattedView.painted.find(([text]) => text === '@FRA')[2], 'escaped newline puts the tag on the next line');
formattedView.painted.length = 0;
formattedView.message({ type: 'assets', textures: { '@FRA': flagUrl } });
assert.ok(formattedView.images.some(([image, _x, _y, width, height]) => image.src === flagUrl && width === height && height > 0));
assert.ok(formattedView.painted.every(([text]) => text !== '@FRA'));
formattedView.painted.length = 0;
formattedView.message({ type: 'options', showTextures: false });
assert.ok(formattedView.painted.some(([text]) => text === '@FRA'), 'texture visibility restores the text fallback');
const fourth = panelClass.refresh(client);
pending[3].resolve({ ...formatted, documentVersion: document.version });
await fourth;
await new Promise(resolve => setImmediate(resolve));
assert.equal(messages.find(message => message.type === 'assets')?.textures['@FRA'], flagUrl);

// Exercise the event panel's HTML, text-only fallback, and source navigation.
{
    const htmlLocation = { uri: 'file:///mod/localisation/a.yml', range: { start: { line: 3, character: 2 }, end: { line: 3, character: 5 } } };
    const eventLocation = { ...htmlLocation, uri: 'file:///mod/events/a.txt' };
    const received = {};
    const listen = name => callback => { received[name] = callback; return { dispose() { delete received[name]; } }; };
    const panel = { visible: true, viewColumn: 2, webview: { html: '', onDidReceiveMessage: listen('message') },
        onDidDispose: listen('close'), onDidChangeViewState: listen('visible'), reveal() {}, dispose() { received.close(); } };
    const uri = value => ({ toString: () => value });
    let opened;
    const api = { ...vscode, Uri: { parse: uri }, ViewColumn: { One: 1, Beside: -2 },
        Position: class { constructor(line, character) { Object.assign(this, { line, character }); } },
        Range: class { constructor(start, end) { Object.assign(this, { start, end }); } },
        workspace: { openTextDocument: async uri => ({ uri }), onDidChangeTextDocument: listen('edit') },
        window: { createWebviewPanel: () => panel, visibleTextEditors: [], showTextDocument(doc, options) { opened = { doc, options }; } } };
    const exports = {};
    const card = { kind: 'event', event: { id: 'a.1', titleKey: 'a.t', title: { value: '§G<Title>§!' },
        descKey: 'a.d', desc: { value: 'Description' }, conditionalDesc: eventLocation,
        sources: { 'a.t': htmlLocation, 'a.d': htmlLocation }, options: [] } };
    const store = new Store();
    store.textureFile = async () => ({ url: 'data:image/png;base64,chrome', width: 656, height: 223 });
    const imports = id => {
        if (id === 'vscode') return api;
        if (id === './previewPanel') return { MissionPreviewPanel: { store: () => store } };
        if (id === './hoverCards') return realRequire(join(extension, 'out/hoverCards.js'));
        if (id === './transparentLoc') return { openTextDocumentForNavigation: async uri => ({ uri }) };
        return realRequire(id);
    };
    runInNewContext(readFileSync(join(extension, 'out/eventPreviewPanel.js'), 'utf8'),
        { exports, require: imports, setTimeout, clearTimeout });
    const controller = new exports.EventPreviewPanel(() => ({ sendRequest: async () => ({ version: 1, card }) }));
    await controller.show({ kind: 'event', id: 'a.1', location: eventLocation });
    assert.ok(panel.webview.html.includes('&lt;Title&gt;'));
    assert.ok(panel.webview.html.includes('Conditional description'));
    const generation = Number(panel.webview.html.match(/generation:(\d+)/)[1]);
    await received.message({ generation, index: 1 });
    assert.equal(opened.doc.uri.toString(), htmlLocation.uri);
    assert.equal(opened.options.selection.start.line, 3);
    await received.message({ generation, index: 2 });
    assert.equal(opened.doc.uri.toString(), eventLocation.uri);

    card.cardAssets = { backgroundTop: { path: '/top' }, backgroundMiddle: { path: '/middle' } };
    card.event.desc.value = '§GFirst @FRA\\n\\nSecond @ZZZ§!\\n' + 'long text\\n'.repeat(900) + 'Last description line';
    card.event.options.push({ nameKey: 'a.opt', name: { value: 'Continue' } });
    await controller.show({ kind: 'event', id: 'a.1', location: eventLocation });
    assert.ok(panel.webview.html.includes('style="color:#2cab32">&lt;Title&gt;'));
    assert.ok(panel.webview.html.includes('style="color:#18701a">First </span><img class="loc-flag" alt="@FRA"'));
    assert.match(panel.webview.html, /\n\nSecond <\/span><span style="color:#18701a">@ZZZ/,
        'blank lines and missing flag text survive formatting');
    assert.ok(panel.webview.html.includes('repeat-y'), 'long events keep their game background');
    assert.ok(panel.webview.html.indexOf('Last description line') < panel.webview.html.indexOf('Continue'));
    controller.dispose();
    assert.equal(received.edit, undefined);
}
