import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { runInNewContext } from 'node:vm';

const compiled = new URL('../out/localisationSync.js', import.meta.url);

function fixture() {
    const exports = {};
    const Position = class { constructor(line, character) { Object.assign(this, { line, character }); } };
    runInNewContext(readFileSync(compiled, 'utf8'), { exports, Buffer, require(id) {
        if (id === 'vscode') return { Position };
        return { LSPErrorCodes: { RequestCancelled: -32800 }, ResponseError: class extends Error {
            constructor(code, message) { super(message); this.code = code; }
        } };
    } });
    const sent = [];
    const client = {
        code2ProtocolConverter: {
            asOpenTextDocumentParams: doc => ({ textDocument: { uri: doc.uri.toString(), version: doc.version, text: doc.getText() } }),
            asCloseTextDocumentParams: doc => ({ textDocument: { uri: doc.uri.toString() } }),
            asChangeTextDocumentParams: event => ({ contentChanges: event.contentChanges }),
        },
        sendNotification: (method, params) => sync.sendNotification(method, (method, params) => sent.push({ method, params }), params),
    };
    let reads = 0;
    const doc = {
        languageId: 'localisation', uri: new URL('file:///localisation/sample.yml'), version: 1, lineCount: 1, text: 'x',
        offsetAt() { return this.text.length; }, getText() { reads++; return this.text; },
    };
    const sync = exports.localisationSync(() => client, { appendLine() {} });
    const close = () => client.sendNotification('textDocument/didClose', client.code2ProtocolConverter.asCloseTextDocumentParams(doc));
    return { sent, doc, sync, close, reads: () => reads, next: () => sent.push({ method: 'forward' }) };
}

test('a large localisation overlay closes once, then reopens with the current text and version', async () => {
    const { doc, sync, sent, next, reads, close } = fixture();
    await sync.didOpen(doc, next);
    doc.text = '中'.repeat(6 * 1024 * 1024);
    doc.version++;
    await sync.didChange({ document: doc, contentChanges: [{ text: doc.text, rangeLength: 1 }] }, next);
    assert.equal(reads(), 1);
    const request = () => sync.sendRequest('textDocument/semanticTokens/full', { textDocument: { uri: doc.uri.toString() } }, undefined, async () => 'tokens');
    await assert.rejects(request, { code: -32800 });
    doc.version++;
    doc.text += 'x';
    await sync.didChange({ document: doc, contentChanges: [{ text: 'x', rangeLength: 0 }] }, next);
    assert.equal(reads(), 1, 'an ordinary keystroke must reuse the previous size check');
    doc.version++;
    doc.text = 'small again';
    await sync.didChange({ document: doc, contentChanges: [{ text: doc.text, rangeLength: 6291457 }] }, next);
    assert.deepEqual(sent.map(row => row.method), ['forward', 'textDocument/didClose', 'textDocument/didOpen']);
    assert.equal(sent.at(-1).params.textDocument.text, doc.text);
    assert.equal(sent.at(-1).params.textDocument.version, doc.version);
    assert.equal(await request(), 'tokens');
    await close();
    assert.equal(sent.at(-1).method, 'textDocument/didClose');
});

test('a document within the UTF-8 budget can exceed the wire budget when game bytes are JSON escaped', async () => {
    const { doc, sync, sent, next, close, reads } = fixture();
    doc.text = '\x10\x10\x10'.repeat(2 * 1024 * 1024);
    await sync.didOpen(doc, next);
    const readsAfterOpen = reads();
    doc.version++;
    doc.text += 'x';
    await sync.didChange({ document: doc, contentChanges: [{ text: 'x', rangeLength: 0 }] }, next);
    assert.equal(reads(), readsAfterOpen, 'a keystroke must not reserialize the oversized open message');
    await close();
    assert.deepEqual(sent, []);
});
