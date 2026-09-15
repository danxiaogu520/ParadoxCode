// Contract tests for media/loc-format.js (§ colour-code parsing). Node loads
// the UMD export directly; the webview loads the same file as a plain script
// before renderer.js, so one implementation serves both.

import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const api = await import('file://' + join(root, 'media', 'loc-format.js').replaceAll('\\', '/'));
const { COLORS: colors, parseLocFormat: parse } = api.default ?? api;

let failures = 0;
function fail(message) {
  failures += 1;
  console.error(`loc-format: ${message}`);
}

function segmentsOf(text) {
  return parse(text).segments;
}

if (Object.keys(colors).length !== 14) {
  fail('the § colour table must cover exactly the 14 game codes');
}
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
  if (colors[code] !== color) {
    fail(`§${code} must be the core.gfx ground truth ${color}, got ${colors[code]}`);
  }
}
for (const [code, color] of Object.entries(colors)) {
  if (!/^#[0-9a-f]{6}$/.test(color)) {
    fail(`colour for §${code} must be a #rrggbb literal`);
  }
}

// Plain text stays one inherit run.
let result = parse('Hello mission');
if (result.plain !== 'Hello mission' || result.segments.length !== 1 || result.segments[0].color !== null) {
  fail('plain text must parse to a single uncoloured run');
}

// A single code colours the rest of the string.
result = segmentsOf('§Rred text');
if (result.length !== 1 || result[0].color !== colors.R || result[0].text !== 'red text') {
  fail('§R must colour the remainder of the string');
}

// The wiki nesting example: §! restores the enclosing colour, then default.
result = segmentsOf('Regular text, §Rred, §Bblue§!back to red§!, and black');
const expected = [
  { text: 'Regular text, ', color: null },
  { text: 'red, ', color: colors.R },
  { text: 'blue', color: colors.B },
  { text: 'back to red', color: colors.R },
  { text: ', and black', color: null },
];
if (JSON.stringify(result) !== JSON.stringify(expected)) {
  fail(`nested §R/§B/§! must restore enclosing colours, got ${JSON.stringify(result)}`);
}

// Unclosed codes simply run to the end of the string.
result = segmentsOf('a §Yyellow to the end');
if (result.length !== 2 || result[1].color !== colors.Y) {
  fail('an unclosed code must colour the remainder');
}

// Extra §! is a no-op, and same-colour neighbours merge.
result = segmentsOf('a§!b');
if (result.length !== 1 || result[0].text !== 'ab' || result[0].color !== null) {
  fail('an extra §! must be a no-op');
}

// Unknown codes stay literal.
result = parse('§Xkept');
if (result.plain !== '§Xkept' || result.segments[0].color !== null) {
  fail('an unknown § code must be kept literally, uncoloured');
}
result = parse('§§');
if (result.plain !== '§§') {
  fail('a bare § must survive literally');
}

// Newlines and $placeholders$ are preserved verbatim.
result = parse('line one\n$VAL|$ §Gline two');
if (result.plain !== 'line one\n$VAL|$ line two') {
  fail('newlines and $…$ placeholders must pass through');
}
const newlineRun = result.segments.find((segment) => segment.text.includes('\n'));
if (!newlineRun) {
  fail('newline must stay inside a segment so the renderer can wrap on it');
}

if (failures > 0) {
  console.error(`loc-format FAILED (${failures})`);
  process.exitCode = 1;
} else {
  console.log('loc-format contract OK');
}
