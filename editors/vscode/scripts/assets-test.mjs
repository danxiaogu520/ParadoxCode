// Contract test for the extension-host game asset pipeline (src/gameAssets.ts)
// and the hover texture preview assembly (src/hoverTextures.ts).
//
// The DDS decoder cases are ports of the former Rust suite
// (crates/game/src/eu4/mission/texture/dds.rs); TGA, BMFont, and the sprite
// index carry equivalents of their Rust tests where those existed. The
// GameAssetStore cases use temp fixtures mirroring the on-disk layout.
import { createRequire } from 'node:module';
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { inflateSync } from 'node:zlib';
import assert from 'node:assert/strict';

const require = createRequire(import.meta.url);
const scriptDir = dirname(fileURLToPath(import.meta.url));
// The TS module compiles to out/gameAssets.js; run after `npm run compile`.
const assets = require(join(scriptDir, '..', 'out', 'gameAssets.js'));

const DDPF_ALPHAPIXELS = 0x1;
const DDPF_FOURCC = 0x4;

function ddsHeader(width, height, formatFlags, fourcc) {
    const bytes = Buffer.alloc(128);
    bytes.write('DDS ', 0, 'latin1');
    bytes.writeUInt32LE(124, 4);
    bytes.writeUInt32LE(0x1007, 8);
    bytes.writeUInt32LE(height, 12);
    bytes.writeUInt32LE(width, 16);
    bytes.writeUInt32LE(width * 4, 20);
    bytes.writeUInt32LE(1, 28);
    bytes.writeUInt32LE(32, 76);
    bytes.writeUInt32LE(formatFlags, 80);
    if (fourcc) {
        bytes.write(fourcc, 84, 'latin1');
    }
    bytes.writeUInt32LE(fourcc ? 0 : 32, 88);
    bytes.writeUInt32LE(0x00ff0000, 92);
    bytes.writeUInt32LE(0x0000ff00, 96);
    bytes.writeUInt32LE(0x000000ff, 100);
    bytes.writeUInt32LE(0xff000000, 104);
    return bytes;
}

function withData(header, data) {
    return Buffer.concat([header, Buffer.from(data)]);
}

// --- DDS: uncompressed -----------------------------------------------------

{
    // 2x1 BGRA32: red + translucent green, like the Rust round-trip case.
    const bytes = withData(ddsHeader(2, 1, 0x40 | DDPF_ALPHAPIXELS, null), [0, 0, 255, 255, 0, 255, 0, 128]);
    const image = assets.decodeDds(bytes);
    assert.equal(image.width, 2);
    assert.equal(image.height, 1);
    assert.deepEqual([...image.pixels.subarray(0, 4)], [255, 0, 0, 255]);
    assert.deepEqual([...image.pixels.subarray(4, 8)], [0, 255, 0, 128]);
}

{
    // 2x1 24-bit with BGRA-style masks and no alpha plane.
    const bytes = ddsHeader(2, 1, 0x40, null);
    bytes.writeUInt32LE(24, 88);
    bytes.writeUInt32LE(0, 104);
    const image = assets.decodeDds(withData(bytes, [10, 20, 30, 40, 50, 60]));
    assert.deepEqual([...image.pixels.subarray(0, 3)], [30, 20, 10]);
    assert.equal(image.pixels[3], 255);
    assert.deepEqual([...image.pixels.subarray(4, 7)], [60, 50, 40]);
}

// --- DDS: DXT1/3/5 -----------------------------------------------------------

{
    // 4x1 DXT1: c0=0xF800 (red) > c1=0x07E0 (green), all indices 0 -> red row.
    const block = Buffer.alloc(8);
    block.writeUInt16LE(0xf800, 0);
    block.writeUInt16LE(0x07e0, 2);
    const image = assets.decodeDds(withData(ddsHeader(4, 1, DDPF_FOURCC, 'DXT1'), block));
    assert.deepEqual([...image.pixels.subarray(0, 4)], [255, 0, 0, 255]);
    assert.deepEqual([...image.pixels.subarray(12, 16)], [255, 0, 0, 255]);
}

{
    // 2x2 DXT3 all-zero blocks decode to 16 output pixels (Rust case).
    const image = assets.decodeDds(withData(ddsHeader(2, 2, DDPF_FOURCC, 'DXT3'), Buffer.alloc(16)));
    assert.equal(image.pixels.length, 16);
}

{
    // 4x1 DXT3: explicit 4-bit alpha (pixel 0 = 0xF, pixel 1 = 0x1).
    const block = Buffer.alloc(16);
    block[0] = 0x1f;
    const image = assets.decodeDds(withData(ddsHeader(4, 1, DDPF_FOURCC, 'DXT3'), block));
    assert.equal(image.pixels[3], 0xf * 17);
    assert.equal(image.pixels[7], 17);
    assert.equal(image.pixels[11], 0);
}

{
    // 4x1 DXT5: a0 > a1 8-level ramp; then a0 <= a1 with pixel 5 index 5 -> 204.
    let block = Buffer.alloc(16);
    block[0] = 255;
    block[1] = 0;
    let image = assets.decodeDds(withData(ddsHeader(4, 1, DDPF_FOURCC, 'DXT5'), block));
    assert.equal(image.pixels[3], 255);

    // Pixel 5 lives in the second 4x1 block, whose alpha header must carry
    // a0=0/a1=255 (a0 <= a1 -> 6 interpolants) and code 5 -> (0*1+255*4)/5=204.
    const secondBlock = Buffer.alloc(16);
    secondBlock[0] = 0;
    secondBlock[1] = 255;
    const bit = 1 * 3;
    const byte = 2 + Math.floor(bit / 8);
    const shift = bit % 8;
    const value = 5 << shift;
    secondBlock[byte] |= value & 0xff;
    secondBlock[byte + 1] |= value >>> 8;
    image = assets.decodeDds(withData(ddsHeader(8, 1, DDPF_FOURCC, 'DXT5'), Buffer.concat([Buffer.alloc(16), secondBlock])));
    assert.equal(image.pixels[5 * 4 + 3], 204);
}

// --- DDS: rejections ---------------------------------------------------------

{
    assert.throws(() => assets.decodeDds(new Uint8Array(0)), /missing DDS header/);
    assert.throws(() => assets.decodeDds(new Uint8Array(128)), /missing DDS header/);
    let bytes = ddsHeader(0, 1, 0x40, null);
    assert.throws(() => assets.decodeDds(bytes), /invalid texture dimensions/);
    bytes = ddsHeader(2, 2, DDPF_FOURCC, 'NOPE');
    assert.throws(() => assets.decodeDds(withData(bytes, Buffer.alloc(8))), /unsupported fourcc/);
    bytes = ddsHeader(2, 2, DDPF_FOURCC, 'DXT1');
    assert.throws(() => assets.decodeDds(bytes), /truncated pixel data/);
    bytes = ddsHeader(2, 1, 0x40, null);
    bytes.writeUInt32LE(16, 88); // unsupported depth
    assert.throws(() => assets.decodeDds(withData(bytes, Buffer.alloc(8))), /unsupported bit depth/);
}

// --- TGA ----------------------------------------------------------------------

function tgaHeader(width, height, bpp, descriptor) {
    const bytes = Buffer.alloc(18);
    bytes[2] = 2; // image type: uncompressed true-color
    bytes.writeUInt16LE(width, 12);
    bytes.writeUInt16LE(height, 14);
    bytes[16] = bpp;
    bytes[17] = descriptor;
    return bytes;
}

{
    // 1x2 32bpp bottom-left origin: rows are stored bottom-up and must flip.
    // TGA pixels are BGR: stored row 0 (bottom) = bytes (0,0,255) = red,
    // stored row 1 (top) = bytes (255,0,0) = blue.
    const image = assets.decodeTga(withData(tgaHeader(1, 2, 32, 0x08), [0, 0, 255, 255, 255, 0, 0, 255]));
    assert.deepEqual([...image.pixels.subarray(0, 4)], [0, 0, 255, 255]); // top row blue
    assert.deepEqual([...image.pixels.subarray(4, 8)], [255, 0, 0, 255]); // bottom row red

    // Same bytes with top-left origin stay in storage order.
    const flipped = assets.decodeTga(withData(tgaHeader(1, 2, 32, 0x28), [0, 0, 255, 255, 255, 0, 0, 255]));
    assert.deepEqual([...flipped.pixels.subarray(0, 4)], [255, 0, 0, 255]);

    // 24bpp gets opaque alpha; truncated data is rejected.
    const opaque = assets.decodeTga(withData(tgaHeader(1, 1, 24, 0), [10, 20, 30]));
    assert.deepEqual([...opaque.pixels], [30, 20, 10, 255]);
    assert.throws(() => assets.decodeTga(tgaHeader(2, 2, 32, 0)), /truncated pixel data/);
    assert.throws(() => assets.decodeTga(Buffer.alloc(4)), /missing TGA header/);
}

// --- decodeAtlasImage dispatch -------------------------------------------------

{
    const dds = withData(ddsHeader(2, 1, 0x40 | DDPF_ALPHAPIXELS, null), [0, 0, 255, 255, 0, 255, 0, 255]);
    assert.equal(assets.decodeAtlasImage(dds).width, 2);
    const tga = withData(tgaHeader(1, 1, 32, 0), [0, 0, 255, 255]);
    assert.equal(assets.decodeAtlasImage(tga).height, 1);
}

// --- PNG -----------------------------------------------------------------------

{
    const image = { width: 2, height: 2, pixels: Uint8Array.from([
        255, 0, 0, 255, 0, 255, 0, 255,
        0, 0, 255, 255, 0, 0, 0, 0,
    ]) };
    const png = assets.encodePng(image);
    assert.deepEqual([...png.subarray(0, 8)], [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
    assert.equal(png.subarray(12, 16).toString('latin1'), 'IHDR');
    assert.equal(png.subarray(37, 41).toString('latin1'), 'IDAT');
    assert.equal(png.subarray(png.length - 8, png.length - 4).toString('latin1'), 'IEND');
    // IDAT inflates to filter-0 scanlines of the original pixels.
    const idatLength = png.readUInt32BE(33);
    const idat = png.subarray(41, 41 + idatLength);
    const raw = inflateSync(idat);
    assert.equal(raw.length, (2 * 4 + 1) * 2);
    assert.deepEqual([...raw.subarray(1, 9)], [...image.pixels.subarray(0, 8)]);
    assert.ok(assets.pngDataUrl(image).startsWith('data:image/png;base64,'));
}

// --- BMFont --------------------------------------------------------------------

{
    const fnt = [
        'info face="Adobe Garamond Pro" size=18 bold=1 italic=0 charset="ANSI" stretchH=100 smooth=1 aa=4 padding=0,0,0,0 spacing=2,2',
        'common lineHeight=18 base=13 scaleW=256 scaleH=256 pages=1',
        'char id=65   x=1   y=2    width=12   height=14    xoffset=0   yoffset=1    xadvance=13   page=0',
        'char id=8   x=9   y=9    width=0   height=0    xoffset=0   yoffset=0    xadvance=6   page=0',
        'kerning first=65 second=65 amount=-1',
    ].join('\n');
    const font = assets.parseBmFont(fnt);
    assert.ok(font);
    assert.equal(font.lineHeight, 18);
    assert.equal(font.base, 13);
    const a = font.chars.get(65);
    assert.deepEqual([a.x, a.y, a.width, a.height, a.xOffset, a.yOffset, a.xAdvance], [1, 2, 12, 14, 0, 1, 13]);
    assert.equal(font.kerning(65, 65), -1);
    assert.equal(font.kerning(65, 66), 0);
    assert.deepEqual(font.kerningPairs, [[65, 65, -1]]);
    // A metrics-less file is not a font; malformed char lines are skipped.
    assert.equal(assets.parseBmFont('char id=1 x=2'), undefined);
    const tolerant = assets.parseBmFont('common lineHeight=16 base=12\nchar id=65 x=1 y=2\nchar id=66 x=1 y=2 width=3 height=4 xoffset=0 yoffset=0 xadvance=5');
    assert.ok(tolerant);
    assert.equal(tolerant.chars.size, 1); // only id=66 has full fields
}

// --- sprite index ----------------------------------------------------------------

{
    const nested = [
        'spriteTypes = {',
        '    spriteType = {',
        '        name = "GFX_mission_icons_frame"',
        '        texturefile = "gfx//interface//missions//mission_icons_frame.dds"',
        '    }',
        '}',
    ].join('\n');
    const entries = assets.parseGfxSprites(nested);
    assert.equal(entries.length, 1);
    assert.equal(entries[0].name, 'GFX_mission_icons_frame');
    assert.equal(entries[0].textureFile, 'gfx/interface/missions/mission_icons_frame.dds');

    const flat = 'spriteType = { name = mission_x texturefile = gfx/interface/missions/mission_x.dds }';
    assert.equal(assets.parseGfxSprites(flat)[0].name, 'mission_x');

    // noOfFrames is captured (quoted or bare value, case-insensitive key);
    // absent, zero, or non-positive values stay undefined.
    const framed = 'spriteType = { name = strip texturefile = "s.dds" noOfFrames = "8" }';
    assert.equal(assets.parseGfxSprites(framed)[0].frames, 8);
    const upper = 'spriteType = { name = strip texturefile = "s.dds" NOOFFRAMES = 4 }';
    assert.equal(assets.parseGfxSprites(upper)[0].frames, 4);
    assert.equal(assets.parseGfxSprites('spriteType = { name = s texturefile = "s.dds" }')[0].frames, undefined);
    assert.equal(assets.parseGfxSprites('spriteType = { name = s texturefile = "s.dds" noOfFrames = 0 }')[0].frames, undefined);

    assert.equal(assets.normalizeTexturePath('../evil.dds'), '');
    assert.equal(assets.normalizeTexturePath('C:\\evil.dds'), '');
    assert.equal(assets.normalizeTexturePath('C:/evil.dds'), '');
    assert.equal(assets.normalizeTexturePath('/absolute/evil.dds'), 'absolute/evil.dds');
    assert.equal(assets.normalizeTexturePath('gfx\\interface\\missions\\a.dds'), 'gfx/interface/missions/a.dds');
    assert.equal(assets.normalizeTexturePath('gfx//interface//missions//a.dds'), 'gfx/interface/missions/a.dds');

    const index = assets.buildSpriteIndex([
        'spriteTypes = { spriteType = { name = a texturefile = "one.dds" noOfFrames = 3 } }',
        'spriteTypes = { spriteType = { name = a texturefile = "two.dds" } }',
    ]);
    assert.equal(index.get('a')?.textureFile, 'one.dds');
    assert.equal(index.get('a')?.frames, 3);
}

// --- GameAssetStore (temp fixtures) ---------------------------------------------

{
    const root = mkdtempSync(join(tmpdir(), 'pdc-assets-'));
    const chineseRoot = mkdtempSync(join(tmpdir(), 'pdc-assets-zh-'));
    try {
        mkdirSync(join(root, 'interface'), { recursive: true });
        mkdirSync(join(root, 'gfx', 'interface', 'missions'), { recursive: true });
        writeFileSync(
            join(root, 'interface', 'test.gfx'),
            'spriteTypes = { spriteType = { name = "test_icon" texturefile = "gfx//interface//missions//t.dds" } }',
        );
        // A 2x1 BGRA32 DDS with one red pixel (Rust assets.rs fixture).
        const dds = withData(ddsHeader(2, 1, 0x40 | DDPF_ALPHAPIXELS, null), [0, 0, 255, 255, 0, 255, 0, 255]);
        writeFileSync(join(root, 'gfx', 'interface', 'missions', 't.dds'), dds);

        // vic_18 font: 2-glyph metrics + 1x1 32bpp TGA atlas.
        mkdirSync(join(root, 'gfx', 'fonts'), { recursive: true });
        writeFileSync(join(root, 'gfx', 'fonts', 'vic_18.fnt'), [
            'info face="Test" size=18 bold=1 italic=0 charset="" stretchH=100 smooth=1 aa=1 padding=0,0,0,0 spacing=1,1',
            'common lineHeight=18 base=13 scaleW=1 scaleH=1 pages=1',
            'char id=65 x=0 y=0 width=1 height=1 xoffset=0 yoffset=0 xadvance=1 page=0',
            'kerning first=65 second=65 amount=-1',
        ].join('\n'));
        writeFileSync(join(root, 'gfx', 'fonts', 'vic_18.tga'), withData(tgaHeader(1, 1, 32, 0), [255, 255, 255, 255]));

        // Chinese font: zh-hans-16.fnt + zh-hans-16.dds (4x1 DXT5 ramp).
        mkdirSync(join(chineseRoot, 'gfx', 'fonts'), { recursive: true });
        writeFileSync(join(chineseRoot, 'gfx', 'fonts', 'zh-hans-16.fnt'), [
            'common lineHeight=16 base=12 scaleW=4 scaleH=1 pages=1',
            'char id=25991 x=0 y=0 width=1 height=1 xoffset=0 yoffset=0 xadvance=2 page=0',
        ].join('\n'));
        writeFileSync(
            join(chineseRoot, 'gfx', 'fonts', 'zh-hans-16.dds'),
            withData(ddsHeader(4, 1, DDPF_FOURCC, 'DXT5'), Buffer.alloc(16)),
        );

        const store = new assets.GameAssetStore(root, chineseRoot);
        const urls = await store.spriteUrls(['test_icon', 'missing_icon']);
        assert.ok(urls.test_icon?.startsWith('data:image/png;base64,'));
        assert.equal(urls.missing_icon, undefined);
        // Cached second fetch ships nothing new; a forced fetch (a rebuilt
        // preview webview) resends the cached entry.
        assert.deepEqual(await store.spriteUrls(['test_icon']), {});
        const forced = await store.spriteUrls(['test_icon'], true);
        assert.ok(forced.test_icon?.startsWith('data:image/png;base64,'));
        // Fonts decode with compact glyph rows and kerning pairs.
        const fonts = await store.loadFonts();
        assert.ok(fonts.english);
        assert.equal(fonts.english.lineHeight, 18);
        assert.deepEqual(fonts.english.chars, [[65, 0, 0, 1, 1, 0, 0, 1]]);
        assert.deepEqual(fonts.english.kernings, [[65, 65, -1]]);
        assert.ok(fonts.chinese);
        assert.equal(fonts.chinese.lineHeight, 16);
        assert.ok(fonts.chinese.chars.some(([id]) => id === 25991));
        // A store without directories degrades silently.
        const empty = new assets.GameAssetStore(undefined, undefined);
        assert.deepEqual(await empty.spriteUrls(['test_icon']), {});
        assert.deepEqual(await empty.loadFonts(), {});
    } finally {
        rmSync(root, { recursive: true, force: true });
        rmSync(chineseRoot, { recursive: true, force: true });
    }
}

// --- GameAssetStore with mod roots (hover texture lookup) ------------------------

{
    const gameRoot = mkdtempSync(join(tmpdir(), 'pdc-hover-game-'));
    const modRoot = mkdtempSync(join(tmpdir(), 'pdc-hover-mod-'));
    try {
        mkdirSync(join(gameRoot, 'interface'), { recursive: true });
        mkdirSync(join(gameRoot, 'gfx', 'interface'), { recursive: true });
        writeFileSync(
            join(gameRoot, 'interface', 'vanilla.gfx'),
            'spriteTypes = { spriteType = { name = "shared_icon" texturefile = "gfx/interface/shared.dds" } '
                + 'spriteType = { name = "game_only" texturefile = "gfx/interface/game.dds" } }',
        );
        const red = withData(ddsHeader(2, 1, 0x40 | DDPF_ALPHAPIXELS, null), [0, 0, 255, 255, 0, 255, 0, 255]);
        writeFileSync(join(gameRoot, 'gfx', 'interface', 'shared.dds'), red);
        writeFileSync(join(gameRoot, 'gfx', 'interface', 'game.dds'), red);

        // The mod redefines one sprite (from a nested interface/assets file)
        // and drop-in replaces a vanilla texture at the same relative path.
        mkdirSync(join(modRoot, 'interface', 'assets'), { recursive: true });
        mkdirSync(join(modRoot, 'gfx', 'interface'), { recursive: true });
        writeFileSync(
            join(modRoot, 'interface', 'assets', 'mod.gfx'),
            'spriteTypes = { spriteType = { name = "shared_icon" texturefile = "gfx/interface/mod.dds" } '
                + 'spriteType = { name = "mod_only" texturefile = "gfx/interface/mod.dds" noOfFrames = 2 } }',
        );
        const green = withData(
            ddsHeader(4, 1, 0x40 | DDPF_ALPHAPIXELS, null),
            [0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255],
        );
        writeFileSync(join(modRoot, 'gfx', 'interface', 'mod.dds'), green);
        writeFileSync(join(modRoot, 'gfx', 'interface', 'shared.dds'), green);
        // A sprite whose texturefile spelling drifts from the shipped file.
        writeFileSync(
            join(modRoot, 'interface', 'assets', 'drift.gfx'),
            'spriteTypes = { spriteType = { name = "drift_icon" texturefile = "gfx/interface/as_dds.tga" } }',
        );
        // Extension-drift fixtures: as_tga ships as .tga in the game root,
        // as_dds ships as .dds in the mod root; the exact/drifted pair under
        // exact_game exists in both spellings across the roots.
        writeFileSync(join(gameRoot, 'gfx', 'interface', 'as_tga.tga'), red);
        writeFileSync(join(modRoot, 'gfx', 'interface', 'as_dds.dds'), green);
        writeFileSync(join(gameRoot, 'gfx', 'interface', 'exact_game.dds'), red);
        writeFileSync(join(modRoot, 'gfx', 'interface', 'exact_game.tga'), green);

        const store = new assets.GameAssetStore(gameRoot, undefined, [modRoot]);
        // Mod definitions replace vanilla ones for the same sprite name; the
        // nested interface/assets directory is part of the index.
        assert.equal(store.spriteTexture('shared_icon')?.textureFile, 'gfx/interface/mod.dds');
        assert.equal(store.spriteTexture('mod_only')?.frames, 2);
        assert.equal(store.spriteTexture('game_only')?.textureFile, 'gfx/interface/game.dds');
        // Texture resolution prefers the mod root, then the game root.
        assert.equal(store.resolveTexture('gfx/interface/shared.dds'), join(modRoot, 'gfx', 'interface', 'shared.dds'));
        assert.equal(store.resolveTexture('gfx/interface/game.dds'), join(gameRoot, 'gfx', 'interface', 'game.dds'));
        assert.equal(store.resolveTexture('gfx/interface/missing.dds'), undefined);
        assert.equal(store.resolveTexture('../escape.dds'), undefined);
        // Extension drift mirrors the server catalog: a `.dds` reference
        // whose file ships as `.tga` (and vice versa) resolves, and an exact
        // game-root hit beats a drifted mod-root hit.
        assert.equal(store.resolveTexture('gfx/interface/as_tga.dds'), join(gameRoot, 'gfx', 'interface', 'as_tga.tga'));
        assert.equal(store.resolveTexture('gfx/interface/as_dds.tga'), join(modRoot, 'gfx', 'interface', 'as_dds.dds'));
        assert.equal(store.resolveTexture('gfx/interface/exact_game.dds'), join(gameRoot, 'gfx', 'interface', 'exact_game.dds'));
        // Drift applies to .tga/.dds spellings only.
        writeFileSync(join(gameRoot, 'gfx', 'interface', 'bitmap.png'), red);
        assert.equal(store.resolveTexture('gfx/interface/bitmap.tga'), undefined);
        // By-path decode returns dimensions alongside the data URL; missing
        // files degrade to undefined.
        const image = await store.textureFile(join(modRoot, 'gfx', 'interface', 'mod.dds'));
        assert.ok(image?.url.startsWith('data:image/png;base64,'));
        assert.equal(image?.width, 4);
        assert.equal(image?.height, 1);
        assert.equal(await store.textureFile(join(modRoot, 'gfx', 'interface', 'absent.dds')), undefined);
        // The mission-preview path resolves mod textures through the same
        // roots, including the extension-drifted sprite.
        const urls = await store.spriteUrls(['mod_only', 'drift_icon']);
        assert.ok(urls.mod_only?.startsWith('data:image/png;base64,'));
        assert.ok(urls.drift_icon?.startsWith('data:image/png;base64,'));
    } finally {
        rmSync(gameRoot, { recursive: true, force: true });
        rmSync(modRoot, { recursive: true, force: true });
    }
}

// --- hover texture assembly --------------------------------------------------------

{
    const hover = require(join(scriptDir, '..', 'out', 'hoverTextures.js'));
    assert.equal(hover.extractSpriteHoverName('### sprite `GFX_x`\n\n#### Resolved definition'), 'GFX_x');
    assert.equal(hover.extractSpriteHoverName('### event_modifier `GFX_x`'), undefined);
    assert.equal(hover.extractSpriteHoverName('plain text'), undefined);

    const line = '\t\ttexturefile = "gfx/interface/missions/conquest_1550.dds"';
    assert.equal(
        hover.texturefileValueAt(line, line.indexOf('"') + 5),
        'gfx/interface/missions/conquest_1550.dds',
    );
    assert.equal(hover.texturefileValueAt(line, 2), undefined); // cursor on the key
    assert.equal(hover.texturefileValueAt('icon = "GFX_x"', 8), undefined);
    assert.equal(hover.texturefileValueAt('texturefile = "unterminated', 20), undefined);
    assert.equal(hover.texturefileValueAt('texturefile = bare/texture.dds', 20), 'bare/texture.dds');
    assert.equal(hover.texturefileValueAt('texturefile = bare/texture.dds', 5), undefined);

    // A frame-strip sprite wider than the hover cap carries |width=400.
    const preview = { name: 'GFX_x', rel: 'gfx/interface/x.dds', frames: 4, url: 'data:image/png;base64,AAA', width: 800 };
    const appended = hover.appendTextureSection('### sprite `GFX_x`', preview);
    assert.ok(appended.startsWith('### sprite `GFX_x`\n\n#### Texture\n\n- Path: `gfx/interface/x.dds`\n- Frames: 4\n\n'));
    assert.ok(appended.endsWith('![GFX_x](data:image/png;base64,AAA|width=400)'));
    // Single-frame, narrow sprites render at natural size without a suffix.
    const single = hover.appendTextureSection('m', { name: 'GFX_y', rel: 'y.dds', url: 'data:image/png;base64,BBB', width: 90 });
    assert.ok(!single.includes('Frames:'));
    assert.ok(single.includes('![GFX_y](data:image/png;base64,BBB)'));
    const standalone = hover.texturefileHoverMarkdown(preview);
    assert.ok(standalone.startsWith('#### Texture\n\n- Path: `gfx/interface/x.dds`\n\n'));
    assert.ok(standalone.includes('|width=400'));
}

console.log('assets contract OK');
