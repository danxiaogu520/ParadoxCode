// Mission- and event-card compositors for hover previews, the Node-side
// twin of the webview's mission tree renderer. The server's `pdc/hoverCard`
// response names *what* to draw (resolved texture paths, title text,
// flags); this module turns that into pixels: the game's 103x123 mission
// frame with the icon underneath and a §-coloured bitmap title, or the
// game's 564-wide event window with stacked chrome, picture banner, and
// option buttons. Everything here is plain Node (no vscode, no canvas) so
// the contract tests can exercise the exact blending math.

import { createHash } from 'crypto';
import { mkdirSync, readdirSync, rmSync, statSync, writeFileSync } from 'fs';
import { tmpdir } from 'os';
import { join } from 'path';
import { pathToFileURL } from 'url';

import { BmChar, DecodedImage, FontBook, FontRaster } from './gameAssets';

interface LocFormatApi {
    parseLocFormat(text: string): { segments: { text: string; color: string | null }[]; plain: string };
}

// The §-colour parser ships as a UMD the webview loads via <script>; from
// the compiled out/ directory the same file is one plain require away.
const locFormat = require('../media/loc-format') as LocFormatApi;

// --- wire types -------------------------------------------------------------

/** One resolved texture the server hands to the client (no pixels). */
export interface HoverCardAssetWire {
    sprite?: string;
    /** Absolute path on this machine; the client only reads and decodes. */
    path: string;
    rootKind: 'currentMod' | 'dependency' | 'vanilla';
    extensionFallback: boolean;
    frames?: number;
}

export interface HoverCardMissionWire {
    id: string;
    icon: string;
    titleKey: string;
    title?: { language?: string; value: string } | null;
    required: string[];
}

export interface HoverCardEventOptionWire {
    nameKey: string;
    name?: { language?: string; value: string } | null;
}

export interface HoverCardEventWire {
    id: string;
    picture?: string | null;
    titleKey: string;
    title?: { language?: string; value: string } | null;
    descKey?: string | null;
    desc?: { language?: string; value: string } | null;
    options: HoverCardEventOptionWire[];
}

/** Chrome assets shared by both card kinds; each kind reads its own subset. */
export interface HoverCardAssetsWire {
    frame?: HoverCardAssetWire;
    backgroundTop?: HoverCardAssetWire;
    backgroundMiddle?: HoverCardAssetWire;
    backgroundBottomS?: HoverCardAssetWire;
    backgroundBottomM?: HoverCardAssetWire;
    backgroundBottomL?: HoverCardAssetWire;
    optionButton?: HoverCardAssetWire;
}

export interface HoverCardWire {
    kind: 'mission' | 'event' | 'sprite' | 'texture';
    asset?: HoverCardAssetWire;
    mission?: HoverCardMissionWire;
    event?: HoverCardEventWire;
    cardAssets?: HoverCardAssetsWire;
}

export interface HoverCardResponseWire {
    version: number;
    card: HoverCardWire;
}

// --- card layout (vanilla mission frame, verified in game files) -------------

const CARD_WIDTH = 103;
const CARD_HEIGHT = 123;
const ICON_X = 22;
const ICON_Y = 20;
const TITLE_X = 8;
const TITLE_Y = 84;
const TITLE_WIDTH = 96;
const MAX_TITLE_LINES = 2;
/** Display width for the composed card in hover markdown (2x render size). */
export const MISSION_CARD_DISPLAY_WIDTH = CARD_WIDTH * 2;

// --- event window layout (vanilla interface/eventwindow.gui) --------------------

// The window is 564 wide; the stacked chrome pieces sit at x=-43 and are
// 656 wide, so their transparent margins hang off both canvas edges.
const EVENT_WIDTH = 564;
const EVENT_BG_X = -43;
const EVENT_BG_TOP_Y = 0;
const EVENT_PICTURE_X = 30;
const EVENT_PICTURE_Y = 81;
const EVENT_TITLE_Y = 39;
const EVENT_DESC_X = 31;
const EVENT_DESC_Y = 230;
const EVENT_DESC_WIDTH = 512;
const EVENT_OPTION_X = 8;
const EVENT_OPTION_WIDTH = 547;
const EVENT_OPTION_HEIGHT = 31;
const EVENT_OPTION_STRIDE = 32;
const EVENT_OPTIONS_GAP = 14;
// Game text colours: title (vic_29s) and option buttons (vic_18) render white
// with a dark shadow; the description (vic_18_black) renders dark on the
// parchment. § colours override all three as authored.
const EVENT_TITLE_COLOR = '#ffffff';
const EVENT_DESC_COLOR = '#000000';
const EVENT_OPTION_COLOR = '#ffffff';
const TEXT_SHADOW_OFFSET = 1;
const TEXT_SHADOW_COLOR = '#000000';
/** Display width for the composed event card in hover markdown. */
export const EVENT_CARD_DISPLAY_WIDTH = 500;

// --- RGBA blending ------------------------------------------------------------

type Rgb = { r: number; g: number; b: number };

function hexToRgb(hex: string): Rgb | null {
    const match = /^#([0-9a-f]{6})$/i.exec(hex.trim());
    if (!match) {
        return null;
    }
    const value = Number.parseInt(match[1], 16);
    return { r: (value >> 16) & 0xff, g: (value >> 8) & 0xff, b: value & 0xff };
}

function blankImage(width: number, height: number): DecodedImage {
    return { width, height, pixels: new Uint8Array(width * height * 4) };
}

/**
 * Alpha-composites a sub-rectangle of `src` onto `dst` at (dstX, dstY),
 * optionally multiplying the source RGB by `tint` (alpha preserved — the §
 * colour model). Regions outside either image are clipped.
 */
export function blitTinted(
    dst: DecodedImage,
    dstX: number,
    dstY: number,
    src: DecodedImage,
    rect: { x: number; y: number; width: number; height: number },
    tint: Rgb | null,
): void {
    const x0 = Math.max(0, dstX);
    const y0 = Math.max(0, dstY);
    const x1 = Math.min(dst.width, dstX + rect.width);
    const y1 = Math.min(dst.height, dstY + rect.height);
    for (let y = y0; y < y1; y++) {
        const sy = rect.y + (y - dstY);
        if (sy < 0 || sy >= src.height) {
            continue;
        }
        let di = (y * dst.width + x0) * 4;
        let si = (sy * src.width + rect.x + (x0 - dstX)) * 4;
        for (let x = x0; x < x1; x++, di += 4, si += 4) {
            const sx = rect.x + (x - dstX);
            if (sx < 0 || sx >= src.width) {
                continue;
            }
            const sa = src.pixels[si + 3];
            if (sa === 0) {
                continue;
            }
            const da = dst.pixels[di + 3];
            const outA = sa + (da * (255 - sa)) / 255;
            if (outA <= 0) {
                continue;
            }
            const mix = (sc: number, dc: number): number =>
                (sc * sa + (dc * da * (255 - sa)) / 255) / outA;
            dst.pixels[di] = Math.round(mix(tint ? (src.pixels[si] * tint.r) / 255 : src.pixels[si], dst.pixels[di]));
            dst.pixels[di + 1] = Math.round(mix(tint ? (src.pixels[si + 1] * tint.g) / 255 : src.pixels[si + 1], dst.pixels[di + 1]));
            dst.pixels[di + 2] = Math.round(mix(tint ? (src.pixels[si + 2] * tint.b) / 255 : src.pixels[si + 2], dst.pixels[di + 2]));
            dst.pixels[di + 3] = Math.round(outA);
        }
    }
}

function blit(dst: DecodedImage, dstX: number, dstY: number, src: DecodedImage): void {
    blitTinted(dst, dstX, dstY, src, { x: 0, y: 0, width: src.width, height: src.height }, null);
}

/**
 * Sub-rectangle of frame `frame` in a horizontally stacked strip. `frames`
 * comes from the sprite's `noOfFrames`; anything invalid means one frame.
 */
export function frameRect(
    image: DecodedImage,
    frames: number | undefined,
    frame: number,
): { x: number; y: number; width: number; height: number } {
    const count = frames !== undefined && frames >= 1 ? Math.floor(frames) : 1;
    const width = Math.floor(image.width / count);
    const index = Math.min(Math.max(frame, 0), count - 1);
    return { x: width * index, y: 0, width, height: image.height };
}

// --- title typography (ported from media/renderer.js, zoom fixed at 1) ---------

// CJK detection drives both font choice and per-character wrapping:
// Chinese replace-file localisation carries l_english headers, so the
// language tag alone cannot be trusted — the content decides.
const CJK_RE = /[\u2e80-\u9fff\uf900-\ufaff\ufe30-\ufe4f\uff00-\uffef\u3000-\u303f]/;

interface StyledToken {
    text: string;
    color: string | null;
}

interface GlyphHit {
    font: FontRaster;
    glyph: BmChar;
}

function fontReady(font: FontRaster | undefined): font is FontRaster {
    return !!font && font.chars.size > 0 && font.atlas.width > 0 && font.atlas.height > 0;
}

function titleFont(title: { language?: string; value: string } | null | undefined, fonts: FontBook): FontRaster | null {
    const chinese = !!title && (title.language === 'simp_chinese' || CJK_RE.test(title.value));
    const primary = fonts[chinese ? 'chinese' : 'english'];
    if (fontReady(primary)) {
        return primary;
    }
    const secondary = fonts[chinese ? 'english' : 'chinese'];
    return fontReady(secondary) ? secondary : null;
}

// Glyph resolution order for one codepoint: the primary font, then the
// other font (mixed-script titles), else null for the fallback advance.
function resolveGlyph(font: FontRaster, codePoint: number, fonts: FontBook): GlyphHit | null {
    const glyph = font.chars.get(codePoint);
    if (glyph) {
        return { font, glyph };
    }
    const other = fonts[font.id === 'english' ? 'chinese' : 'english'];
    const alt = other && other.chars.get(codePoint);
    return alt && fontReady(other) ? { font: other, glyph: alt } : null;
}

function fontKerning(font: FontRaster, first: number, second: number): number {
    return first ? font.kernings.get(`${first}:${second}`) ?? 0 : 0;
}

// Advance for a codepoint missing from both fonts: Node has no system font
// to measure with, so the space glyph's advance (4px in vic_18) stands in.
function fallbackAdvance(font: FontRaster): number {
    return font.chars.get(32)?.xAdvance ?? 4;
}

function charAdvance(font: FontRaster, codePoint: number, prev: number, fonts: FontBook): number {
    const hit = resolveGlyph(font, codePoint, fonts);
    if (hit) {
        return hit.glyph.xAdvance + (hit.font === font ? fontKerning(font, prev, codePoint) : 0);
    }
    return fallbackAdvance(font);
}

function measureStyled(font: FontRaster, text: string, fonts: FontBook): number {
    let width = 0;
    let prev = 0;
    for (const ch of text) {
        const codePoint = ch.codePointAt(0) ?? 0;
        width += charAdvance(font, codePoint, prev, fonts);
        prev = codePoint;
    }
    return width;
}

// Splits styled runs into wrap tokens: CJK characters break individually,
// Latin words stay whole and wrap at word boundaries — whitespace opens a new
// token (leading the word it precedes, stripped when that word starts a line).
function tokenizeStyled(line: StyledToken[]): StyledToken[] {
    const tokens: StyledToken[] = [];
    let word: StyledToken | null = null;
    for (const run of line) {
        for (const ch of run.text) {
            if (CJK_RE.test(ch)) {
                if (word) {
                    tokens.push(word);
                    word = null;
                }
                tokens.push({ text: ch, color: run.color });
            } else if (/\s/.test(ch)) {
                if (word && word.color === run.color && /^\s+$/.test(word.text)) {
                    word.text += ch;
                } else {
                    if (word) {
                        tokens.push(word);
                    }
                    word = { text: ch, color: run.color };
                }
            } else if (word && word.color === run.color) {
                word.text += ch;
            } else {
                if (word) {
                    tokens.push(word);
                }
                word = { text: ch, color: run.color };
            }
        }
    }
    if (word) {
        tokens.push(word);
    }
    return tokens;
}

// Greedy wrap of one styled line at maxWidth; a token moved to a new line
// never carries leading spaces with it.
function wrapStyledLine(line: StyledToken[], maxWidth: number, font: FontRaster, fonts: FontBook): StyledToken[][] {
    const lines: StyledToken[][] = [];
    let current: StyledToken[] = [];
    let width = 0;
    for (let token of tokenizeStyled(line)) {
        if (current.length > 0 && width + measureStyled(font, token.text, fonts) > maxWidth) {
            lines.push(current);
            current = [];
            width = 0;
            token = { text: token.text.replace(/^\s+/, ''), color: token.color };
        } else if (current.length === 0) {
            token = { text: token.text.replace(/^\s+/, ''), color: token.color };
        }
        if (!token.text) {
            continue;
        }
        current.push(token);
        width += measureStyled(font, token.text, fonts);
    }
    if (current.length > 0 || lines.length === 0) {
        lines.push(current);
    }
    return lines;
}

// Splits parsed segments into explicit lines at newline boundaries.
function segmentLines(segments: { text: string; color: string | null }[]): StyledToken[][] {
    const lines: StyledToken[][] = [[]];
    for (const segment of segments) {
        const parts = segment.text.split('\n');
        parts.forEach((part, index) => {
            if (index > 0) {
                lines.push([]);
            }
            if (part) {
                lines[lines.length - 1].push({ text: part, color: segment.color });
            }
        });
    }
    return lines;
}

/** Full title pipeline: parse § colour codes, honor newlines, wrap. */
export function styledTitleLines(
    label: string,
    font: FontRaster,
    fonts: FontBook,
    maxWidth: number = TITLE_WIDTH,
): StyledToken[][] {
    const result: StyledToken[][] = [];
    for (const line of segmentLines(locFormat.parseLocFormat(label).segments)) {
        result.push(...wrapStyledLine(line, maxWidth, font, fonts));
    }
    return result;
}

function lineWidth(tokens: StyledToken[], font: FontRaster, fonts: FontBook): number {
    return tokens.reduce((sum, token) => sum + measureStyled(font, token.text, fonts), 0);
}

// Draws one wrapped line left-to-right: glyphs blit from the game font,
// tinted per § colour and baseline-aligned when the other font owns the
// glyph. Missing glyphs keep their advance but draw nothing.
function drawStyledLine(
    dst: DecodedImage,
    tokens: StyledToken[],
    x: number,
    lineTop: number,
    font: FontRaster,
    fonts: FontBook,
    defaultColor: string,
    forceColor?: string,
): void {
    let pen = x;
    let prev = 0;
    for (const token of tokens) {
        const tint = hexToRgb(forceColor ?? token.color ?? defaultColor);
        for (const ch of token.text) {
            const codePoint = ch.codePointAt(0) ?? 0;
            const hit = resolveGlyph(font, codePoint, fonts);
            if (hit) {
                const kerning = hit.font === font ? fontKerning(font, prev, codePoint) : 0;
                const baseline = lineTop + font.base;
                blitTinted(
                    dst,
                    Math.round(pen + hit.glyph.xOffset + kerning),
                    Math.round(baseline - hit.font.base + hit.glyph.yOffset),
                    hit.font.atlas,
                    { x: hit.glyph.x, y: hit.glyph.y, width: hit.glyph.width, height: hit.glyph.height },
                    tint,
                );
                pen += hit.glyph.xAdvance + kerning;
            } else {
                pen += fallbackAdvance(font);
            }
            prev = codePoint;
        }
    }
}

// Light-on-dark game text (white titles, option labels) carries a one-pixel
// dark shadow, so the glyph is drawn twice: solid black offset, then colour.
function drawShadowedStyledLine(
    dst: DecodedImage,
    tokens: StyledToken[],
    x: number,
    lineTop: number,
    font: FontRaster,
    fonts: FontBook,
    defaultColor: string,
): void {
    drawStyledLine(
        dst,
        tokens,
        x + TEXT_SHADOW_OFFSET,
        lineTop + TEXT_SHADOW_OFFSET,
        font,
        fonts,
        defaultColor,
        TEXT_SHADOW_COLOR,
    );
    drawStyledLine(dst, tokens, x, lineTop, font, fonts, defaultColor);
}

// --- mission card -------------------------------------------------------------

export interface MissionCardParts {
    /** `GFX_mission_icons_frame` texture (103x123). */
    frame?: DecodedImage;
    /** The mission's icon strip; frame 0 is drawn under the frame. */
    icon?: DecodedImage;
    iconFrames?: number;
    fonts?: FontBook;
}

/**
 * Composes the game-look mission card: icon underneath the frame texture
 * and the §-coloured title centred in the frame's lower slot (two
 * line-heights, top-aligned). Without a usable font the card degrades to
 * frame + icon.
 */
export function composeMissionCard(mission: HoverCardMissionWire, parts: MissionCardParts): DecodedImage {
    const card = blankImage(CARD_WIDTH, CARD_HEIGHT);
    if (parts.icon) {
        blitTinted(card, ICON_X, ICON_Y, parts.icon, frameRect(parts.icon, parts.iconFrames, 0), null);
    }
    if (parts.frame) {
        blit(card, 0, 0, parts.frame);
    }
    drawMissionTitle(card, mission, parts.fonts ?? {});
    return card;
}

function drawMissionTitle(card: DecodedImage, mission: HoverCardMissionWire, fonts: FontBook): void {
    const font = titleFont(mission.title, fonts);
    if (!font) {
        return;
    }
    const label = mission.title?.value || mission.id;
    const wrapped = styledTitleLines(label, font, fonts);
    // The title plaque fits two lines; text beyond that folds away instead of
    // spilling past the frame, with "..." marking the cut on the last line.
    const lines = wrapped.length > MAX_TITLE_LINES
        ? [...wrapped.slice(0, MAX_TITLE_LINES - 1), foldWithEllipsis(wrapped[MAX_TITLE_LINES - 1], font, fonts)]
        : wrapped;
    lines.forEach((tokens, index) => {
        const centered = TITLE_X + Math.max(0, (TITLE_WIDTH - lineWidth(tokens, font, fonts)) / 2);
        drawStyledLine(card, tokens, centered, TITLE_Y + index * font.lineHeight, font, fonts, '#ffffff');
    });
}

/** Truncates one wrapped line and appends "..." within the title width. */
function foldWithEllipsis(
    tokens: StyledToken[],
    font: FontRaster,
    fonts: FontBook,
): StyledToken[] {
    const dots = '...';
    const dotsWidth = measureStyled(font, dots, fonts);
    const kept = [...tokens];
    while (kept.length > 1 && lineWidth(kept, font, fonts) + dotsWidth > TITLE_WIDTH) {
        kept.pop();
    }
    const last = kept[kept.length - 1];
    if (last) {
        kept[kept.length - 1] = { text: `${last.text}${dots}`, color: last.color };
    } else {
        kept.push({ text: dots, color: null });
    }
    return kept;
}

/** Markdown image for a composed card, sized for hover display. */
export function missionCardMarkdown(dataUrl: string): string {
    return `![mission card](${toHoverImageUrl(dataUrl)}|width=${MISSION_CARD_DISPLAY_WIDTH})`;
}

// --- event card -------------------------------------------------------------------

export interface EventCardParts {
    backgroundTop?: DecodedImage;
    backgroundMiddle?: DecodedImage;
    backgroundBottomS?: DecodedImage;
    backgroundBottomM?: DecodedImage;
    backgroundBottomL?: DecodedImage;
    optionButton?: DecodedImage;
    /** The event's `picture` texture (a 512x132 banner in vanilla). */
    picture?: DecodedImage;
    fonts?: FontBook;
}

/** Picks the event's primary font from its title, else its description. */
function eventFont(event: HoverCardEventWire, fonts: FontBook): FontRaster | null {
    const probe = event.title ?? (event.desc ? { language: undefined, value: event.desc.value } : null);
    return titleFont(probe, fonts);
}

/**
 * Composes the game-look event window: stacked background chrome with the
 * picture banner at the top, centred white title, wrapped black description,
 * one button row per option, and the bottom piece sized for the option count
 * (S ≤2, M ≤4, L beyond; first available piece stands in when the preferred
 * one is missing). Unlike the engine — which clips the description at a
 * 128px box — the whole description wraps and the window grows (the middle
 * chrome tiles) to fit any length. § colours apply as authored; without a
 * usable font the card degrades to chrome + picture only.
 */
export function composeEventCard(event: HoverCardEventWire, parts: EventCardParts): DecodedImage {
    const fonts = parts.fonts ?? {};
    const font = eventFont(event, fonts);
    const lineHeight = font ? font.lineHeight : 0;
    const descLines = font && event.desc
        ? styledTitleLines(event.desc.value, font, fonts, EVENT_DESC_WIDTH)
        : [];
    const descBottom = EVENT_DESC_Y + descLines.length * lineHeight;
    const optionsStart = Math.max(descBottom, EVENT_DESC_Y) + EVENT_OPTIONS_GAP;
    const optionsBottom = optionsStart + event.options.length * EVENT_OPTION_STRIDE;
    const bottoms = [parts.backgroundBottomS, parts.backgroundBottomM, parts.backgroundBottomL];
    const preferred = event.options.length <= 2 ? 0 : event.options.length <= 4 ? 1 : 2;
    const bottom = bottoms[preferred] ?? bottoms.find((candidate) => candidate !== undefined);
    const height = Math.max(
        EVENT_BG_TOP_Y + (parts.backgroundTop?.height ?? 0),
        parts.picture ? EVENT_PICTURE_Y + parts.picture.height : 0,
        optionsBottom + (bottom?.height ?? 8),
    );
    const card = blankImage(EVENT_WIDTH, height);
    if (parts.backgroundTop) {
        blit(card, EVENT_BG_X, EVENT_BG_TOP_Y, parts.backgroundTop);
    }
    if (parts.picture) {
        blit(card, EVENT_PICTURE_X, EVENT_PICTURE_Y, parts.picture);
    }
    // The middle piece tiles vertically from below the top piece until the
    // options end, exactly where the engine stacks it behind the content.
    if (parts.backgroundMiddle) {
        for (let y = EVENT_BG_TOP_Y + (parts.backgroundTop?.height ?? 0); y < optionsBottom; y += parts.backgroundMiddle.height) {
            blit(card, EVENT_BG_X, y, parts.backgroundMiddle);
        }
    }
    if (bottom) {
        blit(card, EVENT_BG_X, optionsBottom, bottom);
    }
    if (font) {
        const title = styledTitleLines(event.title?.value || event.id, font, fonts, EVENT_WIDTH)[0] ?? [];
        drawShadowedStyledLine(
            card,
            title,
            (EVENT_WIDTH - lineWidth(title, font, fonts)) / 2,
            EVENT_TITLE_Y,
            font,
            fonts,
            EVENT_TITLE_COLOR,
        );
        descLines.forEach((tokens, index) => {
            drawStyledLine(card, tokens, EVENT_DESC_X, EVENT_DESC_Y + index * lineHeight, font, fonts, EVENT_DESC_COLOR);
        });
    }
    event.options.forEach((option, index) => {
        const y = optionsStart + index * EVENT_OPTION_STRIDE;
        if (parts.optionButton) {
            blit(card, EVENT_OPTION_X, y, parts.optionButton);
        }
        if (!font) {
            return;
        }
        const label = option.name?.value || option.nameKey;
        const [tokens] = styledTitleLines(label, font, fonts, EVENT_OPTION_WIDTH - 16);
        if (!tokens) {
            return;
        }
        drawShadowedStyledLine(
            card,
            tokens,
            EVENT_OPTION_X + (EVENT_OPTION_WIDTH - lineWidth(tokens, font, fonts)) / 2,
            y + Math.max(0, Math.floor((EVENT_OPTION_HEIGHT - lineHeight) / 2)),
            font,
            fonts,
            EVENT_OPTION_COLOR,
        );
    });
    return card;
}

/** Markdown image for a composed event card, sized for hover display. */
export function eventCardMarkdown(dataUrl: string): string {
    return `![event card](${toHoverImageUrl(dataUrl)}|width=${EVENT_CARD_DISPLAY_WIDTH})`;
}

// --- hover image URLs ------------------------------------------------------------
// VS Code's markdown renderer hard-truncates hover strings at 100k characters
// (src/vs/base/browser/markdownRenderer.ts: "values that are too long will
// freeze the UI"). An inline `data:` URL for a composed card is 400k-700k
// characters, so the truncation beheads it mid-base64, the closing paren is
// gone, the image markdown no longer parses, and the hover shows the markdown
// source as plain text. Oversized images are therefore written to a temp-file
// cache and referenced via `file:///` URIs (an allowed hover image protocol);
// small images stay inline as data URLs.

const MAX_INLINE_IMAGE_CHARS = 90_000;
const IMAGE_FILE_CACHE_LIMIT = 64;
const IMAGE_FILE_MAX_AGE_MS = 7 * 24 * 60 * 60 * 1000;

const imageFileCache = new Map<string, string>();
let imageCachePruned = false;

function imageCacheDirectory(): string {
    return join(tmpdir(), 'paradoxcode-hover-images');
}

/** Drops stale cache files left by previous sessions (best effort, once). */
function pruneImageCache(directory: string): void {
    if (imageCachePruned) {
        return;
    }
    imageCachePruned = true;
    try {
        const deadline = Date.now() - IMAGE_FILE_MAX_AGE_MS;
        for (const entry of readdirSync(directory)) {
            const file = join(directory, entry);
            try {
                if (statSync(file).mtimeMs < deadline) {
                    rmSync(file, { force: true });
                }
            } catch {
                // racing deletion or unreadable entry: ignore
            }
        }
    } catch {
        // no directory yet or unreadable: nothing to prune
    }
}

/**
 * Keeps small images inline and spills oversized ones into the temp-file
 * cache as `file:///` URIs so the 100k markdown truncation cannot behead
 * them. Any filesystem failure degrades to the inline URL (truncation shows
 * markdown source, but the hover itself still works).
 */
export function toHoverImageUrl(dataUrl: string): string {
    if (dataUrl.length <= MAX_INLINE_IMAGE_CHARS) {
        return dataUrl;
    }
    const comma = dataUrl.indexOf(',');
    if (!dataUrl.startsWith('data:') || comma < 0) {
        return dataUrl;
    }
    const key = createHash('sha256').update(dataUrl).digest('hex');
    const hit = imageFileCache.get(key);
    if (hit !== undefined) {
        imageFileCache.delete(key);
        imageFileCache.set(key, hit);
        return pathToFileURL(hit).toString();
    }
    const directory = imageCacheDirectory();
    const file = join(directory, `${key}.png`);
    try {
        mkdirSync(directory, { recursive: true });
        writeFileSync(file, Buffer.from(dataUrl.slice(comma + 1), 'base64'));
        pruneImageCache(directory);
    } catch {
        return dataUrl;
    }
    imageFileCache.set(key, file);
    if (imageFileCache.size > IMAGE_FILE_CACHE_LIMIT) {
        const oldest = imageFileCache.keys().next().value;
        if (oldest !== undefined) {
            const evicted = imageFileCache.get(oldest);
            imageFileCache.delete(oldest);
            if (evicted !== undefined) {
                try {
                    rmSync(evicted, { force: true });
                } catch {
                    // best effort: the OS eventually cleans its temp dir
                }
            }
        }
    }
    return pathToFileURL(file).toString();
}

// --- composed-card cache --------------------------------------------------------

const CARD_CACHE_LIMIT = 32;
const cardCache = new Map<string, string>();

/**
 * LRU for composed-card data URLs (compositing and PNG encoding are the
 * expensive step of a hover). `key` must fold in whatever can change the
 * output: mission id, title value, and the mtimes of every input asset.
 */
export function cachedCardDataUrl(key: string, produce: () => string): string {
    const hit = cardCache.get(key);
    if (hit !== undefined) {
        cardCache.delete(key);
        cardCache.set(key, hit);
        return hit;
    }
    const value = produce();
    cardCache.set(key, value);
    if (cardCache.size > CARD_CACHE_LIMIT) {
        const oldest = cardCache.keys().next().value;
        if (oldest !== undefined) {
            cardCache.delete(oldest);
        }
    }
    return value;
}

/** Test/inspection hook: drops every cached card. */
export function clearCardCache(): void {
    cardCache.clear();
}

// --- response parsing ------------------------------------------------------------

function parseAsset(value: unknown): HoverCardAssetWire | undefined {
    if (typeof value !== 'object' || value === null) {
        return undefined;
    }
    const record = value as Record<string, unknown>;
    if (typeof record.path !== 'string' || record.path === '') {
        return undefined;
    }
    const rootKind = record.rootKind;
    const frames = record.frames;
    return {
        sprite: typeof record.sprite === 'string' ? record.sprite : undefined,
        path: record.path,
        rootKind: rootKind === 'currentMod' || rootKind === 'dependency' || rootKind === 'vanilla' ? rootKind : 'vanilla',
        extensionFallback: record.extensionFallback === true,
        frames: typeof frames === 'number' && frames >= 1 ? Math.floor(frames) : undefined,
    };
}

/** `{ language, value }` localisation text, `value` mandatory. */
function parseLocText(value: unknown): { language?: string; value: string } | undefined {
    if (typeof value !== 'object' || value === null) {
        return undefined;
    }
    const record = value as Record<string, unknown>;
    if (typeof record.value !== 'string') {
        return undefined;
    }
    return {
        language: typeof record.language === 'string' ? record.language : undefined,
        value: record.value,
    };
}

/**
 * Validates a `pdc/hoverCard` response. Anything malformed (or a future
 * version) yields `undefined` so the middleware falls back to the legacy
 * hover path instead of rendering a half-card.
 */
export function parseHoverCardResponse(value: unknown): HoverCardResponseWire | undefined {
    if (typeof value !== 'object' || value === null) {
        return undefined;
    }
    const { version, card } = value as Record<string, unknown>;
    if (version !== 1 || typeof card !== 'object' || card === null) {
        return undefined;
    }
    const cardRecord = card as Record<string, unknown>;
    const kind = cardRecord.kind;
    if (kind !== 'mission' && kind !== 'event' && kind !== 'sprite' && kind !== 'texture') {
        return undefined;
    }
    let mission: HoverCardMissionWire | undefined;
    const missionValue = cardRecord.mission;
    if (typeof missionValue === 'object' && missionValue !== null) {
        const record = missionValue as Record<string, unknown>;
        if (typeof record.id !== 'string') {
            return undefined;
        }
        mission = {
            id: record.id,
            icon: typeof record.icon === 'string' ? record.icon : '',
            titleKey: typeof record.titleKey === 'string' ? record.titleKey : '',
            title: parseLocText(record.title),
            required: Array.isArray(record.required)
                ? record.required.filter((item): item is string => typeof item === 'string')
                : [],
        };
    }
    if (kind === 'mission' && !mission) {
        return undefined;
    }
    let event: HoverCardEventWire | undefined;
    const eventValue = cardRecord.event;
    if (typeof eventValue === 'object' && eventValue !== null) {
        const record = eventValue as Record<string, unknown>;
        if (typeof record.id !== 'string') {
            return undefined;
        }
        const options: HoverCardEventOptionWire[] = [];
        if (Array.isArray(record.options)) {
            for (const item of record.options) {
                if (typeof item !== 'object' || item === null) {
                    continue;
                }
                const option = item as Record<string, unknown>;
                if (typeof option.nameKey !== 'string') {
                    continue;
                }
                options.push({ nameKey: option.nameKey, name: parseLocText(option.name) });
            }
        }
        event = {
            id: record.id,
            picture: typeof record.picture === 'string' ? record.picture : undefined,
            titleKey: typeof record.titleKey === 'string' ? record.titleKey : '',
            title: parseLocText(record.title),
            descKey: typeof record.descKey === 'string' ? record.descKey : undefined,
            desc: parseLocText(record.desc),
            options,
        };
    }
    if (kind === 'event' && !event) {
        return undefined;
    }
    const asset = parseAsset(cardRecord.asset);
    // Mission and event cards compose from their own chrome; the single
    // asset (icon/picture) is optional for them but mandatory for the
    // single-texture kinds.
    if ((kind === 'sprite' || kind === 'texture') && !asset) {
        return undefined;
    }
    const cardAssetsValue = cardRecord.cardAssets;
    let cardAssets: HoverCardAssetsWire = {};
    if (typeof cardAssetsValue === 'object' && cardAssetsValue !== null) {
        const record = cardAssetsValue as Record<string, unknown>;
        cardAssets = {
            frame: parseAsset(record.frame),
            backgroundTop: parseAsset(record.backgroundTop),
            backgroundMiddle: parseAsset(record.backgroundMiddle),
            backgroundBottomS: parseAsset(record.backgroundBottomS),
            backgroundBottomM: parseAsset(record.backgroundBottomM),
            backgroundBottomL: parseAsset(record.backgroundBottomL),
            optionButton: parseAsset(record.optionButton),
        };
    }
    return { version: 1, card: { kind, asset, mission, event, cardAssets } };
}
