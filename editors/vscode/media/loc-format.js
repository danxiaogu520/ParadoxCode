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
// Colour values: G/R/Y are vanilla ground truth (interface/core.gfx
// `textcolors`). The other 11 are engine-internal with no data file to
// read — approximations pending screenshot calibration. TODO(calibrate).

(function (global) {
    'use strict';

    const COLORS = {
        W: '#ffffff', // white
        B: '#3c6eb4', // blue — TODO(calibrate)
        G: '#2cab32', // green (core.gfx)
        R: '#b53d3d', // red (core.gfx)
        b: '#111111', // black — TODO(calibrate)
        g: '#9e9e9e', // grey — TODO(calibrate)
        Y: '#dbcb47', // yellow (core.gfx)
        M: '#275bb7', // marine — TODO(calibrate)
        T: '#2e8b8b', // teal — TODO(calibrate)
        O: '#e58a2e', // orange — TODO(calibrate)
        l: '#a0c040', // lime — TODO(calibrate)
        J: '#53a886', // jade — TODO(calibrate)
        P: '#7b3fa0', // purple — TODO(calibrate)
        V: '#c351cc', // violet — TODO(calibrate)
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
