import * as vscode from 'vscode';
import {
    runContext, runDiagnostics, runLocGet, runLocList, runLocSearch, runReferences, runRules,
    runSearch, runSymbolReferences, runValidateText, runWorkspace,
} from './tools';

/**
 * Registers the read-only analysis tools with VS Code's Language Model Tools API so
 * chat participants and agent mode can call them autonomously. Users never reference
 * these tools manually. Registration is skipped silently on hosts without `vscode.lm`.
 *
 * The names follow the `paradoxcode_` convention and are mirrored one-to-one by the
 * stdio MCP server's manifest (read from package.json at runtime) and by the @paradox
 * participant's TOOL_RUNNERS table.
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
            name: 'paradoxcode_workspace',
            message: 'Summarise the EU4 workspace with ParadoxCode',
            run: runWorkspace as ToolRun<never>,
        },
        {
            name: 'paradoxcode_search',
            message: 'Search EU4 script symbols with ParadoxCode',
            run: runSearch as ToolRun<never>,
        },
        {
            name: 'paradoxcode_context',
            message: 'Explain EU4 script semantics at a position with ParadoxCode',
            run: runContext as ToolRun<never>,
        },
        {
            name: 'paradoxcode_diagnostics',
            message: 'Report EU4 workspace diagnostics with ParadoxCode',
            run: runDiagnostics as ToolRun<never>,
        },
        {
            name: 'paradoxcode_references',
            message: 'Find EU4 references at a position with ParadoxCode',
            run: runReferences as ToolRun<never>,
        },
        {
            name: 'paradoxcode_symbol_references',
            message: 'Find EU4 references by symbol name with ParadoxCode',
            run: runSymbolReferences as ToolRun<never>,
        },
        {
            name: 'paradoxcode_rules',
            message: 'Search the EU4 semantic rule database with ParadoxCode',
            run: runRules as ToolRun<never>,
        },
        {
            name: 'paradoxcode_validate_text',
            message: 'Validate EU4 script text with ParadoxCode rules',
            run: runValidateText as ToolRun<never>,
        },
        {
            name: 'paradoxcode_loc_get',
            message: 'Address an EU4 localisation key exactly with ParadoxCode',
            run: runLocGet as ToolRun<never>,
        },
        {
            name: 'paradoxcode_loc_search',
            message: 'Search EU4 localisation by displayed text with ParadoxCode',
            run: runLocSearch as ToolRun<never>,
        },
        {
            name: 'paradoxcode_loc_list',
            message: 'List an EU4 localisation key family with ParadoxCode',
            run: runLocList as ToolRun<never>,
        },
    ];
    return tools.map((tool) =>
        vscode.lm.registerTool(tool.name, {
            prepareInvocation: () => invocationMessage(tool.message),
            invoke: invokeAsResult(tool.run),
        }),
    );
}
