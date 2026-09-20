/**
 * The stdio MCP server's tool layer: a dependency-free twin of the extension's
 * `src/agent/tools.ts`. Input validation, request shaping, and response
 * formatting stay behaviour-identical to the VS Code surface so the two faces
 * cannot drift; only the vscode imports (client acquisition, Uri handling) are
 * replaced by the plain-node equivalents fed by `mcpBoot.mjs`.
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
  'Tool discipline (hard rules):',
  '- Before writing or referencing a game concept, check it first: search-symbols for existing definitions (events, decisions, tags, missions), search-rules for what a key accepts and in which scopes.',
  '- After drafting or editing any file, ALWAYS call validate-text on the new content before considering the work done. Fix what it reports and re-validate.',
  '- When validation reports UnknownLocalisationKey, resolve it by searching localisation or by adding the missing key and file.',
  '- When unsure which scope a block is in, look the key up with search-rules and read its allowed scopes instead of guessing.',
  '',
  'Validation diagnostic codes are authoritative: UnknownKey (key not valid in this context), InvalidValue (value does not satisfy the rule), WrongScope (key exists but not in this scope), UnknownLocalisationKey (missing localisation key).',
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

const SEVERITY_LABELS = {
  1: 'error',
  2: 'warning',
  3: 'info',
  4: 'hint',
};

const SYMBOL_KIND_LABELS = {
  1: 'file', 2: 'module', 3: 'namespace', 4: 'package', 5: 'class', 6: 'method',
  7: 'property', 8: 'field', 9: 'constructor', 10: 'enum', 11: 'interface', 12: 'function',
  13: 'variable', 14: 'constant', 15: 'string', 16: 'number', 17: 'boolean', 18: 'array',
  19: 'object', 20: 'key', 21: 'null', 22: 'enum member', 23: 'struct', 24: 'event',
  25: 'operator', 26: 'type parameter',
};

function searchLimit(limit) {
  if (typeof limit !== 'number' || !Number.isFinite(limit)) {
    return DEFAULT_SEARCH_LIMIT;
  }
  return Math.min(Math.max(1, Math.trunc(limit)), MAX_SEARCH_LIMIT);
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

export async function runSearchSymbols(session, root, input) {
  const query = (input?.query ?? '').trim();
  if (query.length < 2) {
    return 'Query too short: pass at least 2 characters of a symbol name.';
  }
  const limit = searchLimit(input?.limit);
  const symbols = await session.request(
    'workspace/symbol',
    { query },
    'Symbol search',
  );
  const entries = Array.isArray(symbols) ? symbols : [];
  if (entries.length === 0) {
    return `No indexed symbols match "${query}" (project, dependencies, and Vanilla are searched).`;
  }
  const { items, omitted } = capList(entries, limit);
  const lines = items.map((symbol) => {
    const name = String(symbol?.name ?? '<unnamed>');
    const kind = SYMBOL_KIND_LABELS[Number(symbol?.kind)] ?? `kind ${String(symbol?.kind)}`;
    const uri = typeof symbol?.location?.uri === 'string' ? symbol.location.uri : '';
    const line = Number(symbol?.location?.range?.start?.line);
    const suffix = Number.isInteger(line) && line >= 0 ? `:${line + 1}` : '';
    return `${name} (${kind}) — ${formatFileReference(uri, root)}${suffix}`;
  });
  if (omitted > 0) {
    lines.push(`… (+${omitted} more symbols; refine the query or raise the limit up to ${MAX_SEARCH_LIMIT})`);
  }
  return capText(`Symbols matching "${query}":\n${lines.join('\n')}`, 4096);
}

export async function runSearchRules(session, input) {
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

export async function runSearchLocalisation(session, input) {
  const key = trimToUndefined(input?.key);
  const text = trimToUndefined(input?.text);
  if (!key && !text) {
    return 'Pass at least one of key or text (key matches localisation key names, text matches displayed values).';
  }
  const limit = searchLimit(input?.limit);
  const result = await session.request(
    'pdc/localisationSearch',
    { key, text, limit },
    'Localisation search',
  );
  const hits = Array.isArray(result?.hits) ? result.hits : [];
  if (hits.length === 0) {
    return 'No localisation entries match. Keys are matched case-insensitively as substrings across the project, dependencies, and Vanilla.';
  }
  const lines = hits.map((hit) => {
    const value = trimToUndefined(String(hit?.value ?? '')) ?? '<no preview>';
    const language = trimToUndefined(String(hit?.language ?? '')) ?? 'unknown language';
    const file = trimToUndefined(String(hit?.file ?? '')) ?? 'unknown file';
    return `${String(hit?.key ?? '<unknown>')} = "${collapseWhitespace(value)}" (${language}) — ${file}`;
  });
  if (result?.truncated === true) {
    lines.push(`… (truncated at ${limit} entries; narrow the query or raise the limit up to ${MAX_SEARCH_LIMIT})`);
  }
  return capText(lines.join('\n'), 4096);
}

export async function runHoverInfo(session, root, input) {
  const path = (input?.path ?? '').trim();
  const line = Math.trunc(Number(input?.line));
  const character = Math.max(0, Math.trunc(Number(input?.character ?? 0)));
  if (!path) {
    return 'Pass the file path (absolute, file: URI, or workspace-relative).';
  }
  if (!Number.isInteger(line) || line < 1) {
    return 'Lines are 1-based: pass line >= 1.';
  }
  const resolved = resolveFilePath(path, root);
  if (!resolved) {
    return `Could not resolve "${path}" to an existing file (tried the absolute form and the workspace root).`;
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

/**
 * Dispatches one MCP `tools/call`. Returns the tool's text result; thrown
 * errors propagate to the protocol layer, which reports them as `isError`
 * content the model can react to — mirroring the extension's register.ts.
 */
export async function callTool(name, arguments_, session, root) {
  switch (name) {
    case 'paradoxcode-validate-text':
      return runValidateText(session, arguments_);
    case 'paradoxcode-search-symbols':
      return runSearchSymbols(session, root, arguments_);
    case 'paradoxcode-search-rules':
      return runSearchRules(session, arguments_);
    case 'paradoxcode-search-localisation':
      return runSearchLocalisation(session, arguments_);
    case 'paradoxcode-hover-info':
      return runHoverInfo(session, root, arguments_);
    default:
      throw Object.assign(new Error(`Unknown tool: ${name}`), { code: -32602 });
  }
}
