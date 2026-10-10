import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { runInNewContext } from 'node:vm';

const extension = join(dirname(fileURLToPath(import.meta.url)), '..');
const plain = value => JSON.parse(JSON.stringify(value));

function webview() {
    class Element {
        constructor(tag, id) { this.tag = tag; this.id = id; this.children = []; this.attributes = {}; this.dataset = {}; this.listeners = new Map(); this.value = ''; this.hidden = false; this.checked = false; this._text = ''; }
        set innerHTML(_) { throw new Error('File content must use text nodes'); }
        set textContent(value) { this._text = String(value); this.children = []; }
        get textContent() { return this._text + this.children.map(child => child.textContent).join(''); }
        append(...nodes) { this.children.push(...nodes); }
        replaceChildren(...nodes) { this._text = ''; this.children = [...nodes]; }
        setAttribute(name, value) { this.attributes[name] = value; }
        getAttribute(name) { return this.attributes[name]; }
        addEventListener(name, callback) { this.listeners.set(name, callback); }
        querySelectorAll(selector) { return this.children.flatMap(child => [...(selector === 'button' && child.tag === 'button' ? [child] : []), ...child.querySelectorAll(selector)]); }
        focus() { this.focused = true; }
        fire(type, event = {}) { this.listeners.get(type)?.({ preventDefault() {}, ...event }); }
    }
    const ids = new Map();
    const html = readFileSync(join(extension, 'media/search.html'), 'utf8');
    for (const match of html.matchAll(/<(\w+)[^>]*id="([^"]+)"[^>]*>/g)) ids.set(match[2], new Element(match[1], match[2]));
    for (const tab of ['rules', 'definitions', 'localisation', 'text']) { const node = new Element('button'); node.dataset.tab = tab; ids.get('tabs').append(node); }
    ids.get('mode').value = 'value'; ids.get('status').value = 'all';
    const sent = []; let receive;
    const timers = new Map(); let timerId = 0;
    const document = { getElementById: id => { assert.ok(ids.has(id), `missing ${id}`); return ids.get(id); }, createElement: tag => new Element(tag), createTextNode: text => { const node = new Element('text'); node.textContent = text; return node; }, querySelectorAll: () => [], documentElement: {} };
    runInNewContext(readFileSync(join(extension, 'media/search.js'), 'utf8'), {
        document, window: { addEventListener: (_, fn) => { receive = fn; } }, acquireVsCodeApi: () => ({ postMessage: message => sent.push(plain(message)) }),
        setTimeout: fn => { const id = ++timerId; timers.set(id, fn); return id; }, clearTimeout: id => timers.delete(id),
    });
    return { ids, sent, message: data => receive({ data }), flush: () => { const queued = [...timers.values()]; timers.clear(); queued.forEach(fn => fn()); }, input: (id, value) => { ids.get(id).value = value; ids.get(id).fire('input'); }, tab: name => ids.get('tabs').children.find(node => node.dataset.tab === name).fire('click') };
}

const context = { revision: 7, roots: [{ id: 1, kind: 'project', path: '/mod' }, { id: 2, kind: 'dependency', path: '/dep' }], languages: ['english', 'french'], preferredLanguage: 'english', kinds: ['event'], contexts: ['effect'], scopes: ['country'] };
const entry = id => ({ id, name: id, kind: 'localisation', language: 'english', rootId: 1, active: true, text: '<script>needle</script>', location: { uri: 'file:///mod/localisation/test.yml', path: 'localisation/test.yml', range: { start: { line: 1, character: 2 }, end: { line: 1, character: 4 } } } });
const page = (ids, revision = 7, nextOffset = null, loaded = ids.length, totalGroups = loaded, totalMatches = totalGroups) => ({ revision, groups: ids.map(id => ({ id, versions: [entry(id)] })), nextOffset, loaded, totalGroups, totalMatches, limitations: [] });
const lastSearch = view => view.sent.filter(message => message.type === 'search').at(-1);

{
    const view = webview();
    view.message({ type: 'initial', query: 'needle' }); view.message({ type: 'context', context });
    assert.equal(lastSearch(view).params.language, 'english');
    const obsolete = lastSearch(view).sequence;
    view.input('query', 'new needle'); view.flush();
    assert.notEqual(lastSearch(view).sequence, obsolete);
    view.message({ type: 'results', sequence: obsolete, response: page(['obsolete']) });
    assert.equal(view.ids.get('results').children.length, 0);
    view.message({ type: 'results', sequence: lastSearch(view).sequence, response: page(['first'], 7, 1, 1, 2) });
    assert.equal(view.ids.get('results').children.length, 1);
    assert.match(view.ids.get('results').textContent, /<script>needle<\/script>/);
    view.ids.get('more').fire('click');
    assert.equal(lastSearch(view).params.offset, 1); assert.equal(lastSearch(view).params.revision, 7);
    view.message({ type: 'results', sequence: lastSearch(view).sequence, response: page(['second'], 7, null, 2, 2) });
    assert.equal(view.ids.get('results').children.length, 2);
    view.tab('definitions'); view.input('query', 'event'); view.flush();
    view.message({ type: 'results', sequence: lastSearch(view).sequence, response: page(['event']) });
    view.tab('localisation');
    assert.equal(view.ids.get('query').value, 'new needle'); assert.equal(view.ids.get('results').children.length, 2);
    view.ids.get('language').value = ''; view.ids.get('language').fire('change'); view.flush();
    assert.equal(lastSearch(view).params.language, undefined);
    view.message({ type: 'invalidate' }); view.message({ type: 'context', context: { ...context, revision: 8 } });
    assert.equal(lastSearch(view).params.language, undefined, 'all languages survives refresh');
    view.message({ type: 'error', sequence: lastSearch(view).sequence, message: 'disk read failed' });
    assert.equal(view.ids.get('status-message').textContent, 'disk read failed'); assert.equal(view.ids.get('retry').hidden, false);
    view.input('query', ''); view.flush();
    assert.match(view.ids.get('status-message').textContent, /Enter a query/);
}
{
    const view = webview(); view.message({ type: 'context', context });
    const dependency = view.ids.get('roots').children[1].children[0]; dependency.checked = false; dependency.fire('change');
    view.tab('text'); view.ids.get('include').value = 'events/**, notes/**'; view.ids.get('exclude').value = '**/old.txt'; view.ids.get('caseSensitive').checked = true;
    view.input('query', 'Empire'); view.flush();
    assert.deepEqual(lastSearch(view).params.roots, [1]); assert.deepEqual(lastSearch(view).params.include, ['events/**', 'notes/**']); assert.deepEqual(lastSearch(view).params.exclude, ['**/old.txt']); assert.equal(lastSearch(view).params.caseSensitive, true);
    view.message({ type: 'invalidate' }); view.message({ type: 'context', context: { ...context, revision: 8, roots: [{ id: 2, kind: 'dependency', path: '/dep' }] } });
    assert.deepEqual(lastSearch(view).params.roots, [1], 'missing selected source never broadens scope');
    view.message({ type: 'results', sequence: lastSearch(view).sequence, response: { ...page([], 8), limitations: ['offline source'] } });
    assert.match(view.ids.get('status-message').textContent, /incomplete/);
}

function host(options = {}) {
    const events = new Map(); const listen = name => callback => { events.set(name, callback); return { dispose() {} }; };
    const sent = []; const pending = []; const copies = []; const opened = []; const shown = []; let receive; let dispose; let created = 0;
    class TokenSource { constructor() { this.token = { isCancellationRequested: false }; } cancel() { this.token.isCancellationRequested = true; } dispose() {} }
    const uri = value => ({ fsPath: value, toString: () => value });
    const panel = { webview: { html: '', cspSource: 'vscode-webview:', asWebviewUri: item => item, onDidReceiveMessage: fn => { receive = fn; }, postMessage: message => { sent.push(plain(message)); return Promise.resolve(true); } }, onDidDispose: fn => { dispose = fn; }, reveal() {}, dispose() { dispose?.(); } };
    const workspace = { textDocuments: [], createFileSystemWatcher: () => ({dispose() {}, onDidCreate: () => ({dispose() {}}), onDidChange: () => ({dispose() {}}), onDidDelete: () => ({dispose() {}})}), onDidChangeTextDocument: listen('change'), onDidSaveTextDocument: listen('save'), onDidCloseTextDocument: listen('close'), onDidChangeConfiguration: () => ({ dispose() {} }), openTextDocument: async target => { opened.push(target); return { version: 1, getText: () => options.text ?? 'needle' }; } };
    const vscode = { RelativePattern: class {}, workspace, CancellationTokenSource: TokenSource, ViewColumn: { Beside: 2 }, Uri: { joinPath: (base, ...pieces) => uri(join(base.fsPath, ...pieces)), parse: uri }, window: { activeTextEditor: { document: { getText: () => 'selected' }, selection: {} }, createWebviewPanel: () => { created++; return panel; }, showTextDocument: async document => { shown.push(document); } }, l10n: { t: text => text }, env: { clipboard: { writeText: async text => { copies.push(text); } } }, Range: class {} };
    let client = { sendRequest: (method, params, token) => new Promise((resolve, reject) => pending.push({ method, params, token: token ?? (params?.isCancellationRequested !== undefined ? params : undefined), resolve, reject })) };
    const module = join(extension, 'out/searchPanel.js'); const realRequire = createRequire(module); const exports = {};
    runInNewContext(readFileSync(module, 'utf8'), { exports, Buffer, process, require: id => id === 'vscode' ? vscode : id === './transparentLoc' ? { realUriOf: () => undefined, openTextDocumentForNavigation: target => workspace.openTextDocument(target) } : id === './webviewI18n' ? { searchPanelStrings: () => ({}), webviewI18nMessage: () => ({ type: 'i18n' }) } : realRequire(id), setTimeout, clearTimeout }, { filename: module });
    const search = new exports.SearchPanel(uri(extension), () => client, () => options.ready ?? true); search.show();
    return { search, fire: name => events.get(name)?.(), send: input => receive(input), sent, pending, copies, opened, shown, workspace, restart: () => { client = { ...client }; }, created: () => created };
}
const settle = async () => { await Promise.resolve(); await Promise.resolve(); };
{
    const view = host(); view.send({ type: 'ready' });
    assert.equal(view.sent.filter(message => message.type === 'initial')[0].query, 'selected');
    view.search.show(); assert.equal(view.created(), 1); assert.equal(view.sent.filter(message => message.type === 'initial').length, 1);
    view.pending.shift().resolve(context); await settle();
    view.send({ type: 'search', sequence: 1, params: { tab: 'localisation', query: 'first' } });
    view.send({ type: 'search', sequence: 2, params: { tab: 'localisation', query: 'second' } });
    assert.equal(view.pending[0].token.isCancellationRequested, true);
    view.pending.shift().resolve(page(['stale'])); view.pending.shift().resolve(page(['current'])); await settle();
    assert.deepEqual(view.sent.filter(message => message.type === 'results').map(message => message.sequence), [2]);
    view.send({ type: 'search', sequence: 3, params: { tab: 'rules', query: 'rule' } }); view.pending.shift().resolve(page(['rule'])); await settle();
    view.send({ type: 'copy', id: 'current' }); await settle();
    assert.deepEqual(view.copies, ['current'], 'cached tab retains actionable results'); assert.equal(view.opened.length, 0, 'copy does not navigate');
    view.send({ type: 'open', id: 'current' }); view.pending.shift().resolve({ revision: 8 }); await settle();
    assert.equal(view.opened.length, 0, 'stale result never applies its old range'); assert.equal(view.sent.at(-1).type, 'invalidate');
    view.send({ type: 'search', sequence: 4, params: { tab: 'localisation', query: 'closing' } }); const active = view.pending.shift(); view.search.dispose();
    assert.equal(active.token.isCancellationRequested, true); active.resolve(page(['closed'])); await settle();
    assert.equal(view.sent.filter(message => message.sequence === 4).length, 0);
}

{
    const view = host();
    view.send({ type: 'search', sequence: 1, params: { tab: 'localisation', query: 'needle' } });
    view.pending.shift().resolve(page(['detail'])); await settle();
    view.send({ type: 'detail', id: 'detail' }); const request = view.pending.shift();
    view.search.dispose(); assert.equal(request.token.isCancellationRequested, true);
    request.resolve({ id: 'detail', text: 'closed' }); await settle();
    assert.equal(view.sent.some(message => message.type === 'detail'), false);
}
{
    const view = host();
    view.send({ type: 'search', sequence: 1, params: { tab: 'localisation', query: 'needle' } });
    view.pending.shift().resolve(page(['restart'])); await settle();
    view.restart(); view.send({ type: 'open', id: 'restart' });
    assert.equal(view.pending.length, 0, 'old client results cannot navigate after a server restart');
    assert.equal(view.sent.at(-1).type, 'invalidate'); view.search.dispose();
}
{
    const view = host({ text: 'changed on disk' });
    view.send({ type: 'search', sequence: 1, params: { tab: 'localisation', query: 'needle' } });
    const response = page(['stale-disk']); response.groups[0].versions[0].location.expectedText = 'needle';
    view.pending.shift().resolve(response); await settle();
    view.send({ type: 'open', id: 'stale-disk' }); view.pending.shift().resolve({ revision: 7 }); await settle(); await settle();
    assert.equal(view.opened.length, 1); assert.equal(view.shown.length, 0, 'disk changes cannot receive an obsolete selection');
    assert.equal(view.sent.at(-1).type, 'error'); view.search.dispose();
}
{
    const view = host({ ready: false }); view.send({ type: 'ready' });
    assert.equal(view.pending.length, 0); assert.equal(view.sent.at(-1).type, 'indexing'); view.search.dispose();
    const web = webview(); web.message({ type: 'indexing' });
    assert.match(web.ids.get('status-message').textContent, /Indexing/);
    web.message({ type: 'context', context }); web.input('query', 'needle'); web.flush();
    web.message({ type: 'cancelled', sequence: lastSearch(web).sequence });
    assert.equal(web.ids.get('status-message').textContent, 'Search cancelled.');
}

{
    const view=host();view.send({type:'ready'});view.pending.shift().resolve(context);await settle();
    view.workspace.textDocuments.push({uri:{scheme:'file',fsPath:'/mod/notes/readme.md',toString:()=> 'file:///mod/notes/readme.md'},languageId:'markdown',isDirty:true,version:3,getText:()=> 'unsaved foreign text'});
    view.workspace.textDocuments.push({uri:{scheme:'file',fsPath:'/other/readme.md',toString:()=> 'file:///other/readme.md'},languageId:'markdown',isDirty:true,version:1,getText:()=> 'outside'});
    view.send({type:'search',sequence:1,params:{tab:'text',query:'foreign',roots:[1]}});
    assert.equal(view.pending[0].params.buffers.length,1);
    assert.equal(view.pending[0].params.buffers[0].version,3);
    assert.equal(view.pending[0].params.buffers[0].text,'unsaved foreign text');
    view.search.dispose();view.pending.shift().resolve(page([]));await settle();
}
{
    const view = host();
    const request = sequence => {
        view.send({ type: 'search', sequence, params: { tab: 'text', query: 'needle' } });
        const current = view.pending.shift();
        current.resolve(page([]));
        return current.params.corpusEpoch;
    };
    const initial = request(1); await settle();
    view.fire('change'); const edited = request(2); await settle();
    assert.equal(edited, initial, 'buffer versions replace content without invalidating disk text');
    view.fire('save'); const saved = request(3); await settle();
    assert.ok(saved > edited, 'saving invalidates disk text');
    view.search.dispose();
}
console.log('search panel behavior passed');
