/**
 * The @paradox participant's domain system prompt and the tool specs mirrored from
 * package.json's languageModelTools contributions. The specs must stay in sync with
 * the manifest; the host suite asserts the name sets match.
 *
 * The tool surface is split into a script zone and a localisation zone that never cross:
 * script tools reject localisation paths with a pointer into the loc zone, and loc tools
 * only ever answer with localisation definitions.
 */

export const AGENT_TOOL_SPECS: { name: string; description: string; inputSchema: object }[] = [
    {
        name: 'paradoxcode_workspace',
        description:
            'Summarise the EU4 workspace: game identity, embedded rule hash, source roots (Vanilla, dependency mods, project), file counts by zone, and the last scan. Call this first when orienting in a workspace.',
        inputSchema: {
            type: 'object',
            properties: {},
        },
    },
    {
        name: 'paradoxcode_search',
        description:
            'Search script-zone symbols (events, decisions, tags, missions, scripted triggers/effects) by name substring across the project mod, dependency mods, and Vanilla. Localisation keys are excluded. Results are limited to 100 symbols.',
        inputSchema: {
            type: 'object',
            required: ['query'],
            properties: {
                query: {
                    type: 'string',
                },
                limit: {
                    type: 'number',
                    minimum: 1,
                    maximum: 100,
                    default: 20,
                },
            },
        },
    },
    {
        name: 'paradoxcode_context',
        description:
            'Explain the rule-driven semantics at one position of an EU4 script file: what the key accepts, which scopes allow it, and the localisation preview when it resolves one. Script files only — for localisation keys use the loc tools.',
        inputSchema: {
            type: 'object',
            required: ['path', 'line'],
            properties: {
                path: {
                    type: 'string',
                    description: 'Absolute path, file: URI, or workspace-relative path.',
                },
                line: {
                    type: 'number',
                    description: '1-based line.',
                },
                character: {
                    type: 'number',
                    description: '0-based UTF-16 column.',
                },
            },
        },
    },
    {
        name: 'paradoxcode_diagnostics',
        description:
            'Diagnostics for the project script files on disk (Vanilla and dependencies excluded). Pass files (logical paths) to focus on specific files, or omit to page through the whole workspace (16 files per page, up to 128). Localisation files are excluded.',
        inputSchema: {
            type: 'object',
            properties: {
                files: {
                    type: 'array',
                    items: {
                        type: 'string',
                    },
                    description: 'Logical paths to focus on.',
                },
                limit: {
                    type: 'number',
                    minimum: 1,
                    maximum: 128,
                    default: 16,
                },
                offset: {
                    type: 'number',
                    minimum: 0,
                    default: 0,
                },
            },
        },
    },
    {
        name: 'paradoxcode_references',
        description:
            'Find references to the symbol at one position of a script file (position-based; works best in files the workspace has indexed). For lookup by name alone use paradoxcode_symbol_references.',
        inputSchema: {
            type: 'object',
            required: ['path', 'line'],
            properties: {
                path: {
                    type: 'string',
                },
                line: {
                    type: 'number',
                    description: '1-based line.',
                },
                character: {
                    type: 'number',
                    description: '0-based UTF-16 column.',
                },
            },
        },
    },
    {
        name: 'paradoxcode_symbol_references',
        description:
            'Find the definition and references of a script-zone symbol addressed by name, without a cursor position. Optional kind (e.g. event, scripted_effect) disambiguates names defined under several kinds; an ambiguous answer lists the candidates. Results are limited to 100 references.',
        inputSchema: {
            type: 'object',
            required: ['name'],
            properties: {
                name: {
                    type: 'string',
                },
                kind: {
                    type: 'string',
                },
                limit: {
                    type: 'number',
                    minimum: 1,
                    maximum: 100,
                    default: 20,
                },
            },
        },
    },
    {
        name: 'paradoxcode_rules',
        description:
            'Search the embedded first-party EU4 semantic rule database. Case-insensitive filters (at least one required): context (exact or prefix, e.g. trigger, effect, type:event), key (substring of the rule key), scope (substring of an allowed scope; rules valid in any scope match every scope query). Results are limited to 50 rules.',
        inputSchema: {
            type: 'object',
            properties: {
                context: {
                    type: 'string',
                },
                key: {
                    type: 'string',
                },
                scope: {
                    type: 'string',
                },
                limit: {
                    type: 'number',
                    minimum: 1,
                    maximum: 50,
                    default: 20,
                },
            },
        },
    },
    {
        name: 'paradoxcode_validate_text',
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
                            path: {
                                type: 'string',
                            },
                            text: {
                                type: 'string',
                            },
                        },
                    },
                },
            },
        },
    },
    {
        name: 'paradoxcode_loc_get',
        description:
            'Address one localisation key exactly (case-insensitive): returns the winning definition with value, language, and file, or nothing. Never truncated. Localisation zone only.',
        inputSchema: {
            type: 'object',
            required: ['key'],
            properties: {
                key: {
                    type: 'string',
                },
            },
        },
    },
    {
        name: 'paradoxcode_loc_search',
        description:
            'Discover localisation entries by displayed-value substring, case-insensitive, across the project, dependencies, and Vanilla. Results are limited to 50 entries.',
        inputSchema: {
            type: 'object',
            required: ['text'],
            properties: {
                text: {
                    type: 'string',
                },
                limit: {
                    type: 'number',
                    minimum: 1,
                    maximum: 50,
                    default: 20,
                },
            },
        },
    },
    {
        name: 'paradoxcode_loc_list',
        description:
            'Enumerate the localisation key family under an anchored key prefix (for example "flavor_kni.1." lists .t, .d, and option keys of that event). Results are limited to 50 entries.',
        inputSchema: {
            type: 'object',
            required: ['keyPrefix'],
            properties: {
                keyPrefix: {
                    type: 'string',
                },
                limit: {
                    type: 'number',
                    minimum: 1,
                    maximum: 50,
                    default: 20,
                },
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
        'Answer in the user\'s language. Prefer checking with the tools over reciting from memory; keep answers focused and actionable.',
    ].join('\n');
}
