import { existsSync } from 'node:fs';
import { sep } from 'node:path';
import * as vscode from 'vscode';
import { acquireAgentClient, withTimeout } from './server';
import { capList, capText, collapseWhitespace } from './budget';

/**
 * The five read-only agent tools. Each function validates its input, issues one LSP
 * request on the shared client, and shapes the response into compact text sized for a
 * chat context. Errors are thrown as plain Errors; the registration layer converts
 * them into tool-result text so the model can react and retry.
 */

const REQUEST_TIMEOUT_MS = 120_000;
const AVAILABILITY_WAIT_MS = 30_000;
const DEFAULT_SEARCH_LIMIT = 20;
const MAX_SEARCH_LIMIT = 50;

export interface ValidateTextInput {
    files: { path: string; text: string }[];
}

export interface SearchSymbolsInput {
    query: string;
    limit?: number;
}

export interface SearchRulesInput {
    context?: string;
    key?: string;
    scope?: string;
    limit?: number;
}

export interface SearchLocalisationInput {
    key?: string;
    text?: string;
    limit?: number;
}

export interface HoverInfoInput {
    path: string;
    line: number;
    character?: number;
}

const SEVERITY_LABELS: Record<number, string> = {
    1: 'error',
    2: 'warning',
    3: 'info',
    4: 'hint',
};

const SYMBOL_KIND_LABELS: Record<number, string> = {
    1: 'file', 2: 'module', 3: 'namespace', 4: 'package', 5: 'class', 6: 'method',
    7: 'property', 8: 'field', 9: 'constructor', 10: 'enum', 11: 'interface', 12: 'function',
    13: 'variable', 14: 'constant', 15: 'string', 16: 'number', 17: 'boolean', 18: 'array',
    19: 'object', 20: 'key', 21: 'null', 22: 'enum member', 23: 'struct', 24: 'event',
    25: 'operator', 26: 'type parameter',
};

function searchLimit(limit: number | undefined): number {
    if (typeof limit !== 'number' || !Number.isFinite(limit)) {
        return DEFAULT_SEARCH_LIMIT;
    }
    return Math.min(Math.max(1, Math.trunc(limit)), MAX_SEARCH_LIMIT);
}

function trimToUndefined(value: string | undefined): string | undefined {
    const trimmed = typeof value === 'string' ? value.trim() : '';
    return trimmed.length > 0 ? trimmed : undefined;
}

/** Displays a file URI as a path, workspace-relative when it lives under the first folder. */
export function formatFileReference(uriOrPath: string): string {
    let path = uriOrPath;
    if (uriOrPath.startsWith('file:')) {
        try {
            path = vscode.Uri.parse(uriOrPath, true).fsPath;
        } catch {
            // Keep the raw string when it is not a parsable file URI.
        }
    }
    const root = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
    if (root && root.length > 0 && (path === root || path.startsWith(root + sep))) {
        return path.slice(root.length + 1);
    }
    return path;
}

/** Resolves an absolute or workspace-relative path to a file URI that exists on disk. */
function resolveFileUri(path: string): vscode.Uri | undefined {
    if (path.startsWith('file:')) {
        try {
            return vscode.Uri.parse(path, true);
        } catch {
            return undefined;
        }
    }
    const absolute = path.startsWith('/') || path.startsWith('\\') || /^[a-zA-Z]:[\\/]/.test(path);
    if (absolute) {
        return vscode.Uri.file(path);
    }
    const folders = vscode.workspace.workspaceFolders ?? [];
    for (const folder of folders) {
        const candidate = vscode.Uri.joinPath(folder.uri, ...path.split(/[\\/]+/).filter(Boolean));
        if (existsSync(candidate.fsPath)) {
            return candidate;
        }
    }
    return undefined;
}

interface TextDiagnosticsResponse {
    path: string;
    diagnostics?: unknown[];
}

function describeDiagnostic(diagnostic: Record<string, unknown>): string {
    const range = diagnostic.range as { start?: { line?: number } } | undefined;
    const line = typeof range?.start?.line === 'number' ? range.start.line + 1 : undefined;
    const severity = SEVERITY_LABELS[Number(diagnostic.severity)] ?? 'diagnostic';
    const code = typeof diagnostic.code === 'string' ? diagnostic.code : undefined;
    const message = collapseWhitespace(String(diagnostic.message ?? ''));
    const location = line === undefined ? '' : `L${line}: `;
    const codePart = code ? `[${code}] ` : '';
    return `${location}${codePart}(${severity}) ${message}`;
}

export async function runValidateText(
    input: ValidateTextInput,
    token: vscode.CancellationToken,
): Promise<string> {
    const files = (Array.isArray(input.files) ? input.files : [])
        .filter((file): file is { path: string; text: string } =>
            typeof file?.path === 'string' && typeof file?.text === 'string')
        .slice(0, 16);
    if (files.length === 0) {
        return 'No files to validate: pass between 1 and 16 {path, text} entries.';
    }
    const client = await acquireAgentClient(AVAILABILITY_WAIT_MS, token);
    const results = await withTimeout(
        client.sendRequest<TextDiagnosticsResponse[]>('pdc/textDiagnostics', { files }, token),
        REQUEST_TIMEOUT_MS,
        'Validation',
    );
    const sections: string[] = [];
    for (const result of Array.isArray(results) ? results : []) {
        const diagnostics = Array.isArray(result.diagnostics) ? result.diagnostics : [];
        const { items, omitted } = capList(diagnostics, 30);
        const lines = items.map((diagnostic) =>
            describeDiagnostic(diagnostic as Record<string, unknown>));
        if (omitted > 0) {
            lines.push(`… (+${omitted} more diagnostics)`);
        }
        sections.push(diagnostics.length === 0
            ? `${result.path}: clean (0 diagnostics)`
            : `${result.path}: ${diagnostics.length} diagnostic(s)\n${lines.join('\n')}`);
    }
    return capText(sections.join('\n\n'), 8192);
}

interface WorkspaceSymbolEntry {
    name?: unknown;
    kind?: unknown;
    location?: { uri?: unknown; range?: { start?: { line?: unknown } } };
}

export async function runSearchSymbols(
    input: SearchSymbolsInput,
    token: vscode.CancellationToken,
): Promise<string> {
    const query = (input.query ?? '').trim();
    if (query.length < 2) {
        return 'Query too short: pass at least 2 characters of a symbol name.';
    }
    const limit = searchLimit(input.limit);
    const client = await acquireAgentClient(AVAILABILITY_WAIT_MS, token);
    const symbols = await withTimeout(
        client.sendRequest<WorkspaceSymbolEntry[]>('workspace/symbol', { query }, token),
        REQUEST_TIMEOUT_MS,
        'Symbol search',
    );
    const entries = Array.isArray(symbols) ? symbols : [];
    if (entries.length === 0) {
        return `No indexed symbols match "${query}" (project, dependencies, and Vanilla are searched).`;
    }
    const { items, omitted } = capList(entries, limit);
    const lines = items.map((symbol) => {
        const name = String(symbol.name ?? '<unnamed>');
        const kind = SYMBOL_KIND_LABELS[Number(symbol.kind)] ?? `kind ${String(symbol.kind)}`;
        const uri = typeof symbol.location?.uri === 'string' ? symbol.location.uri : '';
        const line = Number(symbol.location?.range?.start?.line);
        const suffix = Number.isInteger(line) && line >= 0 ? `:${line + 1}` : '';
        return `${name} (${kind}) — ${formatFileReference(uri)}${suffix}`;
    });
    if (omitted > 0) {
        lines.push(`… (+${omitted} more symbols; refine the query or raise the limit up to ${MAX_SEARCH_LIMIT})`);
    }
    return capText(`Symbols matching "${query}":\n${lines.join('\n')}`, 4096);
}

interface RuleSearchEntry {
    key?: unknown;
    context?: unknown;
    shape?: unknown;
    allowedScopes?: unknown;
    deprecated?: unknown;
    required?: unknown;
    documentation?: unknown;
}

interface RuleSearchResponse {
    rules?: RuleSearchEntry[];
    truncated?: boolean;
}

export async function runSearchRules(
    input: SearchRulesInput,
    token: vscode.CancellationToken,
): Promise<string> {
    const context = trimToUndefined(input.context);
    const key = trimToUndefined(input.key);
    const scope = trimToUndefined(input.scope);
    if (!context && !key && !scope) {
        return 'Pass at least one of context, key, or scope (for example context "trigger", key "add_army_tradition").';
    }
    const limit = searchLimit(input.limit);
    const client = await acquireAgentClient(AVAILABILITY_WAIT_MS, token);
    const result = await withTimeout(
        client.sendRequest<RuleSearchResponse>(
            'pdc/ruleSearch',
            { context, key, scope, limit },
            token,
        ),
        REQUEST_TIMEOUT_MS,
        'Rule search',
    );
    const rules = Array.isArray(result?.rules) ? result.rules : [];
    if (rules.length === 0) {
        return 'No semantic rules match. Try a shorter key substring, or a broader context such as trigger or effect.';
    }
    const filters = [
        context ? `context=${context}` : undefined,
        key ? `key=${key}` : undefined,
        scope ? `scope=${scope}` : undefined,
    ].filter(Boolean).join(' ');
    const lines = rules.map((rule) => {
        const allowed = Array.isArray(rule.allowedScopes) && rule.allowedScopes.length > 0
            ? rule.allowedScopes.map((entry) => String(entry)).join('|')
            : 'any';
        const flags = [
            rule.deprecated === true ? 'deprecated' : undefined,
            rule.required === true ? 'required' : undefined,
        ].filter(Boolean);
        const documentation = trimToUndefined(String(rule.documentation ?? ''));
        const docPart = documentation ? ` — ${collapseWhitespace(documentation)}` : '';
        const flagPart = flags.length > 0 ? ` (${flags.join(', ')})` : '';
        return `${String(rule.key ?? '<unknown>')} [${String(rule.context ?? '?')}]`
            + ` shape=${String(rule.shape ?? '?')} scopes=${allowed}${flagPart}${docPart}`;
    });
    if (result.truncated === true) {
        lines.push(`… (truncated at ${limit} rules; narrow the filters or raise the limit up to ${MAX_SEARCH_LIMIT})`);
    }
    return capText(`Rules matching ${filters}:\n${lines.join('\n')}`, 6144);
}

interface LocalisationSearchHit {
    key?: unknown;
    language?: unknown;
    value?: unknown;
    file?: unknown;
}

interface LocalisationSearchResponse {
    hits?: LocalisationSearchHit[];
    truncated?: boolean;
}

export async function runSearchLocalisation(
    input: SearchLocalisationInput,
    token: vscode.CancellationToken,
): Promise<string> {
    const key = trimToUndefined(input.key);
    const text = trimToUndefined(input.text);
    if (!key && !text) {
        return 'Pass at least one of key or text (key matches localisation key names, text matches displayed values).';
    }
    const limit = searchLimit(input.limit);
    const client = await acquireAgentClient(AVAILABILITY_WAIT_MS, token);
    const result = await withTimeout(
        client.sendRequest<LocalisationSearchResponse>(
            'pdc/localisationSearch',
            { key, text, limit },
            token,
        ),
        REQUEST_TIMEOUT_MS,
        'Localisation search',
    );
    const hits = Array.isArray(result?.hits) ? result.hits : [];
    if (hits.length === 0) {
        return 'No localisation entries match. Keys are matched case-insensitively as substrings across the project, dependencies, and Vanilla.';
    }
    const lines = hits.map((hit) => {
        const value = trimToUndefined(String(hit.value ?? '')) ?? '<no preview>';
        const language = trimToUndefined(String(hit.language ?? '')) ?? 'unknown language';
        const file = trimToUndefined(String(hit.file ?? '')) ?? 'unknown file';
        return `${String(hit.key ?? '<unknown>')} = "${collapseWhitespace(value)}" (${language}) — ${file}`;
    });
    if (result.truncated === true) {
        lines.push(`… (truncated at ${limit} entries; narrow the query or raise the limit up to ${MAX_SEARCH_LIMIT})`);
    }
    return capText(lines.join('\n'), 4096);
}

interface HoverResponse {
    contents?: { value?: unknown };
}

export async function runHoverInfo(
    input: HoverInfoInput,
    token: vscode.CancellationToken,
): Promise<string> {
    const path = (input.path ?? '').trim();
    const line = Math.trunc(Number(input.line));
    const character = Math.max(0, Math.trunc(Number(input.character ?? 0)));
    if (!path) {
        return 'Pass the file path (absolute, file: URI, or workspace-relative).';
    }
    if (!Number.isInteger(line) || line < 1) {
        return 'Lines are 1-based: pass line >= 1.';
    }
    const uri = resolveFileUri(path);
    if (!uri) {
        return `Could not resolve "${path}" to an existing file (tried absolute form and every workspace folder).`;
    }
    const client = await acquireAgentClient(AVAILABILITY_WAIT_MS, token);
    const hover = await withTimeout(
        client.sendRequest<HoverResponse | null>(
            'textDocument/hover',
            {
                textDocument: { uri: uri.toString() },
                position: { line: line - 1, character },
            },
            token,
        ),
        REQUEST_TIMEOUT_MS,
        'Hover',
    );
    const contents = hover?.contents;
    const value = contents && typeof contents === 'object' && 'value' in contents
        ? String(contents.value ?? '')
        : '';
    if (!value.trim()) {
        return `No hover information at ${formatFileReference(uri.toString())}:${line}:${character}.`;
    }
    return capText(collapseWhitespace(value), 2048);
}
