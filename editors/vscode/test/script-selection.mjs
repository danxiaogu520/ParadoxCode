import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync, writeFileSync, existsSync } from 'node:fs';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { runInNewContext } from 'node:vm';

const compiled = new URL('../out/scriptSelection.js', import.meta.url);
const require = createRequire(compiled);
const game = Buffer.from([16, 45, 78, 16, 253, 86]);

function fixture(t, original, current, first, last) {
    const dir = mkdtempSync(join(tmpdir(), 'pdc-selection-'));
    t.after(() => rmSync(dir, { recursive: true, force: true }));
    const file = join(dir, 'sample.txt');
    writeFileSync(file, original);
    const state = { requests: [], writes: 0, messages: [], agree: false };
    const pos = character => ({ line: 0, character });
    const range = { start: pos(first), end: pos(last), isEmpty: first === last, isEqual(other) { return this === other; } };
    const doc = {
        uri: { scheme: 'file', fsPath: file }, version: 1, encoding: 'utf8', eol: 1,
        isDirty: !Buffer.from(current).equals(original),
        getText: selection => selection ? current.slice(first, last) : current,
        offsetAt: p => p.character, lineAt: () => ({ text: current }),
        async save() { state.writes++; writeFileSync(file, current); this.isDirty = false; return true; },
    };
    const editor = { document: doc, selection: range, selections: [range] };
    const vscode = {
        EndOfLine: { CRLF: 2 }, l10n: { t: (s, ...args) => s.replace(/\{(\d+)\}/g, (_, n) => args[n]) },
        workspace: { getWorkspaceFolder: () => dir, openTextDocument: async () => doc },
        window: { activeTextEditor: editor, showErrorMessage: s => state.messages.push(s),
            showWarningMessage: async (_, button) => state.agree ? button : undefined },
    };
    const exports = {};
    runInNewContext(readFileSync(compiled, 'utf8'), { exports, Buffer, require(name) {
        if (name === 'vscode') return vscode;
        if (name === './byteCodec') return { async convertBytes(profile, direction, bytes) {
            assert.equal(profile, 'script');
            state.requests.push(Buffer.from(bytes));
            state.duringConversion?.();
            return direction === 'encode' ? game : Buffer.from('中国');
        } };
        return require(name);
    } });
    return { file, doc, state, convert: exports.convertSelection };
}

test('binary-only selection in a dirty UTF-8 buffer preserves other changes and honours cancellation', async t => {
    const { file, state, convert } = fixture(t, Buffer.from('█ old'), '█ 中国 edited', 2, 4);
    await convert('encode');
    assert.equal(state.writes, 0);
    assert.deepEqual(readFileSync(file), Buffer.from('█ old'));
    assert.equal(existsSync(file + '.pre-transcode.bak'), false);
    state.agree = true;
    await convert('encode');
    assert.deepEqual(state.messages, []);
    assert.equal(state.writes, 1);
    assert.deepEqual(state.requests, [Buffer.from('中国'), Buffer.from('中国')]);
    assert.deepEqual(readFileSync(file), Buffer.concat([Buffer.from('█ '), game, Buffer.from(' edited')]));
    assert.deepEqual(readFileSync(file + '.pre-transcode.bak'), Buffer.from('█ 中国 edited'));
});

test('a selection starting inside an escape is refused before conversion', async t => {
    const { file, doc, state, convert } = fixture(t, game, game.toString(), 1, 3);
    doc.isDirty = false;
    await convert('decode');
    assert.equal(state.requests.length, 0);
    assert.match(state.messages[0], /complete EU4 escape/);
    assert.deepEqual(readFileSync(file), game);
});

test('a file changed during conversion wins over the obsolete selection', async t => {
    const { file, state, convert } = fixture(t, Buffer.from('x中国'), 'x中国', 1, 3);
    state.duringConversion = () => writeFileSync(file, 'newer contents');
    state.agree = true;
    await convert('encode');
    assert.match(state.messages[0], /file changed/);
    assert.equal(readFileSync(file, 'utf8'), 'newer contents');
});
