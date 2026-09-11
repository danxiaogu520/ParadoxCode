// Contract test for the TypeScript codec twin (src/pdxCodec.ts) against the
// Rust implementation (crates/pdx-codec).
//
// Three layers of protection, in increasing strength:
//   1. Behavioural cases ported from the native suite (round trips, iron
//      rule ② refusals, orphan markers, CP1252 single bytes).
//   2. The EDG-KTP golden corpus, byte for byte in both directions — the
//      same ground truth paratranz certified for the Rust crate.
//   3. Differential vectors: `cargo run -p pdx-codec --bin pdx-codec-vectors`
//      emits an exhaustive per-code-point encode sweep plus deterministic
//      random byte/text sequences; every vector is replayed through the TS
//      implementation and compared exactly. Set PDX_SKIP_VECTORS=1 to skip
//      this layer when no Rust toolchain is available (CI never does).
import { spawn } from 'node:child_process';
import { createRequire } from 'node:module';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import assert from 'node:assert/strict';

const require = createRequire(import.meta.url);
const scriptDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(scriptDir, '..', '..', '..');
const corpusRoot = join(repoRoot, 'crates', 'pdx-codec', 'tests', 'corpus');

// The TS module compiles to out/pdxCodec.js; run after `npm run compile`.
const codec = require(join(scriptDir, '..', 'out', 'pdxCodec.js'));

const encoder = new TextEncoder();
const decoder = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true });

const PROFILE_LOCALISATION = codec.PROFILE_LOCALISATION;
const PROFILE_SCRIPT = codec.PROFILE_SCRIPT;
const PARATRANZ = codec.escapeSetMembers('paratranz');
const DLL_FULL = codec.escapeSetMembers('dll-full');

function assertBytesEqual(actual, expected, message) {
    // `assert/strict`'s deepEqual also compares prototypes, and a Buffer is not
    // a plain Uint8Array — compare the byte sequences themselves.
    assert.ok(Buffer.from(actual).equals(Buffer.from(expected)), message);
}

function hex(bytes) {
    return Buffer.from(bytes).toString('hex').toUpperCase();
}

function cpsToText(cps) {
    return cps.length === 0
        ? ''
        : String.fromCodePoint(...cps.filter((cp) => cp >= 0));
}

function textToCps(text) {
    return Array.from(text, (character) => character.codePointAt(0));
}

// --- module surface ----------------------------------------------------------
assert.equal(codec.CODEC_VERSION, 1, 'codec version must match the native crate');

const facade = new codec.PdxCodec();

// --- Localisation profile round trip (BOM + CRLF kept structural) -------------
const ymlText = '﻿l_english:\r\n edg_key:0 "发行本"\r\n other: "Straße Ära"\r\n';
const encoded = facade.encode(encoder.encode(ymlText), PROFILE_LOCALISATION);
assert.ok(!('unencodable' in encoded), 'yml encode must succeed');
assert.equal(facade.classify(encoded.bytes, PROFILE_LOCALISATION), 'escaped');
const decoded = facade.decode(encoded.bytes, PROFILE_LOCALISATION);
assert.notEqual(decoded, 'invalid-utf8');
assert.equal(decoder.decode(decoded.text), ymlText, 'yml round trip must be identical');
assert.equal(decoded.broken, 0);
assert.equal(facade.classify(encoder.encode(ymlText), PROFILE_LOCALISATION), 'readable');
assert.equal(
    facade.classify(encoder.encode('l_english:\n plain:0 "Yes"\n'), PROFILE_LOCALISATION),
    'ascii',
);
// Readable CJK plus orphan markers is Mixed — both transformations refused.
assert.equal(
    facade.classify(encoder.encode('可读\x10\x10'), PROFILE_LOCALISATION),
    'mixed',
);

// --- Script profile: raw single bytes, CP1252 letters stay single bytes --------
const scriptText = 'dynasty = "大明王朝"\r\n';
const scriptEncoded = facade.encode(encoder.encode(scriptText), PROFILE_SCRIPT);
assert.ok(!('unencodable' in scriptEncoded), 'script encode must succeed');
assert.notEqual(scriptEncoded.bytes[0], 0xef, 'script profile must not emit a BOM');
assert.equal(facade.classify(scriptEncoded.bytes, PROFILE_SCRIPT), 'escaped');
assert.equal(
    decoder.decode(facade.decode(scriptEncoded.bytes, PROFILE_SCRIPT).text),
    scriptText,
    'script round trip',
);
// ä (U+00E4) maps to the single CP1252 byte 0xE4 in the script profile.
const cp1252 = facade.encode(encoder.encode('ä'), PROFILE_SCRIPT);
assert.ok(!('unencodable' in cp1252));
assert.deepEqual([...cp1252.bytes], [0xe4], 'CP1252-mapped letters stay single bytes');

// --- Golden corpus: EDG-KTP release file ⇄ master file ------------------------
const master = readFileSync(join(corpusRoot, 'edg_ktp_master.yml'));
const release = readFileSync(join(corpusRoot, 'edg_ktp_release.yml'));
assert.equal(facade.classify(new Uint8Array(release), PROFILE_LOCALISATION), 'escaped');
const corpusDecoded = facade.decode(new Uint8Array(release), PROFILE_LOCALISATION);
assert.notEqual(corpusDecoded, 'invalid-utf8');
assertBytesEqual(
    corpusDecoded.text,
    master,
    'decode(release) must equal the committed master file byte for byte',
);
const corpusReencoded = facade.encode(new Uint8Array(master), PROFILE_LOCALISATION);
assert.ok(!('unencodable' in corpusReencoded));
assertBytesEqual(
    corpusReencoded.bytes,
    release,
    'encode(master) must equal the committed release file byte for byte',
);

// --- Iron rule ② gates --------------------------------------------------------
const refused = facade.encode(encoded.bytes, PROFILE_LOCALISATION);
assert.ok('unencodable' in refused, 'encoding escaped text must be refused');
assert.ok(refused.unencodable.length > 0, 'refusal must list the offending code points');

const boundary = facade.encode(encoder.encode('Š x 😀'), PROFILE_LOCALISATION);
assert.ok('unencodable' in boundary, 'boundary code points must be refused');
assert.deepEqual(
    boundary.unencodable.map((point) => point.codePoint),
    [0x0160, 0x1f600],
    'refusal must list U+0160 (mangled low plane) and U+1F600 (beyond BMP) in order',
);
assert.deepEqual(
    boundary.unencodable.map((point) => point.kind),
    [0, 1],
    'refusal kinds must be 0 (low plane) and 1 (beyond BMP)',
);

// A marker in the final two positions is an orphan: passed through and
// counted for diagnostics instead of being decoded.
const orphanInput = encoder.encode('a\x10\x10');
const orphanDecoded = facade.decode(orphanInput, PROFILE_LOCALISATION);
assert.notEqual(orphanDecoded, 'invalid-utf8');
assert.equal(orphanDecoded.broken, 2, 'orphan markers must be counted');
assertBytesEqual(orphanDecoded.text, orphanInput, 'orphan markers pass through as-is');

// Damaged localisation file: invalid UTF-8 is an error, never a decode.
assert.equal(facade.decode(new Uint8Array([0xff, 0xfe]), PROFILE_LOCALISATION), 'invalid-utf8');
assert.equal(
    facade.classify(new Uint8Array([0xff, 0xfe]), PROFILE_LOCALISATION),
    'mixed',
    'invalid UTF-8 classifies as mixed so no transformation is offered',
);

// --- Differential vectors against the Rust implementation ---------------------
if (process.env.PDX_SKIP_VECTORS !== '1') {
    // Windows does not resolve PATH executables without an extension, so the
    // shell form is used there; the fixed argument string is concatenation-safe.
    const windows = process.platform === 'win32';
    const command = windows
        ? ['cargo run -q --locked -p pdx-codec --bin pdx-codec-vectors', []]
        : ['cargo', ['run', '-q', '--locked', '-p', 'pdx-codec', '--bin', 'pdx-codec-vectors']];
    const child = spawn(command[0], command[1], {
        cwd: repoRoot,
        stdio: ['ignore', 'pipe', 'inherit'],
        shell: windows,
    });
    const lines = child.stdout;
    let buffer = '';
    let compared = 0;
    const failures = [];
    const record = (message) => {
        if (failures.length < 5) {
            failures.push(message);
        }
    };

    const compareVector = (vector) => {
        if (vector.v === 'cp') {
            const text = String.fromCodePoint(vector.cp);
            const lanes = [
                ['loc', codec.encodeText(text, PARATRANZ), (result) =>
                    'text' in result ? hex(encoder.encode(result.text)) : `R${result.unencodable[0].kind}`],
                ['scr', codec.encodeFile(text, PROFILE_SCRIPT, PARATRANZ), (result) =>
                    'bytes' in result ? hex(result.bytes) : `R${result.unencodable[0].kind}`],
                ['dll', codec.encodeFile(text, PROFILE_SCRIPT, DLL_FULL), (result) =>
                    'bytes' in result ? hex(result.bytes) : `R${result.unencodable[0].kind}`],
            ];
            for (const [lane, result, render] of lanes) {
                if (vector[lane] === 'SURROGATE') {
                    continue;
                }
                const actual = render(result);
                if (actual !== vector[lane]) {
                    record(`cp U+${vector.cp.toString(16).toUpperCase()}: ${lane} ${actual} != ${vector[lane]}`);
                    return;
                }
            }
            compared += 1;
            return;
        }
        if (vector.v === 'seq') {
            const bytes = new Uint8Array(Buffer.from(vector.b, 'hex'));
            const script = codec.decodeFile(bytes, PROFILE_SCRIPT);
            assert.ok(typeof script === 'object');
            if (hexTextCps(script.text) !== vector.st || brokenList(script.broken) !== vector.sb) {
                record(`seq ${vector.b}: script decode mismatch`);
                return;
            }
            if (codec.classifyFile(bytes, PROFILE_SCRIPT) !== vector.sc) {
                record(`seq ${vector.b}: script classification mismatch`);
                return;
            }
            if (vector.ok === 1) {
                const localisation = codec.decodeFile(bytes, PROFILE_LOCALISATION);
                if (
                    typeof localisation !== 'object' ||
                    hexTextCps(localisation.text) !== vector.lt ||
                    brokenList(localisation.broken) !== vector.lb
                ) {
                    record(`seq ${vector.b}: localisation decode mismatch`);
                    return;
                }
                if (codec.classifyFile(bytes, PROFILE_LOCALISATION) !== vector.lc) {
                    record(`seq ${vector.b}: localisation classification mismatch`);
                    return;
                }
            } else {
                assert.equal(
                    codec.decodeFile(bytes, PROFILE_LOCALISATION),
                    'invalid-utf8',
                    `seq ${vector.b} must be invalid UTF-8`,
                );
                assert.equal(
                    codec.classifyFile(bytes, PROFILE_LOCALISATION),
                    'mixed',
                    `seq ${vector.b} must classify as mixed (invalid UTF-8)`,
                );
            }
            compared += 1;
            return;
        }
        // text layer
        const text = cpsToText(parseCps(vector.t));
        const encodedText = codec.encodeText(text, PARATRANZ);
        if (vector.err !== undefined) {
            if (!('unencodable' in encodedText)) {
                record(`text ${vector.t}: expected refusal`);
                return;
            }
            const actual = encodedText.unencodable
                .map((point) => `${point.kind}@${point.byteIndex}`)
                .join(',');
            if (actual !== vector.err) {
                record(`text ${vector.t}: refusal ${actual} != ${vector.err}`);
                return;
            }
        } else {
            if (!('text' in encodedText)) {
                record(`text ${vector.t}: unexpected refusal`);
                return;
            }
            if (hexTextCps(encodedText.text) !== vector.e) {
                record(`text ${vector.t}: encode mismatch`);
                return;
            }
            const decodedText = codec.decodeText(encodedText.text);
            if (hexTextCps(decodedText.text) !== vector.d || brokenList(decodedText.broken) !== vector.db) {
                record(`text ${vector.t}: decode mismatch`);
                return;
            }
        }
        if (codec.classifyText(text) !== vector.c) {
            record(`text ${vector.t}: classification mismatch`);
            return;
        }
        compared += 1;
    };

    const brokenList = (broken) => `[${broken.join(',')}]`;
    const hexTextCps = (text) => textToCps(text).map((cp) => cp.toString(16).toUpperCase().padStart(4, '0')).join(' ');
    const parseCps = (field) => (field === '' ? [] : field.split(' ').map((part) => Number.parseInt(part, 16)));

    await new Promise((resolve, reject) => {
        lines.setEncoding('utf8');
        lines.on('data', (chunk) => {
            buffer += chunk;
            let newline = buffer.indexOf('\n');
            while (newline !== -1) {
                const line = buffer.slice(0, newline).trim();
                buffer = buffer.slice(newline + 1);
                if (line.length > 0) {
                    try {
                        compareVector(JSON.parse(line));
                    } catch (error) {
                        reject(error);
                        child.kill();
                        return;
                    }
                }
                newline = buffer.indexOf('\n');
            }
        });
        lines.on('end', resolve);
        lines.on('error', reject);
        child.on('exit', (code) => {
            if (code !== 0) {
                reject(new Error(`pdx-codec-vectors exited with ${code}`));
            }
        });
    });

    assert.equal(failures.length, 0, `differential vectors diverged:\n${failures.join('\n')}`);
    assert.ok(compared > 60000, `expected a full vector sweep, compared only ${compared}`);
    console.log(`codec ts contract OK (${compared} differential vectors)`);
} else {
    console.log('codec ts contract OK (differential vectors skipped)');
}
