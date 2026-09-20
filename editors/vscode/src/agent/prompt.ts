/**
 * The @paradox participant's domain system prompt and the tool specs mirrored from
 * package.json's languageModelTools contributions. The specs must stay in sync with the
 * manifest; the host suite asserts the name sets match.
 */

export const AGENT_TOOL_SPECS: { name: string; description: string; inputSchema: object }[] = [
    {
        name: 'paradoxcode-validate-text',
        description:
            'Validate Paradox EU4 script text (1-16 files) against the embedded EU4 rules and the indexed workspace/Vanilla data without writing anything to disk. Pass each file as {path, text} where path is the mod-relative logical path. Returns per-file diagnostics with stable codes, 1-based line numbers, and messages.',
        inputSchema: {
            type: 'object',
            required: ['files'],
            properties: {
                files: {
                    type: 'array',
                    minItems: 1,
                    maxItems: 16,
                    items: {
                        type: 'object',
                        required: ['path', 'text'],
                        properties: {
                            path: { type: 'string' },
                            text: { type: 'string' },
                        },
                    },
                },
            },
        },
    },
    {
        name: 'paradoxcode-search-symbols',
        description:
            'Search the indexed EU4 symbol table (project mod, dependency mods, and Vanilla) for definitions such as events, decisions, tags, missions, or scripted triggers/effects by name substring.',
        inputSchema: {
            type: 'object',
            required: ['query'],
            properties: {
                query: { type: 'string' },
                limit: { type: 'number' },
            },
        },
    },
    {
        name: 'paradoxcode-search-rules',
        description:
            'Search the embedded first-party EU4 semantic rule database. Case-insensitive filters (at least one required): context (exact or prefix, e.g. trigger, effect, type:event), key (substring of the rule key), scope (substring of an allowed scope; rules valid in any scope match every scope query).',
        inputSchema: {
            type: 'object',
            properties: {
                context: { type: 'string' },
                key: { type: 'string' },
                scope: { type: 'string' },
                limit: { type: 'number' },
            },
        },
    },
    {
        name: 'paradoxcode-search-localisation',
        description:
            'Search indexed EU4 localisation entries (project mod, dependencies, and Vanilla) by key substring and/or displayed-value substring, both case-insensitive. Returns the winning definition per key with value, language, and file.',
        inputSchema: {
            type: 'object',
            properties: {
                key: { type: 'string' },
                text: { type: 'string' },
                limit: { type: 'number' },
            },
        },
    },
    {
        name: 'paradoxcode-hover-info',
        description:
            'Look up the rule-driven hover explanation at one position of an indexed EU4 script or localisation file. Input uses a 1-based line and 0-based UTF-16 character column.',
        inputSchema: {
            type: 'object',
            required: ['path', 'line'],
            properties: {
                path: { type: 'string' },
                line: { type: 'number' },
                character: { type: 'number' },
            },
        },
    },
];

/** The system prompt for the deep participant loop. Written in English (model-facing);
 * the prompt itself instructs the model to answer in the user's language. */
export function buildSystemPrompt(): string {
    return [
        'You are ParadoxCode\'s EU4 modding assistant running inside VS Code. You help write Europa Universalis IV mods in Paradox script.',
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
        'Answer in the user\'s language. Prefer checking with the tools over reciting from memory; keep answers focused and actionable.',
    ].join('\n');
}
