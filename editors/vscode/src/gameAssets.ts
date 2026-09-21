// Extension-host game-asset pipeline for the mission preview and hover
// texture previews.
//
// The language server stays text-only: every pixel the preview draws —
// interface sprites (frame, arrows, mission icons) and the game's bitmap
// fonts — is read, decoded, and cached here in the extension host, then
// shipped to the webview as one-shot `data:image/png;base64,` payloads (or,
// for hovers, embedded into the tooltip markdown).
//
// The DDS decoder, the `.gfx` sprite index, and the PNG encoder are ports of
// the former Rust implementation (`crates/game/src/eu4/mission/texture/`);
// the TGA decoder and BMFont (`.fnt`) parser are new — vanilla English font
// atlases are uncompressed TGA and both game fonts are BMFont text format.

import * as fs from 'fs';
import * as path from 'path';
import { deflateSync } from 'zlib';

/** Sprite name of the mission-node frame texture (from `countrymissionsview.gfx`). */
export const FRAME_SPRITE = 'GFX_mission_icons_frame';

/** Maximum texture dimension accepted by the decoders (guards hostile files). */
const MAX_TEXTURE_DIMENSION = 4096;
/** Maximum single asset file size read from disk (guards hostile files). */
const MAX_ASSET_BYTES = 8 * 1024 * 1024;
/** Maximum `.gfx` sources indexed per root (guards pathological trees). */
const MAX_SPRITE_INDEX_FILES = 256;
/** Steam app id of Europa Universalis IV (workshop content root). */
const EU4_APP_ID = '236850';
/** Steam installation directory name of Europa Universalis IV. */
const EU4_INSTALL_DIR = 'Europa Universalis IV';

/** Failure to decode an image asset. The message mirrors the former Rust errors. */
export class AssetDecodeError extends Error {
    public constructor(message: string) {
        super(message);
        this.name = 'AssetDecodeError';
    }
}

/** One decoded image: RGBA8 pixels, row-major, tightly packed. */
export interface DecodedImage {
    width: number;
    height: number;
    pixels: Uint8Array;
}

// --- DDS ------------------------------------------------------------------

const DDPF_ALPHAPIXELS = 0x0000_0001;
const DDPF_FOURCC = 0x0000_0004;
const DDPF_PITCH = 0x0000_0008;

/**
 * Decodes one DDS file to RGBA8 (mip level 0 only). Covers the formats EU4
 * ships: uncompressed 24/32-bit masked bitmaps and DXT1/3/5 compression.
 */
export function decodeDds(bytes: Uint8Array): DecodedImage {
    if (bytes.length < 128 || String.fromCharCode(...bytes.subarray(0, 4)) !== 'DDS ') {
        throw new AssetDecodeError('missing DDS header');
    }
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    const height = view.getUint32(12, true);
    const width = view.getUint32(16, true);
    const pitch = view.getUint32(20, true);
    if (width === 0 || height === 0 || width > MAX_TEXTURE_DIMENSION || height > MAX_TEXTURE_DIMENSION) {
        throw new AssetDecodeError('invalid texture dimensions');
    }
    const formatFlags = view.getUint32(80, true);
    if ((formatFlags & DDPF_FOURCC) !== 0) {
        const fourcc = String.fromCharCode(...bytes.subarray(84, 88));
        const blockBytes = fourcc === 'DXT1' ? 8 : fourcc === 'DXT3' || fourcc === 'DXT5' ? 16 : 0;
        if (blockBytes === 0) {
            throw new AssetDecodeError('unsupported fourcc (only DXT1/3/5)');
        }
        const blocksW = Math.ceil(width / 4);
        const blocksH = Math.ceil(height / 4);
        const levelBytes = blocksW * blocksH * blockBytes;
        if (bytes.length - 128 < levelBytes) {
            throw new AssetDecodeError('truncated pixel data');
        }
        const pixels = new Uint8Array(width * height * 4);
        decodeCompressed(fourcc, width, height, bytes.subarray(128, 128 + levelBytes), pixels);
        return { width, height, pixels };
    }
    return decodeUncompressedDds(view, bytes, width, height, pitch, formatFlags);
}

function decodeUncompressedDds(
    view: DataView,
    bytes: Uint8Array,
    width: number,
    height: number,
    pitch: number,
    formatFlags: number,
): DecodedImage {
    const bitCount = view.getUint32(88, true);
    const rMask = view.getUint32(92, true);
    const gMask = view.getUint32(96, true);
    const bMask = view.getUint32(100, true);
    const aMask = view.getUint32(104, true);
    const bytesPerPixel = bitCount === 32 ? 4 : bitCount === 24 ? 3 : 0;
    if (bytesPerPixel === 0) {
        throw new AssetDecodeError('unsupported bit depth (only 24/32-bit)');
    }
    const stride = (formatFlags & DDPF_PITCH) !== 0 ? pitch : width * bytesPerPixel;
    if (stride < width * bytesPerPixel) {
        throw new AssetDecodeError('invalid stride');
    }
    if (bytes.length - 128 < stride * height) {
        throw new AssetDecodeError('truncated pixel data');
    }
    // An 8-bit contiguous mask maps directly to a byte offset within the pixel.
    const channel = (mask: number): number => {
        if (mask === 0 || popcount(mask) !== 8) {
            return -1;
        }
        return Math.log2(mask & -mask) / 8;
    };
    const r = channel(rMask);
    const g = channel(gMask);
    const b = channel(bMask);
    const a = channel(aMask) !== -1
        ? channel(aMask)
        : (formatFlags & DDPF_ALPHAPIXELS) !== 0 && bitCount === 32
            ? 3
            : -1;
    if (r === -1 || g === -1 || b === -1) {
        throw new AssetDecodeError('missing or non-8-bit color masks');
    }
    const pixels = new Uint8Array(width * height * 4);
    const data = bytes.subarray(128);
    for (let y = 0; y < height; y += 1) {
        for (let x = 0; x < width; x += 1) {
            const source = y * stride + x * bytesPerPixel;
            const target = (y * width + x) * 4;
            pixels[target] = data[source + r];
            pixels[target + 1] = data[source + g];
            pixels[target + 2] = data[source + b];
            pixels[target + 3] = a === -1 ? 255 : data[source + a];
        }
    }
    return { width, height, pixels };
}

function popcount(value: number): number {
    let count = 0;
    while (value !== 0) {
        value &= value - 1;
        count += 1;
    }
    return count;
}

function decodeCompressed(
    fourcc: string,
    width: number,
    height: number,
    blocks: Uint8Array,
    pixels: Uint8Array,
): void {
    const blocksW = Math.ceil(width / 4);
    const blocksH = Math.ceil(height / 4);
    const blockBytes = blocks.length / (blocksW * blocksH);
    let blockIndex = 0;
    for (let by = 0; by < blocksH; by += 1) {
        for (let bx = 0; bx < blocksW; bx += 1) {
            const block = blocks.subarray(blockIndex, blockIndex + blockBytes);
            blockIndex += blockBytes;
            const rgba = fourcc === 'DXT1'
                ? decodeDxt1Block(block)
                : fourcc === 'DXT3'
                    ? decodeDxt3Block(block)
                    : decodeDxt5Block(block);
            for (let py = 0; py < 4; py += 1) {
                for (let px = 0; px < 4; px += 1) {
                    const x = bx * 4 + px;
                    const y = by * 4 + py;
                    if (x >= width || y >= height) {
                        continue;
                    }
                    const target = (y * width + x) * 4;
                    const source = (py * 4 + px) * 4;
                    pixels[target] = rgba[source];
                    pixels[target + 1] = rgba[source + 1];
                    pixels[target + 2] = rgba[source + 2];
                    pixels[target + 3] = rgba[source + 3];
                }
            }
        }
    }
}

function rgb565(value: number): [number, number, number] {
    const r = (value >> 11) & 0x1f;
    const g = (value >> 5) & 0x3f;
    const b = value & 0x1f;
    return [(r << 3) | (r >> 2), (g << 2) | (g >> 4), (b << 3) | (b >> 2)];
}

function mix(a: number, b: number, aWeight: number, denom: number): number {
    return Math.trunc((a * aWeight + b * (denom - aWeight)) / denom);
}

/** DXT1 color palette: 4 colors, or 3 colors plus transparent black. */
function dxt1Palette(c0Value: number, c1Value: number): number[][] {
    const c0 = rgb565(c0Value);
    const c1 = rgb565(c1Value);
    if (c0Value > c1Value) {
        return [
            [c0[0], c0[1], c0[2], 255],
            [c1[0], c1[1], c1[2], 255],
            [mix(c0[0], c1[0], 2, 3), mix(c0[1], c1[1], 2, 3), mix(c0[2], c1[2], 2, 3), 255],
            [mix(c0[0], c1[0], 1, 3), mix(c0[1], c1[1], 1, 3), mix(c0[2], c1[2], 1, 3), 255],
        ];
    }
    return [
        [c0[0], c0[1], c0[2], 255],
        [c1[0], c1[1], c1[2], 255],
        [mix(c0[0], c1[0], 1, 2), mix(c0[1], c1[1], 1, 2), mix(c0[2], c1[2], 1, 2), 255],
        [0, 0, 0, 0],
    ];
}

function decodeDxt1Block(block: Uint8Array): number[] {
    if (block.length < 8) {
        throw new AssetDecodeError('truncated DXT1 block');
    }
    const palette = dxt1Palette(block[0] | (block[1] << 8), block[2] | (block[3] << 8));
    const indices = (block[4] | (block[5] << 8) | (block[6] << 16) | (block[7] << 24)) >>> 0;
    const out = new Array<number>(64).fill(0);
    for (let i = 0; i < 16; i += 1) {
        const code = (indices >>> (i * 2)) & 3;
        out[i * 4] = palette[code][0];
        out[i * 4 + 1] = palette[code][1];
        out[i * 4 + 2] = palette[code][2];
        out[i * 4 + 3] = palette[code][3];
    }
    return out;
}

function decodeDxt3Block(block: Uint8Array): number[] {
    if (block.length < 16) {
        throw new AssetDecodeError('truncated DXT3 block');
    }
    const out = decodeDxt1Block(block.subarray(8));
    // Explicit 4-bit alpha: two pixels per byte, low nibble first.
    for (let i = 0; i < 16; i += 1) {
        const nibble = i % 2 === 0 ? block[i >> 1] & 0x0f : block[i >> 1] >> 4;
        out[i * 4 + 3] = nibble * 17;
    }
    return out;
}

function decodeDxt5Block(block: Uint8Array): number[] {
    if (block.length < 16) {
        throw new AssetDecodeError('truncated DXT5 block');
    }
    const a0 = block[0];
    const a1 = block[1];
    const alphas = a0 > a1
        ? [
            a0,
            a1,
            mix(a0, a1, 6, 7),
            mix(a0, a1, 5, 7),
            mix(a0, a1, 4, 7),
            mix(a0, a1, 3, 7),
            mix(a0, a1, 2, 7),
            mix(a0, a1, 1, 7),
        ]
        : [a0, a1, mix(a0, a1, 4, 5), mix(a0, a1, 3, 5), mix(a0, a1, 2, 5), mix(a0, a1, 1, 5), 0, 255];
    const out = decodeDxt1Block(block.subarray(8));
    for (let i = 0; i < 16; i += 1) {
        const bit = i * 3;
        const word = block[2 + (bit >> 3)] | (block[3 + (bit >> 3)] << 8);
        out[i * 4 + 3] = alphas[(word >> (bit & 7)) & 0x07];
    }
    return out;
}

// --- TGA ------------------------------------------------------------------

/**
 * Decodes an uncompressed true-color TGA (image type 2, 24/32 bpp) to RGBA8 —
 * the format of the vanilla bitmap-font atlases such as `vic_18.tga`.
 */
export function decodeTga(bytes: Uint8Array): DecodedImage {
    if (bytes.length < 18) {
        throw new AssetDecodeError('missing TGA header');
    }
    const idLength = bytes[0];
    const cmapType = bytes[1];
    const imageType = bytes[2];
    const width = bytes[12] | (bytes[13] << 8);
    const height = bytes[14] | (bytes[15] << 8);
    const bpp = bytes[16];
    const descriptor = bytes[17];
    if (cmapType !== 0 || imageType !== 2) {
        throw new AssetDecodeError('unsupported TGA variant (only uncompressed true-color)');
    }
    if (width === 0 || height === 0 || width > MAX_TEXTURE_DIMENSION || height > MAX_TEXTURE_DIMENSION) {
        throw new AssetDecodeError('invalid texture dimensions');
    }
    const bytesPerPixel = bpp === 32 ? 4 : bpp === 24 ? 3 : 0;
    if (bytesPerPixel === 0) {
        throw new AssetDecodeError('unsupported bit depth (only 24/32-bit)');
    }
    const data = bytes.subarray(18 + idLength);
    if (data.length < width * height * bytesPerPixel) {
        throw new AssetDecodeError('truncated pixel data');
    }
    // Descriptor bit 5 selects the row order: set = top-left origin (no flip),
    // clear = bottom-left origin (rows stored bottom-up).
    const topDown = (descriptor & 0x20) !== 0;
    const pixels = new Uint8Array(width * height * 4);
    for (let y = 0; y < height; y += 1) {
        const sourceY = topDown ? y : height - 1 - y;
        for (let x = 0; x < width; x += 1) {
            const source = (sourceY * width + x) * bytesPerPixel;
            const target = (y * width + x) * 4;
            pixels[target] = data[source + 2];
            pixels[target + 1] = data[source + 1];
            pixels[target + 2] = data[source];
            pixels[target + 3] = bpp === 32 ? data[source + 3] : 255;
        }
    }
    return { width, height, pixels };
}

/** Decodes a DDS or TGA file by magic. */
export function decodeAtlasImage(bytes: Uint8Array): DecodedImage {
    if (bytes.length >= 4 && bytes[0] === 0x44 && bytes[1] === 0x44 && bytes[2] === 0x53 && bytes[3] === 0x20) {
        return decodeDds(bytes);
    }
    return decodeTga(bytes);
}

// --- PNG ------------------------------------------------------------------

const CRC_TABLE = (() => {
    const table = new Uint32Array(256);
    for (let n = 0; n < 256; n += 1) {
        let c = n;
        for (let k = 0; k < 8; k += 1) {
            c = (c & 1) !== 0 ? 0xedb8_8320 ^ (c >>> 1) : c >>> 1;
        }
        table[n] = c >>> 0;
    }
    return table;
})();

function crc32(data: Uint8Array): number {
    let crc = 0xffff_ffff;
    for (const byte of data) {
        crc = CRC_TABLE[(crc ^ byte) & 0xff] ^ (crc >>> 8);
    }
    return (crc ^ 0xffff_ffff) >>> 0;
}

function pngChunk(kind: string, payload: Uint8Array): Buffer {
    const out = Buffer.alloc(payload.length + 12);
    out.writeUInt32BE(payload.length, 0);
    out.write(kind, 4, 'latin1');
    Buffer.from(payload.buffer, payload.byteOffset, payload.byteLength).copy(out, 8);
    out.writeUInt32BE(crc32(out.subarray(4, 8 + payload.length)), 8 + payload.length);
    return out;
}

/** Encodes an RGBA8 image as a PNG (8-bit truecolor with alpha, filter 0). */
export function encodePng(image: DecodedImage): Buffer {
    const { width, height } = image;
    const raw = Buffer.alloc((width * 4 + 1) * height);
    for (let y = 0; y < height; y += 1) {
        const rowStart = y * (width * 4 + 1);
        raw[rowStart] = 0; // filter type: none
        Buffer.from(
            image.pixels.buffer,
            image.pixels.byteOffset + y * width * 4,
            width * 4,
        ).copy(raw, rowStart + 1);
    }
    const ihdr = Buffer.alloc(13);
    ihdr.writeUInt32BE(width, 0);
    ihdr.writeUInt32BE(height, 4);
    ihdr[8] = 8; // bit depth
    ihdr[9] = 6; // color type: RGBA
    // deflateSync emits a full zlib stream (header + adler32), exactly what a
    // PNG IDAT chunk requires.
    const idat = deflateSync(raw, { level: 6 });
    return Buffer.concat([
        Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
        pngChunk('IHDR', ihdr),
        pngChunk('IDAT', idat),
        pngChunk('IEND', new Uint8Array(0)),
    ]);
}

/** Encodes a decoded image as a `data:image/png;base64,...` URL. */
export function pngDataUrl(image: DecodedImage): string {
    return `data:image/png;base64,${encodePng(image).toString('base64')}`;
}

// --- BMFont (.fnt) ----------------------------------------------------------

/** One BMFont glyph placement, in atlas pixels. */
export interface BmChar {
    x: number;
    y: number;
    width: number;
    height: number;
    xOffset: number;
    yOffset: number;
    xAdvance: number;
}

/** Parsed BMFont text-format font (metrics only; the atlas is decoded apart). */
export interface BmFont {
    lineHeight: number;
    base: number;
    chars: Map<number, BmChar>;
    /** Compact kerning rows: `[first, second, amount]`. */
    kerningPairs: number[][];
    /** kerning amount for a (first, second) pair, when present. */
    kerning: (first: number, second: number) => number;
}

/**
 * Parses the BMFont text format the game uses (`vic_18.fnt`, `zh-hans-16.fnt`):
 * `common`/`char`/`kerning` lines with `key=value` fields. Unknown lines are
 * ignored; `char` lines with missing numeric fields are skipped rather than
 * fatal — a partially readable font still renders most glyphs.
 */
export function parseBmFont(text: string): BmFont | undefined {
    let lineHeight: number | undefined;
    let base = 0;
    const chars = new Map<number, BmChar>();
    const kerningPairs: number[][] = [];
    for (const line of text.split(/\r?\n/)) {
        const fields = (key: string): Map<string, string> | undefined => {
            const match = line.match(new RegExp(`^${key}\\b`));
            if (!match) {
                return undefined;
            }
            const values = new Map<string, string>();
            for (const match of line.matchAll(/(\w+)\s*=\s*(?:"([^"]*)"|(\S+))/g)) {
                values.set(match[1], match[2] ?? match[3] ?? '');
            }
            return values;
        };
        const number = (values: Map<string, string>, key: string): number | undefined => {
            const raw = values.get(key);
            if (raw === undefined) {
                return undefined;
            }
            const parsed = Number(raw);
            return Number.isFinite(parsed) ? parsed : undefined;
        };
        const common = fields('common');
        if (common) {
            lineHeight = number(common, 'lineHeight');
            base = number(common, 'base') ?? 0;
            continue;
        }
        const char = fields('char');
        if (char) {
            const id = number(char, 'id');
            const x = number(char, 'x');
            const y = number(char, 'y');
            const width = number(char, 'width');
            const height = number(char, 'height');
            const xOffset = number(char, 'xoffset');
            const yOffset = number(char, 'yoffset');
            const xAdvance = number(char, 'xadvance');
            if (
                id === undefined || x === undefined || y === undefined || width === undefined
                || height === undefined || xOffset === undefined || yOffset === undefined
                || xAdvance === undefined
            ) {
                continue;
            }
            chars.set(id, { x, y, width, height, xOffset, yOffset, xAdvance });
            continue;
        }
        const kerning = fields('kerning');
        if (kerning) {
            const first = number(kerning, 'first');
            const second = number(kerning, 'second');
            const amount = number(kerning, 'amount');
            if (first !== undefined && second !== undefined && amount !== undefined) {
                kerningPairs.push([first, second, amount]);
            }
        }
    }
    if (lineHeight === undefined) {
        return undefined;
    }
    const kerningMap = new Map(kerningPairs.map(([first, second, amount]) => [`${first}:${second}`, amount]));
    return {
        lineHeight,
        base,
        chars,
        kerningPairs,
        kerning: (first, second) => kerningMap.get(`${first}:${second}`) ?? 0,
    };
}

// --- interface/*.gfx sprite index -------------------------------------------

/** One parsed sprite mapping: `name` -> normalized `texturefile` path. */
export interface SpriteEntry {
    name: string;
    textureFile: string;
    /** `noOfFrames` when the spriteType declares a horizontal frame strip. */
    frames?: number;
}

/** One sprite of the merged index plus the root kind that provided it. */
export interface CataloguedSprite extends SpriteEntry {
    /** `vanilla` = the game installation, `mod` = a mod root (workspace or
     * explicit `paradoxcode.modDirectory`), which overrides vanilla by name. */
    origin: 'vanilla' | 'mod';
}

/** One decoded sprite ready for icon-sized display (first frame only). */
export interface IconPreview {
    url: string;
    width: number;
    height: number;
}

/** Directory vanilla mission-icon textures live under (normalized form of
 * the `gfx//interface//missions//…` spellings the game's .gfx files use). */
const MISSION_TEXTURE_PREFIX = 'gfx/interface/missions/';

/**
 * Union filter for sprites usable as mission `icon` values. The vanilla set
 * proves both arms are needed: name-only would miss the 545 sprites that
 * only the texture directory identifies (base-game icons live in
 * `countrymissionsview.gfx`), directory-only would miss sprites named
 * `mission_*` with textures elsewhere; together they cover every icon value
 * vanilla missions use. Deliberately over-inclusive: a few unreferenced
 * mission-view UI sprites ride along.
 */
export function isMissionIconSprite(entry: { name: string; textureFile: string }): boolean {
    if (entry.name.toLowerCase().startsWith('mission')) {
        return true;
    }
    return entry.textureFile.toLowerCase().startsWith(MISSION_TEXTURE_PREFIX);
}

/** Crops a horizontal frame strip to its leftmost frame; anything without a
 * multi-frame declaration returns the image unchanged. */
export function cropFirstFrame(image: DecodedImage, frames?: number): DecodedImage {
    if (frames === undefined || frames <= 1 || frames > image.width) {
        return image;
    }
    const width = Math.floor(image.width / frames);
    const pixels = new Uint8Array(width * image.height * 4);
    for (let y = 0; y < image.height; y += 1) {
        const source = y * image.width * 4;
        pixels.set(image.pixels.subarray(source, source + width * 4), y * width * 4);
    }
    return { width, height: image.height, pixels };
}

/** One decoded texture file ready for markdown embedding. */
export interface TextureImageData {
    url: string;
    width: number;
    height: number;
}

/**
 * Normalizes an EU4 texture path (`gfx//interface//x.dds`, `gfx/interface/…`,
 * or Windows-separator `gfx\interface\x.dds`) into a clean root-relative path
 * with `/` separators. Escaping or absolute paths yield the empty string.
 */
export function normalizeTexturePath(value: string): string {
    const trimmed = value.trim().replace(/^"|"$/g, '').trim();
    let cleaned = trimmed.replaceAll('\\', '/').replaceAll('//', '/');
    while (cleaned.startsWith('/')) {
        cleaned = cleaned.slice(1);
    }
    if (cleaned.startsWith('..') || cleaned.includes('\0') || cleaned[1] === ':') {
        return '';
    }
    return cleaned;
}

/** The `.tga`/`.dds` extension-drift spelling of a normalized path, or
 * `undefined` when the extension is neither (mirrors the engine texture
 * catalog's fallback). */
function extensionDriftSpelling(normalized: string): string | undefined {
    const dot = normalized.lastIndexOf('.');
    if (dot === -1) {
        return undefined;
    }
    const extension = normalized.slice(dot + 1).toLowerCase();
    const replacement = extension === 'tga' ? '.dds' : extension === 'dds' ? '.tga' : undefined;
    return replacement === undefined ? undefined : `${normalized.slice(0, dot)}${replacement}`;
}

/** Probes `root`/`relative` for a file: verbatim first, then with the game's
 * case-insensitive resolution — every path segment is matched against the
 * directory listing ignoring case, because the engine ignores casing on every
 * platform while `statSync` alone stays case-sensitive on Linux. */
function probeFile(root: string, relative: string): string | undefined {
    const direct = path.join(root, relative);
    try {
        if (fs.statSync(direct).isFile()) {
            return direct;
        }
    } catch {
        // Missing verbatim: fall through to the case-insensitive walk.
    }
    const folded = relative.toLowerCase();
    let current = root;
    let remaining = folded;
    while (remaining !== '') {
        const slash = remaining.indexOf('/');
        const segment = slash === -1 ? remaining : remaining.slice(0, slash);
        remaining = slash === -1 ? '' : remaining.slice(slash + 1);
        let match: string | undefined;
        try {
            for (const name of fs.readdirSync(current)) {
                if (name.toLowerCase() === segment) {
                    match = name;
                    break;
                }
            }
        } catch {
            return undefined;
        }
        if (match === undefined) {
            return undefined;
        }
        current = path.join(current, match);
    }
    try {
        return fs.statSync(current).isFile() ? current : undefined;
    } catch {
        return undefined;
    }
}

/**
 * Parses one `.gfx` file and returns its `spriteType` entries in source
 * order. Brace matching keeps the scan tolerant of stray quotes and comments;
 * blocks without both `name` and `texturefile` are dropped.
 */
export function parseGfxSprites(source: string): SpriteEntry[] {
    const entries: SpriteEntry[] = [];
    const blockStart = /(?:^|[\s{])spriteType\s*=\s*\{/g;
    let match: RegExpExecArray | null;
    while ((match = blockStart.exec(source)) !== null) {
        const open = source.indexOf('{', match.index + match[0].length - 1);
        if (open === -1) {
            break;
        }
        let depth = 1;
        let close = open + 1;
        while (close < source.length && depth > 0) {
            const ch = source[close];
            depth += ch === '{' ? 1 : ch === '}' ? -1 : 0;
            close += 1;
        }
        if (depth !== 0) {
            break;
        }
        const block = source.slice(open + 1, close - 1);
        blockStart.lastIndex = close;
        const name = block.match(/\bname\s*=\s*(?:"([^"]+)"|([A-Za-z0-9_.\-]+))/);
        const texture = block.match(/\btexturefile\s*=\s*(?:"([^"]+)"|(\S+))/);
        const frames = block.match(/\bnoofframes\s*=\s*"?(\d+)/i);
        const nameValue = name?.[1] ?? name?.[2];
        const textureValue = texture?.[1] ?? texture?.[2];
        const framesValue = frames ? Number(frames[1]) : undefined;
        if (nameValue && textureValue) {
            const normalized = normalizeTexturePath(textureValue);
            if (normalized !== '') {
                entries.push({
                    name: nameValue,
                    textureFile: normalized,
                    frames: framesValue !== undefined && framesValue > 0 ? framesValue : undefined,
                });
            }
        }
    }
    return entries;
}

/**
 * Builds a name -> sprite entry map from several `.gfx` sources. Earlier
 * files win; repeated names (mod overrides) keep their first occurrence,
 * matching the game's `spriteType` lookup.
 */
export function buildSpriteIndex(files: string[]): Map<string, SpriteEntry> {
    const index = new Map<string, SpriteEntry>();
    for (const source of files) {
        for (const entry of parseGfxSprites(source)) {
            if (!index.has(entry.name)) {
                index.set(entry.name, entry);
            }
        }
    }
    return index;
}

// --- Steam / workshop discovery ----------------------------------------------

/**
 * Standard Steam roots plus every library listed in `libraryfolders.vdf`.
 * Returned paths are library roots (directories containing `steamapps`).
 */
export function findSteamLibraries(): string[] {
    const libraries: string[] = [];
    const home = process.env.USERPROFILE ?? process.env.HOME ?? '';
    const candidates = [
        process.env['ProgramFiles(x86)'] ? path.join(process.env['ProgramFiles(x86)']!, 'Steam') : undefined,
        process.env.ProgramFiles ? path.join(process.env.ProgramFiles, 'Steam') : undefined,
        home ? path.join(home, 'Steam') : undefined,
    ].filter((candidate): candidate is string => candidate !== undefined);
    for (const root of candidates) {
        if (!libraries.includes(root)) {
            libraries.push(root);
        }
        try {
            const vdf = fs.readFileSync(path.join(root, 'steamapps', 'libraryfolders.vdf'), 'latin1');
            for (const [_, entry] of vdf.matchAll(/"path"\s+"([^"]+)"/g)) {
                const library = entry.replaceAll('\\\\', '\\');
                if (!libraries.includes(library)) {
                    libraries.push(library);
                }
            }
        } catch {
            // No vdf (or unreadable): the root itself is the only candidate.
        }
    }
    return libraries;
}

let cachedGameDirectory: string | undefined;
let cachedGameDirectoryProbed = false;

/**
 * Locates the local EU4 installation: an explicitly configured directory
 * wins; otherwise the standard Steam libraries are probed once per session.
 */
export function findGameDirectory(configured?: string): string | undefined {
    if (configured && configured.trim() !== '') {
        try {
            return fs.statSync(configured).isDirectory() ? configured : undefined;
        } catch {
            return undefined;
        }
    }
    if (cachedGameDirectoryProbed) {
        return cachedGameDirectory;
    }
    cachedGameDirectoryProbed = true;
    for (const library of findSteamLibraries()) {
        const gameDir = path.join(library, 'steamapps', 'common', EU4_INSTALL_DIR);
        try {
            if (fs.statSync(gameDir).isDirectory()) {
                cachedGameDirectory = gameDir;
                return gameDir;
            }
        } catch {
            // Not in this library; keep probing.
        }
    }
    return undefined;
}

/**
 * Locates a Chinese localisation mod carrying the `zh-hans-16` bitmap font,
 * the font `vic_18` is remapped to in game by the 1.37 Chinese language mods.
 * An explicitly configured mod directory wins; otherwise the workshop
 * content of the Steam library holding the game is scanned (Steam installs
 * workshop content into the game's library), newest match first.
 */
export function findChineseFontMod(gameDirectory: string | undefined, configured?: string): string | undefined {
    const hasFont = (root: string): boolean => {
        try {
            return fs.statSync(path.join(root, 'gfx', 'fonts', 'zh-hans-16.fnt')).isFile();
        } catch {
            return false;
        }
    };
    if (configured && configured.trim() !== '') {
        return hasFont(configured) ? configured : undefined;
    }
    const workshopRoots: string[] = [];
    for (const library of findSteamLibraries()) {
        workshopRoots.push(path.join(library, 'steamapps', 'workshop', 'content', EU4_APP_ID));
    }
    if (gameDirectory) {
        // <library>/steamapps/common/Europa Universalis IV -> workshop/content/<appid>
        const steamapps = path.resolve(gameDirectory, '..', '..');
        workshopRoots.unshift(path.join(steamapps, 'workshop', 'content', EU4_APP_ID));
    }
    const matches: { directory: string; modified: number }[] = [];
    const seen = new Set<string>();
    for (const root of workshopRoots) {
        if (seen.has(root)) {
            continue;
        }
        seen.add(root);
        let entries: fs.Dirent[];
        try {
            entries = fs.readdirSync(root, { withFileTypes: true });
        } catch {
            continue; // No workshop content for EU4 in this library.
        }
        for (const entry of entries) {
            if (entry.isDirectory() && hasFont(path.join(root, entry.name))) {
                try {
                    const modified = fs.statSync(path.join(root, entry.name)).mtimeMs;
                    matches.push({ directory: path.join(root, entry.name), modified });
                } catch {
                    // Unreadable metadata: still a candidate, oldest priority.
                    matches.push({ directory: path.join(root, entry.name), modified: 0 });
                }
            }
        }
    }
    matches.sort((a, b) => b.modified - a.modified);
    return matches[0]?.directory;
}

// --- asset store -------------------------------------------------------------

/** Wire shape of one bitmap font shipped to the webview (`assets` message). */
export interface FontPayload {
    id: 'english' | 'chinese';
    lineHeight: number;
    base: number;
    atlasUrl: string;
    /** Compact glyph rows: `[id, x, y, w, h, xOffset, yOffset, xAdvance]`. */
    chars: number[][];
    /** Compact kerning rows: `[first, second, amount]`. */
    kernings: number[][];
}

/** Decoded font pair for the preview (`english` is vanilla `vic_18`). */
export interface FontAssets {
    english?: FontPayload;
    chinese?: FontPayload;
}

/** One bitmap font with its atlas decoded to RGBA (unlike the webview payload). */
export interface FontRaster {
    id: 'english' | 'chinese';
    lineHeight: number;
    base: number;
    chars: Map<number, BmChar>;
    /** Kerning amounts keyed `first:second` (BMFont codepoints). */
    kernings: Map<string, number>;
    atlas: DecodedImage;
}

/** Decoded font pair for Node-side compositing (hover mission cards). */
export interface FontBook {
    english?: FontRaster;
    chinese?: FontRaster;
}

/**
 * Loads and caches preview assets (interface sprites and bitmap fonts) for
 * one game installation plus an optional Chinese font mod directory.
 *
 * All results are cached by file mtime so repeated preview refreshes stay
 * cheap, and every failure degrades to `undefined` — a preview must never
 * fail because an asset is missing.
 */
export class GameAssetStore {
    private readonly gameDirectory: string | undefined;
    private readonly chineseFontDirectory: string | undefined;
    /** Mod roots (explicit mod directory, then workspace folders). */
    private readonly modRoots: readonly string[];
    private spriteIndex: Map<string, CataloguedSprite> | undefined;
    private readonly spriteCache = new Map<string, { modified: number; url: string }>();
    private readonly iconUrlCache = new Map<string, { modified: number; preview: IconPreview }>();
    private readonly textureFileCache = new Map<string, { modified: number; image: TextureImageData }>();
    private readonly textureRasterCache = new Map<string, { modified: number; image: DecodedImage }>();
    private fontCache: { modified: string; fonts: FontAssets } | undefined;
    private rasterCache: { modified: string; fonts: FontBook } | undefined;

    public constructor(
        gameDirectory: string | undefined,
        chineseFontDirectory: string | undefined,
        modRoots: readonly string[] = [],
    ) {
        this.gameDirectory = gameDirectory;
        this.chineseFontDirectory = chineseFontDirectory;
        this.modRoots = modRoots;
    }

    /**
     * Resolves sprite names to PNG data URLs. Unknown names and decode
     * failures are simply absent from the result; only newly loaded or
     * mtime-changed sprites are returned, so callers can push incremental
     * `assets` messages without re-sending the world. Names in `force` are
     * also returned from the warm cache, for a consumer that lost part of
     * its accumulated set (a rebuilt preview webview).
     */
    public async spriteUrls(names: readonly string[], force?: ReadonlySet<string>): Promise<Record<string, string>> {
        const urls: Record<string, string> = {};
        if (!this.gameDirectory && this.modRoots.length === 0) {
            return urls;
        }
        const index = this.spriteIndex ?? this.loadSpriteIndex();
        this.spriteIndex = index;
        for (const name of names) {
            const entry = index.get(name);
            if (!entry) {
                continue;
            }
            const file = this.resolveTexture(entry.textureFile);
            if (!file) {
                continue;
            }
            const modified = this.fileModified(file);
            if (modified === undefined) {
                continue;
            }
            const cached = this.spriteCache.get(name);
            if (cached && cached.modified === modified) {
                if (force?.has(name)) {
                    urls[name] = cached.url;
                }
                continue;
            }
            const image = await this.textureFile(file);
            if (!image) {
                continue;
            }
            this.spriteCache.set(name, { modified, url: image.url });
            urls[name] = image.url;
        }
        return urls;
    }

    /** Looks one sprite name up in the merged mod + game sprite index. */
    public spriteTexture(name: string): SpriteEntry | undefined {
        const index = this.spriteIndex ?? this.loadSpriteIndex();
        this.spriteIndex = index;
        return index.get(name);
    }

    /** The whole merged sprite index with the providing root kind attached.
     * The catalog the mission-icon picker browses; loaded on first use. */
    public spriteCatalog(): Map<string, CataloguedSprite> {
        const index = this.spriteIndex ?? this.loadSpriteIndex();
        this.spriteIndex = index;
        return index;
    }

    /**
     * Resolves sprite names to first-frame PNG previews (the icon-sized
     * images the picker grid and completion documentation render). Same
     * contract as `spriteUrls`: unknown names and decode failures are absent
     * from the result, and only newly loaded or mtime-changed sprites are
     * returned unless `force` names them.
     */
    public async spriteIconUrls(
        names: readonly string[],
        force?: ReadonlySet<string>,
    ): Promise<Record<string, IconPreview>> {
        const previews: Record<string, IconPreview> = {};
        if (!this.gameDirectory && this.modRoots.length === 0) {
            return previews;
        }
        const index = this.spriteIndex ?? this.loadSpriteIndex();
        this.spriteIndex = index;
        for (const name of names) {
            const entry = index.get(name);
            if (!entry) {
                continue;
            }
            const file = this.resolveTexture(entry.textureFile);
            if (!file) {
                continue;
            }
            const modified = this.fileModified(file);
            if (modified === undefined) {
                continue;
            }
            const cached = this.iconUrlCache.get(name);
            if (cached && cached.modified === modified) {
                if (force?.has(name)) {
                    previews[name] = cached.preview;
                }
                continue;
            }
            const raster = await this.textureRaster(file);
            if (!raster) {
                continue;
            }
            const image = cropFirstFrame(raster, entry.frames);
            const preview: IconPreview = {
                url: pngDataUrl(image),
                width: image.width,
                height: image.height,
            };
            this.iconUrlCache.set(name, { modified, preview });
            previews[name] = preview;
        }
        return previews;
    }

    /**
     * Resolves one game-root-relative texture path to a file on disk, mod
     * roots before the game installation: a drop-in texture replacement in
     * the mod wins over the vanilla file it shadows. Probes are
     * case-insensitive like the game's own resolution, and when the exact
     * spelling is absent from every root the `.tga`/`.dds` drift spelling is
     * probed the same way, mirroring the server's texture catalog.
     */
    public resolveTexture(texturePath: string): string | undefined {
        const normalized = normalizeTexturePath(texturePath);
        if (normalized === '') {
            return undefined;
        }
        // Exact spellings resolve before drifted ones across every root, so
        // an exact game file wins over a drifted mod file (the catalog's load
        // order: the first providing root wins within each pass).
        const drifted = extensionDriftSpelling(normalized);
        const candidates = drifted === undefined ? [normalized] : [normalized, drifted];
        for (const candidate of candidates) {
            for (const root of [...this.modRoots, this.gameDirectory]) {
                if (!root) {
                    continue;
                }
                const file = probeFile(root, candidate);
                if (file !== undefined) {
                    return file;
                }
            }
        }
        return undefined;
    }

    /**
     * Decodes one texture file to a PNG data URL with its pixel size, cached
     * by mtime. Missing or undecodable files yield `undefined` — a preview
     * must never surface a decode error.
     */
    public async textureFile(file: string): Promise<TextureImageData | undefined> {
        const modified = this.fileModified(file);
        if (modified === undefined) {
            return undefined;
        }
        const cached = this.textureFileCache.get(file);
        if (cached && cached.modified === modified) {
            return cached.image;
        }
        const bytes = await this.readBytes(file);
        if (!bytes) {
            return undefined;
        }
        try {
            const decoded = decodeAtlasImage(bytes);
            const image: TextureImageData = {
                url: pngDataUrl(decoded),
                width: decoded.width,
                height: decoded.height,
            };
            this.textureFileCache.set(file, { modified, image });
            return image;
        } catch {
            return undefined;
        }
    }

    /**
     * Decodes one texture file to raw RGBA for Node-side compositing (hover
     * mission cards), cached by mtime. Same degradation contract as
     * `textureFile`.
     */
    public async textureRaster(file: string): Promise<DecodedImage | undefined> {
        const modified = this.fileModified(file);
        if (modified === undefined) {
            return undefined;
        }
        const cached = this.textureRasterCache.get(file);
        if (cached && cached.modified === modified) {
            return cached.image;
        }
        const bytes = await this.readBytes(file);
        if (!bytes) {
            return undefined;
        }
        try {
            const image = decodeAtlasImage(bytes);
            this.textureRasterCache.set(file, { modified, image });
            return image;
        } catch {
            return undefined;
        }
    }

    /** File mtime in ms, or `undefined` when the file is not readable. */
    public mtimeOf(file: string): number | undefined {
        return this.fileModified(file);
    }

    /**
     * Loads the preview font pair. `undefined` fields (or the whole result)
     * mean the corresponding font is unavailable and the renderer falls back
     * to its system-font path.
     */
    public async loadFonts(): Promise<FontAssets> {
        const modifiedKey = this.fontModifiedKey();
        if (this.fontCache && this.fontCache.modified === modifiedKey) {
            return this.fontCache.fonts;
        }
        const fonts: FontAssets = {};
        for (const { id, root } of this.fontSources()) {
            if (!root) {
                continue;
            }
            const decoded = await this.readFont(id, root);
            if (!decoded) {
                continue;
            }
            fonts[id] = {
                id,
                lineHeight: decoded.font.lineHeight,
                base: decoded.font.base,
                atlasUrl: pngDataUrl(decoded.atlas),
                chars: [...decoded.font.chars.entries()].map(([charId, char]) => [
                    charId,
                    char.x,
                    char.y,
                    char.width,
                    char.height,
                    char.xOffset,
                    char.yOffset,
                    char.xAdvance,
                ]),
                kernings: decoded.font.kerningPairs,
            };
        }
        this.fontCache = { modified: modifiedKey, fonts };
        return fonts;
    }

    /**
     * Loads the same font pair as decoded rasters for the hover-card
     * compositor. Same mtime-cache and degradation contract as
     * `loadFonts`: an absent entry just drops the card's title.
     */
    public async loadFontRasters(): Promise<FontBook> {
        const modifiedKey = this.fontModifiedKey();
        if (this.rasterCache && this.rasterCache.modified === modifiedKey) {
            return this.rasterCache.fonts;
        }
        const fonts: FontBook = {};
        for (const { id, root } of this.fontSources()) {
            if (!root) {
                continue;
            }
            const decoded = await this.readFont(id, root);
            if (!decoded) {
                continue;
            }
            fonts[id] = {
                id,
                lineHeight: decoded.font.lineHeight,
                base: decoded.font.base,
                chars: decoded.font.chars,
                kernings: new Map(
                    decoded.font.kerningPairs.map(([first, second, amount]) => [`${first}:${second}`, amount]),
                ),
                atlas: decoded.atlas,
            };
        }
        this.rasterCache = { modified: modifiedKey, fonts };
        return fonts;
    }

    private fontSources(): { id: 'english' | 'chinese'; root: string | undefined }[] {
        return [
            { id: 'english', root: this.gameDirectory },
            { id: 'chinese', root: this.chineseFontDirectory },
        ];
    }

    /** Cache key folding both fonts' .fnt mtimes ('missing'/'none' when absent). */
    private fontModifiedKey(): string {
        return this.fontSources().map(({ id, root }) => {
            const font = root ? path.join(root, 'gfx', 'fonts', id === 'english' ? 'vic_18.fnt' : 'zh-hans-16.fnt') : '';
            return `${id}:${root ? this.fileModified(font) ?? 'missing' : 'none'}`;
        }).join('|');
    }

    private async readFont(
        id: 'english' | 'chinese',
        root: string,
    ): Promise<{ font: BmFont; atlas: DecodedImage } | undefined> {
        const fontPath = path.join(root, 'gfx', 'fonts', id === 'english' ? 'vic_18' : 'zh-hans-16');
        const text = await this.readFile(fontPath + '.fnt');
        if (!text) {
            return undefined;
        }
        const font = parseBmFont(text);
        if (!font || font.chars.size === 0) {
            return undefined;
        }
        // EU4 font atlases sit next to the metrics with either extension;
        // prefer .dds (compressed, smaller on disk), then .tga.
        const atlasBytes = (await this.readBytes(fontPath + '.dds'))
            ?? (await this.readBytes(fontPath + '.tga'));
        if (!atlasBytes) {
            return undefined;
        }
        try {
            return { font, atlas: decodeAtlasImage(atlasBytes) };
        } catch {
            return undefined;
        }
    }

    private loadSpriteIndex(): Map<string, CataloguedSprite> {
        const gameFiles = this.gameDirectory
            ? this.readGfxFiles(path.join(this.gameDirectory, 'interface'))
            : [];
        const modFiles: string[] = [];
        for (const root of this.modRoots) {
            modFiles.push(...this.readGfxFiles(path.join(root, 'interface')));
        }
        const index = new Map<string, CataloguedSprite>();
        for (const [name, entry] of buildSpriteIndex(gameFiles)) {
            index.set(name, { ...entry, origin: 'vanilla' });
        }
        // Mod definitions replace vanilla ones for the same sprite name.
        for (const [name, entry] of buildSpriteIndex(modFiles)) {
            index.set(name, { ...entry, origin: 'mod' });
        }
        return index;
    }

    /** Reads the `.gfx` sources under one `interface` directory (bounded,
     * alphabetically ordered walk so first-wins merging stays deterministic). */
    private readGfxFiles(interfaceDirectory: string): string[] {
        const sources: string[] = [];
        const walk = (directory: string, depth: number): void => {
            if (depth > 3 || sources.length >= MAX_SPRITE_INDEX_FILES) {
                return;
            }
            let entries: fs.Dirent[];
            try {
                entries = fs.readdirSync(directory, { withFileTypes: true });
            } catch {
                return; // Missing or unreadable: nothing to index here.
            }
            entries.sort((a, b) => a.name.localeCompare(b.name));
            for (const entry of entries) {
                const full = path.join(directory, entry.name);
                if (entry.isDirectory()) {
                    walk(full, depth + 1);
                } else if (entry.isFile() && entry.name.toLowerCase().endsWith('.gfx')) {
                    try {
                        sources.push(fs.readFileSync(full, 'latin1'));
                    } catch {
                        // Unreadable file: skip it.
                    }
                }
            }
        };
        walk(interfaceDirectory, 0);
        return sources;
    }

    private async readBytes(file: string): Promise<Uint8Array | undefined> {
        try {
            const stats = fs.statSync(file);
            if (!stats.isFile() || stats.size > MAX_ASSET_BYTES) {
                return undefined;
            }
            return fs.promises.readFile(file);
        } catch {
            return undefined;
        }
    }

    private async readFile(file: string): Promise<string | undefined> {
        try {
            const stats = fs.statSync(file);
            if (!stats.isFile() || stats.size > MAX_ASSET_BYTES) {
                return undefined;
            }
            return await fs.promises.readFile(file, 'latin1');
        } catch {
            return undefined;
        }
    }

    private fileModified(file: string): number | undefined {
        try {
            return Math.trunc(fs.statSync(file).mtimeMs);
        } catch {
            return undefined;
        }
    }
}
