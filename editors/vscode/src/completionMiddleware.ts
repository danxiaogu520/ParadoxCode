/**
 * Completion-item helpers shared by the VS Code LSP middleware and its tests.
 *
 * The language server intentionally stays editor-neutral. VS Code is the layer that knows how
 * to request another suggestion list after a completion item inserts an assignment or empty block.
 */

export const FOLLOWUP_COMPLETION_TRIGGER_COMMAND = 'paradoxcode.triggerCompletion';

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
