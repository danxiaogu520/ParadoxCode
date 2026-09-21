/**
 * Completion-item helpers shared by the VS Code LSP middleware and its tests.
 *
 * The language server intentionally stays editor-neutral. VS Code is the layer that knows how
 * to request another suggestion list after a completion item inserts an assignment or empty block.
 */

import type { IconPreview } from './gameAssets';
import { toHoverImageUrl } from './hoverCards';

export const FOLLOWUP_COMPLETION_TRIGGER_COMMAND = 'paradoxcode.triggerCompletion';

/** Widest image a completion documentation panel renders; wider sprites scale down. */
export const COMPLETION_IMAGE_MAX_WIDTH = 128;

type CompletionLike = {
    insertText?: unknown;
    command?: unknown;
};

function completionText(item: CompletionLike): string | undefined {
    if (typeof item.insertText === 'string') {
        return item.insertText;
    }
    if (item.insertText && typeof item.insertText === 'object' && 'value' in item.insertText) {
        const value = (item.insertText as { value?: unknown }).value;
        return typeof value === 'string' ? value : undefined;
    }
    return undefined;
}

/** Returns whether accepting this item leaves the cursor at a context worth completing again. */
export function shouldTriggerFollowupCompletion(item: CompletionLike): boolean {
    const text = completionText(item);
    if (text?.endsWith(' = ')) {
        return true;
    }
    // Empty Node/QuotedScript snippets place the final cursor stop inside the newly inserted
    // block. Parameterised scripted-macro snippets also contain `$0`, but their first `$1` stop
    // is a value argument, so they must not trigger a key completion at the wrong position.
    return typeof item.insertText === 'object'
        && text?.includes('$0') === true
        && !/\$(?:[1-9]\d*)|\$\{[1-9]\d*(?=[:}])/.test(text);
}

/** Adds the editor command used to request a follow-up completion list after insertion. */
export function attachFollowupCompletionTrigger(item: CompletionLike, documentUri?: string): void {
    if (!shouldTriggerFollowupCompletion(item) || item.command) {
        return;
    }
    item.command = {
        title: 'Trigger follow-up completion',
        command: FOLLOWUP_COMPLETION_TRIGGER_COMMAND,
        ...(documentUri ? { arguments: [{ uri: documentUri }] } : {}),
    };
}

// --- sprite preview in completion documentation --------------------------------
//
// Completion values for sprite-typed parameters (`icon = …`, event `picture`)
// are bare names; the suggest widget shows text only. When a resolved item's
// label names a sprite in the merged index, its documentation gains the
// decoded first-frame image — the same "server sends names, client owns
// pixels" split the hover previews use, attached on `completionItem/resolve`
// so only items VS Code actually displays pay for decoding.

/** Source of sprite lookups and decoded previews (the shared GameAssetStore). */
export interface SpritePreviewSource {
    spriteTexture(name: string): { name: string; textureFile: string; frames?: number } | undefined;
    spriteIconUrls(
        names: readonly string[],
        force?: ReadonlySet<string>,
    ): Promise<Record<string, IconPreview>>;
}

/** LSP `documentation` shapes the server sends (string or MarkupContent). */
export type DocumentationLike = string | { kind?: unknown; value?: unknown } | undefined;

/** The image markdown appended to a sprite value's documentation. */
export function spritePreviewImageMarkdown(name: string, preview: IconPreview): string {
    const suffix = preview.width > COMPLETION_IMAGE_MAX_WIDTH
        ? `|width=${COMPLETION_IMAGE_MAX_WIDTH}`
        : '';
    // Oversized data URLs spill into the hover file cache so VS Code's 100k
    // markdown truncation cannot behead them (same guard as hover images).
    return `![${name}](${toHoverImageUrl(preview.url)}${suffix})`;
}

/** Merges the preview image into an item's documentation as markdown,
 * preserving the existing text (plaintext is fenced to survive the switch). */
export function mergeSpritePreviewDocumentation(
    documentation: DocumentationLike,
    name: string,
    preview: IconPreview,
): { kind: 'markdown'; value: string } {
    const image = spritePreviewImageMarkdown(name, preview);
    if (typeof documentation === 'object' && documentation !== null) {
        if (documentation.kind === 'markdown' && typeof documentation.value === 'string') {
            return { kind: 'markdown', value: `${documentation.value}\n\n${image}` };
        }
        if (documentation.kind === 'plaintext' && typeof documentation.value === 'string') {
            return { kind: 'markdown', value: `\`\`\`\n${documentation.value}\n\`\`\`\n\n${image}` };
        }
        return { kind: 'markdown', value: image };
    }
    if (typeof documentation === 'string' && documentation !== '') {
        return { kind: 'markdown', value: `${documentation}\n\n${image}` };
    }
    return { kind: 'markdown', value: image };
}

/**
 * Attaches the sprite preview to one resolved completion item when its label
 * names a sprite with a decodable texture. Labels that are not sprite names,
 * missing textures, and decode failures all leave the item untouched —
 * documentation must never fail a completion.
 */
export async function attachSpritePreviewDocumentation(
    item: { label?: unknown; documentation?: unknown },
    source: SpritePreviewSource,
): Promise<void> {
    const label = typeof item.label === 'string' ? item.label : undefined;
    if (!label) {
        return;
    }
    if (!source.spriteTexture(label)) {
        return;
    }
    const previews = await source.spriteIconUrls([label]);
    const preview = previews[label];
    if (!preview) {
        return;
    }
    item.documentation = mergeSpritePreviewDocumentation(
        item.documentation as DocumentationLike,
        label,
        preview,
    );
}
