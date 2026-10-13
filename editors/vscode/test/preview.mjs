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
    async loadFonts() { return {}; }
}
const module = { exports: {} };
const imports = id => {
    if (id === 'vscode') return vscode;
    if (id === './gameAssets') return {
        FRAME_SPRITE: 'frame', GameAssetStore: Store,
        findGameDirectory: () => { discoveries++; return '/game'; },
        findChineseFontMod: () => { fontDiscoveries++; return '/font'; },
    };
    if (id === './transparentLoc') return { realUriOf: doc => doc.uri };
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
    const handlers = new Map();
    const frames = [];
    const canvasContext = new Proxy({
        measureText: text => { measures++; return { width: text.length * 7 }; },
        fillText: (...args) => painted.push(args),
    }, { get: (target, name) => target[name] ?? (() => {}) });
    class Element {
        constructor() {
            this.dataset = {}; this.style = {}; this.value = ''; this.children = [];
            this.clientWidth = 1600; this.clientHeight = 1600;
            this.classList = { add() {}, remove() {}, toggle() {} };
        }
        addEventListener() {}
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
        Image: class {},
    });
    const flush = () => { while (frames.length) frames.shift()(); };
    flush();
    return {
        message: data => { handlers.get('message')({ data }); flush(); },
        resize: () => { handlers.get('resize')(); flush(); },
        painted, get measures() { return measures; },
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
