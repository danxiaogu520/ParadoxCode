import * as path from 'path';

/**
 * Converts a native relative path to the forward-slash logical spelling the
 * server uses for `LogicalPath` values.
 */
export function toLogicalPath(relative: string): string {
    return relative.split(path.sep).join('/');
}

/**
 * Returns `file` relative to `root`, or `file` unchanged when it does not live
 * below `root`.
 *
 * The prefix comparison folds case on Windows, where the configured root (a
 * settings string) and client-reported paths frequently disagree on drive or
 * directory casing while naming the same directory.
 */
export function pathRelative(root: string, file: string): string {
    const normalizedRoot = root.replace(/\\/g, '/').replace(/\/$/, '');
    const normalizedFile = file.replace(/\\/g, '/');
    const fold = (value: string): string =>
        process.platform === 'win32' ? value.toLowerCase() : value;
    return fold(normalizedFile).startsWith(`${fold(normalizedRoot)}/`)
        ? normalizedFile.slice(normalizedRoot.length + 1)
        : normalizedFile;
}

/**
 * Compiles a workspace glob (`*`, `?`, `**`) into a case-insensitive matcher.
 * Case-insensitivity mirrors the game's own file resolution, which ignores
 * directory and file-name casing on every platform.
 */
export function globToRegExp(pattern: string): RegExp {
    const normalized = pattern.replace(/\\/g, '/');
    const escaped = normalized
        .replace(/[.+^${}()|[\]\\]/g, '\\$&')
        .replace(/\*\*/g, '\u0000')
        .replace(/\*/g, '[^/]*')
        .replace(/\u0000/g, '.*')
        .replace(/\?/g, '.');
    return new RegExp(`^${escaped}$`, 'i');
}
