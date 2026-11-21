// EU4 localisation formatting shared by the preview renderers.
//
// EU4 localised text supports 14 single-letter colour codes (`§W` white …
// `§V` violet); `§!` closes the innermost colour, and sections nest. The
// parser turns a raw value into styled runs so every consumer (canvas
// renderer, tooltip, search) sees the same colour model:
//
//   parseLocFormat('§Rred§Bblue§!red§!plain')
//     -> segments [{text:'red', color:R}, {text:'blue', color:B},
//                  {text:'red', color:R}, {text:'plain', color:null}]
//
(function (global) {
    'use strict';

    // Vanilla EU4 1.37.5, interface/core.gfx, bitmapfonts > textcolors.
    const GLOBAL_COLORS = {
        W: '#ffffff', // white — default text colour
        B: '#0000ff', // blue
        G: '#00ff00', // green
        R: '#ff3232', // red
        b: '#000000', // black
        g: '#b0b0b0', // grey — "not possible" text
        Y: '#ffbd00', // yellow
        M: '#23ceff', // marine — Shogunate / tutorials
        T: '#00ffef', // turquoise
        O: '#ffa000', // orange — branching missions
        l: '#9ac14b', // lime — multiplayer
        J: '#00a86b', // jade — mercs without professionalism cost
        P: '#702963', // purple — Byzantium
        V: '#fab6ff', // violet — debug text
    };
    // Titles (vic_18 / vic_29s) and options (vic_18) share the same overrides.
    const COLORS = { ...GLOBAL_COLORS, G: '#2cab32', R: '#b53d3d', Y: '#dbcb47' };
    const EVENT_DESC_COLORS = { ...GLOBAL_COLORS, G: '#18701a', R: '#7c1818', Y: '#000000' };

    // A flag run keeps its source text for when the texture is unavailable.
    // `plain` also keeps the tags so search and accessible labels can use them.
    function parseLocFormat(text, colors = COLORS) {
        text = text.replace(/\r\n?/g, '\n');
        const segments = [];
        let plain = '';
        let current = '';
        let activeColor = null;
        const stack = []; // colours suspended by a nested code
        const push = () => {
            if (current !== '') {
                const last = segments[segments.length - 1];
                if (last && !last.flag && last.color === activeColor) {
                    last.text += current;
                } else {
                    segments.push({ text: current, color: activeColor });
                }
                current = '';
            }
        };
        for (let i = 0; i < text.length; i += 1) {
            const ch = text[i];
            const next = text[i + 1];
            if (ch === '\\' && (next === 'n' || next === '\\')) {
                const escaped = next === 'n' ? '\n' : '\\';
                current += escaped;
                plain += escaped;
                i += 1;
            } else if (ch === '§' && next === '!') {
                push();
                activeColor = stack.pop() ?? null;
                i += 1;
            } else if (ch === '§' && Object.hasOwn(colors, next)) {
                push();
                stack.push(activeColor);
                activeColor = colors[next];
                i += 1;
            } else if (ch === '@') {
                const tag = text.slice(i + 1, i + 4);
                if (/^[A-Z0-9]{3}$/.test(tag) && !/[A-Za-z0-9_]/.test(text[i + 4] ?? '')) {
                    push();
                    const source = '@' + tag;
                    segments.push({ text: source, color: activeColor, flag: tag });
                    plain += source;
                    i += tag.length;
                } else {
                    current += ch;
                    plain += ch;
                }
            } else {
                current += ch;
                plain += ch;
            }
        }
        push();
        return { segments, plain };
    }

    const api = { COLORS, GLOBAL_COLORS, EVENT_DESC_COLORS, parseLocFormat };
    if (typeof module !== 'undefined' && module.exports) {
        module.exports = api;
    } else {
        global.LocFormat = api;
    }
})(typeof window !== 'undefined' ? window : globalThis);
