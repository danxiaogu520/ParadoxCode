// Contract test for the extension-host game asset pipeline (src/gameAssets.ts)
// and the hover texture preview assembly (src/hoverTextures.ts).
//
// The DDS decoder cases are ports of the former Rust suite
// (crates/game/src/eu4/mission/texture/dds.rs); TGA, BMFont, and the sprite
// index carry equivalents of their Rust tests where those existed. The
// GameAssetStore cases use temp fixtures mirroring the on-disk layout.
import { createRequire } from 'node:module';
import { mkdtempSync, mkdirSync, rmSync, writeFileSync, existsSync, readFileSync } from 'node:fs';
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
        // Cached second fetch ships nothing new.
        assert.deepEqual(await store.spriteUrls(['test_icon']), {});
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
        // By-path decode returns dimensions alongside the data URL; missing
        // files degrade to undefined.
        const image = await store.textureFile(join(modRoot, 'gfx', 'interface', 'mod.dds'));
        assert.ok(image?.url.startsWith('data:image/png;base64,'));
        assert.equal(image?.width, 4);
        assert.equal(image?.height, 1);
        assert.equal(await store.textureFile(join(modRoot, 'gfx', 'interface', 'absent.dds')), undefined);
        // The mission-preview path resolves mod textures through the same roots.
        const urls = await store.spriteUrls(['mod_only']);
        assert.ok(urls.mod_only?.startsWith('data:image/png;base64,'));
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

    // VS Code's Hover class always wraps contents in an array, and the
    // languageclient's MarkupContent conversion is a bare MarkdownString;
    // both shapes must yield the markdown payload (regression: the array
    // shape used to silently disable every sprite hover augmentation).
    const markdown = { value: '### sprite `GFX_x`' };
    assert.equal(hover.markdownFromHoverContents(markdown), markdown);
    assert.equal(hover.markdownFromHoverContents([markdown]), markdown);
    assert.equal(
        hover.markdownFromHoverContents([{ value: 'first' }, { value: 'second' }]).value,
        'first',
    );
    assert.equal(hover.markdownFromHoverContents('plain marked string'), undefined);
    assert.equal(hover.markdownFromHoverContents([{ language: 'eu4', value: 'code' }]), undefined);
    assert.equal(hover.markdownFromHoverContents([{ value: 42 }]), undefined);
    assert.equal(hover.markdownFromHoverContents(null), undefined);
}

// --- hover mission-card compositor (src/hoverCards.ts) -----------------------------

{
    const cards = require(join(scriptDir, '..', 'out', 'hoverCards.js'));

    function makeImage(width, height, fill = [0, 0, 0, 0]) {
        const pixels = new Uint8Array(width * height * 4);
        for (let i = 0; i < pixels.length; i += 4) {
            pixels.set(fill, i);
        }
        return { width, height, pixels };
    }

    const px = (image, x, y) => [
        ...image.pixels.subarray((y * image.width + x) * 4, (y * image.width + x) * 4 + 4),
    ];
    const near = (actual, expected, tol = 2) =>
        actual.every((value, index) => Math.abs(value - expected[index]) <= tol);

    // frameRect: horizontal strips divide the atlas width; invalid frames = 1.
    const strip = makeImage(99, 33, [255, 255, 255, 255]);
    assert.deepEqual(cards.frameRect(strip, 3, 0), { x: 0, y: 0, width: 33, height: 33 });
    assert.deepEqual(cards.frameRect(strip, 3, 2), { x: 66, y: 0, width: 33, height: 33 });
    assert.deepEqual(cards.frameRect(strip, undefined, 0), { x: 0, y: 0, width: 99, height: 33 });
    assert.deepEqual(cards.frameRect(strip, 0, 5), { x: 0, y: 0, width: 99, height: 33 });

    // blitTinted: alpha-over onto transparent, then a second coat.
    {
        const dst = makeImage(1, 1);
        cards.blitTinted(dst, 0, 0, makeImage(1, 1, [10, 20, 30, 128]), { x: 0, y: 0, width: 1, height: 1 }, null);
        assert.ok(near(px(dst, 0, 0), [10, 20, 30, 128]));
        cards.blitTinted(dst, 0, 0, makeImage(1, 1, [200, 200, 200, 64]), { x: 0, y: 0, width: 1, height: 1 }, null);
        assert.ok(near(px(dst, 0, 0), [86, 92, 98, 160]));
    }
    // Tint multiplies RGB and keeps alpha (the § colour model).
    {
        const dst = makeImage(1, 1);
        cards.blitTinted(
            dst, 0, 0, makeImage(1, 1, [255, 255, 255, 255]),
            { x: 0, y: 0, width: 1, height: 1 }, { r: 181, g: 61, b: 61 },
        );
        assert.deepEqual(px(dst, 0, 0), [181, 61, 61, 255]);
    }
    // Destination clipping keeps partial blits inside the canvas.
    {
        const dst = makeImage(2, 1);
        cards.blitTinted(dst, -1, 0, makeImage(2, 1, [9, 9, 9, 255]), { x: 0, y: 0, width: 2, height: 1 }, null);
        assert.deepEqual(px(dst, 0, 0), [9, 9, 9, 255]); // src column 1
        assert.deepEqual(px(dst, 1, 0), [0, 0, 0, 0]);
    }

    // A hand-built font: 'A'/'B' as 4x4 white blocks (advance 6, A:B kerning
    // -1), space with advance 4, plus CJK glyphs '测'/'试' (advance 10).
    function glyphFont(id, charIds, advance) {
        const atlas = makeImage(charIds.length * 5, 4, [0, 0, 0, 0]);
        const chars = new Map();
        charIds.forEach((codePoint, index) => {
            const x0 = index * 5;
            for (let y = 0; y < 4; y += 1) {
                for (let x = 0; x < 4; x += 1) {
                    atlas.pixels.set([255, 255, 255, 255], (y * atlas.width + x0 + x) * 4);
                }
            }
            chars.set(codePoint, { x: x0, y: 0, width: 4, height: 4, xOffset: 0, yOffset: 0, xAdvance: advance });
        });
        chars.set(32, { x: 0, y: 0, width: 0, height: 0, xOffset: 0, yOffset: 0, xAdvance: 4 });
        return { id, lineHeight: 10, base: 8, chars, kernings: new Map([['65:66', -1]]), atlas };
    }

    const latinFont = glyphFont('english', [65, 66, 46], 6);
    latinFont.chars.get(46).xAdvance = 4; // '.' narrower than letters
    const cjkFont = glyphFont('chinese', [0x6d4b, 0x8bd5], 10);
    const blankMission = {
        id: 'probe', icon: 'GFX_probe', titleKey: 'probe_title',
        title: null, hasTrigger: false, hasEffect: false, required: [],
    };

    // Card geometry: 103x123 canvas; the icon shows through a translucent
    // frame at (22,20); corner markers draw only for their flags.
    {
        const frame = makeImage(103, 123, [0, 0, 0, 128]);
        const icon = makeImage(59, 63, [255, 0, 0, 255]);
        const card = cards.composeMissionCard(blankMission, { frame, icon });
        assert.equal(card.width, 103);
        assert.equal(card.height, 123);
        assert.ok(near(px(card, 40, 40), [127, 0, 0, 255])); // icon under the half-frame
        assert.deepEqual(px(card, 5, 5), [0, 0, 0, 128]); // frame alone
        assert.deepEqual(px(card, 16, 23), [0, 0, 0, 128]); // no trigger marker
        assert.deepEqual(px(card, 85, 20), [0, 0, 0, 128]); // no effect marker

        const trigger = makeImage(33, 33, [0, 0, 255, 255]);
        // 66-wide strip, white in frame 0's 22 columns and red beyond: if the
        // frame crop failed, the red would bleed into the card.
        const effect = makeImage(66, 33, [255, 0, 0, 255]);
        for (let x = 0; x < 22; x += 1) {
            for (let y = 0; y < 33; y += 1) {
                effect.pixels.set([255, 255, 255, 255], (y * effect.width + x) * 4);
            }
        }
        const flagged = cards.composeMissionCard(
            { ...blankMission, hasTrigger: true, hasEffect: true },
            { frame, icon, triggerMarker: trigger, effectMarker: effect, effectMarkerFrames: 3 },
        );
        assert.deepEqual(px(flagged, 16, 23), [0, 0, 255, 255]); // trigger marker at (0,7)
        assert.deepEqual(px(flagged, 90, 20), [255, 255, 255, 255]); // effect frame 0 (22px wide)
        assert.deepEqual(px(flagged, 93, 20), [0, 0, 0, 128]); // beyond the cropped frame
    }

    // Title: centred, baseline-placed glyphs from the atlas; §R tints red.
    {
        const frame = makeImage(103, 123, [0, 0, 0, 128]);
        const fonts = { english: latinFont };
        const card = cards.composeMissionCard(
            { ...blankMission, title: { language: 'l_english', value: 'AB' } },
            { frame, fonts },
        );
        // 'AB' = 6 + 6 - 1 kerning = 11px wide, centred in the 96px slot:
        // A blits from x=51, B (after -1 kerning) from x=56, both at y=84.
        assert.deepEqual(px(card, 52, 85), [255, 255, 255, 255]);
        assert.deepEqual(px(card, 57, 85), [255, 255, 255, 255]);
        assert.deepEqual(px(card, 55, 85), [0, 0, 0, 128]); // gap between glyphs
        assert.deepEqual(px(card, 8, 85), [0, 0, 0, 128]); // slot padding

        const tinted = cards.composeMissionCard(
            { ...blankMission, title: { language: 'l_english', value: '§RA' } },
            { frame, fonts },
        );
        assert.ok(near(px(tinted, 54, 85), [181, 61, 61, 255], 1)); // §R = #b53d3d
    }

    // CJK wrapping: characters break individually at the 96px slot width.
    {
        const fonts = { chinese: cjkFont };
        const lines = cards.styledTitleLines('测试'.repeat(10), cjkFont, fonts);
        assert.equal(lines.length, 3); // 9 + 9 + 2 tokens at 10px advance
        assert.equal(lines[0].length, 9);
        // CJK content selects the Chinese font even under an l_english tag.
        const card = cards.composeMissionCard(
            { ...blankMission, title: { language: 'l_english', value: '测试测试' } },
            { frame: makeImage(103, 123, [0, 0, 0, 128]), fonts },
        );
        // 4 chars * 10px = 40 wide -> first glyph x = 8 + 28 = 36..39, y = 84.
        assert.deepEqual(px(card, 37, 85), [255, 255, 255, 255]);
        assert.deepEqual(px(card, 47, 85), [255, 255, 255, 255]); // second glyph x = 46..49
    }

    // Latin wrapping + the two-line fold: 15 'AB' words wrap 6/6/3 at the
    // 96px slot; lines beyond two fold away and the cut is marked with dots.
    {
        const fonts = { english: latinFont };
        const wrapped = cards.styledTitleLines(Array(15).fill('AB').join(' '), latinFont, fonts);
        assert.equal(wrapped.length, 3);
        assert.equal(wrapped[0].length, 6); // 'AB' + five space-led words

        const card = cards.composeMissionCard(
            { ...blankMission, title: { language: 'l_english', value: Array(15).fill('AB').join(' ') } },
            { frame: makeImage(103, 123, [0, 0, 0, 128]), fonts },
        );
        // Line 1 keeps six words (86px): first glyph at x=13..16, y=84..87.
        assert.deepEqual(px(card, 14, 85), [255, 255, 255, 255]);
        // Line 2 (y=94) folds to five words + '...' (83px, x=14.5): the dots
        // land at x=86..97 as three 4px blocks.
        assert.deepEqual(px(card, 87, 95), [255, 255, 255, 255]);
        assert.deepEqual(px(card, 91, 95), [255, 255, 255, 255]);
        assert.deepEqual(px(card, 95, 95), [255, 255, 255, 255]);
        // The would-be third line (y=104) stays empty frame.
        assert.deepEqual(px(card, 40, 105), [0, 0, 0, 128]);
    }

    // No font at all: the card degrades to frame + icon + markers.
    {
        const card = cards.composeMissionCard(
            { ...blankMission, title: { language: 'l_english', value: 'AB' } },
            { frame: makeImage(103, 123, [0, 0, 0, 128]) },
        );
        assert.deepEqual(px(card, 52, 85), [0, 0, 0, 128]);
    }

    // Markdown assembly and the LRU behind composed cards.
    assert.equal(
        cards.missionCardMarkdown('data:image/png;base64,AAA'),
        '![mission card](data:image/png;base64,AAA|width=206)',
    );
    cards.clearCardCache();
    let produced = 0;
    assert.equal(cards.cachedCardDataUrl('k', () => { produced += 1; return 'v'; }), 'v');
    assert.equal(cards.cachedCardDataUrl('k', () => { produced += 1; return 'v2'; }), 'v');
    assert.equal(produced, 1);
    for (let i = 0; i < 40; i += 1) {
        cards.cachedCardDataUrl(`k${i}`, () => `x${i}`);
    }
    assert.equal(cards.cachedCardDataUrl('k', () => 'rebuilt'), 'rebuilt'); // evicted at cap 32

    // Wire parsing: valid payloads survive, malformed ones yield undefined
    // (the middleware then falls back to the legacy hover path).
    const parsed = cards.parseHoverCardResponse({
        version: 1,
        card: {
            kind: 'mission',
            mission: {
                id: 'p', icon: 'GFX_p', titleKey: 'k',
                title: { language: 'l_english', value: 'v' },
                hasTrigger: true, hasEffect: false, required: ['q'],
            },
            asset: { sprite: 's', path: 'C:/x.dds', rootKind: 'currentMod', extensionFallback: false, frames: 2 },
            cardAssets: {
                frame: { path: 'C:/f.dds', rootKind: 'vanilla', extensionFallback: false },
                triggerMarker: { path: 'C:/t.dds', rootKind: 'vanilla', extensionFallback: false },
                effectMarker: { path: 'C:/e.dds', rootKind: 'vanilla', extensionFallback: false, frames: 3 },
            },
        },
    });
    assert.equal(parsed.card.kind, 'mission');
    assert.equal(parsed.card.mission.title.value, 'v');
    assert.equal(parsed.card.asset.frames, 2);
    assert.equal(parsed.card.cardAssets.effectMarker.frames, 3);
    const sprite = cards.parseHoverCardResponse({
        version: 1,
        card: { kind: 'sprite', asset: { path: 'x.dds', rootKind: 'dependency', extensionFallback: true, frames: 2.5 } },
    });
    assert.equal(sprite.card.asset.rootKind, 'dependency');
    assert.equal(sprite.card.asset.frames, 2); // fractional frames floor
    assert.equal(cards.parseHoverCardResponse(null), undefined);
    assert.equal(cards.parseHoverCardResponse({ version: 2, card: { kind: 'sprite' } }), undefined);
    assert.equal(cards.parseHoverCardResponse({ version: 1, card: { kind: 'bogus' } }), undefined);
    assert.equal(cards.parseHoverCardResponse({ version: 1, card: { kind: 'mission' } }), undefined);
    assert.equal(cards.parseHoverCardResponse({ version: 1, card: { kind: 'texture', asset: { rootKind: 'vanilla' } } }), undefined);
}

// --- hover event-window compositor --------------------------------------------------

{
    const cards = require(join(scriptDir, '..', 'out', 'hoverCards.js'));

    function makeImage(width, height, fill = [0, 0, 0, 0]) {
        const pixels = new Uint8Array(width * height * 4);
        for (let i = 0; i < pixels.length; i += 4) {
            pixels.set(fill, i);
        }
        return { width, height, pixels };
    }

    const px = (image, x, y) => [
        ...image.pixels.subarray((y * image.width + x) * 4, (y * image.width + x) * 4 + 4),
    ];

    // Event chrome at vanilla geometry: top 656x223, middle 656x58, bottoms
    // 40/60/80 tall standing in for S/M/L, option button 547x31, picture
    // 512x132. Distinct greys tell the pieces apart.
    const parts = {
        backgroundTop: makeImage(656, 223, [120, 110, 100, 255]),
        backgroundMiddle: makeImage(656, 58, [140, 130, 120, 255]),
        backgroundBottomS: makeImage(656, 40, [10, 10, 10, 255]),
        backgroundBottomM: makeImage(656, 60, [20, 20, 20, 255]),
        backgroundBottomL: makeImage(656, 80, [30, 30, 30, 255]),
        optionButton: makeImage(547, 31, [235, 225, 205, 255]),
        picture: makeImage(512, 132, [200, 60, 60, 255]),
    };

    // Hand-built font: 'A'/'B' 4x4 white blocks, advance 6, A:B kerning -1,
    // lineHeight 10, base 8 (same shape as the mission-card fixture).
    const atlas = makeImage(10, 4, [0, 0, 0, 0]);
    for (let y = 0; y < 4; y += 1) {
        for (let x = 0; x < 4; x += 1) {
            atlas.pixels.set([255, 255, 255, 255], (y * atlas.width + x) * 4);
            atlas.pixels.set([255, 255, 255, 255], (y * atlas.width + 5 + x) * 4);
        }
    }
    const font = {
        id: 'english', lineHeight: 10, base: 8,
        chars: new Map([
            [65, { x: 0, y: 0, width: 4, height: 4, xOffset: 0, yOffset: 0, xAdvance: 6 }],
            [66, { x: 5, y: 0, width: 4, height: 4, xOffset: 0, yOffset: 0, xAdvance: 6 }],
            [32, { x: 0, y: 0, width: 0, height: 0, xOffset: 0, yOffset: 0, xAdvance: 4 }],
        ]),
        kernings: new Map([['65:66', -1]]),
        atlas,
    };
    const fonts = { english: font };

    const event = {
        id: 'demo.1',
        titleKey: 'demo.1.t',
        title: { language: 'l_english', value: 'AB' },
        desc: { value: 'AB' },
        options: [
            { nameKey: 'demo.1.a', name: { value: 'A' } },
            { nameKey: 'demo.1.b', name: { value: 'B' } },
        ],
    };

    {
        const card = cards.composeEventCard(event, { ...parts, fonts });
        assert.equal(card.width, 564);
        // 1 desc line (10px) -> options 254..317 -> bottom S (40) -> 358.
        assert.equal(card.height, 358);
        assert.deepEqual(px(card, 300, 30), [120, 110, 100, 255]); // top chrome
        assert.deepEqual(px(card, 300, 100), [200, 60, 60, 255]); // picture banner at (30,81)
        assert.deepEqual(px(card, 20, 100), [120, 110, 100, 255]); // beside the banner
        assert.deepEqual(px(card, 300, 245), [140, 130, 120, 255]); // tiled middle
        assert.deepEqual(px(card, 300, 285), [140, 130, 120, 255]); // gap between buttons
        assert.deepEqual(px(card, 100, 260), [235, 225, 205, 255]); // option button 0
        assert.deepEqual(px(card, 100, 292), [235, 225, 205, 255]); // option button 1
        assert.deepEqual(px(card, 300, 330), [10, 10, 10, 255]); // bottom S below options
        // Title 'AB' (11px) centred at y=39; white glyphs with a one-pixel
        // black shadow (B glyph spans x=282..285, its shadow x=283..286).
        assert.deepEqual(px(card, 278, 40), [255, 255, 255, 255]);
        assert.deepEqual(px(card, 284, 40), [255, 255, 255, 255]);
        assert.deepEqual(px(card, 286, 40), [0, 0, 0, 255]); // shadow tail
        assert.deepEqual(px(card, 300, 40), [120, 110, 100, 255]);
        // Desc 'AB' left-aligned at (31,230), black on the parchment.
        assert.deepEqual(px(card, 32, 231), [0, 0, 0, 255]);
        // Option text centred on the button strip: white glyph, black shadow
        // (A glyph x=279..282, shadow x=280..283 at y=265..268).
        assert.deepEqual(px(card, 280, 265), [255, 255, 255, 255]);
        assert.deepEqual(px(card, 283, 265), [0, 0, 0, 255]);
    }

    {
        // An arbitrary-length description wraps at word boundaries (Latin
        // tokens split at spaces) and the window grows to fit it: 100 'AB'
        // words wrap to 34/34/32-per-line at the 512px box -> 3 desc lines
        // -> desc bottom 260 -> options 274..338 -> bottom S -> height 378.
        const long = cards.composeEventCard(
            { ...event, desc: { value: Array(100).fill('AB').join(' ') } },
            { ...parts, fonts },
        );
        assert.equal(long.height, 378);
        assert.deepEqual(px(long, 32, 231), [0, 0, 0, 255]); // desc line 1
        assert.deepEqual(px(long, 32, 251), [0, 0, 0, 255]); // desc line 3
        assert.deepEqual(px(long, 300, 270), [140, 130, 120, 255]); // middle tiles on
        assert.deepEqual(px(long, 300, 360), [10, 10, 10, 255]); // bottom S moved down
    }

    {
        // Five options pick the L bottom; text-less option keys still draw
        // their buttons.
        const five = {
            ...event,
            options: [1, 2, 3, 4, 5].map((index) => ({ nameKey: `k${index}` })),
        };
        const card = cards.composeEventCard(five, { ...parts, fonts });
        // options 254..413 -> bottom L (80) -> 494.
        assert.equal(card.height, 494);
        assert.deepEqual(px(card, 300, 470), [30, 30, 30, 255]);
    }

    {
        // A missing preferred bottom falls through to the first available.
        const card = cards.composeEventCard(event, {
            ...parts,
            backgroundBottomS: undefined,
            backgroundBottomM: undefined,
            fonts,
        });
        assert.deepEqual(px(card, 300, 330), [30, 30, 30, 255]); // L stands in
    }

    {
        // Without a font the card degrades to chrome + picture + buttons.
        const card = cards.composeEventCard(event, parts);
        assert.deepEqual(px(card, 278, 40), [120, 110, 100, 255]); // no title glyphs
        assert.deepEqual(px(card, 100, 260), [235, 225, 205, 255]); // buttons remain
    }

    assert.equal(
        cards.eventCardMarkdown('data:image/png;base64,AAA'),
        '![event card](data:image/png;base64,AAA|width=500)',
    );

    // Oversized data URLs spill into the file cache: VS Code truncates hover
    // markdown at 100k chars, which beheads an inline data URL mid-base64.
    {
        const small = 'data:image/png;base64,AAA';
        assert.equal(cards.toHoverImageUrl(small), small);

        const bigPayload = Buffer.alloc(150_000, 7).toString('base64');
        const big = `data:image/png;base64,${bigPayload}`;
        const spilled = cards.toHoverImageUrl(big);
        assert.ok(spilled.startsWith('file:///'), `expected file:/// URL, got ${spilled.slice(0, 40)}…`);
        const spilledPath = fileURLToPath(spilled);
        assert.ok(existsSync(spilledPath));
        assert.deepEqual(readFileSync(spilledPath), Buffer.alloc(150_000, 7));
        // Same payload again: LRU hit, identical URL, no duplicate file.
        assert.equal(cards.toHoverImageUrl(big), spilled);

        const markdown = cards.eventCardMarkdown(big);
        assert.ok(markdown.startsWith('![event card](file:///'), 'builder routes oversized URLs through the file cache');
        assert.ok(markdown.endsWith('|width=500)'));

        // The cache is bounded: pushing IMAGE_FILE_CACHE_LIMIT+1 distinct
        // payloads evicts (and deletes) the oldest file.
        for (let i = 0; i < 65; i++) {
            cards.toHoverImageUrl(`data:image/png;base64,${Buffer.alloc(100_000, i).toString('base64')}`);
        }
        assert.ok(!existsSync(spilledPath), 'evicted cache entry file was deleted');

        // Non-data URLs are returned untouched.
        const plain = 'file:///already/a/file.png';
        assert.equal(cards.toHoverImageUrl(plain), plain);
    }

    // Wire parsing: event payloads, optional picture asset, malformed input.
    const parsed = cards.parseHoverCardResponse({
        version: 1,
        card: {
            kind: 'event',
            event: {
                id: 'e1', picture: 'pic', titleKey: 'k',
                title: { language: 'l_english', value: 'T' },
                descKey: 'd', desc: { value: 'D' },
                options: [{ nameKey: 'a', name: { value: 'A' } }, { nameKey: 'b' }],
            },
            asset: { path: 'C:/pic.dds', rootKind: 'vanilla', extensionFallback: false },
            cardAssets: {
                backgroundTop: { path: 'C:/t.dds', rootKind: 'vanilla', extensionFallback: false },
                optionButton: { path: 'C:/b.dds', rootKind: 'vanilla', extensionFallback: false },
            },
        },
    });
    assert.equal(parsed.card.kind, 'event');
    assert.equal(parsed.card.event.options.length, 2);
    assert.equal(parsed.card.event.options[1].name, undefined);
    assert.equal(parsed.card.cardAssets.backgroundTop.path, 'C:/t.dds');
    const bare = cards.parseHoverCardResponse({
        version: 1,
        card: { kind: 'event', event: { id: 'e1', titleKey: 'k', options: [] }, cardAssets: {} },
    });
    assert.equal(bare.card.kind, 'event');
    assert.equal(bare.card.asset, undefined);
    assert.equal(cards.parseHoverCardResponse({ version: 1, card: { kind: 'event', asset: { path: 'x' } } }), undefined);
    const skipped = cards.parseHoverCardResponse({
        version: 1,
        card: { kind: 'event', event: { id: 'e1', titleKey: 'k', options: [{ nameKey: 'ok' }, 5, {}] } },
    });
    assert.equal(skipped.card.event.options.length, 1);
}

// --- GameAssetStore raster surface (hover card inputs) -----------------------------

{
    const gameRoot = mkdtempSync(join(tmpdir(), 'pdc-raster-game-'));
    try {
        mkdirSync(join(gameRoot, 'gfx', 'fonts'), { recursive: true });
        writeFileSync(join(gameRoot, 'gfx', 'fonts', 'vic_18.fnt'), [
            'info face="Test" size=18 bold=1 italic=0 charset="" stretchH=100 smooth=1 aa=1 padding=0,0,0,0 spacing=1,1',
            'common lineHeight=18 base=13 scaleW=1 scaleH=1 pages=1',
            'char id=65 x=0 y=0 width=1 height=1 xoffset=0 yoffset=0 xadvance=1 page=0',
            'kerning first=65 second=65 amount=-1',
        ].join('\n'));
        writeFileSync(join(gameRoot, 'gfx', 'fonts', 'vic_18.tga'), withData(tgaHeader(1, 1, 32, 0), [255, 255, 255, 255]));
        const texture = withData(ddsHeader(2, 1, 0x40 | DDPF_ALPHAPIXELS, null), [0, 0, 255, 255, 0, 255, 0, 255]);
        writeFileSync(join(gameRoot, 'gfx', 'x.dds'), texture);

        const store = new assets.GameAssetStore(gameRoot, undefined);
        // textureRaster decodes raw RGBA (no PNG round-trip), cached by mtime.
        const raster = await store.textureRaster(join(gameRoot, 'gfx', 'x.dds'));
        assert.equal(raster.width, 2);
        assert.deepEqual([...raster.pixels.subarray(0, 4)], [255, 0, 0, 255]);
        assert.equal(await store.textureRaster(join(gameRoot, 'gfx', 'absent.dds')), undefined);
        // mtimeOf feeds the composed-card cache key.
        const mtime = store.mtimeOf(join(gameRoot, 'gfx', 'x.dds'));
        assert.ok(Number.isFinite(mtime));
        assert.equal(store.mtimeOf(join(gameRoot, 'gfx', 'absent.dds')), undefined);
        // loadFontRasters: same fixture as loadFonts, but as pixel atlases.
        const fonts = await store.loadFontRasters();
        assert.ok(fonts.english);
        assert.equal(fonts.english.lineHeight, 18);
        assert.equal(fonts.english.chars.get(65).xAdvance, 1);
        assert.equal(fonts.english.kernings.get('65:65'), -1);
        assert.equal(fonts.english.atlas.width, 1);
        assert.deepEqual([...fonts.english.atlas.pixels], [255, 255, 255, 255]);
        assert.equal(await store.loadFontRasters(), fonts); // mtime cache returns the same pair
        assert.deepEqual(await new assets.GameAssetStore(undefined, undefined).loadFontRasters(), {});
    } finally {
        rmSync(gameRoot, { recursive: true, force: true });
    }
}

console.log('assets contract OK');
