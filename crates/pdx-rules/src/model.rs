use crate::matcher::{FileMatcher, KeyMatcher, ValueMatcher};
use crate::runtime::RulesError;
use pdx_text::LogicalPath;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
/// Parser families understood by the workspace.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ParserKind {
    /// Paradox key/value script grammar.
    Script,
    /// Paradox localisation files.
    Localisation,
    /// A file consumed by an asset provider rather than a parser.
    Asset,
    /// A file where only syntax diagnostics are useful.
    SyntaxOnly,
}

impl ParserKind {
    pub(crate) fn as_str(&self) -> String {
        match self {
            Self::Script => "script".to_owned(),
            Self::Localisation => "localisation".to_owned(),
            Self::Asset => "asset".to_owned(),
            Self::SyntaxOnly => "syntax-only".to_owned(),
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, RulesError> {
        Ok(match value {
            "script" => Self::Script,
            "localisation" => Self::Localisation,
            // Legacy CSV dialects are mapped to syntax-only so that compiled
            // rule artifacts from earlier versions remain loadable without
            // requiring a full rule rebuild.
            "csv-comma" | "csv-tab" | "csv-semicolon" => Self::SyntaxOnly,
            "asset" => Self::Asset,
            "syntax-only" => Self::SyntaxOnly,
            other => return Err(RulesError::InvalidParser(other.to_owned())),
        })
    }
}

/// File-level conflict behavior used by source-root resolution.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FileResolutionPolicy {
    /// One candidate owns a relative path.
    ReplaceByRelativePath,
    /// All candidates contribute semantic content.
    Merge,
    /// A directory-level replacement policy.
    ReplaceDirectory,
}

impl FileResolutionPolicy {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ReplaceByRelativePath => "replace-by-relative-path",
            Self::Merge => "merge",
            Self::ReplaceDirectory => "replace-directory",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, RulesError> {
        match value {
            "replace-by-relative-path" => Ok(Self::ReplaceByRelativePath),
            "merge" => Ok(Self::Merge),
            "replace-directory" => Ok(Self::ReplaceDirectory),
            other => Err(RulesError::InvalidResolutionPolicy(other.to_owned())),
        }
    }
}

/// Symbol-level conflict behavior used by the index.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SymbolResolutionPolicy {
    /// A higher-priority definition shadows lower-priority definitions.
    ReplaceBySymbol,
    /// Definitions from all roots remain visible.
    Merge,
    /// Multiple definitions are an error at validation time.
    Unique,
}

impl SymbolResolutionPolicy {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ReplaceBySymbol => "replace-by-symbol",
            Self::Merge => "merge",
            Self::Unique => "unique",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, RulesError> {
        match value {
            "replace-by-symbol" => Ok(Self::ReplaceBySymbol),
            "merge" => Ok(Self::Merge),
            "unique" => Ok(Self::Unique),
            other => Err(RulesError::InvalidSymbolPolicy(other.to_owned())),
        }
    }
}
/// A complete file-category rule.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileCategory {
    /// Stable importer-assigned identifier.
    pub id: String,
    /// Parser selected for matching files.
    pub parser: ParserKind,
    /// Overlay conflict behavior.
    pub resolution: FileResolutionPolicy,
    /// Path matcher.
    pub matcher: FileMatcher,
}

/// The symbol policy for one semantic definition kind.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SymbolDescriptor {
    /// Stable semantic kind, for example `event` or `localisation`.
    pub kind_id: String,
    /// Conflict behavior.
    pub resolution: SymbolResolutionPolicy,
    /// Whether names are case-sensitive.
    pub case_sensitive: bool,
}
/// A normalized rule row. Values are intentionally scalar and deterministic; runtime crates do
/// not need to understand the first-party source representation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuleRecord {
    /// Normalized table family.
    pub table: String,
    /// Stable logical identity within the table.
    pub logical_id: String,
    /// Source order retained for diagnostics and deterministic lowering.
    pub source_order: u32,
    /// Normalized scalar fields.
    pub fields: BTreeMap<String, String>,
}
/// Source shape of a semantic rule.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleShape {
    /// The rule expects a nested block.
    Node,
    /// The rule expects a quoted scalar whose decoded payload is nested Script.
    QuotedScript,
    /// The rule expects a scalar leaf.
    Leaf,
    /// The rule describes a leaf value.
    LeafValue,
    /// The rule expects a value clause with nested alternatives.
    ValueClause,
}

/// Usage capabilities exposed by a scripted macro type.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptedMacroUsage {
    /// Whether the macro can be used as a replacement.
    #[serde(default, alias = "replace")]
    pub replacement: bool,
    /// Whether the macro can be used as a condition.
    #[serde(default)]
    pub condition: bool,
    /// Whether the macro can provide or consume a dynamic key.
    #[serde(default)]
    pub dynamic_key: bool,
    /// Whether the macro body or value is intentionally opaque text.
    #[serde(default)]
    pub opaque_text: bool,
}

impl ScriptedMacroUsage {
    /// Returns whether at least one usage capability is declared.
    #[must_use]
    pub const fn is_nonempty(&self) -> bool {
        self.replacement || self.condition || self.dynamic_key || self.opaque_text
    }
}

/// Semantic metadata for a type whose instances are scripted macros.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptedMacroDescriptor {
    /// Context used to validate the macro body, for example `effect` or `trigger`.
    pub body_context: String,
    /// Whether this type participates in scripted-macro expansion and lookup.
    #[serde(default, alias = "enabled")]
    pub macro_enabled: bool,
    /// Context-independent usage capabilities for the macro.
    #[serde(default)]
    pub usage: ScriptedMacroUsage,
}

/// File and root-selection metadata declared by a first-party type descriptor.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TypeDescriptor {
    /// Semantic type name.
    pub name: String,
    /// Directory prefix declared by `path`.
    pub path: Option<String>,
    /// Optional filename selector declared by `path_file`.
    pub path_file: Option<String>,
    /// Optional extension selector declared by `path_extension`.
    pub path_extension: Option<String>,
    /// Whether the path selector is strict.
    pub path_strict: bool,
    /// Whether one type instance is expected per file.
    pub type_per_file: bool,
    /// Wrapper-key paths that should be skipped before validating this type.
    pub skip_root_paths: Vec<Vec<String>>,
    /// Optional field carrying the definition name.
    pub name_field: Option<String>,
    /// Whether the filename supplies the definition name.
    pub name_from_file: bool,
    /// Optional first-party `starts_with` discriminator.
    pub starts_with: Option<String>,
    /// Optional filter for keys that instantiate this type, paired with its negation flag.
    ///
    /// Negated filters are represented by `negate = true`.
    pub type_key_filter: Option<(Vec<String>, bool)>,
    /// Optional generic scripted-macro capabilities for this type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scripted_macro: Option<ScriptedMacroDescriptor>,
}

/// One type-instance to localisation-key mapping from the first-party rule source.
///
/// A template contains exactly one `$` placeholder, which is replaced by the concrete
/// definition name. An explicit field mapping has no generated template; its value is
/// validated by the ordinary semantic localisation rules instead.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LocalisationBinding {
    /// Semantic type whose instances own this mapping.
    #[serde(rename = "type")]
    pub type_name: String,
    /// Descriptive source field name, retained for diagnostics and stable identity.
    pub field: String,
    /// Generated key template, when this is not an explicit field mapping.
    pub template: Option<String>,
    /// Whether the generated key must exist.
    #[serde(default)]
    pub required: bool,
    /// Whether the generated key is an optional game convention.
    #[serde(default)]
    pub optional: bool,
    /// Subtype condition under which the mapping applies.
    #[serde(default)]
    pub subtype: Option<String>,
    /// Structural condition that selects the subtype, when it is not represented by a
    /// same-named child field.
    #[serde(default)]
    pub condition: Option<LocalisationBindingCondition>,
    /// Explicit source field whose value is the localisation key.
    #[serde(default)]
    pub explicit_field: Option<String>,
}

/// Data-driven structural selector for one localisation binding subtype.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LocalisationBindingCondition {
    /// Direct child field whose presence/value selects the subtype.
    #[serde(default)]
    pub field: Option<String>,
    /// Optional scalar value required for `field`.
    #[serde(default)]
    pub value: Option<String>,
    /// Prefix applied to the concrete type-instance name.
    #[serde(default)]
    pub key_prefix: Option<String>,
}

impl RuleShape {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::QuotedScript => "quoted-script",
            Self::Leaf => "leaf",
            Self::LeafValue => "leaf-value",
            Self::ValueClause => "value-clause",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, RulesError> {
        match value {
            "node" => Ok(Self::Node),
            "quoted-script" => Ok(Self::QuotedScript),
            "leaf" => Ok(Self::Leaf),
            "leaf-value" => Ok(Self::LeafValue),
            "value-clause" => Ok(Self::ValueClause),
            other => Err(RulesError::InvalidRuleShape(other.to_owned())),
        }
    }
}

/// One executable rule alternative lowered from a first-party declaration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticRule {
    /// Stable source-derived identity.
    pub id: String,
    /// Semantic root, such as `trigger`, `effect`, or `type:event`.
    pub context: String,
    /// Parent keys below the semantic root.
    pub parent_path: Vec<String>,
    /// Key matcher.
    pub key: KeyMatcher,
    /// Operator used by the first-party declaration, when it is semantically significant.
    pub operator: Option<String>,
    /// Value matcher.
    pub value: ValueMatcher,
    /// Source shape.
    pub shape: RuleShape,
    /// Semantic context to use for children of this block.
    pub child_context: Option<String>,
    /// Source alias alternative this rule belongs to, when the first-party declaration has alternatives.
    pub alternative_id: Option<String>,
    /// Optional LSP severity from the semantic rule (`1` error, `2` warning, `3` info).
    pub severity: Option<u8>,
    /// Whether the first-party declaration explicitly requires this field.
    pub required: bool,
    /// Whether the first-party declaration marks this rule as deprecated.
    #[serde(default)]
    pub deprecated: bool,
    /// Documentation comments attached to the first-party declaration.
    pub documentation: Vec<String>,
    /// Scopes in which this rule is valid. An empty list means `scope = any` or no restriction.
    pub allowed_scopes: Vec<String>,
    /// Scope entered by a nested block matched by this rule.
    pub push_scope: Option<String>,
    /// Scope registers replaced by this rule, represented as `(register, scope)` pairs.
    pub replace_scope: Vec<(String, String)>,
    /// Minimum number of occurrences when specified by source cardinality.
    pub min_occurs: Option<u32>,
    /// Whether a minimum violation is strict (`cardinality` without `~`).
    pub strict_min: bool,
    /// Maximum number of occurrences when specified by source cardinality.
    pub max_occurs: Option<u32>,
    /// Source file retained for explainable diagnostics.
    pub source_file: String,
    /// One-based source line retained for explainable diagnostics.
    pub line: u32,
}

/// Static semantic rule data needed by runtime matching.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticModel {
    /// Compiled rule alternatives.
    pub rules: Vec<SemanticRule>,
    /// Enum names and their statically declared members.
    pub enum_values: BTreeMap<String, Vec<String>>,
    /// Concrete root keys selected by `type_key_filter` for each semantic type.
    pub type_root_keys: BTreeMap<String, Vec<String>>,
    /// Initial scope selected by a type subtype's `type_key_filter` and `push_scope`.
    pub type_root_scopes: BTreeMap<String, BTreeMap<String, String>>,
    /// File/root metadata declared by semantic type blocks.
    pub type_descriptors: BTreeMap<String, TypeDescriptor>,
    /// Type-instance to localisation-key mappings.
    pub localisation_bindings: Vec<LocalisationBinding>,
}

/// Normalized logical contents of one game rule database.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RulesModel {
    /// Stable game profile identity, for example `eu4`.
    pub game_id: String,
    /// File classification catalog.
    pub file_categories: Vec<FileCategory>,
    /// Symbol descriptors.
    pub symbol_descriptors: Vec<SymbolDescriptor>,
    /// Normalized semantic rule rows.
    pub records: Vec<RuleRecord>,
    /// Executable semantic matcher model used by semantic analysis.
    pub semantic: SemanticModel,
}

impl RulesModel {
    /// Returns the most specific matching category in stable catalog order.
    #[must_use]
    pub fn classify(&self, path: &LogicalPath) -> Option<&FileCategory> {
        self.file_categories
            .iter()
            .filter(|category| category.matcher.matches(path))
            .max_by_key(|category| category.matcher.specificity())
    }
}
