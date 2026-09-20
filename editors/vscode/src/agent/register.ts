import * as vscode from 'vscode';
import {
    runHoverInfo, runSearchLocalisation, runSearchRules, runSearchSymbols, runValidateText,
    type HoverInfoInput, type SearchLocalisationInput, type SearchRulesInput,
    type SearchSymbolsInput, type ValidateTextInput,
} from './tools';

/**
 * Registers the read-only analysis tools with VS Code's Language Model Tools API so
 * chat participants and agent mode can call them autonomously. Users never reference
 * these tools manually. Registration is skipped silently on hosts without `vscode.lm`.
 */

type ToolRun<Input> = (input: Input, token: vscode.CancellationToken) => Promise<string>;

function invocationMessage(message: string) {
    return { invocationMessage: message };
}

function textResult(text: string): vscode.LanguageModelToolResult {
    return new vscode.LanguageModelToolResult([new vscode.LanguageModelTextPart(text)]);
}

/** Wraps a tool run so failures surface as readable tool results instead of thrown errors. */
function invokeAsResult(run: ToolRun<never>): (
    options: { input?: unknown },
    token: vscode.CancellationToken,
) => Promise<vscode.LanguageModelToolResult> {
    return async (options, token) => {
        try {
            return textResult(await run(options.input as never, token));
        } catch (error) {
            const message = error instanceof Error ? error.message : String(error);
            return textResult(`ParadoxCode tool error: ${message}`);
        }
    };
}

export function registerAgentTools(): vscode.Disposable[] {
    if (!('lm' in vscode) || typeof vscode.lm?.registerTool !== 'function') {
        return [];
    }
    const tools: { name: string; message: string; run: ToolRun<never> }[] = [
        {
            name: 'paradoxcode-validate-text',
            message: 'Validate EU4 script text with ParadoxCode rules',
            run: runValidateText as ToolRun<never>,
        },
        {
            name: 'paradoxcode-search-symbols',
            message: 'Search indexed EU4 symbols with ParadoxCode',
            run: runSearchSymbols as ToolRun<never>,
        },
        {
            name: 'paradoxcode-search-rules',
            message: 'Search the EU4 semantic rule database with ParadoxCode',
            run: runSearchRules as ToolRun<never>,
        },
        {
            name: 'paradoxcode-search-localisation',
            message: 'Search EU4 localisation entries with ParadoxCode',
            run: runSearchLocalisation as ToolRun<never>,
        },
        {
            name: 'paradoxcode-hover-info',
            message: 'Look up EU4 hover semantics with ParadoxCode',
            run: runHoverInfo as ToolRun<never>,
        },
    ];
    return tools.map((tool) =>
        vscode.lm.registerTool(tool.name, {
            prepareInvocation: () => invocationMessage(tool.message),
            invoke: invokeAsResult(tool.run),
        }),
    );
}
