// Hover texture preview assembly (pure functions, no vscode import so the
// contract tests can require the compiled module from plain Node).
//
// pdc's symbol hovers are markdown text; when the hovered symbol is a
// `sprite`, the extension host resolves the texturefile client-side and
// appends a `data:` image section — the same "server sends names, client
// owns pixels" split the mission preview uses.

import { toHoverImageUrl } from './hoverCards';

/** Widest image a hover tooltip renders; wider textures scale down. */
export const HOVER_IMAGE_MAX_WIDTH = 400;

/** Inputs for one assembled hover texture section. */
export interface TexturePreview {
    /** Alt text (sprite name or texture file name). */
    name: string;
    /** Game-root-relative texture path as written in the `.gfx` file. */
    rel: string;
    /** Decoded `data:image/png;base64,...` URL. */
    url: string;
    /** Pixel width of the decoded texture. */
    width: number;
}

/**
 * Extracts the sprite name from a pdc symbol hover. The hover title
 * (`### <kind> \`<name>\``, assembled in crates/ide/src/hover/render.rs) is
 * the only cross-process contract here; only `sprite` hovers bind textures.
 */
export function extractSpriteHoverName(markdown: string): string | undefined {
    return markdown.match(/^### sprite `([^`]+)`/m)?.[1];
}

/**
 * Extracts the markdown payload from a hover's contents. VS Code's `Hover`
 * class normalizes whatever the provider passes — including the single
 * `MarkdownString` vscode-languageclient builds from `MarkupContent` — into a
 * `contents` array, so both shapes must be accepted here; legacy
 * `MarkedString` strings and code blocks are skipped.
 */
export function markdownFromHoverContents(contents: unknown): { value: string } | undefined {
    const candidates = Array.isArray(contents) ? contents : [contents];
    for (const candidate of candidates) {
        if (typeof candidate !== 'object' || candidate === null) {
            continue;
        }
        const { value, language } = candidate as { value?: unknown; language?: unknown };
        if (typeof value === 'string' && language === undefined) {
            return candidate as { value: string };
        }
    }
    return undefined;
}

/**
 * Matches `texturefile = "path"` on one `.gfx` line when the cursor sits
 * inside the (quoted or bare) value; returns the raw value for
 * normalization. The server's semantic hover reports path resolution and
 * provenance; this locates the value so the extension can append the decoded
 * preview pixels.
 */
export function texturefileValueAt(line: string, character: number): string | undefined {
    const prefix = line.match(/^\s*texturefile\s*=\s*/i);
    if (!prefix) {
        return undefined;
    }
    const rest = line.slice(prefix[0].length);
    let value: string;
    let start: number;
    let end: number;
    if (rest.startsWith('"')) {
        const close = rest.indexOf('"', 1);
        if (close === -1) {
            return undefined;
        }
        value = rest.slice(1, close);
        start = prefix[0].length + 1;
        end = prefix[0].length + close;
    } else {
        const trimmed = rest.trimEnd();
        if (trimmed === '') {
            return undefined;
        }
        value = trimmed;
        start = prefix[0].length;
        end = prefix[0].length + trimmed.length;
    }
    if (character < start || character > end) {
        return undefined;
    }
    return value;
}

/** Assembles the markdown image, capping the rendered hover width. */
export function textureImageMarkdown(preview: TexturePreview): string {
    const suffix = preview.width > HOVER_IMAGE_MAX_WIDTH ? `|width=${HOVER_IMAGE_MAX_WIDTH}` : '';
    // Large textures spill into the file cache: an inline data URL beyond
    // VS Code's 100k hover markdown cap renders as literal source text.
    return `![${preview.name}](${toHoverImageUrl(preview.url)}${suffix})`;
}

/** Inserts an extension section above the server's final definition link. */
export function insertHoverSection(markdown: string, section: string): string {
    const footer = markdown.lastIndexOf('\n\nDefined in ');
    return footer < 0
        ? `${markdown}\n\n${section}`
        : `${markdown.slice(0, footer)}\n\n${section}${markdown.slice(footer)}`;
}

/** Adds a `#### Texture` section to an existing sprite hover. */
export function appendTextureSection(markdown: string, preview: TexturePreview): string {
    return insertHoverSection(markdown, texturefileHoverMarkdown(preview));
}

/** Standalone hover body for a hovered texturefile value. */
export function texturefileHoverMarkdown(preview: TexturePreview): string {
    const relative = preview.rel.replaceAll('`', "'");
    return ['#### Texture', '', `- Path: \`${relative}\``, '', textureImageMarkdown(preview)].join('\n');
}
