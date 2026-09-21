// TypeScript twin of crates/transcode (the EU4dll escape transcoder).
//
// The algorithm is fixed and shared with the Rust crate by *equivalence*, not
// by shared source: scripts/transcode.mjs pins this implementation to the
// Rust one with (a) the golden corpus byte for byte and (b) an
// exhaustive code-point sweep plus deterministic random-sequence vectors
// emitted by `cargo run -p transcode --bin transcode-vectors`. Any change to
// either implementation turns that test red.
//
// Porting notes (the traps that made this non-trivial):
// * JS strings are UTF-16 code units — every scan below walks code points via
//   codePointAt, never charAt.
// * The Rust core computes byte offsets with UTF-8 semantics; scanCodePoints
//   tracks the same offsets so unencodable positions match the server's.
// * decode can reconstruct surrogate-range values that String.fromCodePoint
//   rejects; safeFromCodePoint substitutes U+FFFD exactly like the Rust
//   char::from_u32(...).unwrap_or path.

export const TRANSCODE_VERSION = 1;

export const PROFILE_LOCALISATION = 0 as const;
export const PROFILE_SCRIPT = 1 as const;

export type LocalisationProfile = typeof PROFILE_LOCALISATION | typeof PROFILE_SCRIPT;

export type Classification = 'ascii' | 'readable' | 'escaped' | 'mixed';

export interface UnencodablePoint {
    byteIndex: number;
    codePoint: number;
    kind: number; // 0 = mangled low plane (U+0100..U+0FFF), 1 = beyond BMP
}

export type EncodeResult = { bytes: Uint8Array } | { unencodable: UnencodablePoint[] };

export type DecodeResult = { text: Uint8Array; broken: number } | 'invalid-utf8';

export const PARATRANZ_ESCAPE_BYTES: readonly number[] = [
    0x00, 0x0a, 0x0d, 0x20, 0x22, 0x23, 0x24, 0x2f, 0x3b, 0x3d, 0x40, 0x5b, 0x5c, 0x5d, 0x5f,
    0x7b, 0x7d, 0x7e, 0x80, 0xa3, 0xa4, 0xa7, 0xbd,
];

export const DLL_FULL_ESCAPE_BYTES: readonly number[] = [
    ...PARATRANZ_ESCAPE_BYTES,
    0x3a, 0x3c, 0x3e, 0x3f, 0x7c, 0x2a,
];

const PARATRANZ_SET = new Set(PARATRANZ_ESCAPE_BYTES);
const DLL_FULL_SET = new Set(DLL_FULL_ESCAPE_BYTES);

export function escapeSetMembers(set: 'paratranz' | 'dll-full'): ReadonlySet<number> {
    return set === 'paratranz' ? PARATRANZ_SET : DLL_FULL_SET;
}

/** The 27 CP1252 bytes above ASCII mapped outside Latin-1, byte-sorted. */
export const CP1252_MAP: readonly (readonly [number, number])[] = [
    [0x80, 0x20ac],
    [0x82, 0x201a],
    [0x83, 0x0192],
    [0x84, 0x201e],
    [0x85, 0x2026],
    [0x86, 0x2020],
    [0x87, 0x2021],
    [0x88, 0x02c6],
    [0x89, 0x2030],
    [0x8a, 0x0160],
    [0x8b, 0x2039],
    [0x8c, 0x0152],
    [0x8e, 0x017d],
    [0x91, 0x2018],
    [0x92, 0x2019],
    [0x93, 0x201c],
    [0x94, 0x201d],
    [0x95, 0x2022],
    [0x96, 0x2013],
    [0x97, 0x2014],
    [0x98, 0x02dc],
    [0x99, 0x2122],
    [0x9a, 0x0161],
    [0x9b, 0x203a],
    [0x9c, 0x0153],
    [0x9e, 0x017e],
    [0x9f, 0x0178],
];

const BYTE_TO_CP1252 = new Map<number, number>(CP1252_MAP.map(([byte, cp]) => [byte, cp] as const));
const CP1252_TO_BYTE = new Map<number, number>(CP1252_MAP.map(([byte, cp]) => [cp, byte] as const));

export function byteToChar(byte: number): number {
    return BYTE_TO_CP1252.get(byte) ?? byte;
}

export function charToByte(codePoint: number): number {
    const mapped = CP1252_TO_BYTE.get(codePoint);
    if (mapped !== undefined) {
        return mapped;
    }
    return codePoint < 0x100 ? codePoint : codePoint & 0xff;
}

export function mappedByte(codePoint: number): number | undefined {
    return CP1252_TO_BYTE.get(codePoint);
}

interface CodePointAt {
    cp: number;
    byteIndex: number;
}

function utf8ByteLength(codePoint: number): number {
    return codePoint < 0x80 ? 1 : codePoint < 0x800 ? 2 : codePoint < 0x10000 ? 3 : 4;
}

function scanCodePoints(text: string): CodePointAt[] {
    const positions: CodePointAt[] = [];
    let byteIndex = 0;
    for (let index = 0; index < text.length; ) {
        const cp = text.codePointAt(index) as number;
        positions.push({ cp, byteIndex });
        index += cp > 0xffff ? 2 : 1;
        byteIndex += utf8ByteLength(cp);
    }
    return positions;
}

function safeFromCodePoint(codePoint: number): string {
    const surrogate = codePoint >= 0xd800 && codePoint <= 0xdfff;
    if (surrogate || codePoint > 0x10ffff) {
        return '\uFFFD';
    }
    return String.fromCodePoint(codePoint);
}

function isMarker(codePoint: number): boolean {
    return codePoint >= 0x10 && codePoint <= 0x13;
}

function isPassthrough(codePoint: number): boolean {
    return codePoint < 0x100 || codePoint === 0xfeff;
}

/** `undefined` when encodable; otherwise 0 (mangled low plane) or 1 (beyond BMP). */
export function unencodableKind(codePoint: number): number | undefined {
    if (codePoint < 0x100 || codePoint === 0xfeff) {
        return undefined;
    }
    if (codePoint <= 0xfff) {
        return 0;
    }
    if (codePoint >= 0x10000) {
        return 1;
    }
    return undefined;
}

/** The profile-aware form: script keeps the 27 CP1252 letters as single bytes. */
export function fileUnencodableKind(
    codePoint: number,
    profile: LocalisationProfile,
): number | undefined {
    if (profile === PROFILE_SCRIPT && CP1252_TO_BYTE.has(codePoint)) {
        return undefined;
    }
    return unencodableKind(codePoint);
}

function escapeTriple(
    codePoint: number,
    set: ReadonlySet<number>,
): [number, number, number] {
    const low = codePoint & 0xff;
    const high = codePoint >>> 8;
    let marker = 0x10;
    if (set.has(high)) {
        marker += 2;
    }
    if (set.has(low)) {
        marker += 1;
    }
    const compensatedLow = marker === 0x11 || marker === 0x13 ? (low + 0x0e) & 0xff : low;
    const compensatedHigh = marker === 0x12 || marker === 0x13 ? (high - 0x09) & 0xff : high;
    return [marker, compensatedLow, compensatedHigh];
}

function reconstruct(marker: number, low: number, high: number): number {
    let sp = ((high << 8) | low) >>> 0;
    if (marker === 0x11) {
        sp = (sp - 0x0e) >>> 0;
    } else if (marker === 0x12) {
        sp = (sp + 0x900) >>> 0;
    } else if (marker === 0x13) {
        sp = (sp + 0x8f2) >>> 0;
    }
    return sp;
}

// --- text layer -------------------------------------------------------------

export type TextEncodeResult = { text: string } | { unencodable: UnencodablePoint[] };

export function encodeText(input: string, set: ReadonlySet<number>): TextEncodeResult {
    let output = '';
    const unencodable: UnencodablePoint[] = [];
    for (const { cp, byteIndex } of scanCodePoints(input)) {
        const kind = unencodableKind(cp);
        if (kind !== undefined) {
            unencodable.push({ byteIndex, codePoint: cp, kind });
            continue;
        }
        if (isPassthrough(cp)) {
            output += safeFromCodePoint(cp);
            continue;
        }
        const [marker, low, high] = escapeTriple(cp, set);
        output += safeFromCodePoint(marker) + safeFromCodePoint(byteToChar(low)) + safeFromCodePoint(byteToChar(high));
    }
    return unencodable.length > 0 ? { unencodable } : { text: output };
}

export function decodeText(input: string): { text: string; broken: number[] } {
    const positions = scanCodePoints(input);
    let text = '';
    const broken: number[] = [];
    let index = 0;
    while (index < positions.length) {
        const { cp, byteIndex } = positions[index];
        if (isMarker(cp)) {
            if (positions.length - index >= 3) {
                const low = charToByte(positions[index + 1].cp);
                const high = charToByte(positions[index + 2].cp);
                text += safeFromCodePoint(reconstruct(cp, low, high));
                index += 3;
                continue;
            }
            broken.push(byteIndex);
        }
        text += safeFromCodePoint(cp);
        index += 1;
    }
    return { text, broken };
}

export function isRawCjk(codePoint: number): boolean {
    return (
        (codePoint >= 0x2e80 && codePoint <= 0x9fff) ||
        (codePoint >= 0xac00 && codePoint <= 0xd7af) ||
        (codePoint >= 0xf900 && codePoint <= 0xfaff) ||
        (codePoint >= 0xff00 && codePoint <= 0xffef) ||
        (codePoint >= 0x20000 && codePoint <= 0x2fa1f) ||
        (codePoint >= 0x30000 && codePoint <= 0x3134f)
    );
}

export interface ClassificationCounts {
    escapedTriples: number;
    brokenMarkers: number;
    rawCjk: number;
}

function countCodePoints(codePoints: number[]): ClassificationCounts {
    let escapedTriples = 0;
    let brokenMarkers = 0;
    let rawCjk = 0;
    let index = 0;
    while (index < codePoints.length) {
        const cp = codePoints[index];
        if (isMarker(cp)) {
            if (codePoints.length - index >= 3) {
                escapedTriples += 1;
                index += 3;
                continue;
            }
            brokenMarkers += 1;
        } else if (isRawCjk(cp)) {
            rawCjk += 1;
        }
        index += 1;
    }
    return { escapedTriples, brokenMarkers, rawCjk };
}

function classifyCounts(counts: ClassificationCounts): Classification {
    if (counts.escapedTriples >= 3 && counts.brokenMarkers === 0 && counts.rawCjk === 0) {
        return 'escaped';
    }
    if (counts.rawCjk > 0 && counts.escapedTriples === 0 && counts.brokenMarkers === 0) {
        return 'readable';
    }
    if (counts.escapedTriples === 0 && counts.brokenMarkers === 0 && counts.rawCjk === 0) {
        return 'ascii';
    }
    return 'mixed';
}

export function classifyText(text: string): Classification {
    return classifyCounts(countCodePoints(scanCodePoints(text).map((position) => position.cp)));
}

// --- file layer -------------------------------------------------------------

const utf8Encoder = new TextEncoder();
// ignoreBOM keeps the structural U+FEFF inside the decoded string, matching the
// transcoder's passthrough semantics (VS Code itself treats the BOM separately).
const utf8Decoder = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true });

export function encodeFile(
    input: string,
    profile: LocalisationProfile,
    set: ReadonlySet<number>,
): EncodeResult {
    if (profile === PROFILE_LOCALISATION) {
        const encoded = encodeText(input, set);
        return 'text' in encoded ? { bytes: utf8Encoder.encode(encoded.text) } : encoded;
    }
    const bytes: number[] = [];
    const unencodable: UnencodablePoint[] = [];
    for (const { cp, byteIndex } of scanCodePoints(input)) {
        // The 27 CP1252 characters stay single bytes before any rejection
        // check: eight of them (Š, œ, …) sit inside U+0100..=U+0FFF and
        // would otherwise be refused, yet the single-byte form round-trips
        // exactly — required by the real script corpus.
        if (cp < 0x100) {
            bytes.push(cp);
        } else if (cp === 0xfeff) {
            bytes.push(0xef, 0xbb, 0xbf);
        } else if (CP1252_TO_BYTE.has(cp)) {
            bytes.push(CP1252_TO_BYTE.get(cp) as number);
        } else if (unencodableKind(cp) !== undefined) {
            unencodable.push({ byteIndex, codePoint: cp, kind: unencodableKind(cp) as number });
        } else {
            const [marker, low, high] = escapeTriple(cp, set);
            bytes.push(marker, low, high);
        }
    }
    return unencodable.length > 0
        ? { unencodable }
        : { bytes: Uint8Array.from(bytes) };
}

export type FileDecodeResult =
    | { text: string; broken: number[] }
    | 'invalid-utf8';

export function decodeFile(bytes: Uint8Array, profile: LocalisationProfile): FileDecodeResult {
    if (profile === PROFILE_LOCALISATION) {
        let input: string;
        try {
            input = utf8Decoder.decode(bytes);
        } catch {
            return 'invalid-utf8';
        }
        return decodeText(input);
    }
    let text = '';
    const broken: number[] = [];
    let index = 0;
    while (index < bytes.length) {
        const byte = bytes[index];
        if (isMarker(byte)) {
            if (bytes.length - index >= 3) {
                text += safeFromCodePoint(reconstruct(byte, bytes[index + 1], bytes[index + 2]));
                index += 3;
                continue;
            }
            broken.push(index);
        }
        text += safeFromCodePoint(byteToChar(byte));
        index += 1;
    }
    return { text, broken };
}

export function classifyFile(bytes: Uint8Array, profile: LocalisationProfile): Classification {
    if (profile === PROFILE_LOCALISATION) {
        const text = tryUtf8(bytes);
        return text === undefined ? 'mixed' : classifyText(text);
    }
    const asUtf8 = tryUtf8(bytes);
    if (asUtf8 !== undefined) {
        return classifyText(asUtf8);
    }
    return classifyCounts(countCodePoints(Array.from(bytes, byteToChar)));
}

function tryUtf8(bytes: Uint8Array): string | undefined {
    try {
        return utf8Decoder.decode(bytes);
    } catch {
        return undefined;
    }
}

// --- scoped layer (quote/comment-aware transcoding) ---------------------------
//
// Mirrors crates/transcode/src/scoped.rs: escape triples live only inside
// quoted strings; comments and code stay readable UTF-8 on disk. The scanner
// runs on raw bytes — structural bytes are escape-set members, so payloads
// never contain them and spans are stable across encode/decode.

export type ScopedForm = 'plain' | 'whole' | 'scoped' | 'damaged';

/** Maximal quoted spans outside comments: byte ranges including the quotes. */
export function scanStringSpans(bytes: Uint8Array): Array<[number, number]> {
    const spans: Array<[number, number]> = [];
    let inString = false;
    let start = 0;
    let index = 0;
    while (index < bytes.length) {
        const byte = bytes[index];
        if (inString) {
            if (byte === 0x5c && index + 1 < bytes.length) {
                index += 2;
                continue;
            }
            if (byte === 0x22) {
                spans.push([start, index + 1]);
                inString = false;
            } else if (byte === 0x0a) {
                spans.push([start, index]);
                inString = false;
            }
        } else if (byte === 0x23) {
            while (index < bytes.length && bytes[index] !== 0x0a) {
                index += 1;
            }
            continue;
        } else if (byte === 0x22) {
            inString = true;
            start = index;
        }
        index += 1;
    }
    if (inString) {
        spans.push([start, bytes.length]);
    }
    return spans;
}

export function scopedForm(bytes: Uint8Array, profile: LocalisationProfile): ScopedForm {
    const classification = classifyFile(bytes, profile);
    if (classification === 'ascii' || classification === 'readable') {
        return 'plain';
    }
    if (classification === 'escaped') {
        return readableCjkGap(bytes) ? 'scoped' : 'whole';
    }
    if (readableCjkGap(bytes)) {
        return 'scoped';
    }
    // Localisation input that is not valid UTF-8 can never decode; report it
    // as damaged instead of a form the decoder refuses.
    if (profile === PROFILE_LOCALISATION && tryUtf8(bytes) === undefined) {
        return 'damaged';
    }
    return markerOutsideSpans(bytes).length === 0 ? 'scoped' : 'damaged';
}

/** Whether any between-spans run is unescaped, valid UTF-8 with raw CJK — the
 * fingerprint of readable scoped comments, which a legacy escaped file's
 * CP1252 byte layer can never produce. */
function readableCjkGap(bytes: Uint8Array): boolean {
    const spans = scanStringSpans(bytes);
    let cursor = 0;
    for (const [start, end] of spans) {
        if (gapIsReadableCjk(bytes.subarray(cursor, start))) {
            return true;
        }
        cursor = end;
    }
    return gapIsReadableCjk(bytes.subarray(cursor));
}

function gapIsReadableCjk(gap: Uint8Array): boolean {
    if (gap.length === 0) {
        return false;
    }
    for (const byte of gap) {
        if (isMarker(byte)) {
            return false;
        }
    }
    const text = tryUtf8(gap);
    if (text === undefined) {
        return false;
    }
    for (let index = 0; index < text.length; ) {
        const cp = text.codePointAt(index) as number;
        if (isRawCjk(cp)) {
            return true;
        }
        index += cp > 0xffff ? 2 : 1;
    }
    return false;
}

/** Input byte offsets of escape markers that sit outside every quoted span. */
export function markerOutsideSpans(bytes: Uint8Array): number[] {
    const spans = scanStringSpans(bytes);
    const markers: number[] = [];
    let cursor = 0;
    const scanGap = (gap: Uint8Array, base: number): void => {
        for (let offset = 0; offset < gap.length; offset += 1) {
            if (isMarker(gap[offset])) {
                markers.push(base + offset);
            }
        }
    };
    for (const [start, end] of spans) {
        scanGap(bytes.subarray(cursor, start), cursor);
        cursor = end;
    }
    scanGap(bytes.subarray(cursor), cursor);
    return markers;
}

export type ScopedFileDecodeResult =
    | { text: string; inSpanBroken: number[]; outOfSpanMarkers: number[] }
    | 'invalid-utf8';

export function scopedDecodeFile(
    bytes: Uint8Array,
    profile: LocalisationProfile,
): ScopedFileDecodeResult {
    if (profile === PROFILE_LOCALISATION && tryUtf8(bytes) === undefined) {
        return 'invalid-utf8';
    }
    const spans = scanStringSpans(bytes);
    let text = '';
    const inSpanBroken: number[] = [];
    const outOfSpanMarkers: number[] = [];
    const appendGap = (gap: Uint8Array, base: number): void => {
        if (gap.length === 0) {
            return;
        }
        for (let offset = 0; offset < gap.length; offset += 1) {
            if (isMarker(gap[offset])) {
                outOfSpanMarkers.push(base + offset);
            }
        }
        text += decodeRunSegment(gap, profile, base).text;
    };
    let cursor = 0;
    for (const [start, end] of spans) {
        appendGap(bytes.subarray(cursor, start), cursor);
        const run = decodeRunSegment(bytes.subarray(start, end), profile, start);
        inSpanBroken.push(...run.broken);
        text += run.text;
        cursor = end;
    }
    appendGap(bytes.subarray(cursor), cursor);
    return { text, inSpanBroken, outOfSpanMarkers };
}

/** Decodes one run (span or gap): with markers it takes the profile's triple
 * decode (orphan markers come back as whole-input offsets); without markers it
 * is readable content — verbatim UTF-8 when valid, CP1252-mapped otherwise. */
function decodeRunSegment(
    segment: Uint8Array,
    profile: LocalisationProfile,
    base: number,
): { text: string; broken: number[] } {
    let hasMarker = false;
    for (const byte of segment) {
        if (isMarker(byte)) {
            hasMarker = true;
            break;
        }
    }
    if (hasMarker) {
        const decoded = decodeFile(segment, profile);
        if (decoded === 'invalid-utf8') {
            // Localisation runs were validated as UTF-8 above and script decode
            // never fails — mirrors the Rust .expect().
            throw new Error('scoped segment decode cannot fail');
        }
        return { text: decoded.text, broken: decoded.broken.map((offset) => offset + base) };
    }
    const run = tryUtf8(segment);
    if (run !== undefined) {
        return { text: run, broken: [] };
    }
    let text = '';
    for (const byte of segment) {
        text += safeFromCodePoint(byteToChar(byte));
    }
    return { text, broken: [] };
}

export type ScopedEncodeResult =
    | { bytes: Uint8Array }
    | { alreadyEscaped: number[] }
    | { unencodable: UnencodablePoint[] };

/** Encodes readable text into scoped-escaped bytes: triples only inside quoted
 * spans, verbatim UTF-8 outside them. Iron rule ②: refuses when any span
 * already carries a marker; refused in-span code points come back with offsets
 * rebased to the whole input. */
export function scopedEncodeFile(
    input: string,
    profile: LocalisationProfile,
    set: ReadonlySet<number>,
): ScopedEncodeResult {
    const bytes = utf8Encoder.encode(input);
    const spans = scanStringSpans(bytes);
    const positions: number[] = [];
    for (const [start, end] of spans) {
        for (let offset = start; offset < end; offset += 1) {
            if (isMarker(bytes[offset])) {
                positions.push(offset);
            }
        }
    }
    if (positions.length > 0) {
        return { alreadyEscaped: positions };
    }
    const output: number[] = [];
    const appendRange = (source: Uint8Array, from: number, to: number): void => {
        for (let index = from; index < to; index += 1) {
            output.push(source[index]);
        }
    };
    let cursor = 0;
    for (const [start, end] of spans) {
        appendRange(bytes, cursor, start);
        const spanText = tryUtf8(bytes.subarray(start, end));
        if (spanText === undefined) {
            // Span boundaries fall on ASCII structural bytes, so every span of
            // a UTF-8-encoded string decodes — mirrors the Rust byte slice.
            throw new Error('scoped encode span is not valid UTF-8');
        }
        const encoded = encodeFile(spanText, profile, set);
        if ('unencodable' in encoded) {
            return {
                unencodable: encoded.unencodable.map((point) => ({
                    ...point,
                    byteIndex: point.byteIndex + start,
                })),
            };
        }
        appendRange(encoded.bytes, 0, encoded.bytes.length);
        cursor = end;
    }
    appendRange(bytes, cursor, bytes.length);
    return { bytes: Uint8Array.from(output) };
}

// --- facade used by the pdcloc:// provider -----------------------------------

/**
 * Byte-oriented facade over the file layer, mirroring the surface the provider
 * needs: classify disk/readable buffers, decode escaped bytes to readable text
 * (UTF-8 encoded for the editor), and encode readable text (UTF-8 bytes in) to
 * on-disk escaped bytes.
 */
export class Transcoder {
    classify(bytes: Uint8Array, profile: LocalisationProfile): Classification {
        return classifyFile(bytes, profile);
    }

    decode(bytes: Uint8Array, profile: LocalisationProfile): DecodeResult {
        const decoded = decodeFile(bytes, profile);
        if (decoded === 'invalid-utf8') {
            return decoded;
        }
        return { text: utf8Encoder.encode(decoded.text), broken: decoded.broken.length };
    }

    encode(bytes: Uint8Array, profile: LocalisationProfile): EncodeResult {
        let input: string;
        try {
            input = utf8Decoder.decode(bytes);
        } catch {
            throw new Error('transcode encode input is not valid UTF-8');
        }
        return encodeFile(input, profile, PARATRANZ_SET);
    }

    /** Scoped counterparts for the pdcloc:// provider's quote-aware path. */
    scopedClassify(bytes: Uint8Array, profile: LocalisationProfile): ScopedForm {
        return scopedForm(bytes, profile);
    }

    scopedDecode(
        bytes: Uint8Array,
        profile: LocalisationProfile,
    ): { text: Uint8Array; inSpanBroken: number[]; outOfSpanMarkers: number[] } | 'invalid-utf8' {
        const decoded = scopedDecodeFile(bytes, profile);
        if (decoded === 'invalid-utf8') {
            return decoded;
        }
        return { ...decoded, text: utf8Encoder.encode(decoded.text) };
    }

    scopedEncode(bytes: Uint8Array, profile: LocalisationProfile): ScopedEncodeResult {
        let input: string;
        try {
            input = utf8Decoder.decode(bytes);
        } catch {
            throw new Error('transcode scoped encode input is not valid UTF-8');
        }
        return scopedEncodeFile(input, profile, PARATRANZ_SET);
    }
}
