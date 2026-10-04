# Rules language semantics

This guide explains the semantics and rationale of the JSON rule language used by `rulec`.
Exact source fields, required members, enum values, and serialization defaults are defined in
[src/source.rs](src/source.rs) and projected into the generated [source reference](SOURCE-REFERENCE.md)
and the JSON Schema generated on demand by `rulec schema`. Those generated views own the JSON shape;
this guide owns interpretation and composition. The compiler and its behavior tests enforce the semantics.
The key words MUST, MUST NOT, SHOULD, and MAY express language requirements.
Rule-source compile diagnostics are distinct from [script diagnostics](../ide/DIAGNOSTICS.md).
First-party maintenance commands live in the [rule-package guide](../../rules/README.md).

## 1. Top-level sections and directory layout

A rule source directory contains a root `game.json` and regular `.json` source
files discovered recursively. No source manifest is used. Sources MUST be merged
in normalized relative-path order; directory names do not affect interpretation.
Non-JSON files are ignored; symbolic links are rejected to prevent aliases and
cycles. Package identity (`source_format_version`, `game_id`, and optional
`target_game_version`) lives in `game.json`; another source format version is
rejected. Identity metadata is separate from the runtime profile and language IR.
Each source file MAY contain any subset
of seven top-level sections:

| Section | Contents |
|---|---|
| `files` | Path selection → parser / root schema |
| `schemas` | Structure: `fields` / `patterns` / `items` / `include` / `def` / parameters |
| `mixins` | Pure structural field bundles (expanded at compile time) |
| `types` | Symbol namespaces: `resolution` / `subtypes` / `open` / `builtin` / `impl` |
| `traits` | Named trait markers; implementation data belongs to `types.*.impl` |
| `enums` | Literal member lists |
| `scopes` | Scope types / registers / links / compat |

The compiler merges the same-named section across all files into one namespace.
A name defined twice — a duplicate schema, mixin, type, enum, trait, files
entry, or scope declaration — is an error; there is no shadowing or override
between files.

There is **no top-level `intrinsics` section**: control-flow primitives are
field attributes (§9), so no key name is globally significant. There is **no
separate `tables` section**: enums contain literal member lists (§6).

The directory layout is organised by game domain; the shared parts live in
`core/`:

```
rules/eu4/
  game.json              # package identity; install, filesystem scan, hover_cards, fallback_keys
  core/
    scopes.json          # scopes section
    special.json         # helper schemas; control attributes belong to fields
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
          | name "strip_prefix" name     (* ref<estate strip_prefix estate_>：去掉词缀 *)
          | param ;
literal   = "'" { char | "{" expr "}" } "'" ;   (* 无洞即常量；有洞即模板 *)
param     = "$" name ;                    (* 参数化 schema 的形参 *)
name      = ident ;
```

Constructor paths name a type and optional subtype (`ref<event.country>`),
or use a formal parameter of the enclosing parameterised schema. The remaining
lexical rules are normative:

- Whitespace outside literals is insignificant between tokens and at the ends
  of the expression. Whitespace inside literal text is literal text.
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
- The `arg` of a constructor (between `<` and `>`) is `seg ["." seg]`,
  or `seg "strip_prefix" Ident`, where `seg = Ident | "$" Ident`.
  `strip_prefix` names the affix removed from the resolved
  member name before substitution; the affix must be an identifier
  (`ref<estate strip_prefix estate_>`), which strips the declared affix before binding.
- A bare `param` (`$name`) is a legal alternative on its own.
- Union has the lowest precedence and is tried in written order. An empty
  branch is a parse error.
- `path` takes an optional `<category>` (for example `path<gfx>`); the category
  is an identifier.
- A link key (a key of `scopes.links`) is **template text**: plain text with
  `{expression}` holes, without outer quotes, using the same escape rules as
  literals.

### 2.2 Expression semantics

| Expression | Meaning |
|---|---|
| `scalar` | Any scalar |
| `'yes'` | Constant |
| `'monthly_{ref<government_mechanic_power>}'` | Text pattern |
| `'{ref<estate strip_prefix estate_>}_loyalty_modifier'` | Text pattern whose hole strips an affix from the member name (`estate_burghers` → `burghers`); the affix is declared on the reference |
| `int[1..10]` `float[0..]` `bool` `date` | Scalar types; bounds are always numbers |
| `loc` | Localisation key |
| `path` `path<gfx>` | File path; `<…>` is a path category |
| `ref<event>` `ref<event.country>` | Symbol reference; optionally qualified by subtype |
| `def<country_flag>` | **Defines** a symbol at this position |
| `enum<country_tags>` | Enum member |
| `scope<country>` `scope<any>` | Scope expression (register, link, tag, …) |
| `link` | Key position only: any scope link / register / prefix link (§8) |
| `opaque` | Unchecked text |
| `a\|b` | Union, tried in written order |

Semantic conventions:

- Keys and symbol names are case-insensitive everywhere in the language.
- Type-expression unions describe scalar values. Quoted values remain scalars.
  Script text is interpreted only when a Template use consumes it as a script
  fragment; ordinary fields cannot request a secondary script parse. Shape
  variation between scalar and block uses field-array overloads (§3).

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
`include` for mixin expansion and `def` positions
(§4–§5), and optionally formal parameters in its name.

`forms` expresses alternative combinations of direct-field counts. Every
declared field keeps its ordinary type, scope and cardinality checks; the block
must additionally satisfy at least one complete form. A form names exact fields
case-insensitively in `fields`, and written pattern indices in `patterns`:

```jsonc
"variable_operation": {
  "fields": {
    "which": { "value": "ref<variable>", "card": "0..2" },
    "value": { "value": "float", "card": "0..1" }
  },
  "forms": [
    { "fields": { "which": "2", "value": "0" } },
    { "fields": { "which": "1", "value": "1" } }
  ]
}
```

This accepts two `which` operands or one `which` plus one `value`; empty,
incomplete and mixed forms remain errors. A pattern constraint counts all keys
selected by that pattern, after exact-key dispatch, rather than counting each
spelling independently. `{"patterns":{"0":"1..2"}}` constrains the first
written pattern. Counts include all shape overloads of a named field. Fields
omitted from a form retain their ordinary bounds. Empty forms, undeclared
field/pattern targets and invalid bounds are compile errors, so bake refuses
them. The compiler resolves constraints to field ids; runtime does not know
game command names.

### 3.1 Field specifications

A `fields` entry or `patterns` element uses the generated
[FieldSpec reference](SOURCE-REFERENCE.md#fieldspec). `card` is explicit and mandatory:
cardinality determines missing-required and repeated-entry diagnostics, so a guessed default
would change script interpretation. Exact field defaults belong to the source model.

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

See the generated [FileRule reference](SOURCE-REFERENCE.md#filerule) for JSON fields and defaults.
A script file category requires a root schema; omission would leave the file unvalidated.

A category whose structure the rules do not model still declares a root: an
open schema (`{ "open": true }`) says "any key, no rules" explicitly instead of
leaving the file unvalidated by omission.

### 4.2 The `def` specification

- **Shorthand.** `def<T>` in a value or key position; the instance name is the
  scalar itself.
- **Full form.** `{ "type": "T[.subtype]", "name": ... }`, where `name` is
  `"key"` (see the source reference) / `"field:<field-name>"` / `"file"` (file name without
  extension). Optional `"strip_prefix"` / `"strip_suffix"` strip affixes from
  the instance name (replacing `name_strip_prefix`/`name_strip_suffix` and
  `lexicon.member_name_suffixes`).
- **Subtype at `def`.** Writing a subtype in the def type (`event.country`)
  tags the instance with that subtype (§5).

One type may have several `def` positions. After this section, the Types
subsystem carries only pure symbol semantics: *where instances are collected
and where references come from* is answered entirely by structure.

## 5. Explicit subtypes

Subtypes classify symbol instances by their definition position:

```jsonc
"types": { "event": { "subtypes": { "country": {}, "province": {} } } }
```

A `def` of type `event.country` grants the `country` subtype;
`ref<event.country>` accepts only instances explicitly assigned that subtype.
Subtype entries are empty objects. They do not carry predicates or trait impls.

All schema fields are available regardless of sibling field values. There is
no field `when`/`unless` syntax, subtype `when` predicate, or conditional binding
selection. Type, shape, scope and cardinality checks still apply. Structural
alternatives based on field counts use `forms` (§3); script control flow is
specified separately by `control` (§9).

Every localisation/icon binding is declared at the type level and available
for every instance. Former conditional bindings become optional display
bindings; existing type-level required bindings retain their requirement.
Former references to structurally inferred subtypes use the base type.
Source format version 13 rejects the removed syntax.

## 6. Enums

An enum is an array of literal member names. Related groups use separate enums
and explicit patterns; they may share a parameterised body:

```jsonc
"enums": {
  "on_actions_country": ["on_startup"],
  "on_actions_province": ["on_province_religion_converted"]
},
"schemas": {
  "on_actions_file": { "patterns": [
    { "key": "enum<on_actions_country>", "body": "on_action_body<country>", "card": "0..*" },
    { "key": "enum<on_actions_province>", "body": "on_action_body<province>", "card": "0..*" }
  ] }
}
```

There are no enum attribute columns, row subsets, or implicit `$key` parameters.
Only declared schema formals may be used as `$name`. The compiler monomorphises
`on_action_body<S>` for the explicitly supplied scope names.
`profile.enum_extra_members` merge into the member lists. Engine-provided symbol
members use type `builtin` (§7).

## 7. Types, mixins, and traits

### 7.1 Types

| Field | Meaning |
|---|---|
| `resolution` | `replace`: definitions override by name at game load, and a same-name definition raises a shadow warning; `independent`: structurally duplicated keys (each unit file's `maneuver`) stand alone with no shadow warning. Navigation and rename behaviour are identical (both resolve the same symbol namespace) |
| `subtypes` | See §5 |
| `open` | Open world: any name may exist (`event_target`, `saved_name`, …), `ref` does not report undefined names |
| `builtin` | Members preset by the engine |
| `impl` | Implemented traits and their arguments |

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

A trait is a named marker recognised by a runtime consumer. It belongs to the
Types subsystem. Trait declarations are empty objects; implementation data
belongs to each type. There are no trait declaration parameters, bindings,
requirements, or capability labels.
The first version's built-in set is fixed at four; adding a trait requires
changing Rust, because the runtime must understand its semantics:

```jsonc
"traits": {
  "Localised":      {},                                   // bindings come from the impl
  "HasIcon":        {},                                   // bindings come from the impl
  "ModifierSource": {},
  "Template":       {}
},
"types": {
  "decision": { "impl": { "Localised": {
    "name": { "loc": "$_title", "required": true },
    "desc": { "loc": "$_desc" }
  } } },
  "idea_group": { "impl": { "Localised": {
    "name": { "loc": "$", "required": true },
    "bonus": { "loc": "$_bonus", "required": true },
    "start": { "loc": "$_start" }
  } } },
  "building":        { "impl": { "Localised": { "name": { "loc": "building_$", "required": true } },
                                 "HasIcon": { "icon": { "sprite": "GFX_$", "required": true } },
                                 "ModifierSource": {} } },
  "scripted_effect": { "impl": { "Template": { "body": "effect" } }, "resolution": "replace" }
}
```

- `Localised` and `HasIcon` take **one binding per impl argument**: the argument
  name is the binding's name (the hover row label and part of its stable
  identity), and the value declares the template plus whether the bound symbol
  must resolve. The binding set is per-type data, so the trait itself declares
  no bindings; the impl enumerates each presentation binding explicitly. `Localised` bindings use `loc`, `HasIcon`
  bindings use `sprite`; declaring both, or the wrong one for the trait, is an
  error.
- In binding templates, `$` is the **instance-name placeholder**. This is a
  different syntax from the type-expression `$param`: binding templates are not
  type expressions.
- `impl` is declared on the type and applies to every instance.
- `Localised`/`HasIcon` replace `bindings/localisation.json` and
  `bindings/sprite.json`; `Template` replaces `dynamic_definition` and
  `token_definitions` (its `$param$` arguments are handled uniformly by
  `Template`); `ModifierSource` replaces the 22 `type:X → [modifier]` rows of
  profile `semantic_context_inheritance`. Modifier diagnostics consult its type
  implementations directly. References name concrete types and optional explicit
  subtypes. A `Template` implementation supplies the body schema; its trait
  identity enables scripted-call argument processing and replay.

Template arguments substitute text before runtime branches execute. Every
`$param$` occurrence outside a `[[param] ... ]` activation chunk is required,
including occurrences embedded in a word such as `PREFIX_$param$_END` and
occurrences inside ordinary `if`/`else` blocks. Activation chunks are selected
by parameter presence; inactive chunks do not demand their substitutions.
Constraints apply to the rendered token and use the invocation's scope.
An unprotected forwarding substitution is still required even when the callee
uses that argument only in an activation chunk. Protect the entire forwarding
call to make omission valid. Presentation-only blocks do not exempt textual
substitutions from this requirement.

Symbol indexing records syntactic writes and references inside presentation-only
blocks, including quoted Template payloads. These declarations support navigation
and rename; they do not imply that the preview executes. Scalar scope alternatives
match only actual scope expressions and must not suppress other typed references.
An enum fallback also retains navigation when an earlier reference branch resolves
to an installed symbol. Resolved overlapping reference branches retain their
indexed targets, including overlapping subtypes of the same namespace. A scalar
fallback does not create an unresolved reference. Unknown-scope overloads keep
all scope alternatives for validation while collecting references only from
value domains that match.

A quoted script supplied to a parameter must be valid at every distinct active
usage of that parameter. Overloads at the same source usage and scope are
alternatives; different usages impose simultaneous constraints. A bare spliced
fragment does not repeat the enclosing block's mandatory keys, while complete
blocks created inside that fragment retain their own structural requirements.

**Trait vs mixin.** Define something as a trait only if at least one holds:
(a) the engine/IDE handles it uniformly (hover, localisation checks, call
argument derivation); (b) it appears in type constraints. Otherwise use a
mixin.

**Anti-over-design constraints.** There is no trait inheritance. A type may
impl a given trait only once at the type level. Everything is expanded to
flat data at compile time; the runtime does no dynamic dispatch.

## 8. Scopes

```jsonc
"scopes": {
  "types":     ["country", "province", "unit", "monarch", "heir", "consort",
                "mercenary_company", "rebel_faction", "religion", "culture", "advisor", "leader",
                "trade_company", "global", "none"],
  "registers": { "root": { "role": "root" }, "this": { "role": "current" }, "prev": { "role": "previous", "chain": true }, "from": { "role": "from", "chain": true } },
  "links": {
    "owner":      { "from": ["province", "unit"], "to": "country" },
    "controller": { "from": ["province"], "to": "country" },
    "capital":    { "from": ["country"], "to": "province" },
    "emperor":    { "from": ["any"], "to": "country" },
    "event_target:{ref<event_target>}":        { "from": ["any"], "to": "any" },
    "global_event_target:{ref<global_event_target>}": { "from": ["any"], "to": "any" }
  },
  "compat": []
}
```

- EU4 trade-node execution uses `province` everywhere, including node iterators, named-node scope blocks, and trading-policy registers. `trade_node` remains a symbol namespace for definitions and `ref<trade_node>`; it is not a scope type or an alias.
- `any` is a reserved word meaning *any scope*; it never appears in `types`.
- `registers.role` is required: `root`, `current`, `previous`, or `from` selects the runtime state slot. Register spellings are arbitrary; the runtime never infers the role from a name.
- `registers.chain` is allowed only for `previous` and `from` roles. It means the register concatenates (`prev_prev`, `fromfrom`,
  …), replacing the former hard-coded register list and the hand-written
  `prev_prev` entries in `scope_names`.
- Link keys may be templates (§2), replacing `dynamic_scope_prefixes`;
  `dynamic_value_prefixes` are likewise expressed as value templates
  (`'variable:{ref<variable>}'`).
- `scope_completions` is derived from registers + types and is no longer
  written by hand.
- A `THIS = { ... }` block keeps the current scope. It does not erase scope
  constraints or push another previous-scope entry. In a Template body,
  `add_prestige = 1 THIS = { change_province_name = "X" }` therefore has a
  conflicting country/province entry requirement.

The scope effect on a rule is one field, and it has **only the object form**
(no string shorthand, eliminating the ambiguity of `"scope": "country"`):

```jsonc
"scope": { "in": ["country"], "push": "province", "set": { "root": "country", "this": "country", "from": "any" } }
```

`in` replaces `allowed_scopes` (default: any), `push` replaces `push_scope`,
and `set` replaces `replace_scope` together with `TypeRootScope`. All values of
`in`/`push`/`set`, and the keys of `set`, MUST be declared scope types, `any`
(for the values) or declared registers (for the keys) — §10, check 5.

Context-neutral scope-switch blocks in trigger/effect schemas share one
pattern:

```jsonc
"trigger": { "patterns": [ { "key": "link", "body": "self", "card": "0..*" } ] }
```

When a `link` key matches, the compiler already knows the link's `from`/`to`;
the runtime checks the current scope against `from` and pushes the `to` scope.
Scalar triggers that share a link's name (such as `controller = ROOT`) stay in
`fields` as ordinary exact fields: by the §3 lookup rules the scalar shape hits
the exact field and the block shape falls through to the `link` pattern. The
pure-link rows with identical trigger/effect semantics are folded into
`scopes.links` by the conversion. Context-specific blocks retain explicit
fields with `body`, `scope.in` and `scope.push`. In EU4, `any_*` belongs to
trigger/limit; effects use `every_*` or `random_*`. For example:

```jsonc
"trigger": { "fields": { "any_country": {
  "body": "self", "card": "0..*", "scope": { "push": "country" }
} } }
```

This field is inherited only with the trigger vocabulary. It does not make
`any_country` a globally available link or an effect key.

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
  "trigger_switch": { "body": "trigger_switch_body", "card": "0..1", "control": { "kind": "switch", "on": "on_trigger", "selector_schema": "trigger" } },
  "hidden_effect":  { "body": "self", "card": "0..*", "control": { "kind": "transparent" } },
  "tooltip":        { "body": "self", "card": "0..1", "control": { "kind": "display_only" } }
}}}
```

`kind` is a **closed enum** in Rust. Each value corresponds to one piece of
previously hard-coded behaviour:

| kind | Semantics |
|---|---|
| `branch` / `branch_continue` | if chain: `chain` lists the sibling keys that may follow; `guard` names the guard sub-block |
| `guard` | Guard sub-block; its body is in trigger context |
| `logic` | `AND`/`OR`/`NOT`, carrying `"op"`; scope-transparent; the `NOT` multi-condition lint is driven by `op` |
| `constant` | A scalar boolean predicate whose result equals its value; logic folding uses only these declared predicates |
| `weighted` / `chance` | Weighted branches with values as keys / probability block |
| `switch` | Branch keys are *legal values of the predicate named by the `on` field*: `on` and an explicit `selector_schema` are required. The runtime finds the selected scalar field in that schema and validates each branch key with its value matcher; branch bodies follow the field's body declaration. `selector_schema` cannot be `self` and is only valid for `switch` |
| `transparent` | Scope-transparent wrapper |
| `display_only` | Affects presentation only; not executed |

`trigger`/`mtth`/`mean_time_to_happen` context switching is **not** control
flow; those are ordinary fields (`"body": "trigger"`, `"body": "mtth"`). The
corresponding hard-coding disappears naturally with the tree-shaped IR.
`control_flow_keys` is derived from the fields that carry `control`.

`constant` preserves a scalar `bool` matcher and adds no schema instances or template expansion.
EU4 declares it on `always`; ordinary boolean predicates such as `is_capital` are not constants.
Three local Vanilla uses are `common/scripted_triggers/00_scripted_triggers_estates.txt:97`,
`common/scripted_triggers/02_scripted_triggers_for_mission_conditions.txt:154`, and
`decisions/England.txt:143`. It has no effect on the 64-instance limit.

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
4. **Parameterisation.** A formal parameter may appear only in a type expression or as an actual
   argument of another
   parameterised schema. Only one level of parameters is allowed: an actual
   argument may only be a concrete name or the formal parameter of the
   enclosing parameterised schema. Each parameterised schema is limited to 64
   instances; exceeding the limit is an error.
5. **Scope link `from` mismatch.** The `from`/`to` of links, the
   `actual`/`expected` of compat entries, and the values of `scope.in`,
   `scope.push`, and `scope.set` MUST be declared scope types or `any`. The
   keys of `scope.set` MUST be declared registers. `from` MUST NOT be empty.
   Violation is an error.
### 10.2 JSON Schema

A JSON Schema is generated on demand by `rulec schema`, using `schemars` and the Rust
source types, for editor completion and validation. The default output is
`target/rules-language.schema.json`; it is local generated output, not a checked-in artifact.
Type expressions receive only coarse `pattern` validation in the JSON Schema;
precise validation is `rulec`'s job (an accepted trade-off).

### 10.3 Tools

- `rulec check` runs parsing and semantic checks over a rule source directory
  and prints diagnostics as text. Editors may call it directly.
- `rulec fmt <source-dir>` writes the canonical form of the declared language
  source files: two-space JSON, sorted object keys, and omitted defaults.
  It preserves overload, pattern, and enum-member order, declaration names,
  literal values, and schema shorthands. Configuration files are not rewritten.
- `rulec fmt <source-dir> --expanded` mechanically spells out defaults on the
  source structures. Absent optional mechanisms remain `null`; required
  `card` values are retained. It does not expand mixins or monomorphise schemas.
- `--check` may be combined with either formatting mode: it reports files
  requiring changes and exits non-zero without writing. Parsing and rendering
  of the entire bundle finish before a write begins, so invalid JSON in a later
  source file does not leave earlier files partially formatted.

### 10.4 Provenance

The compiler generates provenance — source file + JSON pointer — for every
compiled field. Rule sources MUST NOT hand-write `id`/`source_file`/`line`
metadata; provenance is derived, never authored.

### 10.5 Installation configuration

`game.json` MAY carry an `install` object, independent of the script language.
The first-party game package requires it and compiles it into its installation
descriptor at build time. The executable markers, validation directories,
launcher directory names, and optional Steam identity come only from this data;
platform discovery remains a generic mechanism.

```json
{ "install": {
  "display_name": "Example Game",
  "executable_paths": { "windows": ["game.exe"], "linux": ["game"], "macos": [] },
  "validation_directories": ["common"],
  "installation_directory_names": ["Example Game"],
  "steam_app_id": null
} }
```

The object and its platform-path object reject unknown keys. All fields except
`steam_app_id` are required; an unsupported platform uses an explicit empty
list. The descriptor shares the enclosing `game_id` instead of declaring it
again. Installation data participates in artifact identity, without supplying
script keys, scopes, definitions or references.

## 11. Diagnostics summary

| Diagnostic | Severity | Trigger |
|---|---|---|
| Parse error | error | An illegal identifier or number; a range after a primitive other than `int`/`float`; non-whole `int` bounds or a lower bound above its upper bound; an invalid escape; an unterminated literal or hole; an invalid `arg`; an empty union branch; a union mixing scalar and quoted branches (§2); a malformed link-key template, schema reference, `card`, or `def` name; a missing `card`; a `script` files entry without `root`; a `map` without exactly one of `value`/`body`; a trait binding without exactly one of `loc`/`sprite` |
| `CardLint` | warning / info | `card` is `0..0` (warning: disables the field rather than bounding it), `N..N` (info: a fixed-length tuple better written as `list` plus the arity), or two overloads of one key disagree on the upper bound (info) |
| `DuplicateName` | error | A schema, mixin, type, enum, trait, files entry, or scope declaration is defined twice across sources (§1) |
| `UndefinedReference` | error | A referenced schema, type, enum, mixin, or trait is not defined (check 1) |
| `UnusedDefinition` | warning | A definition is never referenced (check 1) |
| `IncludeConflict` | error | Two mixins, or a mixin and the schema, declare the same key without an `"override": true` schema field; or two mixins conflict (check 2) |
| `UnreachableOverload` | error | A same-key same-shape overload, or a later pattern, is fully shadowed (check 3) |
| `ParameterError` | error | A formal parameter out of position; more than one parameter level; more than 64 instances of one parameterised schema (check 4) |
| `ScopeReferenceError` | error | A links/compat/scope-effect value or `set` key is not a declared scope type, `any`, or register; or a link `from` is empty (check 5) |

## 12. Structured mission-view capability

`game.json.mission_view` declares the first supported structured view. It is
optional: an absent capability disables mission diagnostics and preview, regardless
of `game_id`. This is a concrete mission model, not a general tree-view DSL.

- `path`: profile text matcher selecting the view's logical files.
- `symbol_kind`: declared node type used for workspace references and title bindings.
- `tree_fields`: script spellings for `slot`, `generic`, `ai`, `has_country_shield`,
  `potential`, and `potential_on_load` roles.
- `node_fields`: script spellings for `icon`, `type`, `provinces_to_highlight`,
  `required_missions`, `position`, `completed_by`, `trigger`, and `effect` roles.
- `tree_field_order` / `node_field_order`: each role MUST appear exactly once.

Field spellings MUST be nonempty and unique within each level. Unknown members,
incomplete or duplicate orders, and an undeclared `symbol_kind` reject the bake.
The game package exposes this through its mission-view capability interface;
engine and IDE do not branch on the game identity. Generic graph assembly, stable
cycle detection, field ordering and block replacement live in the engine. Grid
geometry and layout validation remain in the game package.

## Complete example

The following complete source file is parsed and semantically checked by `tools documentation check`.
Fragment examples above illustrate individual constructs; they are not claimed to compile alone.

```json rule-file
{
  "files": { "events": { "path": "events", "ext": "txt", "root": "events_file" } },
  "schemas": {
    "events_file": { "fields": { "country_event": { "body": "event_body", "card": "0..*" } } },
    "event_body": { "fields": { "id": { "value": "scalar", "card": "1" } } }
  }
}
```
