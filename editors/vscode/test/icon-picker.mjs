import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { pickerHost, pickerWebview, sprite } from './icon-picker-harness.mjs';

const require = createRequire(import.meta.url);
const { GameAssetStore } = require('../out/gameAssets.js');
const url = 'data:image/png;base64,AAA';

function catalog(h, count) {
    h.receive({ type: 'catalog', sprites: Array.from({ length: count }, (_, i) => sprite(`mission_${i}`)) });
}

test('initial observer notifications only request intersecting tiles', () => {
    const h = pickerWebview();
    catalog(h, 6);
    h.frame();
    h.intersect((i) => i === 0);
    assert.deepEqual(h.sent, [{ type: 'requestImages', names: ['mission_0'] }]);
    assert.equal(h.observer.observed.size, 5, 'offscreen tiles remain observed');
});

test('an offscreen tile is requested when it later enters view', () => {
    const h = pickerWebview();
    catalog(h, 2);
    h.frame();
    h.intersect(() => false);
    assert.equal(h.sent.length, 0);
    h.intersect((i) => i === 1);
    assert.deepEqual(h.sent[0].names, ['mission_1']);
});

test('all visible tiles are requested in bounded batches without waiting for replies', () => {
    const h = pickerWebview();
    catalog(h, 75);
    h.frame();
    h.intersect();
    assert.deepEqual(h.sent.map((message) => message.names.length), [32, 32, 11]);
    assert.equal(new Set(h.sent.flatMap((message) => message.names)).size, 75);
    assert.equal(h.observer.observed.size, 0);
});

test('duplicate observer notifications do not duplicate pending requests', () => {
    const h = pickerWebview();
    catalog(h, 1);
    h.frame();
    const targets = [...h.observer.observed];
    h.intersect(() => true, targets);
    h.intersect(() => true, targets);
    assert.equal(h.sent.length, 1);
});

test('missing first batch finishes while later valid images are displayed', async () => {
    const root = mkdtempSync(join(tmpdir(), 'pdc-picker-'));
    try {
        mkdirSync(join(root, 'interface'));
        mkdirSync(join(root, 'gfx'));
        writeFileSync(join(root, 'interface', 'icons.gfx'), 'spriteTypes = {\n'
            + Array.from({ length: 40 }, (_, i) => `spriteType = { name = "mission_${i}" texturefile = "gfx/${i < 32 ? 'missing' : 'valid'}.tga" }`).join('\n')
            + '\n}');
        const tga = Buffer.alloc(22);
        tga[2] = 2;
        tga.writeUInt16LE(1, 12);
        tga.writeUInt16LE(1, 14);
        tga[16] = 32;
        tga[17] = 0x28;
        tga[20] = 255;
        tga[21] = 255;
        writeFileSync(join(root, 'gfx', 'valid.tga'), tga);
        const store = new GameAssetStore(root, undefined);
        assert.equal(store.spriteCatalog().size, 40);
        const h = pickerWebview();
        const replies = [];
        const picker = pickerHost(store, (message) => { replies.push(message); h.receive(message); });
        catalog(h, 40);
        h.frame();
        h.intersect();
        assert.equal(h.sent.length, 2);
        for (const message of h.sent) await picker.postImages(message.names);
        assert.equal(replies.length, 2);
        assert.equal(Object.keys(replies[0].textures).length, 0);
        assert.equal(replies[0].names.length, 32);
        assert.equal(h.elements.grid.querySelectorAll('img[data-pending="1"]').length, 0);
        for (const tile of h.elements.grid.children.slice(32)) {
            assert.ok(tile.children[0].children[0].src.startsWith('data:image/png;base64,'));
        }
        h.intersect();
        assert.equal(h.sent.length, 2, 'missing textures do not cause retries');
    } finally {
        rmSync(root, { recursive: true, force: true });
    }
});

test('mixed image replies finish both successful and missing tiles', () => {
    const h = pickerWebview();
    catalog(h, 2);
    h.frame();
    h.intersect();
    h.receive({ type: 'images', names: ['mission_0', 'mission_1'], textures: { mission_1: url } });
    assert.equal(h.elements.grid.querySelectorAll('img[data-pending="1"]').length, 0);
    assert.equal(h.elements.grid.children[1].children[0].children[0].src, url);
    assert.equal(h.observer.observed.size, 0);
});

test('buffered observer entries for tiles removed by a search are ignored', () => {
    const h = pickerWebview();
    catalog(h, 2);
    h.frame();
    const oldTiles = [...h.observer.observed];
    h.search('no_match');
    h.debounce();
    h.intersect(() => true, oldTiles);
    assert.equal(h.sent.length, 0);
});

test('zero-match search cancels unfinished rendering', () => {
    const h = pickerWebview();
    catalog(h, 4001);
    h.search('no_match');
    for (let i = 0; i < 7; i += 1) h.frame();
    assert.equal(h.tiles().length, 2800);
    h.debounce();
    h.flushFrames();
    assert.deepEqual(h.tiles(), []);
    assert.equal(h.elements.count.textContent, '0 / 4001 sprites');
    assert.equal(h.elements.message.hidden, false);
    assert.equal(h.observer.observed.size, 0);
});

test('an error cancels unfinished rendering', () => {
    const h = pickerWebview();
    catalog(h, 401);
    h.frame();
    h.receive({ type: 'error', message: 'test error' });
    h.flushFrames();
    assert.deepEqual(h.tiles(), []);
    assert.equal(h.elements.message.textContent, 'test error');
});

test('rapid search changes leave one render task and only current results', () => {
    const h = pickerWebview();
    catalog(h, 1000);
    h.frame();
    h.search('mission_99');
    h.debounce();
    h.search('mission_1');
    h.debounce();
    assert.equal(h.frames.size, 1);
    h.flushFrames();
    assert.ok(h.tiles().every((name) => name.includes('mission_1')));
    assert.equal(new Set(h.tiles()).size, h.tiles().length);
});

test('Enter immediately uses the trimmed current search text', () => {
    const h = pickerWebview();
    h.receive({ type: 'catalog', sprites: [sprite('mission_a'), sprite('mission_b')] });
    h.frame();
    h.search('  MISSION_B ');
    h.elements.search.emit('keydown', { key: 'Enter' });
    assert.deepEqual(h.sent, [{ type: 'insert', name: 'mission_b' }]);
    assert.equal(h.timers.size, 0);
});

test('Enter with no current matches cannot insert an old result', () => {
    const h = pickerWebview();
    catalog(h, 2);
    h.frame();
    h.search('no_match');
    h.elements.search.emit('keydown', { key: 'Enter' });
    assert.equal(h.sent.length, 0);
    assert.deepEqual(h.tiles(), []);
});

test('Enter can select a matching icon before the next render frame', () => {
    const h = pickerWebview();
    catalog(h, 2);
    h.search('mission_1');
    h.debounce();
    h.elements.search.emit('keydown', { key: 'Enter' });
    assert.deepEqual(h.sent, [{ type: 'insert', name: 'mission_1' }]);
});

test('tab switching applies pending search text and respects the sprite scope', () => {
    const h = pickerWebview();
    h.receive({ type: 'catalog', sprites: [sprite('mission_a'), sprite('ui_button', false)] });
    h.frame();
    h.search('ui_');
    h.elements['tab-all'].emit('click');
    h.flushFrames();
    assert.deepEqual(h.tiles(), ['ui_button']);
    assert.equal(h.timers.size, 0);
    h.elements['tab-mission'].emit('click');
    h.flushFrames();
    assert.deepEqual(h.tiles(), []);
});

test('clearing search restores the full current sprite scope', () => {
    const h = pickerWebview();
    catalog(h, 3);
    h.flushFrames();
    h.search('mission_1');
    h.debounce();
    h.flushFrames();
    assert.deepEqual(h.tiles(), ['mission_1']);
    h.search('');
    h.debounce();
    h.flushFrames();
    assert.deepEqual(h.tiles(), ['mission_0', 'mission_1', 'mission_2']);
});

test('a late image response cannot restore tiles excluded by the current search', () => {
    const h = pickerWebview();
    catalog(h, 1);
    h.frame();
    h.intersect();
    h.search('no_match');
    h.debounce();
    h.receive({ type: 'images', names: ['mission_0'], textures: { mission_0: url } });
    assert.deepEqual(h.tiles(), []);
    assert.equal(h.observer.observed.size, 0);
});

test('a late response still fills a rebuilt tile for the same sprite', () => {
    const h = pickerWebview();
    catalog(h, 2);
    h.frame();
    h.intersect();
    h.search('mission_1');
    h.debounce();
    h.frame();
    h.intersect();
    h.receive({ type: 'images', names: ['mission_0', 'mission_1'], textures: { mission_0: url, mission_1: url } });
    assert.deepEqual(h.tiles(), ['mission_1']);
    assert.equal(h.elements.grid.children[0].children[0].children[0].src, url);
});

test('hidden tiles are not requested again when a visible image reply arrives', () => {
    const h = pickerWebview();
    catalog(h, 6);
    h.frame();
    h.intersect((i) => i === 0);
    h.receive({ type: 'images', names: ['mission_0'], textures: { mission_0: url } });
    h.intersect(() => false);
    assert.equal(h.sent.length, 1);
    assert.equal(h.observer.observed.size, 5);
});

test('host acknowledges undecodable names and retains its request limit', async () => {
    const messages = [];
    const picker = pickerHost({ spriteIconUrls: async () => ({}) }, (message) => messages.push(message));
    await picker.postImages(Array.from({ length: 65 }, (_, i) => `mission_${i}`));
    assert.equal(messages.length, 1);
    assert.equal(messages[0].type, 'images');
    assert.equal(messages[0].names.length, 64);
    assert.equal(messages[0].names[63], 'mission_63');
    assert.equal(Object.keys(messages[0].textures).length, 0);
});

test('host does not post a delayed reply to a replacement panel', async () => {
    let complete;
    const messages = [];
    const picker = pickerHost({ spriteIconUrls: () => new Promise((resolve) => { complete = resolve; }) },
        (message) => messages.push(message));
    const loading = picker.postImages(['mission_0']);
    picker.panel = { webview: { postMessage: (message) => messages.push(message) } };
    complete({ mission_0: { url } });
    await loading;
    assert.equal(messages.length, 0);
});

for (const suffix of ['# comment', '}']) {
    test(`inserting an icon preserves adjacent ${suffix}`, async () => {
        const line = `icon = mission_old${suffix}`;
        let output;
        const editor = {
            selection: { active: { line: 0, character: 10 } },
            document: { languageId: 'eu4', lineAt: () => ({ text: line }) },
            async edit(callback) {
                callback({ replace(range, value) {
                    output = line.slice(0, range.start.character) + value + line.slice(range.end.character);
                } });
                return true;
            },
        };
        class Position { constructor(line, character) { this.line = line; this.character = character; } }
        class Range { constructor(line, start, endLine, end) { this.start = new Position(line, start); this.end = new Position(endLine, end); } }
        const picker = pickerHost({}, () => {}, { window: { activeTextEditor: editor }, Range, Position, Selection: class {} });
        await picker.insert('mission_new');
        assert.equal(output, `icon = mission_new${suffix}`);
    });
}
