/**
 * The stdio MCP server's tool layer: a dependency-free twin of the extension's
 * `src/agent/tools.ts`. Input validation, request shaping, and response
 * formatting stay behaviour-identical to the VS Code surface so the two faces
 * cannot drift; only the vscode imports (client acquisition, Uri handling) are
 * replaced by the plain-node equivalents fed by `mcpBoot.mjs`.
 *
 * The tool surface is split into a script zone and a localisation zone that never
 * cross, mirroring the extension: script tools reject localisation paths with a
 * pointer into the loc zone, and loc tools only ever answer with localisation
 * definitions.
 *
 * The tool manifest (names, descriptions, JSON schemas) is read at runtime
 * from package.json's `contributes.languageModelTools` — the same single
 * source of truth the extension registers from.
 */

import { existsSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const manifestPath = join(
  dirname(fileURLToPath(import.meta.url)),
  '..',
  '..',
  'package.json',
);

/** Server instructions surfaced through the MCP `initialize` response. */
export const MCP_INSTRUCTIONS = [
  "You are ParadoxCode's EU4 modding assistant, exposed to MCP clients. You help write Europa Universalis IV mods in Paradox script.",
  '',
  'Paradox script essentials:',
  '- Curly-braced blocks: `country_event = { id = my_mod.1 title = my_mod.1.t }`. Keys are snake_case. In trigger contexts `=` assigns while `>`, `<`, `>=`, `<=`, `!=` compare.',
  '- Scope discipline: every trigger and effect runs in a scope (country, province, ...). THIS refers to the current scope; the most common modding error is using a key in the wrong scope.',
  '- Localisation lives in `localisation/*_l_english.yml` files with `key:0 "text"` entries and `$PLACEHOLDER$` substitution; event titles usually follow the `<event id>.t` convention.',
  '- Comments start with `#`. Strings use double quotes; yes/no are bare words.',
  '',
  'Zone discipline (hard rules):',
  '- The tools are split into a script zone (workspace, search, context, diagnostics, references, symbol_references, rules, validate_text) and a localisation zone (loc_get, loc_search, loc_list). The zones never cross: script tools never return localisation entries and loc tools never return script facts.',
  '- When a script tool points you to the localisation zone, continue there: validation reporting UnknownLocalisationKey means the script references a missing key — resolve it with paradoxcode_loc_get / paradoxcode_loc_list, or add the key and file.',
  '- Validation diagnostic codes are authoritative: UnknownKey (key not valid in this context), InvalidValue (value does not satisfy the rule), WrongScope (key exists but not in this scope), UnknownLocalisationKey (missing localisation key).',
  '',
  'Read workflow (answer questions about a workspace):',
  '1. Orient: call paradoxcode_workspace once per conversation to learn the roots and rule identity.',
  '2. Find symbols by name with paradoxcode_search; address one exactly with paradoxcode_loc_get (localisation) or paradoxcode_symbol_references (script symbols).',
  '3. Check what a key accepts and in which scopes with paradoxcode_rules instead of guessing from memory.',
  '4. Explain a position with paradoxcode_context; measure the impact of a name with paradoxcode_symbol_references before proposing edits.',
  '',
  'Edit workflow (write or change files):',
  '1. Read first: follow the read workflow for everything you touch.',
  '2. Draft, then ALWAYS call paradoxcode_validate_text on the new content before considering the work done. Fix what it reports and re-validate.',
  '3. When renaming or removing a definition, first list its references with paradoxcode_symbol_references so every use is updated.',
  '4. When validation reports UnknownLocalisationKey, close the loop in the localisation zone (loc_get, loc_list, or add the key).',
  '',
  'Budget discipline:',
  '- You may issue several tool calls in one round; batch independent lookups instead of chaining them.',
  '- Plan queries before searching; by the eighth tool round stop expanding searches and synthesise the answer from what you have.',
  '',
  'Directory conventions: gameplay definitions live under common/ (one file per category), events under events/, decisions under decisions/, mission trees under missions/, province and country history under history/, interface assets under interface/ and gfx/.',
  '',
  "Answer in the user's language. Prefer checking with the tools over reciting from memory; keep answers focused and actionable.",
].join('\n');

/** The MCP tool manifest, mirrored from the extension's lm-tool contributions. */
export function readToolManifest() {
  const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
  const contributed = manifest?.contributes?.languageModelTools;
  if (!Array.isArray(contributed)) {
    throw new Error('package.json contributes.languageModelTools is missing');
  }
  return contributed.map((tool) => ({
    name: tool.name,
    title: typeof tool.displayName === 'string' ? tool.displayName : undefined,
    description:
      typeof tool.modelDescription === 'string' && tool.modelDescription.length > 0
        ? tool.modelDescription
        : `ParadoxCode tool ${tool.name}`,
    inputSchema: tool.inputSchema ?? { type: 'object' },
  }));
}

/** Keeps at most `limit` items, reporting how many were dropped. */
export function capList(items, limit) {
  if (items.length <= limit) {
    return { items: [...items], omitted: 0 };
  }
  return { items: items.slice(0, limit), omitted: items.length - limit };
}

/** Truncates text to a character budget with an explicit continuation marker. */
export function capText(text, limit) {
  if (text.length <= limit) {
    return text;
  }
  const kept = text.slice(0, limit);
  return `${kept}… (+${text.length - kept.length} more characters)`;
}

/** Collapses runs of whitespace and trims, for hover/markdown shaping. */
export function collapseWhitespace(text) {
  return text.replace(/\s+/g, ' ').trim();
}

const DEFAULT_SEARCH_LIMIT = 20;
const MAX_SEARCH_LIMIT = 50;
const MAX_SYMBOL_SEARCH_LIMIT = 100;
const DEFAULT_DIAGNOSTIC_FILES = 16;
const MAX_DIAGNOSTIC_FILES = 128;

const LOCALISATION_POINTER = 'That is a localisation file; localisation keys belong to the '
  + 'localisation zone. Use paradoxcode_loc_get (exact key) or paradoxcode_loc_list '
  + '(key prefix) instead.';

const SEVERITY_LABELS = {
  1: 'error',
  2: 'warning',
  3: 'info',
  4: 'hint',
};

function searchLimit(limit, max = MAX_SEARCH_LIMIT) {
  if (typeof limit !== 'number' || !Number.isFinite(limit)) {
    return DEFAULT_SEARCH_LIMIT;
  }
  return Math.min(Math.max(1, Math.trunc(limit)), max);
}

function trimToUndefined(value) {
  const trimmed = typeof value === 'string' ? value.trim() : '';
  return trimmed.length > 0 ? trimmed : undefined;
}

function toPosix(path) {
  return path.replaceAll('\\', '/');
}

/** Displays a file path or URI as a path, workspace-relative when under `root`. */
export function formatFileReference(pathOrUri, root) {
  let path = pathOrUri;
  if (typeof path === 'string' && path.startsWith('file:')) {
    try {
      path = fileURLToPath(path);
    } catch {
      // Keep the raw string when it is not a parsable file URI.
    }
  }
  const normalized = toPosix(String(path));
  const normalizedRoot = root ? toPosix(root).replace(/\/+$/, '') : '';
  if (normalizedRoot && (normalized === normalizedRoot || normalized.startsWith(normalizedRoot + '/'))) {
    return normalized.slice(normalizedRoot.length + 1);
  }
  return normalized;
}

/** Resolves an absolute or workspace-relative path to an existing filesystem path. */
function resolveFilePath(input, root) {
  if (input.startsWith('file:')) {
    try {
      return fileURLToPath(input);
    } catch {
      return undefined;
    }
  }
  const absolute = input.startsWith('/') || input.startsWith('\\') || /^[a-zA-Z]:[\\/]/.test(input);
  if (absolute) {
    return existsSync(input) ? input : undefined;
  }
  if (!root) return undefined;
  const candidate = join(root, ...input.split(/[\\/]+/).filter(Boolean));
  return existsSync(candidate) ? candidate : undefined;
}

/** True when a path addresses a localisation-zone file (the `localisation/` tree). */
function isLocalisationPath(path) {
  const normalised = toPosix(path).toLowerCase();
  return normalised.startsWith('localisation/') || normalised.includes('/localisation/');
}

function describeDiagnostic(diagnostic) {
  const range = diagnostic.range;
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

export async function runWorkspace(session, root) {
  const summary = await session.request('pdc/workspaceSummary', undefined, 'Workspace summary');
  const roots = (Array.isArray(summary?.roots) ? summary.roots : [])
    .map((root_) => `${String(root_?.kind ?? '?')} root ${formatFileReference(String(root_?.path ?? '?'), root)}`
      + (root_?.writable === false ? ' (read-only)' : ''));
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

export async function runSearch(session, root, input) {
  const query = (input?.query ?? '').trim();
  if (query.length < 2) {
    return 'Query too short: pass at least 2 characters of a symbol name.';
  }
  const limit = searchLimit(input?.limit, MAX_SYMBOL_SEARCH_LIMIT);
  const result = await session.request(
    'pdc/symbolSearch',
    { query, limit },
    'Symbol search',
  );
  const symbols = Array.isArray(result?.symbols) ? result.symbols : [];
  if (symbols.length === 0) {
    return `No script symbols match "${query}" (project, dependencies, and Vanilla are searched; `
      + 'localisation keys are excluded — use the loc tools for those).';
  }
  const lines = symbols.map((symbol) => {
    const name = String(symbol?.name ?? '<unnamed>');
    const kind = String(symbol?.kind ?? 'unknown');
    const location = trimToUndefined(String(symbol?.path ?? ''))
      ?? (typeof symbol?.uri === 'string' ? formatFileReference(symbol.uri, root) : '?');
    const line = Number(symbol?.line);
    const suffix = Number.isInteger(line) && line >= 1 ? `:${line}` : '';
    return `${name} (${kind}) — ${location}${suffix}`;
  });
  if (result?.truncated === true) {
    lines.push(`… (truncated at ${limit} symbols; refine the query or raise the limit up to ${MAX_SYMBOL_SEARCH_LIMIT})`);
  }
  return capText(`Symbols matching "${query}":\n${lines.join('\n')}`, 8192);
}

export async function runContext(session, root, input) {
  const path = (input?.path ?? '').trim();
  const line = Math.trunc(Number(input?.line));
  const character = Math.max(0, Math.trunc(Number(input?.character ?? 0)));
  if (!path) {
    return 'Pass the file path (absolute, file: URI, or workspace-relative).';
  }
  if (isLocalisationPath(path)) {
    return LOCALISATION_POINTER;
  }
  if (!Number.isInteger(line) || line < 1) {
    return 'Lines are 1-based: pass line >= 1.';
  }
  const resolved = resolveFilePath(path, root);
  if (!resolved) {
    return `Could not resolve "${path}" to an existing file (tried the absolute form and the workspace root).`;
  }
  if (isLocalisationPath(resolved)) {
    return LOCALISATION_POINTER;
  }
  const uri = pathToFileURL(resolved).href;
  const hover = await session.request(
    'textDocument/hover',
    {
      textDocument: { uri },
      position: { line: line - 1, character },
    },
    'Hover',
  );
  const contents = hover?.contents;
  const value = contents && typeof contents === 'object' && 'value' in contents
    ? String(contents.value ?? '')
    : '';
  if (!value.trim()) {
    return `No hover information at ${formatFileReference(uri, root)}:${line}:${character}.`;
  }
  return capText(collapseWhitespace(value), 2048);
}

export async function runDiagnostics(session, input) {
  const files = (Array.isArray(input?.files) ? input.files : [])
    .map((file) => (typeof file === 'string' ? file.trim() : ''))
    .filter(Boolean);
  if (files.length === 0 && input?.files !== undefined) {
    return 'Pass at least one non-empty logical path in files, or omit files to diagnose the whole workspace.';
  }
  const limit = typeof input?.limit === 'number' && Number.isFinite(input.limit)
    ? Math.min(Math.max(1, Math.trunc(input.limit)), MAX_DIAGNOSTIC_FILES)
    : DEFAULT_DIAGNOSTIC_FILES;
  const offset = Math.max(0, Math.trunc(Number(input?.offset ?? 0)));
  const result = await session.request(
    'pdc/workspaceDiagnostics',
    { offset, limit, parser: 'script', ...(files.length > 0 ? { files } : {}) },
    'Workspace diagnostics',
  );
  const items = Array.isArray(result?.items) ? result.items : [];
  const total = Number(result?.total ?? items.length);
  if (items.length === 0) {
    return `No script files to diagnose (total ${total}).`;
  }
  const sections = [];
  for (const item of items) {
    const logicalPath = String(item?.logicalPath ?? '<unknown>');
    const diagnostics = Array.isArray(item?.diagnostics) ? item.diagnostics : [];
    const { items: shown, omitted } = capList(diagnostics, 30);
    const lines = shown.map((diagnostic) => describeDiagnostic(diagnostic));
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

export async function runReferences(session, root, input) {
  const path = (input?.path ?? '').trim();
  const line = Math.trunc(Number(input?.line));
  const character = Math.max(0, Math.trunc(Number(input?.character ?? 0)));
  if (!path) {
    return 'Pass the file path (absolute, file: URI, or workspace-relative).';
  }
  if (isLocalisationPath(path)) {
    return LOCALISATION_POINTER;
  }
  if (!Number.isInteger(line) || line < 1) {
    return 'Lines are 1-based: pass line >= 1.';
  }
  const resolved = resolveFilePath(path, root);
  if (!resolved) {
    return `Could not resolve "${path}" to an existing file (tried the absolute form and the workspace root).`;
  }
  if (isLocalisationPath(resolved)) {
    return LOCALISATION_POINTER;
  }
  const uri = pathToFileURL(resolved).href;
  const references = await session.request(
    'textDocument/references',
    {
      textDocument: { uri },
      position: { line: line - 1, character },
      context: { includeDeclaration: true },
    },
    'References',
  );
  const entries = Array.isArray(references) ? references : [];
  if (entries.length === 0) {
    return `No references at ${formatFileReference(uri, root)}:${line}:${character}. `
      + 'The position must point at a symbol in a file the workspace has indexed; '
      + 'for name-driven lookup use paradoxcode_symbol_references.';
  }
  const { items, omitted } = capList(entries, MAX_SYMBOL_SEARCH_LIMIT);
  const lines = items.map((reference) => {
    const referenceUri = typeof reference?.uri === 'string' ? reference.uri : '';
    const referenceLine = Number(reference?.range?.start?.line);
    const suffix = Number.isInteger(referenceLine) && referenceLine >= 0 ? `:${referenceLine + 1}` : '';
    return `- ${formatFileReference(referenceUri, root)}${suffix}`;
  });
  if (omitted > 0) {
    lines.push(`… (+${omitted} more references)`);
  }
  return capText(`References (${entries.length}):\n${lines.join('\n')}`, 8192);
}

function describeSymbolLocation(location, root) {
  const path = trimToUndefined(String(location?.path ?? ''))
    ?? (typeof location?.uri === 'string' ? formatFileReference(location.uri, root) : '?');
  const line = Number(location?.line);
  return Number.isInteger(line) && line >= 1 ? `${path}:${line}` : path;
}

export async function runSymbolReferences(session, root, input) {
  const name = trimToUndefined(input?.name);
  const kind = trimToUndefined(input?.kind);
  if (!name) {
    return 'Pass the symbol name (for example an event id or a scripted effect name).';
  }
  const limit = searchLimit(input?.limit, MAX_SYMBOL_SEARCH_LIMIT);
  const result = await session.request(
    'pdc/symbolReferences',
    { name, kind, limit },
    'Symbol references',
  );
  if (result?.matched !== true) {
    const reason = trimToUndefined(String(result?.reason ?? ''))
      ?? 'the name does not resolve to a unique active definition.';
    const candidates = (Array.isArray(result?.candidates) ? result.candidates : [])
      .map((candidate) => `${String(candidate?.name ?? name)} (${String(candidate?.kind ?? '?')})`);
    const candidatePart = candidates.length > 0
      ? `\nCandidates:\n${candidates.map((entry) => `- ${entry}`).join('\n')}`
      : '';
    return `No unique symbol for "${name}": ${reason}${candidatePart}`;
  }
  const definition = result.symbol?.definition;
  const header = `${String(result.symbol?.name ?? name)} (${String(result.symbol?.kind ?? '?')})`
    + (definition ? ` defined at ${describeSymbolLocation(definition, root)}` : '');
  const references = Array.isArray(result?.references) ? result.references : [];
  const total = Number(result?.total ?? references.length);
  const lines = references.map((reference) => `- ${describeSymbolLocation(reference, root)}`);
  if (result?.truncated === true) {
    lines.push(`… (truncated at ${limit} of ${total} references; raise the limit up to ${MAX_SYMBOL_SEARCH_LIMIT})`);
  }
  if (references.length === 0) {
    return `${header}\nNo references found.`;
  }
  return capText(`${header}\nReferences (${total}):\n${lines.join('\n')}`, 8192);
}

export async function runRules(session, input) {
  const context = trimToUndefined(input?.context);
  const key = trimToUndefined(input?.key);
  const scope = trimToUndefined(input?.scope);
  if (!context && !key && !scope) {
    return 'Pass at least one of context, key, or scope (for example context "trigger", key "add_army_tradition").';
  }
  const limit = searchLimit(input?.limit);
  const result = await session.request(
    'pdc/ruleSearch',
    { context, key, scope, limit },
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
    const allowed = Array.isArray(rule?.allowedScopes) && rule.allowedScopes.length > 0
      ? rule.allowedScopes.map((entry) => String(entry)).join('|')
      : 'any';
    const flags = [
      rule?.deprecated === true ? 'deprecated' : undefined,
      rule?.required === true ? 'required' : undefined,
    ].filter(Boolean);
    const documentation = trimToUndefined(String(rule?.documentation ?? ''));
    const docPart = documentation ? ` — ${collapseWhitespace(documentation)}` : '';
    const flagPart = flags.length > 0 ? ` (${flags.join(', ')})` : '';
    return `${String(rule?.key ?? '<unknown>')} [${String(rule?.context ?? '?')}]`
      + ` shape=${String(rule?.shape ?? '?')} scopes=${allowed}${flagPart}${docPart}`;
  });
  if (result?.truncated === true) {
    lines.push(`… (truncated at ${limit} rules; narrow the filters or raise the limit up to ${MAX_SEARCH_LIMIT})`);
  }
  return capText(`Rules matching ${filters}:\n${lines.join('\n')}`, 6144);
}

export async function runValidateText(session, input) {
  const files = (Array.isArray(input?.files) ? input.files : [])
    .filter((file) => typeof file?.path === 'string' && typeof file?.text === 'string')
    .slice(0, 16);
  if (files.length === 0) {
    return 'No files to validate: pass between 1 and 16 {path, text} entries.';
  }
  const results = await session.request('pdc/textDiagnostics', { files }, 'Validation');
  const sections = [];
  for (const result of Array.isArray(results) ? results : []) {
    const diagnostics = Array.isArray(result?.diagnostics) ? result.diagnostics : [];
    const { items, omitted } = capList(diagnostics, 30);
    const lines = items.map((diagnostic) => describeDiagnostic(diagnostic));
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

function describeLocalisationHit(hit) {
  const value = trimToUndefined(String(hit?.value ?? '')) ?? '<no preview>';
  const language = trimToUndefined(String(hit?.language ?? '')) ?? 'unknown language';
  const file = trimToUndefined(String(hit?.file ?? '')) ?? 'unknown file';
  return `${String(hit?.key ?? '<unknown>')} = "${collapseWhitespace(value)}" (${language}) — ${file}`;
}

async function localisationSearch(session, params) {
  return session.request('pdc/localisationSearch', params, 'Localisation search');
}

export async function runLocGet(session, input) {
  const key = trimToUndefined(input?.key);
  if (!key) {
    return 'Pass the exact localisation key (matching is case-insensitive).';
  }
  const result = await localisationSearch(session, { key, keyMatch: 'exact', limit: 1 });
  const hits = Array.isArray(result?.hits) ? result.hits : [];
  if (hits.length === 0) {
    return `No localisation key "${key}" is defined. Use paradoxcode_loc_list with the key's `
      + 'family prefix to see which neighbouring keys exist.';
  }
  return capText(describeLocalisationHit(hits[0]), 2048);
}

export async function runLocSearch(session, input) {
  const text = trimToUndefined(input?.text);
  if (!text) {
    return 'Pass the text filter (matched case-insensitively against displayed values).';
  }
  const limit = searchLimit(input?.limit);
  const result = await localisationSearch(session, { text, limit });
  const hits = Array.isArray(result?.hits) ? result.hits : [];
  if (hits.length === 0) {
    return 'No localisation entries mention that text across the project, dependencies, and Vanilla.';
  }
  const lines = hits.map(describeLocalisationHit);
  if (result?.truncated === true) {
    lines.push(`… (truncated at ${limit} entries; narrow the text or raise the limit up to ${MAX_SEARCH_LIMIT})`);
  }
  return capText(`Localisation matching "${text}":\n${lines.join('\n')}`, 4096);
}

export async function runLocList(session, input) {
  const keyPrefix = trimToUndefined(input?.keyPrefix);
  if (!keyPrefix) {
    return 'Pass the key prefix (for example "my_event.1." to enumerate one event\'s key family).';
  }
  const limit = searchLimit(input?.limit);
  const result = await localisationSearch(session, { key: keyPrefix, keyMatch: 'prefix', limit });
  const hits = Array.isArray(result?.hits) ? result.hits : [];
  if (hits.length === 0) {
    return `No localisation keys start with "${keyPrefix}".`;
  }
  const lines = hits.map(describeLocalisationHit);
  if (result?.truncated === true) {
    lines.push(`… (truncated at ${limit} entries; extend the prefix or raise the limit up to ${MAX_SEARCH_LIMIT})`);
  }
  return capText(`Keys under "${keyPrefix}" (${hits.length}${result?.truncated === true ? '+' : ''}):\n${lines.join('\n')}`, 4096);
}

/**
 * Dispatches one MCP `tools/call`. Returns the tool's text result; thrown
 * errors propagate to the protocol layer, which reports them as `isError`
 * content the model can react to — mirroring the extension's register.ts.
 */
export async function callTool(name, arguments_, session, root) {
  switch (name) {
    case 'paradoxcode_workspace':
      return runWorkspace(session, root);
    case 'paradoxcode_search':
      return runSearch(session, root, arguments_);
    case 'paradoxcode_context':
      return runContext(session, root, arguments_);
    case 'paradoxcode_diagnostics':
      return runDiagnostics(session, arguments_);
    case 'paradoxcode_references':
      return runReferences(session, root, arguments_);
    case 'paradoxcode_symbol_references':
      return runSymbolReferences(session, root, arguments_);
    case 'paradoxcode_rules':
      return runRules(session, arguments_);
    case 'paradoxcode_validate_text':
      return runValidateText(session, arguments_);
    case 'paradoxcode_loc_get':
      return runLocGet(session, arguments_);
    case 'paradoxcode_loc_search':
      return runLocSearch(session, arguments_);
    case 'paradoxcode_loc_list':
      return runLocList(session, arguments_);
    default:
      throw Object.assign(new Error(`Unknown tool: ${name}`), { code: -32602 });
  }
}
