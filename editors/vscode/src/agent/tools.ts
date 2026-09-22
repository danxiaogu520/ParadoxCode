import { existsSync } from 'node:fs';
import { sep } from 'node:path';
import * as vscode from 'vscode';
import { acquireAgentClient, withTimeout } from './server';
import { capList, capText, collapseWhitespace } from './budget';
import { realUriOf } from '../transparentLoc';

/**
 * The eleven read-only agent tools, split into a script zone (workspace, search, context,
 * diagnostics, references, symbol_references, rules, validate_text) and a localisation zone
 * (loc_get, loc_search, loc_list). The two zones never cross: script tools reject
 * localisation paths with a pointer into the loc zone, and loc tools only ever answer with
 * localisation definitions. Each function validates its input, issues one LSP request on the
 * shared client, and shapes the response into compact text sized for a chat context. Errors
 * are thrown as plain Errors; the registration layer converts them into tool-result text so
 * the model can react and retry.
 */

const REQUEST_TIMEOUT_MS = 120_000;
const AVAILABILITY_WAIT_MS = 30_000;
const DEFAULT_SEARCH_LIMIT = 20;
const MAX_SEARCH_LIMIT = 50;
const MAX_SYMBOL_SEARCH_LIMIT = 100;
const DEFAULT_DIAGNOSTIC_FILES = 16;
const MAX_DIAGNOSTIC_FILES = 128;

const LOCALISATION_POINTER = 'That is a localisation file; localisation keys belong to the '
    + 'localisation zone. Use paradoxcode_loc_get (exact key) or paradoxcode_loc_list '
    + '(key prefix) instead.';

export interface ValidateTextInput {
    files: { path: string; text: string }[];
}

export interface SearchInput {
    query: string;
    limit?: number;
}

export interface RulesInput {
    context?: string;
    key?: string;
    scope?: string;
    limit?: number;
}

export interface ContextInput {
    path: string;
    line: number;
    character?: number;
}

export interface DiagnosticsInput {
    files?: string[];
    limit?: number;
    offset?: number;
}

export interface ReferencesInput {
    path: string;
    line: number;
    character?: number;
}

export interface SymbolReferencesInput {
    name: string;
    kind?: string;
    limit?: number;
}

export interface LocGetInput {
    key: string;
}

export interface LocSearchInput {
    text: string;
    limit?: number;
}

export interface LocListInput {
    keyPrefix: string;
    limit?: number;
}

const SEVERITY_LABELS: Record<number, string> = {
    1: 'error',
    2: 'warning',
    3: 'info',
    4: 'hint',
};

function searchLimit(limit: number | undefined, max: number = MAX_SEARCH_LIMIT): number {
    if (typeof limit !== 'number' || !Number.isFinite(limit)) {
        return DEFAULT_SEARCH_LIMIT;
    }
    return Math.min(Math.max(1, Math.trunc(limit)), max);
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

/** Prefers a document that is actually open, falling back to the on-disk
 * `file://` URI. With transparent encoding the file the user edits is its
 * `pdcloc://` decoded twin — that URI carries the live (possibly unsaved)
 * text the server knows, while the `file://` twin was never synced. */
export function openDocumentUriFor(fileUri: vscode.Uri): vscode.Uri {
    const fold = (value: string): string =>
        process.platform === 'win32' ? value.toLowerCase() : value;
    const wanted = fold(fileUri.fsPath);
    let twin: vscode.Uri | undefined;
    let raw: vscode.Uri | undefined;
    for (const document of vscode.workspace.textDocuments) {
        const decoded = realUriOf(document.uri);
        const real = decoded ?? document.uri;
        if (fold(real.fsPath) !== wanted) {
            continue;
        }
        if (decoded) {
            twin = document.uri;
        } else {
            raw = document.uri;
        }
    }
    return twin ?? raw ?? fileUri;
}

/** True when a path addresses a localisation-zone file (the `localisation/` tree). */
function isLocalisationPath(path: string): boolean {
    const normalised = path.replace(/\\/g, '/').toLowerCase();
    return normalised.startsWith('localisation/') || normalised.includes('/localisation/');
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

// ---------------------------------------------------------------------------
// Script zone
// ---------------------------------------------------------------------------

interface WorkspaceSummaryResponse {
    gameId?: unknown;
    ruleHash?: unknown;
    revision?: unknown;
    roots?: { kind?: unknown; path?: unknown; order?: unknown; writable?: unknown }[];
    fileCounts?: { script?: unknown; localisation?: unknown; total?: unknown };
    scan?: {
        discoveredFiles?: unknown;
        indexedFiles?: unknown;
        legacyEncodedFiles?: unknown;
        skippedEntries?: unknown;
        issues?: unknown;
    };
}

export async function runWorkspace(
    _input: Record<string, never>,
    token: vscode.CancellationToken,
): Promise<string> {
    const client = await acquireAgentClient(AVAILABILITY_WAIT_MS, token);
    const summary = await withTimeout(
        client.sendRequest<WorkspaceSummaryResponse>('pdc/workspaceSummary', undefined, token),
        REQUEST_TIMEOUT_MS,
        'Workspace summary',
    );
    const roots = (Array.isArray(summary?.roots) ? summary.roots : [])
        .map((root) => `${String(root.kind ?? '?')} root ${formatFileReference(String(root.path ?? '?'))}`
            + (root.writable === false ? ' (read-only)' : ''));
    const counts = summary?.fileCounts ?? {};
    const scan = summary?.scan ?? {};
    const lines = [
        `Game: ${String(summary?.gameId ?? 'unknown')} (rules ${String(summary?.ruleHash ?? 'unknown').slice(0, 12)}…, revision ${String(summary?.revision ?? '?')})`,
        `Files: ${String(counts.script ?? '?')} script, ${String(counts.localisation ?? '?')} localisation (${String(counts.total ?? '?')} total)`,
        `Scan: ${String(scan.indexedFiles ?? '?')} indexed of ${String(scan.discoveredFiles ?? '?')} discovered`
        + `, ${String(scan.issues ?? '?')} issue(s)`,
        ...roots,
    ];
    return capText(lines.join('\n'), 4096);
}

interface SymbolSearchResponse {
    symbols?: { name?: unknown; kind?: unknown; uri?: unknown; line?: unknown; path?: unknown }[];
    truncated?: boolean;
}

export async function runSearch(
    input: SearchInput,
    token: vscode.CancellationToken,
): Promise<string> {
    const query = (input.query ?? '').trim();
    if (query.length < 2) {
        return 'Query too short: pass at least 2 characters of a symbol name.';
    }
    const limit = searchLimit(input.limit, MAX_SYMBOL_SEARCH_LIMIT);
    const client = await acquireAgentClient(AVAILABILITY_WAIT_MS, token);
    const result = await withTimeout(
        client.sendRequest<SymbolSearchResponse>('pdc/symbolSearch', { query, limit }, token),
        REQUEST_TIMEOUT_MS,
        'Symbol search',
    );
    const symbols = Array.isArray(result?.symbols) ? result.symbols : [];
    if (symbols.length === 0) {
        return `No script symbols match "${query}" (project, dependencies, and Vanilla are searched; `
            + 'localisation keys are excluded — use the loc tools for those).';
    }
    const lines = symbols.map((symbol) => {
        const name = String(symbol.name ?? '<unnamed>');
        const kind = String(symbol.kind ?? 'unknown');
        const location = trimToUndefined(String(symbol.path ?? ''))
            ?? (typeof symbol.uri === 'string' ? formatFileReference(symbol.uri) : '?');
        const line = Number(symbol.line);
        const suffix = Number.isInteger(line) && line >= 1 ? `:${line}` : '';
        return `${name} (${kind}) — ${location}${suffix}`;
    });
    if (result.truncated === true) {
        lines.push(`… (truncated at ${limit} symbols; refine the query or raise the limit up to ${MAX_SYMBOL_SEARCH_LIMIT})`);
    }
    return capText(`Symbols matching "${query}":\n${lines.join('\n')}`, 8192);
}

export async function runContext(
    input: ContextInput,
    token: vscode.CancellationToken,
): Promise<string> {
    const path = (input.path ?? '').trim();
    const line = Math.trunc(Number(input.line));
    const character = Math.max(0, Math.trunc(Number(input.character ?? 0)));
    if (!path) {
        return 'Pass the file path (absolute, file: URI, or workspace-relative).';
    }
    if (isLocalisationPath(path)) {
        return LOCALISATION_POINTER;
    }
    if (!Number.isInteger(line) || line < 1) {
        return 'Lines are 1-based: pass line >= 1.';
    }
    const uri = resolveFileUri(path);
    if (!uri) {
        return `Could not resolve "${path}" to an existing file (tried absolute form and every workspace folder).`;
    }
    if (isLocalisationPath(uri.fsPath)) {
        return LOCALISATION_POINTER;
    }
    const client = await acquireAgentClient(AVAILABILITY_WAIT_MS, token);
    const hover = await withTimeout(
        client.sendRequest<{ contents?: { value?: unknown } } | null>(
            'textDocument/hover',
            {
                textDocument: { uri: openDocumentUriFor(uri).toString() },
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

interface WorkspaceDiagnosticsItem {
    uri?: unknown;
    logicalPath?: unknown;
    diagnostics?: unknown[];
}

interface WorkspaceDiagnosticsResponse {
    offset?: unknown;
    nextOffset?: unknown;
    total?: unknown;
    items?: WorkspaceDiagnosticsItem[];
}

export async function runDiagnostics(
    input: DiagnosticsInput,
    token: vscode.CancellationToken,
): Promise<string> {
    const files = (Array.isArray(input.files) ? input.files : [])
        .map((file) => (typeof file === 'string' ? file.trim() : ''))
        .filter(Boolean);
    if (files.length === 0 && input.files !== undefined) {
        return 'Pass at least one non-empty logical path in files, or omit files to diagnose the whole workspace.';
    }
    const limit = typeof input.limit === 'number' && Number.isFinite(input.limit)
        ? Math.min(Math.max(1, Math.trunc(input.limit)), MAX_DIAGNOSTIC_FILES)
        : DEFAULT_DIAGNOSTIC_FILES;
    const offset = Math.max(0, Math.trunc(Number(input.offset ?? 0)));
    const client = await acquireAgentClient(AVAILABILITY_WAIT_MS, token);
    const result = await withTimeout(
        client.sendRequest<WorkspaceDiagnosticsResponse>(
            'pdc/workspaceDiagnostics',
            { offset, limit, parser: 'script', ...(files.length > 0 ? { files } : {}) },
            token,
        ),
        REQUEST_TIMEOUT_MS,
        'Workspace diagnostics',
    );
    const items = Array.isArray(result?.items) ? result.items : [];
    const total = Number(result?.total ?? items.length);
    if (items.length === 0) {
        return `No script files to diagnose (total ${total}).`;
    }
    const sections: string[] = [];
    for (const item of items) {
        const logicalPath = String(item.logicalPath ?? '<unknown>');
        const diagnostics = Array.isArray(item.diagnostics) ? item.diagnostics : [];
        const { items: shown, omitted } = capList(diagnostics, 30);
        const lines = shown.map((diagnostic) =>
            describeDiagnostic(diagnostic as Record<string, unknown>));
        if (omitted > 0) {
            lines.push(`… (+${omitted} more diagnostics)`);
        }
        sections.push(diagnostics.length === 0
            ? `${logicalPath}: clean (0 diagnostics)`
            : `${logicalPath}: ${diagnostics.length} diagnostic(s)\n${lines.join('\n')}`);
    }
    const nextOffset = Number(result?.nextOffset);
    if (Number.isInteger(nextOffset) && nextOffset > 0) {
        sections.push(`… (showing ${items.length} of ${total} files; pass offset=${nextOffset} for the next page)`);
    }
    return capText(sections.join('\n\n'), 8192);
}

export async function runReferences(
    input: ReferencesInput,
    token: vscode.CancellationToken,
): Promise<string> {
    const path = (input.path ?? '').trim();
    const line = Math.trunc(Number(input.line));
    const character = Math.max(0, Math.trunc(Number(input.character ?? 0)));
    if (!path) {
        return 'Pass the file path (absolute, file: URI, or workspace-relative).';
    }
    if (isLocalisationPath(path)) {
        return LOCALISATION_POINTER;
    }
    if (!Number.isInteger(line) || line < 1) {
        return 'Lines are 1-based: pass line >= 1.';
    }
    const uri = resolveFileUri(path);
    if (!uri) {
        return `Could not resolve "${path}" to an existing file (tried absolute form and every workspace folder).`;
    }
    if (isLocalisationPath(uri.fsPath)) {
        return LOCALISATION_POINTER;
    }
    const client = await acquireAgentClient(AVAILABILITY_WAIT_MS, token);
    const references = await withTimeout(
        client.sendRequest<{ uri?: unknown; range?: { start?: { line?: unknown } } }[]>(
            'textDocument/references',
            {
                textDocument: { uri: openDocumentUriFor(uri).toString() },
                position: { line: line - 1, character },
                context: { includeDeclaration: true },
            },
            token,
        ),
        REQUEST_TIMEOUT_MS,
        'References',
    );
    const entries = Array.isArray(references) ? references : [];
    if (entries.length === 0) {
        return `No references at ${formatFileReference(uri.toString())}:${line}:${character}. `
            + 'The position must point at a symbol in a file the workspace has indexed; '
            + 'for name-driven lookup use paradoxcode_symbol_references.';
    }
    const { items, omitted } = capList(entries, MAX_SYMBOL_SEARCH_LIMIT);
    const lines = items.map((reference) => {
        const referenceUri = typeof reference.uri === 'string' ? reference.uri : '';
        const referenceLine = Number(reference.range?.start?.line);
        const suffix = Number.isInteger(referenceLine) && referenceLine >= 0 ? `:${referenceLine + 1}` : '';
        return `- ${formatFileReference(referenceUri)}${suffix}`;
    });
    if (omitted > 0) {
        lines.push(`… (+${omitted} more references)`);
    }
    return capText(`References (${entries.length}):\n${lines.join('\n')}`, 8192);
}

interface SymbolReferencesResponse {
    matched?: boolean;
    reason?: unknown;
    candidates?: { kind?: unknown; name?: unknown; definition?: unknown }[];
    symbol?: { name?: unknown; kind?: unknown; definition?: { uri?: unknown; line?: unknown; path?: unknown } };
    references?: { uri?: unknown; line?: unknown; path?: unknown }[];
    truncated?: boolean;
    total?: unknown;
}

function describeSymbolLocation(location: { uri?: unknown; line?: unknown; path?: unknown }): string {
    const path = trimToUndefined(String(location.path ?? ''))
        ?? (typeof location.uri === 'string' ? formatFileReference(location.uri) : '?');
    const line = Number(location.line);
    return Number.isInteger(line) && line >= 1 ? `${path}:${line}` : path;
}

export async function runSymbolReferences(
    input: SymbolReferencesInput,
    token: vscode.CancellationToken,
): Promise<string> {
    const name = trimToUndefined(input.name);
    const kind = trimToUndefined(input.kind);
    if (!name) {
        return 'Pass the symbol name (for example an event id or a scripted effect name).';
    }
    const limit = searchLimit(input.limit, MAX_SYMBOL_SEARCH_LIMIT);
    const client = await acquireAgentClient(AVAILABILITY_WAIT_MS, token);
    const result = await withTimeout(
        client.sendRequest<SymbolReferencesResponse>(
            'pdc/symbolReferences',
            { name, kind, limit },
            token,
        ),
        REQUEST_TIMEOUT_MS,
        'Symbol references',
    );
    if (result?.matched !== true) {
        const reason = trimToUndefined(String(result?.reason ?? ''))
            ?? 'the name does not resolve to a unique active definition.';
        const candidates = (Array.isArray(result?.candidates) ? result.candidates : [])
            .map((candidate) => `${String(candidate.name ?? name)} (${String(candidate.kind ?? '?')})`);
        const candidatePart = candidates.length > 0
            ? `\nCandidates:\n${candidates.map((entry) => `- ${entry}`).join('\n')}`
            : '';
        return `No unique symbol for "${name}": ${reason}${candidatePart}`;
    }
    const definition = result.symbol?.definition;
    const header = `${String(result.symbol?.name ?? name)} (${String(result.symbol?.kind ?? '?')})`
        + (definition ? ` defined at ${describeSymbolLocation(definition)}` : '');
    const references = Array.isArray(result?.references) ? result.references : [];
    const total = Number(result?.total ?? references.length);
    const lines = references.map((reference) => `- ${describeSymbolLocation(reference)}`);
    if (result?.truncated === true) {
        lines.push(`… (truncated at ${limit} of ${total} references; raise the limit up to ${MAX_SYMBOL_SEARCH_LIMIT})`);
    }
    if (references.length === 0) {
        return `${header}\nNo references found.`;
    }
    return capText(`${header}\nReferences (${total}):\n${lines.join('\n')}`, 8192);
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

export async function runRules(
    input: RulesInput,
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
        client.sendRequest<{ path: string; diagnostics?: unknown[] }[]>('pdc/textDiagnostics', { files }, token),
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

// ---------------------------------------------------------------------------
// Localisation zone
// ---------------------------------------------------------------------------

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

function describeLocalisationHit(hit: LocalisationSearchHit): string {
    const value = trimToUndefined(String(hit.value ?? '')) ?? '<no preview>';
    const language = trimToUndefined(String(hit.language ?? '')) ?? 'unknown language';
    const file = trimToUndefined(String(hit.file ?? '')) ?? 'unknown file';
    return `${String(hit.key ?? '<unknown>')} = "${collapseWhitespace(value)}" (${language}) — ${file}`;
}

async function localisationSearch(
    params: { key?: string; text?: string; keyMatch?: string; limit: number },
    token: vscode.CancellationToken,
): Promise<LocalisationSearchResponse> {
    const client = await acquireAgentClient(AVAILABILITY_WAIT_MS, token);
    return withTimeout(
        client.sendRequest<LocalisationSearchResponse>('pdc/localisationSearch', params, token),
        REQUEST_TIMEOUT_MS,
        'Localisation search',
    );
}

export async function runLocGet(
    input: LocGetInput,
    token: vscode.CancellationToken,
): Promise<string> {
    const key = trimToUndefined(input.key);
    if (!key) {
        return 'Pass the exact localisation key (matching is case-insensitive).';
    }
    const result = await localisationSearch({ key, keyMatch: 'exact', limit: 1 }, token);
    const hits = Array.isArray(result?.hits) ? result.hits : [];
    if (hits.length === 0) {
        return `No localisation key "${key}" is defined. Use paradoxcode_loc_list with the key's `
            + 'family prefix to see which neighbouring keys exist.';
    }
    return capText(describeLocalisationHit(hits[0]), 2048);
}

export async function runLocSearch(
    input: LocSearchInput,
    token: vscode.CancellationToken,
): Promise<string> {
    const text = trimToUndefined(input.text);
    if (!text) {
        return 'Pass the text filter (matched case-insensitively against displayed values).';
    }
    const limit = searchLimit(input.limit);
    const result = await localisationSearch({ text, limit }, token);
    const hits = Array.isArray(result?.hits) ? result.hits : [];
    if (hits.length === 0) {
        return 'No localisation entries mention that text across the project, dependencies, and Vanilla.';
    }
    const lines = hits.map(describeLocalisationHit);
    if (result.truncated === true) {
        lines.push(`… (truncated at ${limit} entries; narrow the text or raise the limit up to ${MAX_SEARCH_LIMIT})`);
    }
    return capText(`Localisation matching "${text}":\n${lines.join('\n')}`, 4096);
}

export async function runLocList(
    input: LocListInput,
    token: vscode.CancellationToken,
): Promise<string> {
    const keyPrefix = trimToUndefined(input.keyPrefix);
    if (!keyPrefix) {
        return 'Pass the key prefix (for example "my_event.1." to enumerate one event\'s key family).';
    }
    const limit = searchLimit(input.limit);
    const result = await localisationSearch({ key: keyPrefix, keyMatch: 'prefix', limit }, token);
    const hits = Array.isArray(result?.hits) ? result.hits : [];
    if (hits.length === 0) {
        return `No localisation keys start with "${keyPrefix}".`;
    }
    const lines = hits.map(describeLocalisationHit);
    if (result.truncated === true) {
        lines.push(`… (truncated at ${limit} entries; extend the prefix or raise the limit up to ${MAX_SEARCH_LIMIT})`);
    }
    return capText(`Keys under "${keyPrefix}" (${hits.length}${result.truncated === true ? '+' : ''}):\n${lines.join('\n')}`, 4096);
}
