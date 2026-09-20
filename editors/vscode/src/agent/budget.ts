/**
 * Shared budget helpers for agent tool results.
 *
 * Tool output is consumed by language models inside a chat context, so every shaping
 * path funnels through these bounds to keep responses compact and predictable.
 */

export interface CappedList<T> {
    items: T[];
    omitted: number;
}

/** Keeps at most `limit` items, reporting how many were dropped. */
export function capList<T>(items: readonly T[], limit: number): CappedList<T> {
    if (items.length <= limit) {
        return { items: [...items], omitted: 0 };
    }
    return { items: items.slice(0, limit), omitted: items.length - limit };
}

/** Truncates text to a character budget with an explicit continuation marker. */
export function capText(text: string, limit: number): string {
    if (text.length <= limit) {
        return text;
    }
    const kept = text.slice(0, limit);
    return `${kept}… (+${text.length - kept.length} more characters)`;
}

/** Collapses runs of whitespace and trims, for hover/markdown shaping. */
export function collapseWhitespace(text: string): string {
    return text.replace(/\s+/g, ' ').trim();
}
