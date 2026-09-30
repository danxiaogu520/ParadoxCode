//! The rules-v2 source model (see `docs/rules-language.md`).
//!
//! These types mirror the JSON shape of a rules source file one-to-one,
//! including the mini-syntax strings (type expressions, schema references,
//! cards, def names, enum columns): those stay raw here so the compile pass
//! can parse them with full provenance (source file + JSON pointer +
//! expression-internal column) instead of losing position data inside
//! `Deserialize` errors.
//!
//! The JSON Schema for editors is generated from these types with `schemars`
//! (`rulec schema`); type expressions are only coarsely validated there and
//! precisely by `rulec check`.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One rules source file: any subset of the seven top-level sections. The
/// compiler merges same-named sections across files into one namespace per
/// section and rejects duplicate names.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuleFile {
    /// Path-selection rules: logical path → parser / root schema.
    #[serde(default)]
    pub files: BTreeMap<String, FileRule>,
    /// Block descriptions, keyed by schema name (formal parameters may be
    /// spelled in the key: `"on_action_body<S>"`).
    #[serde(default)]
    pub schemas: BTreeMap<String, SchemaSpec>,
    /// Pure structural field bundles, expanded at compile time.
    #[serde(default)]
    pub mixins: BTreeMap<String, MixinSpec>,
    /// Symbol namespaces: resolution / subtypes / open / builtin / impl.
    #[serde(default)]
    pub types: BTreeMap<String, TypeSpec>,
    /// Capabilities, bindings, and constraints, expanded at compile time.
    #[serde(default)]
    pub traits: BTreeMap<String, TraitSpec>,
    /// Enums, optionally with attribute columns.
    #[serde(default)]
    pub enums: BTreeMap<String, EnumSpec>,
    /// Scope types, registers, links, and compatibility overrides.
    #[serde(default)]
    pub scopes: Option<ScopesSpec>,
}

/// One `files` entry: which documents a rule selects and how their root
/// structure is described.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileRule {
    /// Logical directory prefix (the legacy `game/` prefix is not spelled).
    pub path: String,
    /// File extension without the dot.
    #[serde(default)]
    pub ext: Option<String>,
    /// Exact file name within `path`.
    #[serde(default)]
    pub file: Option<String>,
    /// When true, `path` does not recurse into subdirectories.
    #[serde(default)]
    pub strict: Option<bool>,
    /// Document parser; defaults to `script`.
    #[serde(default)]
    pub parser: Option<SourceParser>,
    /// Definition-priority policy; defaults to `replace-by-path`.
    #[serde(default)]
    pub resolution: Option<SourceFileResolution>,
    /// Root structure: a schema name, or a field spec with `def` describing a
    /// whole-file symbol instance. Parsers without script structure
    /// (`localisation`, `asset`) may omit it.
    #[serde(default)]
    pub root: Option<RootSpec>,
}

/// The document parser selected by a [`FileRule`].
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceParser {
    /// Paradox script.
    Script,
    /// `localisation/*.yml`.
    Localisation,
    /// Asset manifests (`.gfx`, `.asset`).
    Asset,
    /// Structure only: parse and report syntax, no semantic rules.
    SyntaxOnly,
}

/// The definition-priority policy selected by a [`FileRule`].
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceFileResolution {
    /// Later paths shadow earlier ones per file path (default).
    ReplaceByPath,
    /// All definitions remain visible.
    Merge,
    /// Later directories shadow earlier ones wholesale.
    ReplaceDirectory,
}

/// A [`FileRule`]'s root structure: either a schema name, or one field spec
/// carrying `def` for the whole-file-instance case.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(untagged)]
pub enum RootSpec {
    /// The name of the root schema.
    Schema(String),
    /// A field spec with `def` (and usually `body`) describing the file as a
    /// single symbol instance.
    Instance(Box<FieldSpec>),
}

/// A schema: a full block description, or one of the two schema-level
/// shorthands (`{"map": …}`, `{"list": …}`).
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(untagged)]
pub enum SchemaSpec {
    /// Full block description.
    Block(BlockSchema),
    /// Shorthand for a schema with a single map pattern.
    Map {
        /// The homogeneous map.
        map: MapSpec,
    },
    /// Shorthand for a schema with only `items`.
    List {
        /// The element type expression for bare values.
        list: String,
    },
}

/// The full description of one block.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BlockSchema {
    /// Mixin names, expanded at compile time.
    #[serde(default)]
    pub include: Vec<String>,
    /// Exact keys, hash lookup.
    #[serde(default)]
    pub fields: BTreeMap<String, FieldOverloads>,
    /// Non-exact keys, tried in written order.
    #[serde(default)]
    pub patterns: Vec<FieldSpec>,
    /// Type expression for bare values in this block (list blocks).
    #[serde(default)]
    pub items: Option<String>,
    /// Whether undeclared keys are allowed; defaults to false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub open: bool,
}

/// One exact key's field spec, or several shape overloads of it.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(untagged)]
pub enum FieldOverloads {
    /// A single field spec.
    One(Box<FieldSpec>),
    /// Several shape overloads, matched in written order.
    Many(Vec<FieldSpec>),
}

/// One field specification (an element of `patterns`, a value of `fields`, or
/// the `root` instance form). Exactly one of `value` / `body` / `list` / `map`
/// must be present (checked by the compiler).
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FieldSpec {
    /// Key type expression; only meaningful inside `patterns`.
    #[serde(default)]
    pub key: Option<String>,
    /// Scalar type expression for the value.
    #[serde(default)]
    pub value: Option<String>,
    /// Block: schema name (or `"self"`), optionally instantiated.
    #[serde(default)]
    pub body: Option<String>,
    /// Bare-value list block whose element type is this expression.
    #[serde(default)]
    pub list: Option<String>,
    /// Homogeneous map block.
    #[serde(default)]
    pub map: Option<MapSpec>,
    /// Cardinality, e.g. `"1"`, `"0..1"`, `"2..5"`; defaults to `"0..1"`.
    #[serde(default)]
    pub card: Option<String>,
    /// Scope effect: `in` / `push` / `set`.
    #[serde(default)]
    pub scope: Option<ScopeEffect>,
    /// Defines a symbol instance at this position.
    #[serde(default)]
    pub def: Option<DefSpec>,
    /// Only applies to instances of this subtype.
    #[serde(default)]
    pub when: Option<String>,
    /// Only applies to instances outside this subtype.
    #[serde(default)]
    pub unless: Option<String>,
    /// Control-flow primitive (field attribute, never a global key name).
    #[serde(default)]
    pub control: Option<ControlSpec>,
    /// Documentation text.
    #[serde(default)]
    pub doc: Option<String>,
    /// Diagnostic severity for violations of this field.
    #[serde(default)]
    pub severity: Option<Severity>,
    /// Whether the field is deprecated.
    #[serde(default)]
    pub deprecated: Option<bool>,
    /// Explicitly overrides a mixin-declared field of the same key.
    #[serde(default, rename = "override")]
    pub override_field: Option<bool>,
}

/// A homogeneous map block: same key and value shapes for every entry.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MapSpec {
    /// Key type expression (often `def<…>` or `enum<…>`).
    pub key: String,
    /// Scalar type expression for the value.
    #[serde(default)]
    pub value: Option<String>,
    /// Block: schema name (or `"self"`), optionally instantiated.
    #[serde(default)]
    pub body: Option<String>,
}

/// A mixin: a pure structural field bundle.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MixinSpec {
    /// Field specs contributed to every including schema.
    #[serde(default)]
    pub fields: BTreeMap<String, FieldOverloads>,
}

/// The scope effect of a field: only the object form exists.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeEffect {
    /// Scopes the field is valid in (empty = any).
    #[serde(default, rename = "in")]
    pub scope_in: Option<Vec<String>>,
    /// Scope entered by the nested block.
    #[serde(default)]
    pub push: Option<String>,
    /// Registers replaced on entry: register name → scope type.
    #[serde(default)]
    pub set: Option<BTreeMap<String, String>>,
}

/// Defines a symbol instance at the field's position.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DefSpec {
    /// Type name, optionally with the subtype granted at this position
    /// (`"event.country"`).
    #[serde(rename = "type")]
    pub type_name: String,
    /// Instance-name source: `"key"` (default), `"field:<name>"`, or `"file"`.
    #[serde(default)]
    pub name: Option<String>,
    /// Prefix removed from the derived instance name.
    #[serde(default)]
    pub strip_prefix: Option<String>,
    /// Suffix removed from the derived instance name.
    #[serde(default)]
    pub strip_suffix: Option<String>,
}

/// A subtype condition: a conjunction of "field → type expression", where a
/// `null` expression means the field must be absent.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(transparent)]
pub struct SubtypeCond(pub BTreeMap<String, Option<String>>);

/// A control-flow primitive attached to a field.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ControlSpec {
    /// Which primitive this field implements.
    pub kind: ControlKind,
    /// Guard sub-block key (`"limit"` for branch kinds).
    #[serde(default)]
    pub guard: Option<String>,
    /// Sibling keys that may continue this chain (`["else_if", "else"]`).
    #[serde(default)]
    pub chain: Option<Vec<String>>,
    /// Logic operator for `logic` kinds (`AND` / `OR` / `NOT`).
    #[serde(default)]
    pub op: Option<String>,
    /// Field name whose scalar values are the branch keys of a `switch`.
    #[serde(default)]
    pub on: Option<String>,
}

/// The closed set of control-flow primitives.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlKind {
    /// Head of an if-chain (`if`).
    Branch,
    /// Continuation of an if-chain (`else_if`, `else`).
    BranchContinue,
    /// Guard sub-block whose body is a trigger (`limit`).
    Guard,
    /// Boolean combinator (`AND` / `OR` / `NOT`).
    Logic,
    /// Value-as-key weighted branch (`random_list`).
    Weighted,
    /// Probability block (`random`).
    Chance,
    /// Branch keys validated against a trigger's values (`trigger_switch`).
    Switch,
    /// Scope-transparent wrapper (`hidden_effect`).
    Transparent,
    /// Display only, not executed (`tooltip`).
    DisplayOnly,
}

/// Diagnostic severity for a field's violations.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Errors (the default).
    Error,
    /// Warnings.
    Warning,
    /// Information.
    Info,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
        })
    }
}

/// One symbol namespace.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TypeSpec {
    /// Conflict behavior; defaults to `independent`.
    #[serde(default)]
    pub resolution: Option<TypeResolution>,
    /// Named subtypes of this type.
    #[serde(default)]
    pub subtypes: BTreeMap<String, SubtypeSpec>,
    /// Open world: any name may exist; `ref` never reports undefined names.
    #[serde(default)]
    pub open: Option<bool>,
    /// Engine-provided members.
    #[serde(default)]
    pub builtin: Option<Vec<String>>,
    /// Traits implemented by this type and their arguments.
    #[serde(default, rename = "impl")]
    pub trait_impls: BTreeMap<String, ImplSpec>,
}

/// The conflict behavior of a symbol type.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TypeResolution {
    /// Later definitions shadow earlier ones; same-name collisions warn.
    Replace,
    /// Structurally repeated keys stay independent (the default).
    Independent,
}

/// One named subtype.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SubtypeSpec {
    /// Instance-body predicate granting this subtype.
    #[serde(default)]
    pub when: Option<SubtypeCond>,
    /// Traits implemented only for instances of this subtype.
    #[serde(default, rename = "impl")]
    pub trait_impls: BTreeMap<String, ImplSpec>,
}

/// Arguments of one trait implementation: parameter name → value (a `$…`
/// instance-name template or a schema name, per the trait's contract).
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ImplSpec(pub BTreeMap<String, String>);

/// One trait: capabilities, bindings, and constraints.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TraitSpec {
    /// Parameters with defaults (`"$"` = the instance name) or no default.
    #[serde(default)]
    pub params: BTreeMap<String, Option<String>>,
    /// Symbol bindings contributed by the trait.
    #[serde(default)]
    pub bindings: BTreeMap<String, BindingSpec>,
    /// Requirements the implementing type must satisfy.
    #[serde(default)]
    pub requires: Option<RequiresSpec>,
    /// Capabilities the runtime interprets for this trait.
    #[serde(default)]
    pub capabilities: Option<Vec<String>>,
}

/// One trait binding: how a trait parameter maps to a localisation key or a
/// sprite name.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BindingSpec {
    /// Localisation-key template (`"{name}"` references a trait parameter).
    #[serde(default)]
    pub loc: Option<String>,
    /// Sprite-name template.
    #[serde(default)]
    pub sprite: Option<String>,
    /// Whether the bound symbol must resolve.
    #[serde(default)]
    pub required: Option<bool>,
}

/// Requirements of a trait.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RequiresSpec {
    /// Mixin that must be included by the schemas defining the impl'ing type.
    #[serde(default)]
    pub include: Option<String>,
}

/// One enum: plain members, or a table with attribute columns.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(untagged)]
pub enum EnumSpec {
    /// Shorthand: members without columns.
    Members(Vec<String>),
    /// A table with columns and rows.
    Table {
        /// Column name → column kind, e.g. `"scope": "scope_type?"`
        /// (the trailing `?` marks an optional column).
        columns: BTreeMap<String, String>,
        /// Row name → column values.
        rows: BTreeMap<String, BTreeMap<String, String>>,
    },
}

/// The scope model.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScopesSpec {
    /// Scope type names. `any` is reserved and not listed here.
    #[serde(default)]
    pub types: Vec<String>,
    /// Scope registers (`root`, `this`, `prev`, `from`, …).
    #[serde(default)]
    pub registers: BTreeMap<String, RegisterSpec>,
    /// Scope links, keyed by template text
    /// (`"event_target:{ref<event_target>}"`).
    #[serde(default)]
    pub links: BTreeMap<String, LinkSpec>,
    /// Compatibility overrides ("actual accepted where expected is required").
    #[serde(default)]
    pub compat: Vec<CompatSpec>,
}

/// One scope register.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RegisterSpec {
    /// Whether the register chains (`prev_prev`, `fromfrom`, …).
    #[serde(default)]
    pub chain: Option<bool>,
}

/// One scope link.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LinkSpec {
    /// Scopes the link may start from (`"any"` allowed).
    pub from: Vec<String>,
    /// Scope the link lands in (`"any"` allowed).
    pub to: String,
}

/// One compatibility override between scope types.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CompatSpec {
    /// The scope type actually produced.
    pub actual: String,
    /// The scope type accepted in its place.
    pub expected: String,
}

/// Renders the JSON Schema of the rules source language as pretty JSON.
///
/// This is the editor-facing artifact (`rules/rules-language.schema.json`),
/// generated by `rulec schema` and pinned by the `schema_artifact_is_current`
/// test so the checked-in copy cannot drift from these types. Type
/// expressions are plain strings here on purpose: the schema validates their
/// coarse shape only, and `rulec check` provides the precise grammar.
#[must_use]
pub fn json_schema_pretty() -> String {
    let schema = schemars::schema_for!(RuleFile);
    serde_json::to_string_pretty(&schema).expect("schema serializes")
        + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVENTS: &str = r#"{
      "files": {
        "events": { "path": "events", "ext": "txt", "root": "events_file" },
        "localisation": { "path": "localisation", "ext": "yml", "parser": "localisation" }
      },
      "schemas": {
        "events_file": { "fields": {
          "namespace":      { "value": "scalar", "card": "0..*" },
          "country_event":  { "def": { "type": "event.country",  "name": "field:id" },
                              "scope": { "set": { "root": "country",  "this": "country" } },
                              "body": "event_body", "card": "0..*" }
        }},
        "on_action_body<S>": { "fields": {
          "events":        { "list": "ref<event.$S>" },
          "random_events": { "map": { "key": "int", "value": "ref<event.$S> | '0'" } }
        }},
        "scripted_effects_file": { "map": { "key": "def<scripted_effect>", "body": "effect" } },
        "bare_list": { "list": "scalar" }
      },
      "mixins": {
        "gated": { "fields": { "potential": { "body": "trigger" } } }
      },
      "types": {
        "event": {
          "resolution": "replace",
          "subtypes": {
            "country": {},
            "triggered": { "when": { "is_triggered_only": "'yes'" } }
          }
        },
        "decision": {
          "impl": { "Localised": { "name": "$_title", "desc": "$_desc" } }
        }
      },
      "traits": {
        "Localised": {
          "params": { "name": "$", "desc": null },
          "bindings": { "name": { "loc": "{name}", "required": true }, "desc": { "loc": "{desc}" } }
        },
        "ModifierSource": { "requires": { "include": "modifier_block" } },
        "Callable": { "params": { "body": "schema" }, "capabilities": ["replacement", "condition"] }
      },
      "enums": {
        "dlc_event_pictures": ["one", "two"],
        "on_actions": {
          "columns": { "scope": "scope_type?", "from": "scope_type?" },
          "rows": {
            "on_startup": { "scope": "country" },
            "on_province_religion_converted": { "scope": "province" }
          }
        }
      },
      "scopes": {
        "types": ["country", "province", "unit"],
        "registers": { "root": {}, "this": {}, "prev": { "chain": true }, "from": { "chain": true } },
        "links": {
          "owner": { "from": ["province", "unit"], "to": "country" },
          "event_target:{ref<event_target>}": { "from": ["any"], "to": "any" }
        },
        "compat": [{ "actual": "trade_node", "expected": "province" }]
      }
    }"#;

    #[test]
    fn design_examples_deserialize() {
        let file: RuleFile = serde_json::from_str(EVENTS).expect("deserializes");
        assert_eq!(file.files.len(), 2);
        assert_eq!(
            file.files["events"].parser,
            None,
            "parser defaults are not spelled"
        );
        assert_eq!(
            file.files["localisation"].parser,
            Some(SourceParser::Localisation)
        );
        assert!(matches!(
            file.files["events"].root,
            Some(RootSpec::Schema(ref name)) if name == "events_file"
        ));
        assert!(matches!(
            file.schemas["scripted_effects_file"],
            SchemaSpec::Map { .. }
        ));
        assert!(matches!(file.schemas["bare_list"], SchemaSpec::List { .. }));
        let SchemaSpec::Block(actions) = &file.schemas["on_action_body<S>"] else {
            panic!("parameterized schema is a block schema");
        };
        assert_eq!(
            actions.fields["events"],
            FieldOverloads::One(Box::new(FieldSpec {
                key: None,
                value: None,
                body: None,
                list: Some("ref<event.$S>".to_owned()),
                map: None,
                card: None,
                scope: None,
                def: None,
                when: None,
                unless: None,
                control: None,
                doc: None,
                severity: None,
                deprecated: None,
                override_field: None,
            }))
        );
    }

    #[test]
    fn enums_accept_both_forms() {
        let file: RuleFile = serde_json::from_str(EVENTS).expect("deserializes");
        assert_eq!(
            file.enums["dlc_event_pictures"],
            EnumSpec::Members(vec!["one".to_owned(), "two".to_owned()])
        );
        let EnumSpec::Table { columns, rows } = &file.enums["on_actions"] else {
            panic!("on_actions is a table");
        };
        assert_eq!(columns["scope"], "scope_type?");
        assert_eq!(rows["on_startup"]["scope"], "country");
    }

    #[test]
    fn scopes_parse_links_and_compat() {
        let file: RuleFile = serde_json::from_str(EVENTS).expect("deserializes");
        let scopes = file.scopes.expect("scopes section");
        assert_eq!(scopes.types, ["country", "province", "unit"]);
        assert_eq!(scopes.registers["prev"].chain, Some(true));
        assert_eq!(
            scopes.links["owner"],
            LinkSpec {
                from: vec!["province".to_owned(), "unit".to_owned()],
                to: "country".to_owned(),
            }
        );
        assert_eq!(
            scopes.compat,
            vec![CompatSpec {
                actual: "trade_node".to_owned(),
                expected: "province".to_owned(),
            }]
        );
    }

    #[test]
    fn types_and_traits_parse() {
        let file: RuleFile = serde_json::from_str(EVENTS).expect("deserializes");
        assert_eq!(file.types["event"].resolution, Some(TypeResolution::Replace));
        assert_eq!(
            file.types["event"].subtypes["triggered"].when,
            Some(SubtypeCond(BTreeMap::from([(
                "is_triggered_only".to_owned(),
                Some("'yes'".to_owned()),
            )])))
        );
        assert_eq!(
            file.types["decision"].trait_impls["Localised"].0["name"],
            "$_title"
        );
        assert_eq!(
            file.traits["Localised"].params,
            BTreeMap::from([
                ("name".to_owned(), Some("$".to_owned())),
                ("desc".to_owned(), None),
            ])
        );
        assert_eq!(
            file.traits["ModifierSource"].requires.as_ref().map(|r| r.include.as_deref()),
            Some(Some("modifier_block"))
        );
    }

    #[test]
    fn when_conditions_accept_null_for_absence() {
        let cond: SubtypeCond =
            serde_json::from_str(r#"{ "is_triggered_only": "'yes'", "picture": null }"#)
                .expect("deserializes");
        assert_eq!(cond.0.get("picture"), Some(&None));
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let error = serde_json::from_str::<FileRule>(
            r#"{ "path": "events", "ext": "txt", "root": "events_file", "roots": {} }"#,
        )
        .expect_err("rejects unknown fields");
        assert!(error.to_string().contains("roots"), "{error}");
    }

    #[test]
    fn schema_shorthands_and_overloads_deserialize() {
        let overloads: FieldOverloads = serde_json::from_str(
            r#"[ { "value": "loc", "card": "0..*" }, { "body": "conditional_desc", "card": "0..*" } ]"#,
        )
        .expect("deserializes");
        assert!(matches!(overloads, FieldOverloads::Many(ref items) if items.len() == 2));

        let schema: SchemaSpec =
            serde_json::from_str(r#"{ "map": { "key": "int", "body": "self" } }"#)
                .expect("deserializes");
        assert!(matches!(schema, SchemaSpec::Map { .. }));
    }

    #[test]
    fn control_specs_parse_all_kinds() {
        let spec: ControlSpec = serde_json::from_str(
            r#"{ "kind": "branch", "guard": "limit", "chain": ["else_if", "else"] }"#,
        )
        .expect("deserializes");
        assert_eq!(spec.kind, ControlKind::Branch);
        let spec: ControlSpec =
            serde_json::from_str(r#"{ "kind": "switch", "on": "on_trigger" }"#)
                .expect("deserializes");
        assert_eq!(spec.kind, ControlKind::Switch);
        let spec: ControlSpec = serde_json::from_str(r#"{ "kind": "display_only" }"#)
            .expect("deserializes");
        assert_eq!(spec.kind, ControlKind::DisplayOnly);
    }

    #[test]
    fn schema_artifact_is_current() {
        let artifact = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../rules/rules-language.schema.json"
        );
        let checked_in = std::fs::read_to_string(artifact).unwrap_or_else(|failure| {
            panic!("rules/rules-language.schema.json is readable (run `rulec schema`): {failure}")
        });
        assert_eq!(
            checked_in,
            json_schema_pretty(),
            "the checked-in schema drifted from the source types; regenerate with `rulec schema`"
        );
    }

    #[test]
    fn json_schema_generates_for_editors() {
        let rendered = json_schema_pretty();
        let value: serde_json::Value = serde_json::from_str(&rendered).expect("valid JSON");
        assert_eq!(value["title"], "RuleFile");
        for section in ["files", "schemas", "mixins", "types", "traits", "enums", "scopes"] {
            assert!(
                value["properties"][section].is_object(),
                "`{section}` is described: {rendered}"
            );
        }
    }
}
