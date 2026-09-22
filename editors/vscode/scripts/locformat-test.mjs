// Contract tests for media/loc-format.js (§ colour-code parsing). Node loads
// the UMD export directly; the webview loads the same file as a plain script
// before renderer.js, so one implementation serves both.

import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const api = await import('file://' + join(root, 'media', 'loc-format.js').replaceAll('\\', '/'));
const { COLORS: colors, parseLocFormat: parse } = api.default ?? api;

function segmentsOf(text) {
  return parse(text).segments;
}

assert.equal(Object.keys(colors).length, 14, 'the § colour table must cover exactly the 14 game codes');
// Game ground truth from vanilla interface/core.gfx: the global textcolors
// block plus the vic_18 bitmapfont's own G/R/Y overrides (mission titles
// render in that font; the Chinese font mods ship identical values).
const groundTruthColors = {
  W: '#ffffff',
  B: '#0000ff',
  G: '#2cab32',
  R: '#b53d3d',
  b: '#000000',
  g: '#b0b0b0',
  Y: '#dbcb47',
  M: '#23ceff',
  T: '#00ffef',
  O: '#ffa000',
  l: '#9ac14b',
  J: '#00a86b',
  P: '#702963',
  V: '#fab6ff',
};
for (const [code, color] of Object.entries(groundTruthColors)) {
  assert.equal(colors[code], color, `§${code} must be the core.gfx ground truth`);
}
for (const [code, color] of Object.entries(colors)) {
  assert.match(color, /^#[0-9a-f]{6}$/, `colour for §${code} must be a #rrggbb literal`);
}

// Plain text stays one inherit run.
let result = parse('Hello mission');
assert.ok(result.plain === 'Hello mission' && result.segments.length === 1 && result.segments[0].color === null,
  'plain text must parse to a single uncoloured run');

// A single code colours the rest of the string.
result = segmentsOf('§Rred text');
assert.ok(result.length === 1 && result[0].color === colors.R && result[0].text === 'red text',
  '§R must colour the remainder of the string');

// The wiki nesting example: §! restores the enclosing colour, then default.
result = segmentsOf('Regular text, §Rred, §Bblue§!back to red§!, and black');
const expected = [
  { text: 'Regular text, ', color: null },
  { text: 'red, ', color: colors.R },
  { text: 'blue', color: colors.B },
  { text: 'back to red', color: colors.R },
  { text: ', and black', color: null },
];
assert.deepEqual(result, expected, 'nested §R/§B/§! must restore enclosing colours');

// Unclosed codes simply run to the end of the string.
result = segmentsOf('a §Yyellow to the end');
assert.ok(result.length === 2 && result[1].color === colors.Y,
  'an unclosed code must colour the remainder');

// Extra §! is a no-op, and same-colour neighbours merge.
result = segmentsOf('a§!b');
assert.ok(result.length === 1 && result[0].text === 'ab' && result[0].color === null,
  'an extra §! must be a no-op');

// Unknown codes stay literal.
result = parse('§Xkept');
assert.ok(result.plain === '§Xkept' && result.segments[0].color === null,
  'an unknown § code must be kept literally, uncoloured');
result = parse('§§');
assert.equal(result.plain, '§§', 'a bare § must survive literally');

// Newlines and $placeholders$ are preserved verbatim.
result = parse('line one\n$VAL|$ §Gline two');
assert.equal(result.plain, 'line one\n$VAL|$ line two',
  'newlines and $…$ placeholders must pass through');
const newlineRun = result.segments.find((segment) => segment.text.includes('\n'));
assert.ok(newlineRun, 'newline must stay inside a segment so the renderer can wrap on it');

console.log('loc-format contract OK');
