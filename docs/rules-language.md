# Rules language specification

This document is the normative specification of the ParadoxCode rule source
language: the JSON-based language rule authors write and `rulec` compiles into
the runtime rule IR. It is written for both rule authors and implementers.

`docs/rules-redesign.md` is the design background for this language; where the
two documents appear to disagree, this document specifies the language and the
design document explains why it is shaped that way. The key words MUST, MUST
NOT, SHOULD, and MAY are used in the RFC 2119 sense. Diagnostics named below
are `rulec` compile diagnostics for rule sources; they are distinct from the
script diagnostics catalogued in `docs/diagnostics.md`.

## 1. Top-level sections and directory layout

A rule source is a set of JSON files listed in `manifest.json` (which also
carries `game_id` and `target_game_version`). Each file MAY contain any subset
of seven top-level sections:

| Section | Contents |
|---|---|
| `files` | Path selection → parser / root schema |
| `schemas` | Structure: `fields` / `patterns` / `items` / `include` / `when` / `def` / parameters |
| `mixins` | Pure structural field bundles (expanded at compile time) |
| `types` | Symbol namespaces: `resolution` / `subtypes` / `open` / `builtin` / `impl` |
| `traits` | Capabilities + bindings + constraints (expanded at compile time) |
| `enums` | Enums, optionally with attribute columns (carry `on_actions` and the like) |
| `scopes` | Scope types / registers / links / compat |

The compiler merges the same-named section across all files into one namespace.
A name defined twice — a duplicate schema, mixin, type, enum, trait, files
entry, or scope declaration — is an error; there is no shadowing or override
between files.

There is **no top-level `intrinsics` section**: control-flow primitives are
field attributes (§9), so no key name is globally significant. There is **no
separate `tables` section**: an enum with columns is one concept (§6).

The directory layout is organised by game domain; the shared parts live in
`core/`:

```
rules/eu4/
  manifest.json          # lists every file; game_id, target_game_version
  game.json              # non-language parts: install, filesystem scan, hover_cards, fallback_keys
  core/
    scopes.json          # scopes section
    control-flow.json    # control-flow mixins (if chains, AND/OR/NOT, random_list, …)
    traits.json          # built-in traits
    trigger.json effect.json modifier.json
  events.json            # files + schemas + types + enums(on_actions), one domain per file
  decisions.json  missions.json  history/…  common/…  interface/…  map/…
```

`game.json` is configuration, not language: it does not participate in the
semantics specified here.

## 2. Type expressions

Keys and values share one string mini-syntax. The compiler parses an
expression into a single unified matcher form that replaces the old
`KeyMatcher`/`ValueMatcher` split. A parse error is reported as **source file +
JSON pointer + column within the expression** (for example: `events.json`,
`/schemas/event_body/fields/picture/value`, column 12).

### 2.1 Grammar

```ebnf
expr      = alt { "|" alt } ;
alt       = prim [ range ] | ctor "<" arg ">" | "path" [ "<" name ">" ] | literal | param ;
prim      = "scalar" | "int" | "float" | "bool" | "date" | "loc" | "link" | "opaque" ;
range     = "[" [ number ] ".." [ number ] "]" ;
ctor      = "ref" | "def" | "enum" | "scope" | "quoted" ;
arg       = name [ "." name ]            (* ref<event.country>：类型.subtype *)
          | "impl" name                  (* ref<impl ModifierSource> *)
          | param ;
literal   = "'" { char | "{" expr "}" } "'" ;   (* 无洞即常量；有洞即模板 *)
param     = "$" name [ "." name ] ;       (* 参数化 schema 的形参；$key.<列> 见 2.6 *)
name      = ident ;
```

The grammar block above is quoted verbatim from the design document; the three
comments gloss `arg = name ["." name]` as *type.subtype* (`ref<event.country>`),
`"impl" name` as *trait reference* (`ref<impl ModifierSource>`), and
`param` as *a formal parameter of a parameterised schema; `$key.<column>` is
described in §6*. The remaining lexical rules are normative:

- Whitespace outside literals is insignificant between tokens and at the ends
  of the expression; `ref<impl ModifierSource>` is written exactly as shown,
  space included. Whitespace inside literal text is literal text.
- An identifier `name` matches `[A-Za-z_][A-Za-z0-9_]*`.
- A number (range bound) matches `-?[0-9]+(\.[0-9]+)?`: a plain decimal digit
  sequence with an optional `-` and optional fractional part, and no exponent
  form. Both bounds of an `int[..]` range MUST be whole numbers; a violation is
  reported at the offending bound.
- A range `[min..max]` MAY omit either end (`[0..]`, `[..5]`, `[..]`). A range
  is only allowed after `int` or `float`; any other primitive with a range is a
  parse error. A range whose lower bound exceeds its upper bound is a parse
  error, reported at the `[`.
- A literal `'...'` supports the backslash escapes `\'`, `\{`, `\}`, and `\\`;
  any other backslash escape is a parse error. `{` opens a hole, which MUST be
  a complete expression followed by `}`. Every other character, including `}`,
  is literal text. Literals nest: a hole may contain another quoted literal. A
  literal with no hole is a constant; a literal with holes is a template.
- The `arg` of a constructor (between `<` and `>`) is `seg ["." seg]` or
  `"impl" Ident`, where `seg = Ident | "$" Ident ["." Ident]`. `$name.column`
  reads a column of a parameter (§6).
- A bare `param` (`$name[.column]`) is a legal alternative on its own.
- Union has the lowest precedence and is tried in written order. An empty
  branch is a parse error.
- `path` takes an optional `<category>` (for example `path<gfx>`); the category
  is an identifier.
- A link key (a key of `scopes.links`) is **template text**: plain text with
  `{expression}` holes, without outer quotes, using the same escape rules as
  literals.

### 2.2 Expression semantics

| Expression | Meaning | Replaces the old matcher |
|---|---|---|
| `scalar` | Any scalar | `AnyScalar` |
| `'yes'` | Constant | `Exact` |
| `'monthly_{ref<government_mechanic_power>}'` | Template | `Template`, `TypedPrefix` (`'trigger_value:{ref<scripted_trigger>}'`) |
| `int[1..10]` `float[0..]` `bool` `date` | Scalar types; bounds are always numbers | `Int`/`Float`/`Bool`/`Date` |
| `loc` | Localisation key | `Localisation` |
| `path` `path<gfx>` | File path; `<…>` is a path category | `Filepath`/`TexturePath` |
| `ref<event>` `ref<event.country>` `ref<impl ModifierSource>` | Symbol reference; optionally qualified by subtype or trait | `Type`, `Dynamic`, `lexicon.member_kind_aliases` |
| `def<country_flag>` | **Defines** a symbol at this position | `DynamicSet`, `profile.value_definitions` |
| `enum<country_tags>` | Enum member | `Enum` |
| `scope<country>` `scope<any>` | Scope expression (register, link, tag, …) | `Scope` |
| `link` | Key position only: any scope link / register / prefix link (§8) | the per-row hand-written link lines in trigger/effect |
| `quoted<trigger>` | Quoted string whose content is parsed as a schema | `RuleShape::QuotedScript`, `quoted_script_definition_keys` |
| `opaque` | Unchecked text | `Opaque` |
| `a\|b` | Union, tried in written order | multi-row alternatives |

Semantic conventions:

- Keys and symbol names are case-insensitive everywhere in the language.
- All branches of a union MUST have the same shape. Scalar-shaped branches and
  quoted-script (`quoted<…>`) branches are distinct shapes and MUST NOT be
  mixed. Shape variation of a block is expressed with field-array overloads
  (§3), never with a union.

## 3. Schemas

A schema describes one block:

```jsonc
"schemas": {
  "event_body": {
    "include": [],                                   // mixin names, expanded at compile time (§7)
    "fields": {                                      // exact keys, hash lookup
      "id":      { "value": "scalar", "card": "1" },
      "title":   { "value": "loc", "card": "0..1" },
      "picture": { "value": "ref<sprite>|enum<dlc_event_pictures>", "card": "0..*" },
      "desc":    [ { "value": "loc", "card": "0..*" },               // array = shape overloads
                   { "body": "conditional_desc", "card": "0..*" } ],
      "option":  { "body": "event_option", "card": "0..*" }
    },
    "patterns": [                                    // non-exact keys, tried in order
      { "key": "ref<scripted_trigger>", "value": "scalar", "card": "0..1" }
    ],
    "items": null,                                   // type of bare values in the block (list block)
    "open": false                                    // are undeclared keys allowed; default false
  }
}
```

A schema combines exact `fields`, ordered `patterns`, `items` for list blocks,
`include` for mixin expansion, `when`/`unless`-gated fields and `def` positions
(§4–§5), and optionally formal parameters in its name.

### 3.1 Field specifications

A field specification is the value of a `fields` entry or an element of
`patterns`:

| Key | Meaning | Default |
|---|---|---|
| `key` | Patterns only: the type expression for the key | — |
| `value` / `body` / `list` / `map` | Exactly one of four: a scalar type expression / a block schema name (or `"self"`) / a bare-value list block whose value is the element type expression / a homogeneous map block `{"key": expr, "value": expr}` or `{"key": expr, "body": schema}` | — |
| `card` | `"1"`, `"0..1"`, `"1..*"`, `"0..*"`, `"2..5"`; **mandatory** (D14) | — |
| `scope` | Scope effect, see §8 | none |
| `def` | Defines a symbol instance at this position, see §4 | none |
| `when` / `unless` | Subtype conditions, see §5 | none |
| `control` | Control-flow primitive, see §9 | none |
| `doc` / `severity` / `deprecated` | Documentation / diagnostic severity / deprecation | empty / `error` / `false` |

`card` is a **required** key and is rejected while parsing when missing. It has
no default because the corpus has no majority value — of the 8,463 legacy rule
rows, `1` is 50%, `0..1` is 35%, and the repeatable forms are 13% — and because
it is what drives the missing-required-key and repeated-key diagnostics; a
guessed default would decide both. `fmt` may expand the value, but the source
always spells it.

**`value` and `body` are strictly distinct.** `value` (and the `"value"` of a
`map`) is always a type expression; `body` is always a schema name. The two are
never guessed from each other.

**Schema-level shorthands.** A whole schema written as `{ "map": {...} }` is
equivalent to a schema with a single pattern; `{ "list": expr }` is equivalent
to a schema with only `items`.

### 3.2 Lookup and shape dispatch

Lookup proceeds in two steps. First `fields` is consulted by exact key and the
overloads are filtered by shape (scalar / block / quoted script). Only when no
exact field overload matches the shape does lookup fall through to `patterns`,
which are tried in written order. Overloads of the same shape match in written
order. The compiler checks for unreachable overloads: an overload fully covered
by an earlier one is an error (§10, check 3).

**`"body": "self"`** reuses the current schema. Scope-switch blocks (§8) and
control-flow blocks (§9) are written this way.

### 3.3 Parameterised schemas

A schema name may carry a formal parameter list; arguments are given at each
reference site. The compiler **monomorphises** every instantiation, so the
runtime carries no generics:

```jsonc
"on_action_body<S>": { "fields": {
  "events":        { "list": "ref<event.$S>", "card": "0..*" },
  "random_events": { "map": { "key": "int", "value": "ref<event.$S>|'0'" }, "card": "0..*" }
}}
```

Constraints, all checked at compile time:

- A formal parameter may appear only in an `arg` position of a type expression
  or as an actual argument of another parameterised schema.
- Only one level of parameters is allowed: an actual argument may only be a
  concrete name or a formal parameter of the enclosing parameterised schema.
- Each parameterised schema is limited to 64 instances; exceeding the limit is
  an error (§10, check 4).

## 4. Files and `def`

The `files` section replaces `catalog/file-categories.json` and every path
field in the type descriptions. A file is itself a schema, and symbol instances
are declared with `def` at chosen positions in schemas:

```jsonc
"files": {
  "events":           { "path": "events", "ext": "txt", "root": "events_file" },
  "scripted_effects": { "path": "common/scripted_effects", "ext": "txt", "root": "scripted_effects_file" },
  "event_pictures":   { "path": "interface", "ext": "gfx", "root": "gfx_file" },
  "localisation":     { "path": "localisation", "ext": "yml", "parser": "localisation" }
},
"schemas": {
  "events_file": { "fields": {
    "namespace":      { "value": "scalar", "card": "0..*" },
    "country_event":  { "def": { "type": "event.country",  "name": "field:id" },
                        "scope": { "set": { "root": "country",  "this": "country" } },
                        "body": "event_body", "card": "0..*" },
    "province_event": { "def": { "type": "event.province", "name": "field:id" },
                        "scope": { "set": { "root": "province", "this": "province" } },
                        "body": "event_body", "card": "0..*" }
  }},
  "decisions_file": { "fields": {
    "country_decisions": { "map": { "key": "def<decision>", "body": "decision_body" } }
  }},
  "scripted_effects_file": { "map": { "key": "def<scripted_effect>", "body": "effect" } }
}
```

### 4.1 `files` entries

| Field | Meaning |
|---|---|
| `path` | Directory prefix (the legacy `game/` prefix is dropped) |
| `ext` | File extension(s): one string, or an array when the category selects several (the legacy `extensions` list). Absent means every extension |
| `file` | Exact file name (replaces `path_file`) |
| `strict` | Do not recurse into subdirectories |
| `exclude` | Path prefixes this entry does not apply to (the legacy `path_exclude_prefixes`) |
| `parser` | `script` / `localisation` / `asset` / `syntax-only`; default `script` |
| `resolution` | `replace-by-path` / `merge` / `replace-directory`; default `merge` (D14: a default must cover the corpus majority) |
| `root` | Root schema name; or a field specification carrying `def`, meaning the whole file is one instance. **Required for the `script` parser** — without it the entry would validate nothing, and the no-guessing rule forbids that; `localisation` and `asset` entries MAY omit it |

A category whose structure the rules do not model still declares a root: an
open schema (`{ "open": true }`) says "any key, no rules" explicitly instead of
leaving the file unvalidated by omission.

### 4.2 The `def` specification

- **Shorthand.** `def<T>` in a value or key position; the instance name is the
  scalar itself.
- **Full form.** `{ "type": "T[.subtype]", "name": ... }`, where `name` is
  `"key"` (default) / `"field:<field-name>"` / `"file"` (file name without
  extension). Optional `"strip_prefix"` / `"strip_suffix"` strip affixes from
  the instance name (replacing `name_strip_prefix`/`name_strip_suffix` and
  `lexicon.member_name_suffixes`).
- **Subtype at `def`.** Writing a subtype in the def type (`event.country`)
  tags the instance with that subtype. This is the first of the two subtype
  sources (§5).

One type may have several `def` positions. After this section, the Types
subsystem carries only pure symbol semantics: *where instances are collected
and where references come from* is answered entirely by structure.

### 4.3 Old mechanism mapping

| Old mechanism | New expression |
|---|---|
| `skip_root_paths` | Spell out the outer wrapper block explicitly in the file schema |
| `type_key_filter` (excluding `potential`/`slot`…) | Exact fields take precedence over patterns; remaining keys fall through to the pattern carrying the `def` |
| `starts_with` | Template key at the def: `"key": "'mission_{scalar}'"` |
| `type_per_file` / `name_from_file` | The file root is the def: `"root": { "def": { "type": "T", "name": "file" }, "body": "..." }` |
| `root_entries` / `body_context` | Given directly by the `body` at the def position |
| `root-keys.json` / `root-scopes.json` / profile `root_scopes` | Keys and `scope.set` at the def position |
| `root_entry_specs.insertion` | Insertion form derived from the value type (`def<country_tag>` → quoted assignment, `enum<…>` → bare value, block → block) |
| `profile.symbols.definitions` (45) / `container_value_definitions` (3) | def positions (the latter as `"name": "field:name"`) |
| `profile.symbols.value_definitions` (19, `set_country_flag`) | Value position `"value": "def<country_flag>"` |
| `profile.symbols.references` (18) | `ref<…>` in value positions; there is **no global reference table** (decision D9) |
| `entry_wrapper_reroutes` heuristic | Removed; schemas describe the structure explicitly |

## 5. Conditions and subtypes

A subtype has two sources, both declared in `types`:

```jsonc
"types": { "event": {
  "subtypes": {
    "country":   {},                                         // source 1: assigned by the def position (events_file)
    "province":  {},
    "triggered": { "when": { "is_triggered_only": "'yes'" } } // source 2: decided by scalar fields of the instance body
  }
}}
```

A `when` predicate is a conjunction of *field → type expression* entries. A
value of `null` means "this field is absent" (replacing the `absent_field` of
`conditional_definitions`). Field specifications reference subtypes with
`when`/`unless`:

```jsonc
"event_body": { "fields": {
  "is_triggered_only":   { "value": "bool", "card": "0..1" },
  "mean_time_to_happen": { "body": "mtth", "card": "0..1", "unless": "triggered" },
  "trigger":             { "body": "trigger", "card": "0..*" }
}}
```

**Evaluation order** (this eliminates circular dependencies):

1. Determine the subtypes assigned by `def`.
2. Evaluate every `when` predicate against the **direct child scalar fields**
   of the instance body. A field read by any `when` predicate MUST NOT itself
   carry `when`/`unless`; this is checked at compile time (§10, check 5).
3. Validate the whole instance body against the resulting subtype set. Several
   subtypes may hold at once (`country` + `triggered`).

`ref<event.triggered>` accepts only instances that satisfy that subtype.

## 6. Enums with columns

An enum is either a shorthand array of member names or a `columns` + `rows`
table:

```jsonc
"enums": {
  "dlc_event_pictures": ["...", "..."],                        // shorthand: no columns
  "on_actions": {
    "columns": { "scope": "scope_type", "from": "scope_type?" },
    "rows": {
      "on_startup":                     { "scope": "country" },
      "on_province_religion_converted": { "scope": "province" }
    }
  }
},
"schemas": {
  "on_actions_file": { "map": { "key": "enum<on_actions>", "body": "on_action_body<$key.scope>" } }
}
```

`$key` is the implicit binding of the key matched in a `map` or `pattern`.
When the key is an enum with columns, its columns are readable as
`$key.<column>` (and as `$name.column` for a formal parameter, §2). The
compiler groups enum rows by column values and generates one pattern per group
(the key matcher covers that group's rows, the value is the monomorphised
schema such as `on_action_body<country>`); the runtime still sees only plain
patterns.

`on_actions` shrinks from 1,130 rows to one parameterised schema plus one enum,
and the country/province event distinction is restored. The old
`profile.enum_extra_members` merge directly into `rows`. `engine_set_flags`
moves to `builtin` (§7).

## 7. Types, mixins, and traits

### 7.1 Types

| Field | Meaning | Replaces |
|---|---|---|
| `resolution` | `replace`: definitions override by name at game load, and a same-name definition raises a shadow warning; `independent`: structurally duplicated keys (each unit file's `maneuver`) stand alone with no shadow warning. Navigation and rename behaviour are identical (both match today's `replace-by-symbol`) | `symbol-descriptors`, `RESOLVED_SYMBOL_KINDS` |
| `subtypes` | See §5 | the unenforced subtypes in records |
| `open` | Open world: any name may exist (`event_target`, `saved_name`, …), `ref` does not report undefined names | `open_world_value_kinds` |
| `builtin` | Members preset by the engine | `engine_set_flags`, the `hardcoded_*` classes |
| `impl` | Implemented traits and their arguments | see below |

`resolution` defaults to `independent`; only the 13 types in
`RESOLVED_SYMBOL_KINDS` write `replace`. `closed_dynamic_kinds` is simply the
default case — not `open` and with a `def<>` position — and needs no
declaration.

### 7.2 Mixins

A mixin is structural reuse. It belongs to the Schemas subsystem, is expanded
at compile time, and does not exist at runtime:

```jsonc
"mixins": {
  "gated":       { "fields": { "potential": { "body": "trigger", "card": "0..1" }, "allow": { "body": "trigger", "card": "0..1" } } },
  "ai_weighted": { "fields": { "ai_will_do": { "body": "modifier_rule", "card": "0..1" } } },
  "modifier_block": { "fields": { "modifier": { "body": "modifier", "card": "0..1" } } }
},
"schemas": { "decision_body": { "include": ["gated", "ai_weighted"], "fields": { "effect": { "body": "effect", "card": "0..*" } } } }
```

`include` lists mixin names to expand into the schema. An include conflict —
two mixins, or a mixin and the including schema itself, declaring the same
field key — is a compile error, unless the including schema's own field is
marked `"override": true`, in which case the schema's field wins. A conflict
between two mixins cannot be overridden and is always an error (§10, check 2).

### 7.3 Traits

A trait is a capability plus constraints. It belongs to the Types subsystem.
The first version's built-in set is fixed at four; adding a trait requires
changing Rust, because the runtime must understand its semantics:

```jsonc
"traits": {
  "Localised":      { "params": { "name": "$", "desc": null },
                      "bindings": { "name": { "loc": "{name}", "required": true }, "desc": { "loc": "{desc}" } } },
  "HasIcon":        { "params": { "sprite": "GFX_$" }, "bindings": { "icon": { "sprite": "{sprite}" } } },
  "ModifierSource": { "requires": { "include": "modifier_block" } },
  "Callable":       { "params": { "body": "schema" }, "capabilities": ["replacement", "condition", "dynamic_key"] }
},
"types": {
  "decision":        { "impl": { "Localised": { "name": "$_title", "desc": "$_desc" } } },
  "building":        { "impl": { "Localised": { "name": "building_$" }, "HasIcon": {}, "ModifierSource": {} } },
  "scripted_effect": { "impl": { "Callable": { "body": "effect" } }, "resolution": "replace" }
}
```

- In trait arguments, `$` is the **instance-name placeholder**. This is a
  different syntax from the type-expression `$param`: trait arguments are not
  type expressions.
- `impl` may be written inside a `subtype`, taking effect only for that subtype
  (replacing the `subtype`/`condition` of the old bindings).
- `Localised`/`HasIcon` replace `bindings/localisation.json` and
  `bindings/sprite.json`; `Callable` replaces `dynamic_definition` and
  `token_definitions` (its `$param$` arguments are handled uniformly by
  `Callable`); `ModifierSource` replaces the 22 `type:X → [modifier]` rows of
  profile `semantic_context_inheritance` and makes `ref<impl ModifierSource>`
  usable.

**Trait vs mixin.** Define something as a trait only if at least one holds:
(a) the engine/IDE handles it uniformly (hover, localisation checks, call
argument derivation); (b) it appears in type constraints. Otherwise use a
mixin.

**Anti-over-design constraints.** There is no trait inheritance. A type may
impl a given trait only once (type-level and all subtype-level impls together,
§10, check 8). Everything is expanded to flat data at compile time; the runtime
does no dynamic dispatch.

## 8. Scopes

```jsonc
"scopes": {
  "types":     ["country", "province", "trade_node", "unit", "monarch", "heir", "consort",
                "mercenary_company", "rebel_faction", "religion", "culture", "advisor", "leader",
                "trade_company", "global", "none"],
  "registers": { "root": {}, "this": {}, "prev": { "chain": true }, "from": { "chain": true } },
  "links": {
    "owner":      { "from": ["province", "unit"], "to": "country" },
    "controller": { "from": ["province"], "to": "country" },
    "capital":    { "from": ["country"], "to": "province" },
    "emperor":    { "from": ["any"], "to": "country" },
    "event_target:{ref<event_target>}":        { "from": ["any"], "to": "any" },
    "global_event_target:{ref<global_event_target>}": { "from": ["any"], "to": "any" }
  },
  "compat": [{ "actual": "trade_node", "expected": "province" }]
}
```

- `any` is a reserved word meaning *any scope*; it never appears in `types`.
- `registers.chain` means the register concatenates (`prev_prev`, `fromfrom`,
  …), replacing the hard-coded list in `dynamic_rules.rs` and the hand-written
  `prev_prev` entries in `scope_names`.
- Link keys may be templates (§2), replacing `dynamic_scope_prefixes`;
  `dynamic_value_prefixes` are likewise expressed as value templates
  (`'variable:{ref<variable>}'`).
- `scope_completions` is derived from registers + types and is no longer
  written by hand.

The scope effect on a rule is one field, and it has **only the object form**
(no string shorthand, eliminating the ambiguity of `"scope": "country"`):

```jsonc
"scope": { "in": ["country"], "push": "province", "set": { "root": "country", "this": "country", "from": "any" } }
```

`in` replaces `allowed_scopes` (default: any), `push` replaces `push_scope`,
and `set` replaces `replace_scope` together with `TypeRootScope`. All values of
`in`/`push`/`set`, and the keys of `set`, MUST be declared scope types, `any`
(for the values) or declared registers (for the keys) — §10, check 6.

Scope-switch blocks in trigger/effect schemas are no longer written row by
row; one pattern describes them uniformly:

```jsonc
"trigger": { "patterns": [ { "key": "link", "body": "self", "card": "0..*" } ] }
```

When a `link` key matches, the compiler already knows the link's `from`/`to`;
the runtime checks the current scope against `from` and pushes the `to` scope.
Scalar triggers that share a link's name (such as `controller = ROOT`) stay in
`fields` as ordinary exact fields: by the §3 lookup rules the scalar shape hits
the exact field and the block shape falls through to the `link` pattern. The
pure-link part of the existing 540 trigger/effect rows with `push_scope` is
folded into `scopes.links` by the conversion.

## 9. Control flow

Control-flow primitives no longer act globally by key name. They are `control`
attributes on field specifications, written in the mixins of
`core/control-flow.json` and included by the trigger/effect schemas:

```jsonc
"mixins": { "effect_control": { "fields": {
  "if":             { "body": "self", "card": "0..*", "control": { "kind": "branch", "guard": "limit", "chain": ["else_if", "else"] } },
  "else_if":        { "body": "self", "card": "0..*", "control": { "kind": "branch_continue", "guard": "limit" } },
  "else":           { "body": "self", "card": "0..*", "control": { "kind": "branch_continue" } },
  "limit":          { "body": "trigger", "card": "0..1", "control": { "kind": "guard" } },
  "random_list":    { "map": { "key": "int", "body": "self" }, "card": "0..*", "control": { "kind": "weighted" } },
  "random":         { "body": "random_body", "card": "0..1", "control": { "kind": "chance" } },       // random_body = self + chance field
  "trigger_switch": { "body": "trigger_switch_body", "card": "0..1", "control": { "kind": "switch", "on": "on_trigger" } },
  "hidden_effect":  { "body": "self", "card": "0..*", "control": { "kind": "transparent" } },
  "tooltip":        { "body": "self", "card": "0..1", "control": { "kind": "display_only" } }
}}}
```

`kind` is a **closed enum** in Rust. Each value corresponds to one piece of
previously hard-coded behaviour:

| kind | Semantics | Hard-coding removed |
|---|---|---|
| `branch` / `branch_continue` | if chain: `chain` lists the sibling keys that may follow; `guard` names the guard sub-block | the if-chain lints in `lints.rs`, `diagnostics.rs`, `hir/model.rs` |
| `guard` | Guard sub-block; its body is in trigger context | the `limit` handling in `diagnostics.rs` / `dynamic_rules.rs` |
| `logic` | `AND`/`OR`/`NOT`, carrying `"op"`; scope-transparent; the `NOT` multi-condition lint is driven by `op` | the `NOT` lints in `lints.rs`, `transparent_scope_wrappers` |
| `weighted` / `chance` | Weighted branches with values as keys / probability block | the `random`/`random_list` handling in `diagnostics.rs` |
| `switch` | Branch keys are *legal values of the trigger named by the `on` field*: the runtime reads the `on_trigger` value, looks up that key's scalar field in the trigger schema, and validates each branch key with its value matcher; branch bodies are `self`. This behaviour is fully implemented by the `kind` and needs no extra expression syntax | the `trigger_switch` handling in `diagnostics.rs` |
| `transparent` | Scope-transparent wrapper | `transparent_scope_wrappers` |
| `display_only` | Affects presentation only; not executed | the `tooltip` handling in `diagnostics.rs` |

`trigger`/`mtth`/`mean_time_to_happen` context switching is **not** control
flow; those are ordinary fields (`"body": "trigger"`, `"body": "mtth"`). The
corresponding hard-coding disappears naturally with the tree-shaped IR.
`control_flow_keys` is derived from the fields that carry `control`.

## 10. Infrastructure

### 10.1 Compile-time semantic checks

The compiler runs the following checks after parsing. Each is normative; the
trigger conditions are exhaustive with respect to the check's scope.

1. **Name resolution.** A schema, type, enum, mixin (including a trait) that is
   referenced but not defined is an error. A definition that is never
   referenced is a warning. Counted as referenced: schemas reachable from the
   `files` roots; mixins reached by an `include`; traits reached by an `impl`.
2. **Include conflict.** Two mixins (or a mixin and the including schema)
   declaring the same field key is an error, unless the schema's own field is
   marked `"override": true`, in which case the schema's field wins. A conflict
   between two mixins cannot be overridden and is always an error.
3. **Unreachable overload.** Within one `fields`, an overload of the same key
   and the same shape that is fully shadowed by an earlier overload is an
   error. Within `patterns`, a later pattern fully covered by an earlier
   pattern — key expression and shape structurally identical — is an error.
   The test is conservative: structural equivalence only.
4. **Parameterisation.** A formal parameter may appear only in an `arg`
   position of a type expression or as an actual argument of another
   parameterised schema. Only one level of parameters is allowed: an actual
   argument may only be a concrete name or the formal parameter of the
   enclosing parameterised schema. Each parameterised schema is limited to 64
   instances; exceeding the limit is an error.
5. **Subtype `when` dependency.** A field read by any `when` predicate MUST NOT
   itself carry `when`/`unless`; violation is an error. Undefined field names
   read by a `when`, and undefined names inside the predicate's value
   expressions, fall under check 1.
6. **Scope link `from` mismatch.** The `from`/`to` of links, the
   `actual`/`expected` of compat entries, and the values of `scope.in`,
   `scope.push`, and `scope.set` MUST be declared scope types or `any`. The
   keys of `scope.set` MUST be declared registers. `from` MUST NOT be empty.
   Violation is an error.
7. **Unsatisfied trait `requires`.** When a type impls a trait, that trait's
   `requires` conditions (for example `{ "include": "modifier_block" }`) MUST
   hold on the schema used at the type's `def` position; if they do not, it is
   an error.
8. **Duplicate trait impl.** A type — type-level together with all
   subtype-level impls — may impl a given trait only once. A duplicate is an
   error.

### 10.2 JSON Schema

A JSON Schema artifact is generated by `schemars` from the Rust source types
into `rules/rules-language.schema.json`, for editor completion and validation.
Type expressions receive only coarse `pattern` validation in the JSON Schema;
precise validation is `rulec`'s job (an accepted trade-off).

### 10.3 Tools

- `rulec check` runs parsing and semantic checks over a rule source directory
  and prints diagnostics as text. Editors may call it directly.
- `rulec fmt` writes the canonical form: normalised formatting, omitted
  defaults, sorted fields. (Not implemented at this stage.)

### 10.4 Provenance

The compiler generates provenance — source file + JSON pointer — for every
compiled field. Rule sources MUST NOT hand-write `id`/`source_file`/`line`
metadata; provenance is derived, never authored.

## 11. Diagnostics summary

| Diagnostic | Severity | Trigger |
|---|---|---|
| Parse error | error | An illegal identifier or number; a range after a primitive other than `int`/`float`; non-whole `int` bounds or a lower bound above its upper bound; an invalid escape; an unterminated literal or hole; an invalid `arg`; an empty union branch; a union mixing scalar and quoted branches (§2); a malformed link-key template, schema reference, `card`, `def` name, or enum column declaration; a missing `card`; a `script` files entry without `root`; a `map` without exactly one of `value`/`body`; a trait binding without exactly one of `loc`/`sprite` |
| `CardLint` | warning / info | `card` is `0..0` (warning: disables the field rather than bounding it), `N..N` (info: a fixed-length tuple better written as `list` plus the arity), or two overloads of one key disagree on the upper bound (info) |
| `DuplicateName` | error | A schema, mixin, type, enum, trait, files entry, or scope declaration is defined twice across sources (§1) |
| `UndefinedReference` | error | A referenced schema, type, enum, mixin, or trait is not defined (check 1) |
| `UnusedDefinition` | warning | A definition is never referenced (check 1) |
| `IncludeConflict` | error | Two mixins, or a mixin and the schema, declare the same key without an `"override": true` schema field; or two mixins conflict (check 2) |
| `UnreachableOverload` | error | A same-key same-shape overload, or a later pattern, is fully shadowed (check 3) |
| `ParameterError` | error | A formal parameter out of position; more than one parameter level; more than 64 instances of one parameterised schema (check 4) |
| `SubtypeWhenDependency` | error | A field read by a `when` predicate carries `when`/`unless` (check 5) |
| `ScopeReferenceError` | error | A links/compat/scope-effect value or `set` key is not a declared scope type, `any`, or register; or a link `from` is empty (check 6) |
| `UnsatisfiedTraitRequirement` | error | A trait's `requires` does not hold at the type's `def` position (check 7) |
| `DuplicateTraitImpl` | error | The same type impls one trait more than once (check 8) |
