# Rule source reference

<!-- Generated; edit src/source.rs, then run cargo run --locked -p tools -- documentation write. -->

[Language semantics](LANGUAGE.md) · [Rule-package workflow](../../rules/README.md).

Each object below comes from the compiler's generated JSON Schema. `Required` and
`default` describe source serialization; additional semantic constraints are checked by `rulec`.

## RuleFile

One rules source file: any subset of the seven top-level sections. The
compiler merges same-named sections across files into one namespace per
section and rejects duplicate names.

| Field | Required | Source default | Shape | Meaning |
| --- | --- | --- | --- | --- |
| `enums` | no | `{}` | `{"additionalProperties":{"$ref":"#/$defs/EnumSpec"},"type":"object"}` | Enums as literal member lists. |
| `files` | no | `{}` | `{"additionalProperties":{"$ref":"#/$defs/FileRule"},"type":"object"}` | Path-selection rules: logical path → parser / root schema. |
| `mixins` | no | `{}` | `{"additionalProperties":{"$ref":"#/$defs/MixinSpec"},"type":"object"}` | Pure structural field bundles, expanded at compile time. |
| `schemas` | no | `{}` | `{"additionalProperties":{"$ref":"#/$defs/SchemaSpec"},"type":"object"}` | Block descriptions, keyed by schema name (formal parameters may be spelled in the key: `"on_action_body<S>"`). |
| `scopes` | no | `null` | `{"anyOf":[{"$ref":"#/$defs/ScopesSpec"},{"type":"null"}]}` | Scope types, registers, links, and compatibility overrides. |
| `traits` | no | `{}` | `{"additionalProperties":{"$ref":"#/$defs/TraitSpec"},"type":"object"}` | Named trait markers; implementation data is declared on each type. |
| `types` | no | `{}` | `{"additionalProperties":{"$ref":"#/$defs/TypeSpec"},"type":"object"}` | Symbol namespaces: resolution / subtypes / open / builtin / impl. |

## BindingSpec

One type implementation binding: a localisation-key or sprite-name template.

| Field | Required | Source default | Shape | Meaning |
| --- | --- | --- | --- | --- |
| `loc` | no | `null` | `{"type":["string","null"]}` | Localisation-key template (`$` stands for the instance name). |
| `required` | no | `null` | `{"type":["boolean","null"]}` | Whether the bound symbol must resolve. |
| `sprite` | no | `null` | `{"type":["string","null"]}` | Sprite-name template. |

## BlockForm

One legal combination of fields in a block. This describes structural
alternatives without weakening the requirements of every branch.

| Field | Required | Source default | Shape | Meaning |
| --- | --- | --- | --- | --- |
| `fields` | no | `{}` | `{"additionalProperties":{"type":"string"},"type":"object"}` | Exact field names and their occurrence bounds in this form. |
| `patterns` | no | `{}` | `{"additionalProperties":{"type":"string"},"type":"object"}` | Written pattern indices and their total occurrence bounds in this form. Exact fields are excluded from pattern counts by normal dispatch. |

## BlockSchema

The full description of one block.

| Field | Required | Source default | Shape | Meaning |
| --- | --- | --- | --- | --- |
| `fields` | no | `{}` | `{"additionalProperties":{"$ref":"#/$defs/FieldOverloads"},"type":"object"}` | Exact keys, hash lookup. |
| `forms` | no | — | `{"items":{"$ref":"#/$defs/BlockForm"},"type":"array"}` | Alternative combinations of direct-field occurrence bounds. At least one complete form must match; ordinary field bounds still apply. |
| `include` | no | `[]` | `{"items":{"type":"string"},"type":"array"}` | Mixin names, expanded at compile time. |
| `items` | no | `null` | `{"type":["string","null"]}` | Type expression for bare values in this block (list blocks). |
| `open` | no | — | `{"type":"boolean"}` | Whether undeclared keys are allowed; defaults to false. |
| `patterns` | no | `[]` | `{"items":{"$ref":"#/$defs/FieldSpec"},"type":"array"}` | Non-exact keys, tried in written order. |

## CompatSpec

One compatibility override between scope types.

| Field | Required | Source default | Shape | Meaning |
| --- | --- | --- | --- | --- |
| `actual` | yes | — | `{"type":"string"}` | The scope type actually produced. |
| `expected` | yes | — | `{"type":"string"}` | The scope type accepted in its place. |

## ControlKind

The closed set of control-flow primitives.

```json
{
  "description": "The closed set of control-flow primitives.",
  "oneOf": [
    {
      "const": "branch",
      "description": "Head of an if-chain (`if`).",
      "type": "string"
    },
    {
      "const": "branch_continue",
      "description": "Continuation of an if-chain (`else_if`, `else`).",
      "type": "string"
    },
    {
      "const": "guard",
      "description": "Guard sub-block whose body is a trigger (`limit`).",
      "type": "string"
    },
    {
      "const": "logic",
      "description": "Boolean combinator (`AND` / `OR` / `NOT`).",
      "type": "string"
    },
    {
      "const": "weighted",
      "description": "Value-as-key weighted branch (`random_list`).",
      "type": "string"
    },
    {
      "const": "chance",
      "description": "Probability block (`random`).",
      "type": "string"
    },
    {
      "const": "switch",
      "description": "Branch keys validated against a trigger's values (`trigger_switch`).",
      "type": "string"
    },
    {
      "const": "transparent",
      "description": "Scope-transparent wrapper (`hidden_effect`).",
      "type": "string"
    },
    {
      "const": "display_only",
      "description": "Display only, not executed (`tooltip`).",
      "type": "string"
    },
    {
      "const": "constant",
      "description": "A predicate whose boolean value is its constant result.",
      "type": "string"
    }
  ]
}
```

## ControlSpec

A control-flow primitive attached to a field.

| Field | Required | Source default | Shape | Meaning |
| --- | --- | --- | --- | --- |
| `chain` | no | `null` | `{"items":{"type":"string"},"type":["array","null"]}` | Sibling keys that may continue this chain (`["else_if", "else"]`). |
| `guard` | no | `null` | `{"type":["string","null"]}` | Guard sub-block key (`"limit"` for branch kinds). |
| `kind` | yes | — | `{"$ref":"#/$defs/ControlKind"}` | Which primitive this field implements. |
| `on` | no | `null` | `{"type":["string","null"]}` | Field name whose scalar values are the branch keys of a `switch`. |
| `op` | no | `null` | `{"type":["string","null"]}` | Logic operator for `logic` kinds (`AND` / `OR` / `NOT`). |
| `selector_schema` | no | `null` | `{"type":["string","null"]}` | Schema supplying the scalar selector keys and their branch value matchers. |

## DefSpec

Defines a symbol instance at the field's position.

| Field | Required | Source default | Shape | Meaning |
| --- | --- | --- | --- | --- |
| `name` | no | `null` | `{"type":["string","null"]}` | Instance-name source: `"key"` (default), `"field:<name>"`, or `"file"`. |
| `strip_prefix` | no | `null` | `{"type":["string","null"]}` | Prefix removed from the derived instance name. |
| `strip_suffix` | no | `null` | `{"type":["string","null"]}` | Suffix removed from the derived instance name. |
| `type` | yes | — | `{"type":"string"}` | Type name, optionally with the subtype granted at this position (`"event.country"`). |

## EnumSpec

An enum is a list of literal member names.

```json
{
  "anyOf": [
    {
      "description": "Literal members.",
      "items": {
        "type": "string"
      },
      "type": "array"
    }
  ],
  "description": "An enum is a list of literal member names."
}
```

## ExtSpec

One file extension, or a list of them. The singular form keeps the
one-extension case readable; the list form replaces the legacy
`extensions` array without splitting one category into several entries.

```json
{
  "anyOf": [
    {
      "description": "One extension.",
      "type": "string"
    },
    {
      "description": "Several extensions.",
      "items": {
        "type": "string"
      },
      "type": "array"
    }
  ],
  "description": "One file extension, or a list of them. The singular form keeps the\none-extension case readable; the list form replaces the legacy\n`extensions` array without splitting one category into several entries."
}
```

## FieldOverloads

One exact key's field spec, or several shape overloads of it.

```json
{
  "anyOf": [
    {
      "$ref": "#/$defs/FieldSpec",
      "description": "A single field spec."
    },
    {
      "description": "Several shape overloads, matched in written order.",
      "items": {
        "$ref": "#/$defs/FieldSpec"
      },
      "type": "array"
    }
  ],
  "description": "One exact key's field spec, or several shape overloads of it."
}
```

## FieldSpec

One field specification (an element of `patterns`, a value of `fields`, or
the `root` instance form). Exactly one of `value` / `body` / `list` / `map`
must be present (checked by the compiler).

| Field | Required | Source default | Shape | Meaning |
| --- | --- | --- | --- | --- |
| `body` | no | `null` | `{"type":["string","null"]}` | Block: schema name (or `"self"`), optionally instantiated. |
| `capabilities` | no | — | `{"items":{"type":"string"},"type":"array"}` | Explicit opt-in tags used by schema-query capability filters. |
| `card` | yes | — | `{"type":"string"}` | Cardinality, e.g. `"1"`, `"0..1"`, `"2..5"`. Mandatory: the corpus has no majority value, and the field drives missing/repeated-key diagnostics, so it is never defaulted (D14). |
| `control` | no | `null` | `{"anyOf":[{"$ref":"#/$defs/ControlSpec"},{"type":"null"}]}` | Control-flow primitive (field attribute, never a global key name). |
| `def` | no | `null` | `{"anyOf":[{"$ref":"#/$defs/DefSpec"},{"type":"null"}]}` | Defines a symbol instance at this position. |
| `deprecated` | no | `null` | `{"type":["boolean","null"]}` | Whether the field is deprecated. |
| `doc` | no | `null` | `{"type":["string","null"]}` | Documentation text. |
| `key` | no | `null` | `{"type":["string","null"]}` | Key type expression; only meaningful inside `patterns`. |
| `list` | no | `null` | `{"type":["string","null"]}` | Bare-value list block whose element type is this expression. |
| `map` | no | `null` | `{"anyOf":[{"$ref":"#/$defs/MapSpec"},{"type":"null"}]}` | Homogeneous map block. |
| `override` | no | `null` | `{"type":["boolean","null"]}` | Explicitly overrides a mixin-declared field of the same key. |
| `scope` | no | `null` | `{"anyOf":[{"$ref":"#/$defs/ScopeEffect"},{"type":"null"}]}` | Scope effect: `in` / `push` / `set`. |
| `severity` | no | `null` | `{"anyOf":[{"$ref":"#/$defs/Severity"},{"type":"null"}]}` | Diagnostic severity for violations of this field. |
| `value` | no | `null` | `{"type":["string","null"]}` | Scalar type expression for the value. |

## FileRule

One `files` entry: which documents a rule selects and how their root
structure is described.

| Field | Required | Source default | Shape | Meaning |
| --- | --- | --- | --- | --- |
| `exclude` | no | — | `{"items":{"type":"string"},"type":"array"}` | Path prefixes this entry does not apply to (the legacy `path_exclude_prefixes`). |
| `ext` | no | `null` | `{"anyOf":[{"$ref":"#/$defs/ExtSpec"},{"type":"null"}]}` | File extension without the dot, or a list of extensions (the legacy `extensions` list); absent means every extension. |
| `file` | no | `null` | `{"type":["string","null"]}` | Exact file name within `path`. |
| `parser` | no | `null` | `{"anyOf":[{"$ref":"#/$defs/SourceParser"},{"type":"null"}]}` | Document parser; defaults to `script`. |
| `path` | yes | — | `{"type":"string"}` | Logical directory prefix (the legacy `game/` prefix is not spelled). |
| `resolution` | no | `"merge"` | `{"$ref":"#/$defs/SourceFileResolution"}` | Definition-priority policy; defaults to `merge` (D14: the default must cover the corpus majority). |
| `root` | no | `null` | `{"anyOf":[{"$ref":"#/$defs/RootSpec"},{"type":"null"}]}` | Root structure: a schema name, or a field spec with `def` describing a whole-file symbol instance. Parsers without script structure (`localisation`, `asset`) may omit it. |
| `strict` | no | `null` | `{"type":["boolean","null"]}` | When true, `path` does not recurse into subdirectories. |

## ImplSpec

Arguments of one trait implementation: binding (or parameter) name → value.

`Localised` and `HasIcon` take one [`BindingSpec`] per binding they
contribute, because the binding set is per-type data rather than a fixed
trait shape; the other built-ins take plain strings (`Template`'s `body`).

```json
{
  "additionalProperties": {
    "$ref": "#/$defs/ImplValue"
  },
  "description": "Arguments of one trait implementation: binding (or parameter) name → value.\n\n`Localised` and `HasIcon` take one [`BindingSpec`] per binding they\ncontribute, because the binding set is per-type data rather than a fixed\ntrait shape; the other built-ins take plain strings (`Template`'s `body`).",
  "type": "object"
}
```

## ImplValue

One trait-implementation argument.

```json
{
  "anyOf": [
    {
      "description": "A plain argument value (`Template`'s `body` schema name).",
      "type": "string"
    },
    {
      "$ref": "#/$defs/BindingSpec",
      "description": "One localisation or sprite binding contributed by the impl."
    }
  ],
  "description": "One trait-implementation argument."
}
```

## LinkSpec

One scope link.

| Field | Required | Source default | Shape | Meaning |
| --- | --- | --- | --- | --- |
| `from` | yes | — | `{"items":{"type":"string"},"type":"array"}` | Scopes the link may start from (`"any"` allowed). |
| `to` | yes | — | `{"type":"string"}` | Scope the link lands in (`"any"` allowed). |

## MapSpec

A homogeneous map block: same key and value shapes for every entry.

| Field | Required | Source default | Shape | Meaning |
| --- | --- | --- | --- | --- |
| `body` | no | `null` | `{"type":["string","null"]}` | Block: schema name (or `"self"`), optionally instantiated. |
| `key` | yes | — | `{"type":"string"}` | Key type expression (often `def<…>` or `enum<…>`). |
| `value` | no | `null` | `{"type":["string","null"]}` | Scalar type expression for the value. |

## MixinSpec

A mixin: a pure structural field bundle.

| Field | Required | Source default | Shape | Meaning |
| --- | --- | --- | --- | --- |
| `fields` | no | `{}` | `{"additionalProperties":{"$ref":"#/$defs/FieldOverloads"},"type":"object"}` | Field specs contributed to every including schema. |

## RegisterRole

Game-independent state slots selected by scope register declarations.

```json
{
  "description": "Game-independent state slots selected by scope register declarations.",
  "oneOf": [
    {
      "const": "root",
      "description": "Root scope of the invocation.",
      "type": "string"
    },
    {
      "const": "current",
      "description": "Current scope, without pushing a new frame.",
      "type": "string"
    },
    {
      "const": "previous",
      "description": "Earlier scope frames, nearest first.",
      "type": "string"
    },
    {
      "const": "from",
      "description": "Calling event's scope frames, nearest first.",
      "type": "string"
    }
  ]
}
```

## RegisterSpec

One scope register.

| Field | Required | Source default | Shape | Meaning |
| --- | --- | --- | --- | --- |
| `chain` | no | `null` | `{"type":["boolean","null"]}` | Whether the register chains (`prev_prev`, `fromfrom`, …). |
| `role` | yes | — | `{"$ref":"#/$defs/RegisterRole"}` | Runtime register selected by this declared spelling. |

## RootSpec

A [`FileRule`]'s root structure: either a schema name, or one field spec
carrying `def` for the whole-file-instance case.

```json
{
  "anyOf": [
    {
      "description": "The name of the root schema.",
      "type": "string"
    },
    {
      "$ref": "#/$defs/FieldSpec",
      "description": "A field spec with `def` (and usually `body`) describing the file as a\nsingle symbol instance."
    }
  ],
  "description": "A [`FileRule`]'s root structure: either a schema name, or one field spec\ncarrying `def` for the whole-file-instance case."
}
```

## SchemaSpec

A schema: a full block description, or one of the two schema-level
shorthands (`{"map": …}`, `{"list": …}`).

```json
{
  "anyOf": [
    {
      "$ref": "#/$defs/BlockSchema",
      "description": "Full block description."
    },
    {
      "description": "Shorthand for a schema with a single map pattern.",
      "properties": {
        "map": {
          "$ref": "#/$defs/MapSpec",
          "description": "The homogeneous map."
        }
      },
      "required": [
        "map"
      ],
      "type": "object"
    },
    {
      "description": "Shorthand for a schema with only `items`.",
      "properties": {
        "list": {
          "description": "The element type expression for bare values.",
          "type": "string"
        }
      },
      "required": [
        "list"
      ],
      "type": "object"
    }
  ],
  "description": "A schema: a full block description, or one of the two schema-level\nshorthands (`{\"map\": …}`, `{\"list\": …}`)."
}
```

## ScopeEffect

The scope effect of a field: only the object form exists.

| Field | Required | Source default | Shape | Meaning |
| --- | --- | --- | --- | --- |
| `in` | no | `null` | `{"items":{"type":"string"},"type":["array","null"]}` | Scopes the field is valid in (empty = any). |
| `push` | no | `null` | `{"type":["string","null"]}` | Scope entered by the nested block. |
| `set` | no | `null` | `{"additionalProperties":{"type":"string"},"type":["object","null"]}` | Registers replaced on entry: register name → scope type. |

## ScopesSpec

The scope model.

| Field | Required | Source default | Shape | Meaning |
| --- | --- | --- | --- | --- |
| `compat` | no | `[]` | `{"items":{"$ref":"#/$defs/CompatSpec"},"type":"array"}` | Compatibility overrides ("actual accepted where expected is required"). |
| `links` | no | `{}` | `{"additionalProperties":{"$ref":"#/$defs/LinkSpec"},"type":"object"}` | Scope links, keyed by template text (`"event_target:{ref<event_target>}"`). |
| `registers` | no | `{}` | `{"additionalProperties":{"$ref":"#/$defs/RegisterSpec"},"type":"object"}` | Scope registers (`root`, `this`, `prev`, `from`, …). |
| `types` | no | `[]` | `{"items":{"type":"string"},"type":"array"}` | Scope type names. `any` is reserved and not listed here. |

## Severity

Diagnostic severity for a field's violations.

```json
{
  "description": "Diagnostic severity for a field's violations.",
  "oneOf": [
    {
      "const": "error",
      "description": "Errors (the default).",
      "type": "string"
    },
    {
      "const": "warning",
      "description": "Warnings.",
      "type": "string"
    },
    {
      "const": "info",
      "description": "Information.",
      "type": "string"
    }
  ]
}
```

## SourceFileResolution

The definition-priority policy selected by a [`FileRule`].

```json
{
  "description": "The definition-priority policy selected by a [`FileRule`].",
  "oneOf": [
    {
      "const": "replace-by-path",
      "description": "Later paths shadow earlier ones per file path (default).",
      "type": "string"
    },
    {
      "const": "merge",
      "description": "All definitions remain visible.",
      "type": "string"
    }
  ]
}
```

## SourceParser

The document parser selected by a [`FileRule`].

```json
{
  "description": "The document parser selected by a [`FileRule`].",
  "oneOf": [
    {
      "const": "script",
      "description": "Paradox script.",
      "type": "string"
    },
    {
      "const": "localisation",
      "description": "`localisation/*.yml`.",
      "type": "string"
    },
    {
      "const": "asset",
      "description": "Asset manifests (`.gfx`, `.asset`).",
      "type": "string"
    },
    {
      "const": "syntax-only",
      "description": "Structure only: parse and report syntax, no semantic rules.",
      "type": "string"
    }
  ]
}
```

## SubtypeSpec

One named subtype.

```json
{
  "additionalProperties": false,
  "description": "One named subtype.",
  "type": "object"
}
```

## TraitSpec

One named trait marker; implementation data belongs to the type.

```json
{
  "additionalProperties": false,
  "description": "One named trait marker; implementation data belongs to the type.",
  "type": "object"
}
```

## TypeResolution

The conflict behavior of a symbol type.

```json
{
  "description": "The conflict behavior of a symbol type.",
  "oneOf": [
    {
      "const": "replace",
      "description": "Later definitions shadow earlier ones; same-name collisions warn.",
      "type": "string"
    },
    {
      "const": "independent",
      "description": "Structurally repeated keys stay independent (the default).",
      "type": "string"
    }
  ]
}
```

## TypeSpec

One symbol namespace.

| Field | Required | Source default | Shape | Meaning |
| --- | --- | --- | --- | --- |
| `builtin` | no | `null` | `{"items":{"type":"string"},"type":["array","null"]}` | Engine-provided members. |
| `impl` | no | `{}` | `{"additionalProperties":{"$ref":"#/$defs/ImplSpec"},"type":"object"}` | Traits implemented by this type and their arguments. |
| `open` | no | `null` | `{"type":["boolean","null"]}` | Open world: any name may exist; `ref` never reports undefined names. |
| `resolution` | no | `null` | `{"anyOf":[{"$ref":"#/$defs/TypeResolution"},{"type":"null"}]}` | Conflict behavior; defaults to `independent`. |
| `subtypes` | no | `{}` | `{"additionalProperties":{"$ref":"#/$defs/SubtypeSpec"},"type":"object"}` | Named subtypes of this type. |
