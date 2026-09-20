import * as vscode from 'vscode';
import { AGENT_TOOL_SPECS, buildSystemPrompt } from './prompt';
import {
    runContext, runDiagnostics, runLocGet, runLocList, runLocSearch, runReferences, runRules,
    runSearch, runSymbolReferences, runValidateText, runWorkspace,
    type RulesInput, type SearchInput, type ValidateTextInput,
} from './tools';

/**
 * The @paradox chat participant: a deep agent loop over the shared tool layer. The model
 * comes from the chat request (the user's selected model) with a Copilot fallback, the
 * system prompt encodes EU4 modding discipline and the script/localisation zone split, and
 * tools are invoked as direct function calls — no manual referencing. Slash commands run
 * the same tools deterministically for hosts without a chat model.
 */

const MAX_TOOL_ROUNDS = 12;
const HISTORY_TURNS = 6;

const TOOL_RUNNERS: Record<string, (input: never, token: vscode.CancellationToken) => Promise<string>> = {
    'paradoxcode_workspace': runWorkspace as typeof TOOL_RUNNERS['paradoxcode_workspace'],
    'paradoxcode_search': runSearch as typeof TOOL_RUNNERS['paradoxcode_search'],
    'paradoxcode_context': runContext as typeof TOOL_RUNNERS['paradoxcode_context'],
    'paradoxcode_diagnostics': runDiagnostics as typeof TOOL_RUNNERS['paradoxcode_diagnostics'],
    'paradoxcode_references': runReferences as typeof TOOL_RUNNERS['paradoxcode_references'],
    'paradoxcode_symbol_references': runSymbolReferences as typeof TOOL_RUNNERS['paradoxcode_symbol_references'],
    'paradoxcode_rules': runRules as typeof TOOL_RUNNERS['paradoxcode_rules'],
    'paradoxcode_validate_text': runValidateText as typeof TOOL_RUNNERS['paradoxcode_validate_text'],
    'paradoxcode_loc_get': runLocGet as typeof TOOL_RUNNERS['paradoxcode_loc_get'],
    'paradoxcode_loc_search': runLocSearch as typeof TOOL_RUNNERS['paradoxcode_loc_search'],
    'paradoxcode_loc_list': runLocList as typeof TOOL_RUNNERS['paradoxcode_loc_list'],
};

const DEGRADATION_MESSAGE = [
    'ParadoxCode agent needs a chat model to answer conversationally, and none is available in this window.',
    '',
    'The deterministic commands keep working without a model:',
    '- `@paradox /validate` — validate the active editor file',
    '- `@paradox /symbols <query>` — search indexed script symbols',
    '- `@paradox /rules context=trigger key=... ` — search the rule database',
    '- `@paradox /loc key=... | text=... | prefix=...` — address or discover localisation',
    '- `@paradox /hover` — hover lookup at the cursor',
    '',
    'To enable conversational answers, sign in to GitHub Copilot or install another chat provider.',
].join('\n');

export function registerParadoxParticipant(): vscode.Disposable[] {
    if (!('chat' in vscode) || typeof vscode.chat?.createChatParticipant !== 'function') {
        return [];
    }
    const participant = vscode.chat.createChatParticipant('paradoxcode.modding', handleParadoxRequest);
    participant.followupProvider = {
        provideFollowups(): vscode.ChatFollowup[] {
            return [
                { prompt: 'Validate the active file', command: 'validate' },
                { prompt: 'Which scopes allow add_army_tradition?' },
            ];
        },
    };
    return [participant];
}

async function handleParadoxRequest(
    request: vscode.ChatRequest,
    context: vscode.ChatContext,
    stream: vscode.ChatResponseStream,
    token: vscode.CancellationToken,
): Promise<vscode.ChatResult> {
    try {
        if (request.command) {
            return await runSlashCommand(request, stream, token);
        }
        return await runAgentLoop(request, context, stream, token);
    } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        stream.markdown(`\n\nParadoxCode: ${message}`);
        return { errorDetails: { message } };
    }
}

/** Parses `context=x key=y scope=z` tokens; bare tokens fall back to a key filter. */
export function parseRuleFilters(prompt: string): RulesInput {
    const filters: RulesInput = {};
    for (const token of prompt.split(/\s+/).filter(Boolean)) {
        const separator = token.indexOf('=');
        if (separator <= 0) {
            filters.key ??= token;
            continue;
        }
        const prefix = token.slice(0, separator);
        const value = token.slice(separator + 1);
        if (prefix === 'context' || prefix === 'key' || prefix === 'scope') {
            filters[prefix] = value;
        } else {
            filters.key ??= token;
        }
    }
    return filters;
}

/** The resolved `/loc` query: exact key address, value search, or key-prefix family list. */
export interface LocalisationQuery {
    mode: 'get' | 'search' | 'list';
    key?: string;
    text?: string;
    keyPrefix?: string;
}

/** Parses `key=x`, `text=y`, or `prefix=z` tokens; token values run to the next mode token or
 * the end of the prompt, a bare word addresses a key exactly, bare multi-word text searches
 * displayed values. */
export function parseLocalisationQuery(prompt: string): LocalisationQuery {
    let key: string | undefined;
    let text: string | undefined;
    let keyPrefix: string | undefined;
    const segments = prompt.trim().split(/\s+(?=(?:key|text|prefix)=)/).filter(Boolean);
    for (const segment of segments) {
        const match = /^(key|text|prefix)=/.exec(segment);
        if (!match) {
            continue;
        }
        const value = segment.slice(match[0].length).trim();
        if (!value) {
            continue;
        }
        if (match[1] === 'key') {
            key = value;
        } else if (match[1] === 'text') {
            text = value;
        } else {
            keyPrefix = value;
        }
    }
    if (key) {
        return { mode: 'get', key };
    }
    if (text) {
        return { mode: 'search', text };
    }
    if (keyPrefix) {
        return { mode: 'list', keyPrefix };
    }
    const trimmed = prompt.trim();
    if (!trimmed) {
        return { mode: 'get' };
    }
    if (trimmed.includes(' ')) {
        return { mode: 'search', text: trimmed };
    }
    return { mode: 'get', key: trimmed };
}

function parseSearchInput(prompt: string): SearchInput {
    const filters = parseRuleFilters(prompt);
    if (!filters.context && !filters.scope) {
        return { query: filters.key ?? prompt.trim() };
    }
    return { query: filters.key ?? '' };
}

async function runSlashCommand(
    request: vscode.ChatRequest,
    stream: vscode.ChatResponseStream,
    token: vscode.CancellationToken,
): Promise<vscode.ChatResult> {
    const prompt = (request.prompt ?? '').trim();
    switch (request.command) {
        case 'validate': {
            const editor = vscode.window.activeTextEditor;
            if (!editor) {
                stream.markdown('Open an EU4 script or localisation file in the editor first.');
                return {};
            }
            const path = vscode.workspace.asRelativePath(editor.document.uri);
            stream.progress('ParadoxCode: validating…');
            stream.markdown(await runValidateText(
                { files: [{ path, text: editor.document.getText() }] } as ValidateTextInput,
                token,
            ));
            return {};
        }
        case 'symbols': {
            stream.markdown(await runSearch(parseSearchInput(prompt), token));
            return {};
        }
        case 'rules': {
            stream.markdown(await runRules(parseRuleFilters(prompt), token));
            return {};
        }
        case 'loc': {
            const query = parseLocalisationQuery(prompt);
            if (query.mode === 'list') {
                stream.markdown(await runLocList({ keyPrefix: query.keyPrefix ?? '' }, token));
            } else if (query.mode === 'search') {
                stream.markdown(await runLocSearch({ text: query.text ?? '' }, token));
            } else if (query.key) {
                stream.markdown(await runLocGet({ key: query.key }, token));
            } else {
                stream.markdown('Pass a key (key=event.1.t), a text filter (text=Templar), '
                    + 'or a key prefix (prefix=event.1.).');
            }
            return {};
        }
        case 'hover': {
            const editor = vscode.window.activeTextEditor;
            if (!editor) {
                stream.markdown('Open an indexed EU4 file and place the cursor on a key first.');
                return {};
            }
            const selection = editor.selection.active;
            stream.markdown(await runContext({
                path: editor.document.uri.toString(),
                line: selection.line + 1,
                character: selection.character,
            }, token));
            return {};
        }
        default:
            stream.markdown(`Unknown command /${request.command}.`);
            return {};
    }
}

async function selectModel(request: vscode.ChatRequest): Promise<vscode.LanguageModelChat | undefined> {
    if (request.model) {
        return request.model;
    }
    const copilot = await vscode.lm.selectChatModels({ vendor: 'copilot' });
    return copilot[0] ?? (await vscode.lm.selectChatModels())[0];
}

function historyMessages(context: vscode.ChatContext): vscode.LanguageModelChatMessage[] {
    const messages: vscode.LanguageModelChatMessage[] = [];
    for (const turn of context.history.slice(-HISTORY_TURNS)) {
        if (turn instanceof vscode.ChatRequestTurn) {
            messages.push(vscode.LanguageModelChatMessage.User(turn.prompt));
        } else if (turn instanceof vscode.ChatResponseTurn) {
            const text = turn.response
                .filter((part): part is vscode.ChatResponseMarkdownPart => part instanceof vscode.ChatResponseMarkdownPart)
                .map((part) => part.value.value)
                .join('');
            if (text.trim()) {
                messages.push(vscode.LanguageModelChatMessage.Assistant(text));
            }
        }
    }
    return messages;
}

async function runAgentLoop(
    request: vscode.ChatRequest,
    context: vscode.ChatContext,
    stream: vscode.ChatResponseStream,
    token: vscode.CancellationToken,
): Promise<vscode.ChatResult> {
    const model = await selectModel(request);
    if (!model) {
        stream.markdown(DEGRADATION_MESSAGE);
        return {};
    }
    const tools: vscode.LanguageModelChatTool[] = AGENT_TOOL_SPECS;
    const messages = [
        vscode.LanguageModelChatMessage.Assistant(buildSystemPrompt()),
        ...historyMessages(context),
        vscode.LanguageModelChatMessage.User(request.prompt),
    ];
    for (let round = 0; round < MAX_TOOL_ROUNDS; round += 1) {
        const response = await model.sendRequest(messages, { tools }, token);
        const toolCalls: vscode.LanguageModelToolCallPart[] = [];
        for await (const part of response.stream) {
            if (part instanceof vscode.LanguageModelTextPart) {
                stream.markdown(part.value);
            } else if (part instanceof vscode.LanguageModelToolCallPart) {
                toolCalls.push(part);
            }
        }
        if (toolCalls.length === 0) {
            return {};
        }
        const results: vscode.LanguageModelToolResultPart[] = [];
        for (const call of toolCalls) {
            stream.progress(`ParadoxCode: running ${call.name.replace('paradoxcode_', '')}…`);
            results.push(new vscode.LanguageModelToolResultPart(call.callId, [
                new vscode.LanguageModelTextPart(await executeToolCall(call, token)),
            ]));
        }
        messages.push(vscode.LanguageModelChatMessage.Assistant(toolCalls));
        messages.push(new vscode.LanguageModelChatMessage(
            vscode.LanguageModelChatMessageRole.User,
            results,
        ));
    }
    // Tool budget exhausted: force a wrap-up answer from the gathered results instead of
    // giving up, so broad tasks still produce a (partial) synthesis.
    messages.push(vscode.LanguageModelChatMessage.User(
        'Tool budget exhausted for this answer. Do not call any more tools. Answer now from '
        + 'the results already gathered, and state which parts you could not verify.',
    ));
    const wrapUp = await model.sendRequest(messages, {}, token);
    for await (const part of wrapUp.stream) {
        if (part instanceof vscode.LanguageModelTextPart) {
            stream.markdown(part.value);
        }
    }
    return {};
}

async function executeToolCall(
    call: vscode.LanguageModelToolCallPart,
    token: vscode.CancellationToken,
): Promise<string> {
    const run = TOOL_RUNNERS[call.name];
    if (!run) {
        return `ParadoxCode tool error: unknown tool ${call.name}`;
    }
    try {
        return await run(call.input as never, token);
    } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        return `ParadoxCode tool error: ${message}`;
    }
}
