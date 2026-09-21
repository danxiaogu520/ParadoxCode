// Mission-icon insertion helpers shared by the icon-picker panel and its
// contract tests (pure functions, no vscode import so plain Node can require
// the compiled module).
//
// The picker writes a chosen sprite name into the editor the user came from:
// when the cursor sits inside an `icon = <value>` value the value is replaced
// in place, otherwise the name is inserted at the cursor.

/** UTF-16 span (line-relative) of one `icon` assignment value. */
export interface IconValueSpan {
    line: number;
    start: number;
    end: number;
}

/**
 * Matches `icon = <value>` on one script line when `character` sits inside
 * the (quoted or bare) value. Trailing comments after a bare value are not
 * part of the span; an empty value (cursor right after `=`) yields
 * `undefined` so the caller inserts at the cursor instead.
 */
export function iconValueSpanAt(line: string, character: number, lineIndex: number): IconValueSpan | undefined {
    const prefix = line.match(/^\s*icon\s*=\s*/);
    if (!prefix) {
        return undefined;
    }
    const rest = line.slice(prefix[0].length);
    let start: number;
    let end: number;
    if (rest.startsWith('"')) {
        const close = rest.indexOf('"', 1);
        if (close === -1) {
            return undefined;
        }
        start = prefix[0].length + 1;
        end = prefix[0].length + close;
    } else {
        const token = rest.match(/^\S+/);
        if (!token) {
            return undefined;
        }
        start = prefix[0].length;
        end = start + token[0].length;
    }
    if (character < start || character > end) {
        return undefined;
    }
    return { line: lineIndex, start, end };
}
