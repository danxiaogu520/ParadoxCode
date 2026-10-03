//! The rules-v2 source model (see `crates/rules/LANGUAGE.md`).
//!
//! These types mirror the JSON shape of a rules source file one-to-one,
//! including the mini-syntax strings (type expressions, schema references,
//! cards and def names): those stay raw here so the compile pass
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
    /// Named trait markers; implementation data is declared on each type.
    #[serde(default)]
    pub traits: BTreeMap<String, TraitSpec>,
    /// Enums as literal member lists.
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
    /// File extension without the dot, or a list of extensions (the legacy
    /// `extensions` list); absent means every extension.
    #[serde(default)]
    pub ext: Option<ExtSpec>,
    /// Exact file name within `path`.
    #[serde(default)]
    pub file: Option<String>,
    /// When true, `path` does not recurse into subdirectories.
    #[serde(default)]
    pub strict: Option<bool>,
    /// Path prefixes this entry does not apply to (the legacy
    /// `path_exclude_prefixes`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
    /// Document parser; defaults to `script`.
    #[serde(default)]
    pub parser: Option<SourceParser>,
    /// Definition-priority policy; defaults to `merge` (D14: the default must
    /// cover the corpus majority).
    #[serde(default = "default_file_resolution")]
    pub resolution: SourceFileResolution,
    /// Root structure: a schema name, or a field spec with `def` describing a
    /// whole-file symbol instance. Parsers without script structure
    /// (`localisation`, `asset`) may omit it.
    #[serde(default)]
    pub root: Option<RootSpec>,
}

/// One file extension, or a list of them. The singular form keeps the
/// one-extension case readable; the list form replaces the legacy
/// `extensions` array without splitting one category into several entries.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ExtSpec {
    /// One extension.
    One(String),
    /// Several extensions.
    Many(Vec<String>),
}

impl ExtSpec {
    /// Every extension this spec selects.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        match self {
            Self::One(ext) => std::slice::from_ref(ext).iter().map(String::as_str),
            Self::Many(exts) => exts.iter().map(String::as_str),
        }
    }
}

/// The `files` resolution default: `merge` (D14).
fn default_file_resolution() -> SourceFileResolution {
    SourceFileResolution::Merge
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
    /// Alternative combinations of direct-field occurrence bounds. At least
    /// one complete form must match; ordinary field bounds still apply.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forms: Vec<BlockForm>,
    /// Whether undeclared keys are allowed; defaults to false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub open: bool,
}

/// One legal combination of fields in a block. This describes structural
/// alternatives without weakening the requirements of every branch.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BlockForm {
    /// Exact field names and their occurrence bounds in this form.
    #[serde(default)]
    pub fields: BTreeMap<String, String>,
    /// Written pattern indices and their total occurrence bounds in this form.
    /// Exact fields are excluded from pattern counts by normal dispatch.
    #[serde(default)]
    pub patterns: BTreeMap<String, String>,
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
    /// Cardinality, e.g. `"1"`, `"0..1"`, `"2..5"`. Mandatory: the corpus has
    /// no majority value, and the field drives missing/repeated-key
    /// diagnostics, so it is never defaulted (D14).
    pub card: String,
    /// Scope effect: `in` / `push` / `set`.
    #[serde(default)]
    pub scope: Option<ScopeEffect>,
    /// Defines a symbol instance at this position.
    #[serde(default)]
    pub def: Option<DefSpec>,
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
    /// Schema supplying the scalar selector keys and their branch value matchers.
    #[serde(default)]
    pub selector_schema: Option<String>,
}

/// The closed set of control-flow primitives.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
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
    /// A predicate whose boolean value is its constant result.
    Constant,
}

/// Diagnostic severity for a field's violations.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
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
pub struct SubtypeSpec {}

/// Arguments of one trait implementation: binding (or parameter) name → value.
///
/// `Localised` and `HasIcon` take one [`BindingSpec`] per binding they
/// contribute, because the binding set is per-type data rather than a fixed
/// trait shape; the other built-ins take plain strings (`Callable`'s `body`).
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ImplSpec(pub BTreeMap<String, ImplValue>);

/// One trait-implementation argument.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ImplValue {
    /// A plain argument value (`Callable`'s `body` schema name).
    Text(String),
    /// One localisation or sprite binding contributed by the impl.
    Binding(BindingSpec),
}

impl ImplValue {
    /// The binding form, when this argument declares one.
    #[must_use]
    pub fn binding(&self) -> Option<&BindingSpec> {
        match self {
            Self::Binding(spec) => Some(spec),
            Self::Text(_) => None,
        }
    }

    /// The plain-text form, when this argument is one.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::Binding(_) => None,
        }
    }
}

/// One named trait marker; implementation data belongs to the type.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TraitSpec {}

/// One type implementation binding: a localisation-key or sprite-name template.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BindingSpec {
    /// Localisation-key template (`$` stands for the instance name).
    #[serde(default)]
    pub loc: Option<String>,
    /// Sprite-name template.
    #[serde(default)]
    pub sprite: Option<String>,
    /// Whether the bound symbol must resolve.
    #[serde(default)]
    pub required: Option<bool>,
}

/// An enum is a list of literal member names.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(untagged)]
pub enum EnumSpec {
    /// Literal members.
    Members(Vec<String>),
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
    /// Runtime register selected by this declared spelling.
    pub role: RegisterRole,
    /// Whether the register chains (`prev_prev`, `fromfrom`, …).
    #[serde(default)]
    pub chain: Option<bool>,
}

/// Game-independent state slots selected by scope register declarations.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegisterRole {
    /// Root scope of the invocation.
    Root,
    /// Current scope, without pushing a new frame.
    Current,
    /// Earlier scope frames, nearest first.
    Previous,
    /// Calling event's scope frames, nearest first.
    From,
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
/// Generated on demand by `rulec schema` for editors and used in memory by
/// the source-reference documentation generator. Type expressions are plain
/// strings here on purpose: the schema validates their
/// coarse shape only, and `rulec check` provides the precise grammar.
#[must_use]
pub fn json_schema_pretty() -> String {
    let schema = schemars::schema_for!(RuleFile);
    serde_json::to_string_pretty(&schema).expect("schema serializes") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVENTS: &str = r#"{
  "files": {
    "events": {
      "path": "events",
      "ext": "txt",
      "root": "events_file"
    },
    "localisation": {
      "path": "localisation",
      "ext": "yml",
      "parser": "localisation"
    }
  },
  "schemas": {
    "events_file": {
      "fields": {
        "namespace": {
          "value": "scalar",
          "card": "0..*"
        },
        "country_event": {
          "def": {
            "type": "event.country",
            "name": "field:id"
          },
          "scope": {
            "set": {
              "root": "country",
              "this": "country"
            }
          },
          "body": "event_body",
          "card": "0..*"
        }
      }
    },
    "on_action_body<S>": {
      "fields": {
        "events": {
          "list": "ref<event.$S>",
          "card": "0..*"
        },
        "random_events": {
          "map": {
            "key": "int",
            "value": "ref<event.$S> | '0'"
          },
          "card": "0..*"
        }
      }
    },
    "scripted_effects_file": {
      "map": {
        "key": "def<scripted_effect>",
        "body": "effect"
      }
    },
    "bare_list": {
      "list": "scalar"
    }
  },
  "mixins": {
    "gated": {
      "fields": {
        "potential": {
          "body": "trigger",
          "card": "0..1"
        }
      }
    }
  },
  "types": {
    "event": {
      "resolution": "replace",
      "subtypes": {
        "country": {},
        "triggered": {}
      }
    },
    "decision": {
      "impl": {
        "Localised": {
          "name": {
            "loc": "$_title",
            "required": true
          },
          "desc": {
            "loc": "$_desc"
          }
        }
      }
    }
  },
  "traits": {
    "Localised": {},
    "ModifierSource": {},
    "Callable": {}
  },
  "enums": {
    "dlc_event_pictures": [
      "one",
      "two"
    ],
    "on_actions_country": [
      "on_startup"
    ],
    "on_actions_province": [
      "on_province_religion_converted"
    ]
  },
  "scopes": {
    "types": [
      "country",
      "province",
      "unit",
      "district"
    ],
    "registers": {
      "root": {
        "role": "root"
      },
      "this": {
        "role": "current"
      },
      "prev": {
        "role": "previous",
        "chain": true
      },
      "from": {
        "role": "from",
        "chain": true
      }
    },
    "links": {
      "owner": {
        "from": [
          "province",
          "unit"
        ],
        "to": "country"
      },
      "event_target:{ref<event_target>}": {
        "from": [
          "any"
        ],
        "to": "any"
      }
    },
    "compat": [
      {
        "actual": "district",
        "expected": "province"
      }
    ]
  }
}"#;

    #[test]
    fn design_examples_deserialize() {
        let file: RuleFile = serde_json::from_str(EVENTS).expect("deserializes");
        assert_eq!(file.files.len(), 2);
        assert_eq!(
            file.files["events"].parser, None,
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
                card: "0..*".to_owned(),
                scope: None,
                def: None,

                control: None,
                doc: None,
                severity: None,
                deprecated: None,
                override_field: None,
            }))
        );
    }

    #[test]
    fn enums_accept_literal_members() {
        let file: RuleFile = serde_json::from_str(EVENTS).expect("deserializes");
        assert_eq!(
            file.enums["dlc_event_pictures"],
            EnumSpec::Members(vec!["one".to_owned(), "two".to_owned()])
        );
    }

    #[test]
    fn scopes_parse_links_and_compat() {
        let file: RuleFile = serde_json::from_str(EVENTS).expect("deserializes");
        let scopes = file.scopes.expect("scopes section");
        assert_eq!(scopes.types, ["country", "province", "unit", "district"]);
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
                actual: "district".to_owned(),
                expected: "province".to_owned(),
            }]
        );
    }

    #[test]
    fn types_and_traits_parse() {
        let file: RuleFile = serde_json::from_str(EVENTS).expect("deserializes");
        assert_eq!(
            file.types["event"].resolution,
            Some(TypeResolution::Replace)
        );
        assert!(file.types["event"].subtypes.contains_key("triggered"));
        // D19/Option A: a `Localised` impl enumerates its own bindings.
        let name = file.types["decision"].trait_impls["Localised"].0["name"]
            .binding()
            .expect("a binding argument");
        assert_eq!(name.loc.as_deref(), Some("$_title"));
        assert_eq!(name.required, Some(true));
        let desc = file.types["decision"].trait_impls["Localised"].0["desc"]
            .binding()
            .expect("a binding argument");
        assert_eq!(desc.loc.as_deref(), Some("$_desc"));
        assert_eq!(desc.required, None);
        assert!(file.traits.contains_key("Localised"));
    }

    #[test]
    fn condition_syntax_and_subtype_traits_are_rejected() {
        for key in ["when", "unless"] {
            let field = format!(r#"{{"value":"bool","card":"0..1","{key}":"enabled"}}"#);
            let error =
                serde_json::from_str::<FieldSpec>(&field).expect_err("removed field condition");
            assert!(error.to_string().contains(key), "{error}");
        }
        for subtype in [
            r#"{"when":{"enabled":"'yes'"}}"#,
            r#"{"impl":{"Localised":{"name":{"loc":"$"}}}}"#,
        ] {
            assert!(
                serde_json::from_str::<SubtypeSpec>(subtype).is_err(),
                "{subtype}"
            );
        }
    }

    #[test]
    fn unused_trait_metadata_enum_tables_and_file_policy_are_rejected() {
        for metadata in [
            r#"{"params":{"body":"schema"}}"#,
            r#"{"bindings":{"name":{"loc":"$"}}}"#,
            r#"{"requires":{"include":"modifier"}}"#,
            r#"{"capabilities":["replacement"]}"#,
        ] {
            assert!(
                serde_json::from_str::<TraitSpec>(metadata).is_err(),
                "{metadata}"
            );
        }
        assert!(
            serde_json::from_str::<EnumSpec>(
                r#"{"columns":{"scope":"scope_type"},"rows":{"on_startup":{"scope":"country"}}}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<FileRule>(r#"{"path":"x","resolution":"replace-directory"}"#)
                .is_err()
        );
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
        let spec: ControlSpec = serde_json::from_str(
            r#"{ "kind": "switch", "on": "on_trigger", "selector_schema": "predicates" }"#,
        )
        .expect("deserializes");
        assert_eq!(spec.kind, ControlKind::Switch);
        assert_eq!(spec.selector_schema.as_deref(), Some("predicates"));
        let spec: ControlSpec =
            serde_json::from_str(r#"{ "kind": "display_only" }"#).expect("deserializes");
        assert_eq!(spec.kind, ControlKind::DisplayOnly);
    }

    #[test]
    fn json_schema_generates_for_editors() {
        let rendered = json_schema_pretty();
        let value: serde_json::Value = serde_json::from_str(&rendered).expect("valid JSON");
        assert_eq!(value["title"], "RuleFile");
        for section in [
            "files", "schemas", "mixins", "types", "traits", "enums", "scopes",
        ] {
            assert!(
                value["properties"][section].is_object(),
                "`{section}` is described: {rendered}"
            );
        }
    }

    /// D14: `card` has no default, so a field spec without one is rejected
    /// while parsing — not silently widened to `0..1`.
    ///
    /// The direct `FieldSpec` parse carries the precise message; inside a
    /// `RuleFile` the untagged `SchemaSpec` wrapper reports the mismatch
    /// generically, so that case only asserts rejection.
    #[test]
    fn missing_card_is_a_parse_error() {
        let failure = serde_json::from_str::<FieldSpec>(r#"{ "value": "bool" }"#)
            .expect_err("a field spec must spell `card`");
        assert!(failure.to_string().contains("card"), "{failure}");
        assert!(
            serde_json::from_str::<RuleFile>(
                r#"{ "schemas": { "s": { "fields": { "a": { "value": "bool" } } } } }"#
            )
            .is_err(),
            "the document-level parse rejects a card-less field spec"
        );
    }

    /// D14: `files` resolution defaults to `merge`, and an extension list is
    /// spelled as an array.
    #[test]
    fn file_defaults_and_extension_lists() {
        let file: RuleFile = serde_json::from_str(
            r#"{
              "files": {
                "default": { "path": "a", "ext": ["txt", "gui"], "root": "s",
                             "exclude": ["a/vendor"] }
              },
              "schemas": { "s": { "fields": { "k": { "value": "bool", "card": "0..1" } } } }
            }"#,
        )
        .expect("deserializes");
        let rule = &file.files["default"];
        assert_eq!(rule.resolution, SourceFileResolution::Merge);
        assert_eq!(rule.exclude, vec!["a/vendor".to_owned()]);
        let Some(ExtSpec::Many(exts)) = &rule.ext else {
            panic!("the extension list survives: {:?}", rule.ext);
        };
        assert_eq!(exts, &vec!["txt".to_owned(), "gui".to_owned()]);
    }
}
