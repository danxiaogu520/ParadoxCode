// § localisation colour-code parser for the ParadoxCode preview webview.
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
// Colour values are game ground truth from vanilla `interface/core.gfx`:
// the global `textcolors` block for eleven of the codes, plus the `vic_18`
// bitmapfont's own G/R/Y overrides — mission titles render in `vic_18`
// (the Chinese mods remap that same font to zh-hans-16 and ship identical
// values, so one flat table serves both languages).

(function (global) {
    'use strict';

    const COLORS = {
        W: '#ffffff', // white — default text colour
        B: '#0000ff', // blue
        G: '#2cab32', // green — vic_18 override (44 171 50)
        R: '#b53d3d', // red — vic_18 override (181 61 61)
        b: '#000000', // black
        g: '#b0b0b0', // grey — "not possible" text
        Y: '#dbcb47', // yellow — vic_18 override (219 203 71)
        M: '#23ceff', // marine — Shogunate / tutorials
        T: '#00ffef', // turquoise
        O: '#ffa000', // orange — branching missions
        l: '#9ac14b', // lime — multiplayer
        J: '#00a86b', // jade — mercs without professionalism cost
        P: '#702963', // purple — Byzantium
        V: '#fab6ff', // violet — debug text
    };

    // Parses a localised value into colour runs. `color` is null while no
    // code is active — the consumer paints those runs with its own default.
    // Newlines and `$…$` placeholders are kept verbatim (the preview cannot
    // resolve variables, so showing them literally is the honest view); an
    // unknown `§x` pair stays in the text rather than being eaten.
    function parseLocFormat(text) {
        const segments = [];
        let plain = '';
        let current = '';
        let activeColor = null;
        const stack = []; // colours suspended by a nested code
        const push = () => {
            if (current !== '') {
                const last = segments[segments.length - 1];
                if (last && last.color === activeColor) {
                    last.text += current;
                } else {
                    segments.push({ text: current, color: activeColor });
                }
                current = '';
            }
        };
        for (let i = 0; i < text.length; i += 1) {
            const ch = text[i];
            if (ch !== '§' || i + 1 >= text.length) {
                current += ch;
                plain += ch;
                continue;
            }
            const code = text[i + 1];
            if (code === '!') {
                // Pop back to the enclosing colour; an extra §! is a no-op.
                push();
                activeColor = stack.pop() ?? null;
                i += 1;
                continue;
            }
            const color = COLORS[code];
            if (color) {
                push();
                stack.push(activeColor);
                activeColor = color;
                i += 1;
                continue;
            }
            // Unknown code: keep the pair literally.
            current += ch + code;
            plain += ch + code;
            i += 1;
        }
        push();
        return { segments, plain };
    }

    const api = { COLORS, parseLocFormat };
    if (typeof module !== 'undefined' && module.exports) {
        module.exports = api;
    } else {
        global.LocFormat = api;
    }
})(typeof window !== 'undefined' ? window : globalThis);
