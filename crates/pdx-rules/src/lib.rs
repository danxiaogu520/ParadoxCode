//! Game-independent PDX rules schema, runtime, and first-party compiler.
//!
//! This crate owns the normalized runtime model, read-only loading, validation, the canonical
//! logical hash, and the first-party rule compiler (`pdx-bake`). The SQLite layout is
//! deliberately boring so the runtime remains inspectable without an authoring-format parser.

pub mod rulec;

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use pdx_text::LogicalPath;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The first runtime schema version reserved for the generated rule database.
pub const CURRENT_SCHEMA_VERSION: u32 = 15;

static EMBEDDED_LOAD_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A stable digest of canonical rule content.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct RuleHash([u8; 32]);

impl RuleHash {
    /// Returns the all-zero placeholder hash used by the empty Phase 0 database.
    #[must_use]
    pub const fn empty() -> Self {
        Self([0; 32])
    }

    /// Returns the raw digest bytes.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }

    /// Creates a hash from raw digest bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the lower-case hexadecimal form stored in manifests and diagnostics.
    #[must_use]
    pub fn to_hex(self) -> String {
        self.0.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// Parses a lower-case or upper-case hexadecimal SHA-256 digest.
    pub fn from_hex(value: &str) -> Result<Self, RulesError> {
        if value.len() != 64 {
            return Err(RulesError::InvalidHash(value.to_owned()));
        }
        let mut bytes = [0_u8; 32];
        for (index, slot) in bytes.iter_mut().enumerate() {
            *slot = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
                .map_err(|_| RulesError::InvalidHash(value.to_owned()))?;
        }
        Ok(Self(bytes))
    }
}

impl fmt::Debug for RuleHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("RuleHash").field(&self.0).finish()
    }
}

/// Text encoding policy selected by a game profile for source files.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SourceEncoding {
    /// Strict UTF-8 source, which is the generic engine default.
    #[default]
    Utf8,
    /// Legacy Windows-1252 source decoded to UTF-8 before parsing.
    Windows1252,
}

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
    fn as_str(&self) -> String {
        match self {
            Self::Script => "script".to_owned(),
            Self::Localisation => "localisation".to_owned(),
            Self::Asset => "asset".to_owned(),
            Self::SyntaxOnly => "syntax-only".to_owned(),
        }
    }

    fn parse(value: &str) -> Result<Self, RulesError> {
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
    fn as_str(self) -> &'static str {
        match self {
            Self::ReplaceByRelativePath => "replace-by-relative-path",
            Self::Merge => "merge",
            Self::ReplaceDirectory => "replace-directory",
        }
    }

    fn parse(value: &str) -> Result<Self, RulesError> {
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
    fn as_str(self) -> &'static str {
        match self {
            Self::ReplaceBySymbol => "replace-by-symbol",
            Self::Merge => "merge",
            Self::Unique => "unique",
        }
    }

    fn parse(value: &str) -> Result<Self, RulesError> {
        match value {
            "replace-by-symbol" => Ok(Self::ReplaceBySymbol),
            "merge" => Ok(Self::Merge),
            "unique" => Ok(Self::Unique),
            other => Err(RulesError::InvalidSymbolPolicy(other.to_owned())),
        }
    }
}

/// A path matcher from the rules file-category catalog.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileMatcher {
    /// Optional path prefix, without a leading slash.
    pub path_prefix: Option<String>,
    /// Accepted file extensions without the leading dot.
    pub extensions: Vec<String>,
    /// Optional suffix match on the logical path.
    pub path_suffix: Option<String>,
    /// Whether path and extension matching preserves case.
    pub case_sensitive: bool,
}

impl FileMatcher {
    /// Matches a validated logical path.
    #[must_use]
    pub fn matches(&self, path: &LogicalPath) -> bool {
        let candidate = path.as_str();
        if let Some(prefix) = &self.path_prefix {
            let is_directory_prefix = if self.case_sensitive {
                candidate == prefix
                    || candidate
                        .strip_prefix(prefix)
                        .is_some_and(|remainder| remainder.starts_with('/'))
            } else {
                candidate.len() == prefix.len() && candidate.eq_ignore_ascii_case(prefix)
                    || candidate.len() > prefix.len()
                        && candidate
                            .get(..prefix.len())
                            .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
                        && candidate.as_bytes().get(prefix.len()) == Some(&b'/')
            };
            if !is_directory_prefix {
                return false;
            }
        }
        if let Some(suffix) = &self.path_suffix {
            let matches_suffix = if self.case_sensitive {
                candidate.ends_with(suffix)
            } else {
                candidate.len() >= suffix.len()
                    && candidate
                        .get(candidate.len() - suffix.len()..)
                        .is_some_and(|tail| tail.eq_ignore_ascii_case(suffix))
            };
            if !matches_suffix {
                return false;
            }
        }
        if self.extensions.is_empty() {
            return true;
        }
        let Some(extension) = candidate.rsplit_once('.').map(|(_, extension)| extension) else {
            return false;
        };
        self.extensions.iter().any(|item| {
            if self.case_sensitive {
                item == extension
            } else {
                item.eq_ignore_ascii_case(extension)
            }
        })
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

/// Matching mode for small, data-only game-profile selectors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileMatchMode {
    /// Accepts every candidate.
    Any,
    /// Requires the whole candidate to match.
    Exact,
    /// Requires the candidate to start with the pattern.
    Prefix,
    /// Requires the candidate to end with the pattern.
    Suffix,
    /// Requires the candidate to contain the pattern.
    Contains,
    /// Requires a logical path's immediate parent directory to match the pattern.
    Directory,
}

/// A deterministic text selector used by built-in game-profile data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileTextMatcher {
    /// Match operation.
    pub mode: ProfileMatchMode,
    /// Pattern used by every mode except [`ProfileMatchMode::Any`].
    pub pattern: String,
    /// Whether ASCII case is significant.
    pub case_sensitive: bool,
}

impl ProfileTextMatcher {
    /// Creates a case-insensitive selector.
    #[must_use]
    pub fn insensitive(mode: ProfileMatchMode, pattern: impl Into<String>) -> Self {
        Self {
            mode,
            pattern: pattern.into(),
            case_sensitive: false,
        }
    }

    /// Creates a selector that accepts every candidate.
    #[must_use]
    pub fn any() -> Self {
        Self::insensitive(ProfileMatchMode::Any, "")
    }

    /// Tests one candidate without allocating for case-insensitive matching.
    #[must_use]
    pub fn matches(&self, candidate: &str) -> bool {
        if self.mode == ProfileMatchMode::Any {
            return true;
        }
        if self.case_sensitive {
            return match self.mode {
                ProfileMatchMode::Any => true,
                ProfileMatchMode::Exact => candidate == self.pattern,
                ProfileMatchMode::Prefix => candidate.starts_with(&self.pattern),
                ProfileMatchMode::Suffix => candidate.ends_with(&self.pattern),
                ProfileMatchMode::Contains => candidate.contains(&self.pattern),
                ProfileMatchMode::Directory => {
                    candidate
                        .rsplit_once('/')
                        .map_or("", |(directory, _)| directory)
                        == self.pattern
                }
            };
        }
        match self.mode {
            ProfileMatchMode::Any => true,
            ProfileMatchMode::Exact => candidate.eq_ignore_ascii_case(&self.pattern),
            ProfileMatchMode::Prefix => candidate
                .get(..self.pattern.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(&self.pattern)),
            ProfileMatchMode::Suffix => candidate
                .get(candidate.len().saturating_sub(self.pattern.len())..)
                .is_some_and(|suffix| suffix.eq_ignore_ascii_case(&self.pattern)),
            ProfileMatchMode::Contains => {
                self.pattern.is_empty()
                    || candidate
                        .as_bytes()
                        .windows(self.pattern.len())
                        .any(|window| window.eq_ignore_ascii_case(self.pattern.as_bytes()))
            }
            ProfileMatchMode::Directory => candidate
                .rsplit_once('/')
                .map_or("", |(directory, _)| directory)
                .eq_ignore_ascii_case(&self.pattern),
        }
    }
}

/// One top-level symbol-definition interpretation supplied by a game profile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileDefinitionRule {
    /// Logical-path selector.
    pub path: ProfileTextMatcher,
    /// Top-level property-key selector.
    pub key: ProfileTextMatcher,
    /// Stable symbol kind.
    pub kind: String,
    /// Optional nested scalar field that supplies the definition name.
    pub name_field: Option<String>,
    /// Whether parser recovery must have produced a value wrapper.
    pub requires_value: bool,
}

/// One scalar-reference interpretation supplied by a game profile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileReferenceRule {
    /// Property-key selector.
    pub key: ProfileTextMatcher,
    /// Stable target symbol kind.
    pub kind: String,
}

/// One scalar value that declares a workspace symbol.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileValueDefinitionRule {
    /// Property-key selector.
    pub key: ProfileTextMatcher,
    /// Optional immediate parent-key selector.
    pub parent_key: Option<ProfileTextMatcher>,
    /// Stable declared symbol kind.
    pub kind: String,
}

/// One block whose direct child keys declare workspace symbols.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileContainerDefinitionRule {
    /// Logical-path selector.
    pub path: ProfileTextMatcher,
    /// Top-level container-key selector.
    pub key: ProfileTextMatcher,
    /// Stable child symbol kind.
    pub kind: String,
}

/// One definition emitted when nested scalar conditions are satisfied.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileConditionalDefinitionRule {
    /// Logical-path selector.
    pub path: ProfileTextMatcher,
    /// Stable emitted symbol kind.
    pub kind: String,
    /// Nested scalar field that must exist.
    pub required_field: String,
    /// Required scalar spelling.
    pub required_value: String,
    /// Nested field that must be absent.
    pub absent_field: String,
}

/// One delimited identifier embedded in parser tokens.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileTokenDefinitionRule {
    /// Logical-path selector.
    pub path: ProfileTextMatcher,
    /// Opening and closing delimiter.
    pub delimiter: char,
    /// Kind emitted for the inner spelling.
    pub inner_kind: String,
    /// Kind emitted for the delimiter-wrapped spelling.
    pub wrapped_kind: String,
}

/// One root property that selects an initial semantic scope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileRootScopeRule {
    /// Root property-key selector.
    pub key: ProfileTextMatcher,
    /// Initial root/current scope.
    pub scope: String,
}

/// One asymmetric scope compatibility accepted by a game profile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileScopeCompatibility {
    /// Actual current scope.
    pub actual: String,
    /// Rule-expected scope accepted for the actual scope.
    pub expected: String,
}

/// Data-only game-specific interpretation selected by the composition root.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GameProfile {
    /// Stable identity shared with the selected rules artifact.
    pub game_id: String,
    /// Source text encoding policy used before parser input is materialized.
    pub source_encoding: SourceEncoding,
    /// Logical directory whitelist used for source-root discovery.
    ///
    /// An empty whitelist means that the profile does not authorize disk discovery. This keeps
    /// the generic engine conservative until a game profile supplies its resource roots.
    pub scan_roots: Vec<String>,
    /// Optional file-extension whitelist used after directory discovery.
    ///
    /// Entries omit the leading dot and are compared case-insensitively. An empty list keeps
    /// every extension, preserving the generic profile behavior.
    pub scan_extensions: Vec<String>,
    /// Ordered top-level definition rules; the first match wins.
    pub definitions: Vec<ProfileDefinitionRule>,
    /// Ordered scalar-reference rules; the first match wins.
    pub references: Vec<ProfileReferenceRule>,
    /// Ordered scalar value-definition rules; the first match wins.
    pub value_definitions: Vec<ProfileValueDefinitionRule>,
    /// Blocks whose direct child keys declare symbols.
    pub container_definitions: Vec<ProfileContainerDefinitionRule>,
    /// Additional definitions gated by nested fields.
    pub conditional_definitions: Vec<ProfileConditionalDefinitionRule>,
    /// Delimited identifiers embedded in parser tokens.
    pub token_definitions: Vec<ProfileTokenDefinitionRule>,
    /// Known concrete scopes and scope expressions.
    pub scope_names: Vec<String>,
    /// Scope spellings offered by completion.
    pub scope_completions: Vec<String>,
    /// Root-key fallbacks used when semantic type metadata has no initial scope.
    pub root_scopes: Vec<ProfileRootScopeRule>,
    /// Additional asymmetric scope compatibility pairs.
    pub scope_compatibilities: Vec<ProfileScopeCompatibility>,
    /// Property keys whose blocks are transparent logical wrappers.
    ///
    /// A wrapper preserves the current semantic context and scope while its children are
    /// validated.  The spelling is game-specific (for example, EU4's AND/OR/NOT).
    pub transparent_scope_wrappers: Vec<String>,
    /// semantic type/enum spellings mapped to workspace symbol kinds.
    pub member_kind_aliases: BTreeMap<String, String>,
    /// Profile fallback keys used when no imported semantic rule selects a property.
    pub fallback_keys: Vec<String>,
    /// Additional static enum members supplied by the profile.
    pub enum_extra_members: BTreeMap<String, Vec<String>>,
}

impl GameProfile {
    /// Creates an identity-only profile with no game-specific interpretation.
    #[must_use]
    pub fn empty(game_id: impl Into<String>) -> Self {
        Self {
            game_id: game_id.into(),
            source_encoding: SourceEncoding::Utf8,
            scan_roots: Vec::new(),
            scan_extensions: Vec::new(),
            definitions: Vec::new(),
            references: Vec::new(),
            value_definitions: Vec::new(),
            container_definitions: Vec::new(),
            conditional_definitions: Vec::new(),
            token_definitions: Vec::new(),
            scope_names: Vec::new(),
            scope_completions: Vec::new(),
            root_scopes: Vec::new(),
            scope_compatibilities: Vec::new(),
            transparent_scope_wrappers: Vec::new(),
            member_kind_aliases: BTreeMap::new(),
            fallback_keys: Vec::new(),
            enum_extra_members: BTreeMap::new(),
        }
    }

    /// Returns the logical directory whitelist used for source-root discovery.
    #[must_use]
    pub fn scan_roots(&self) -> &[String] {
        &self.scan_roots
    }

    /// Returns the optional file-extension whitelist used during source discovery.
    #[must_use]
    pub fn scan_extensions(&self) -> &[String] {
        &self.scan_extensions
    }

    /// Returns whether a logical file path belongs to a whitelisted scan directory.
    #[must_use]
    pub fn allows_scan_path(&self, logical_path: &str) -> bool {
        self.scan_roots.iter().any(|root| {
            root.is_empty()
                || logical_path == root
                || logical_path
                    .strip_prefix(root)
                    .is_some_and(|remainder| remainder.starts_with('/'))
        })
    }

    /// Returns whether a logical file path belongs to the directory and extension whitelists.
    #[must_use]
    pub fn allows_scan_file(&self, logical_path: &str) -> bool {
        if !self.allows_scan_path(logical_path) || self.scan_extensions.is_empty() {
            return self.allows_scan_path(logical_path);
        }
        let Some(extension) = logical_path
            .rsplit('/')
            .next()
            .and_then(|name| name.rsplit_once('.'))
            .map(|(_, extension)| extension)
        else {
            return false;
        };
        self.scan_extensions.iter().any(|allowed| {
            extension.eq_ignore_ascii_case(allowed.strip_prefix('.').unwrap_or(allowed))
        })
    }

    /// Returns the first matching top-level definition rule.
    #[must_use]
    pub fn definition(&self, logical_path: &str, key: &str) -> Option<&ProfileDefinitionRule> {
        self.definitions
            .iter()
            .find(|rule| rule.path.matches(logical_path) && rule.key.matches(key))
    }

    /// Returns the target kind for a scalar property reference.
    #[must_use]
    pub fn reference_kind(&self, key: &str) -> Option<&str> {
        self.references
            .iter()
            .find(|rule| rule.key.matches(key))
            .map(|rule| rule.kind.as_str())
    }

    /// Returns the declared kind for one scalar value property.
    #[must_use]
    pub fn value_definition_kind(&self, key: &str, parent_key: Option<&str>) -> Option<&str> {
        self.value_definitions
            .iter()
            .find(|rule| {
                rule.key.matches(key)
                    && rule.parent_key.as_ref().is_none_or(|matcher| {
                        parent_key.is_some_and(|parent| matcher.matches(parent))
                    })
            })
            .map(|rule| rule.kind.as_str())
    }

    /// Returns the profile fallback scope for one root key.
    #[must_use]
    pub fn root_scope(&self, key: &str) -> Option<&str> {
        self.root_scopes
            .iter()
            .find(|rule| rule.key.matches(key))
            .map(|rule| rule.scope.as_str())
    }

    /// Returns whether a scope spelling is known to this profile.
    #[must_use]
    pub fn is_scope(&self, value: &str) -> bool {
        self.scope_names
            .iter()
            .any(|scope| scope.eq_ignore_ascii_case(value))
    }

    /// Tests profile scope compatibility, including the generic `any` scope.
    #[must_use]
    pub fn scopes_compatible(&self, actual: &str, expected: &str) -> bool {
        actual.eq_ignore_ascii_case("any")
            || expected.eq_ignore_ascii_case("any")
            || actual.eq_ignore_ascii_case(expected)
            || self.scope_compatibilities.iter().any(|pair| {
                pair.actual.eq_ignore_ascii_case(actual)
                    && pair.expected.eq_ignore_ascii_case(expected)
            })
    }

    /// Returns whether a property is a profile-defined transparent logical wrapper.
    #[must_use]
    pub fn is_transparent_scope_wrapper(&self, key: &str) -> bool {
        self.transparent_scope_wrappers
            .iter()
            .any(|wrapper| wrapper.eq_ignore_ascii_case(key))
    }

    /// Returns the workspace symbol kind aliased by one semantic member name.
    #[must_use]
    pub fn member_kind_alias(&self, name: &str) -> Option<&str> {
        self.member_kind_aliases
            .iter()
            .find(|(alias, _)| alias.eq_ignore_ascii_case(name))
            .map(|(_, kind)| kind.as_str())
    }

    /// Returns whether a profile-specific extra enum member is known.
    #[must_use]
    pub fn enum_extra_member(&self, enum_name: &str, member: &str) -> bool {
        self.enum_extra_members.iter().any(|(name, members)| {
            name.eq_ignore_ascii_case(enum_name)
                && members
                    .iter()
                    .any(|value| value.eq_ignore_ascii_case(member))
        })
    }
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

/// A key matcher compiled from a first-party field declaration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyMatcher {
    /// Matches one concrete script key.
    Exact(String),
    /// Matches a key supplied by the workspace index for a named type.
    Type(String),
    /// Matches a member of a named static enum.
    Enum(String),
    /// Matches any non-empty scalar key.
    AnyScalar,
    /// Matches a key that declares a dynamic value set.
    Dynamic(String),
}

impl KeyMatcher {
    /// Tests a key against static and workspace-provided members.
    #[must_use]
    pub fn matches(
        &self,
        key: &str,
        type_members: impl Fn(&str, &str) -> bool,
        enum_members: impl Fn(&str, &str) -> bool,
    ) -> bool {
        match self {
            Self::Exact(expected) => expected.eq_ignore_ascii_case(key),
            Self::Type(type_name) => type_members(type_name, key),
            Self::Enum(enum_name) => enum_members(enum_name, key),
            Self::AnyScalar => !key.is_empty(),
            Self::Dynamic(_) => !key.is_empty(),
        }
    }
}

/// A value matcher compiled from a first-party field declaration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueMatcher {
    /// Accepts any scalar value.
    AnyScalar,
    /// Accepts one exact scalar value.
    Exact(String),
    /// Accepts `yes` or `no`.
    Bool,
    /// Accepts an integer, optionally constrained by an inclusive range.
    Int { min: Option<i64>, max: Option<i64> },
    /// Accepts a floating point value, optionally constrained by an inclusive range.
    Float {
        min: Option<String>,
        max: Option<String>,
    },
    /// Accepts a member supplied by the workspace index.
    Type(String),
    /// Accepts a member of a named static enum.
    Enum(String),
    /// Accepts a known scope name.
    Scope(Option<String>),
    /// Accepts a localisation key.
    Localisation,
    /// Accepts a path-like scalar.
    Filepath,
    /// Accepts a workspace- or scope-derived value set.
    Dynamic(String),
    /// Accepts any non-empty value while defining a dynamic value set.
    DynamicSet(String),
    /// Retains a semantic matcher that has not been implemented yet.
    Opaque(String),
}

impl ValueMatcher {
    /// Tests a scalar value against the compiled matcher.
    #[must_use]
    pub fn matches(
        &self,
        value: &str,
        type_members: impl Fn(&str, &str) -> bool,
        enum_members: impl Fn(&str, &str) -> bool,
        scopes: impl Fn(Option<&str>, &str) -> bool,
    ) -> bool {
        match self {
            Self::AnyScalar | Self::Opaque(_) => true,
            Self::Exact(expected) => expected == value,
            Self::Bool => matches!(value.to_ascii_lowercase().as_str(), "yes" | "no"),
            Self::Int { min, max } => {
                let Ok(value) = value.parse::<i64>() else {
                    return false;
                };
                min.is_none_or(|min| value >= min) && max.is_none_or(|max| value <= max)
            }
            Self::Float { min, max } => {
                let Ok(value) = value.parse::<f64>() else {
                    return false;
                };
                let lower = min.as_deref().and_then(|min| min.parse::<f64>().ok());
                let upper = max.as_deref().and_then(|max| max.parse::<f64>().ok());
                lower.is_none_or(|min| value >= min) && upper.is_none_or(|max| value <= max)
            }
            Self::Type(type_name) => type_members(type_name, value),
            Self::Enum(enum_name) => enum_members(enum_name, value),
            Self::Scope(scope) => scopes(scope.as_deref(), value),
            Self::Localisation => !value.is_empty(),
            Self::Filepath | Self::Dynamic(_) | Self::DynamicSet(_) => !value.is_empty(),
        }
    }
}

/// Source shape of a semantic rule.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleShape {
    /// The rule expects a nested block.
    Node,
    /// The rule expects a scalar leaf.
    Leaf,
    /// The rule describes a leaf value.
    LeafValue,
    /// The rule expects a value clause with nested alternatives.
    ValueClause,
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
    fn as_str(self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Leaf => "leaf",
            Self::LeafValue => "leaf-value",
            Self::ValueClause => "value-clause",
        }
    }

    fn parse(value: &str) -> Result<Self, RulesError> {
        match value {
            "node" => Ok(Self::Node),
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
    /// Returns the first matching category in stable catalog order.
    #[must_use]
    pub fn classify(&self, path: &LogicalPath) -> Option<&FileCategory> {
        self.file_categories
            .iter()
            .filter(|category| category.matcher.matches(path))
            .max_by_key(|category| {
                category
                    .matcher
                    .path_prefix
                    .as_ref()
                    .map_or(0, |prefix| prefix.len())
            })
    }
}

/// Errors from rule construction, validation, or SQLite loading.
#[derive(Debug)]
pub enum RulesError {
    /// Filesystem failure while materializing an embedded artifact.
    Io(std::io::Error),
    /// SQLite or filesystem error.
    Sql(rusqlite::Error),
    /// An invalid schema version was found.
    SchemaVersion(u32),
    /// The stored canonical hash disagrees with logical contents.
    HashMismatch { stored: String, computed: String },
    /// A malformed digest.
    InvalidHash(String),
    /// An unsupported parser name.
    InvalidParser(String),
    /// An unsupported file policy.
    InvalidResolutionPolicy(String),
    /// An unsupported symbol policy.
    InvalidSymbolPolicy(String),
    /// A required metadata key is absent.
    MissingMetadata(String),
    /// The artifact belongs to a different game profile.
    GameMismatch { expected: String, actual: String },
    /// An unknown persisted semantic rule shape was found.
    InvalidRuleShape(String),
}

impl fmt::Display for RulesError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "rules I/O error: {error}"),
            Self::Sql(error) => write!(formatter, "rules SQLite error: {error}"),
            Self::SchemaVersion(version) => {
                write!(formatter, "unsupported rules schema version: {version}")
            }
            Self::HashMismatch { stored, computed } => {
                write!(
                    formatter,
                    "rules hash mismatch: stored {stored}, computed {computed}"
                )
            }
            Self::InvalidHash(value) => write!(formatter, "invalid rule hash: {value}"),
            Self::InvalidParser(value) => write!(formatter, "invalid parser kind: {value}"),
            Self::InvalidResolutionPolicy(value) => {
                write!(formatter, "invalid file resolution policy: {value}")
            }
            Self::InvalidSymbolPolicy(value) => {
                write!(formatter, "invalid symbol resolution policy: {value}")
            }
            Self::MissingMetadata(key) => write!(formatter, "missing rules metadata: {key}"),
            Self::GameMismatch { expected, actual } => {
                write!(
                    formatter,
                    "rules game mismatch: expected {expected}, found {actual}"
                )
            }
            Self::InvalidRuleShape(value) => {
                write!(formatter, "invalid semantic rule shape: {value}")
            }
        }
    }
}

impl std::error::Error for RulesError {}

impl From<rusqlite::Error> for RulesError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sql(error)
    }
}

impl From<std::io::Error> for RulesError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// An immutable runtime rule set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuleSet {
    schema_version: u32,
    rule_hash: RuleHash,
    model: RulesModel,
    exact_semantic_rules: BTreeMap<String, Vec<usize>>,
    semantic_rules_by_context: BTreeMap<String, Vec<usize>>,
}

impl RuleSet {
    /// Creates an empty rule set for bootstrapping the crate graph.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            rule_hash: RuleHash::empty(),
            model: RulesModel {
                game_id: String::new(),
                file_categories: Vec::new(),
                symbol_descriptors: Vec::new(),
                records: Vec::new(),
                semantic: SemanticModel {
                    rules: Vec::new(),
                    enum_values: BTreeMap::new(),
                    type_root_keys: BTreeMap::new(),
                    type_root_scopes: BTreeMap::new(),
                    type_descriptors: BTreeMap::new(),
                    localisation_bindings: Vec::new(),
                },
            },
            exact_semantic_rules: BTreeMap::new(),
            semantic_rules_by_context: BTreeMap::new(),
        }
    }

    /// Builds a runtime rule set and computes its canonical logical hash.
    #[must_use]
    pub fn from_model(mut model: RulesModel) -> Self {
        model
            .file_categories
            .sort_by(|left, right| left.id.cmp(&right.id));
        model
            .symbol_descriptors
            .sort_by(|left, right| left.kind_id.cmp(&right.kind_id));
        model.records.sort_by(|left, right| {
            (&left.table, &left.logical_id, left.source_order).cmp(&(
                &right.table,
                &right.logical_id,
                right.source_order,
            ))
        });
        model
            .semantic
            .rules
            .sort_by(|left, right| left.id.cmp(&right.id));
        model.semantic.localisation_bindings.sort_by(|left, right| {
            (
                left.type_name.as_str(),
                left.subtype.as_deref().unwrap_or_default(),
                left.field.as_str(),
                left.template.as_deref().unwrap_or_default(),
            )
                .cmp(&(
                    right.type_name.as_str(),
                    right.subtype.as_deref().unwrap_or_default(),
                    right.field.as_str(),
                    right.template.as_deref().unwrap_or_default(),
                ))
        });
        for values in model.semantic.enum_values.values_mut() {
            values.sort();
            values.dedup();
        }
        for values in model.semantic.type_root_keys.values_mut() {
            values.sort();
            values.dedup();
        }
        let rule_hash = canonical_hash(&model);
        let mut exact_semantic_rules = BTreeMap::<String, Vec<usize>>::new();
        let mut semantic_rules_by_context = BTreeMap::<String, Vec<usize>>::new();
        for (index, rule) in model.semantic.rules.iter().enumerate() {
            if let KeyMatcher::Exact(key) = &rule.key {
                exact_semantic_rules
                    .entry(key.to_ascii_lowercase())
                    .or_default()
                    .push(index);
            }
            semantic_rules_by_context
                .entry(rule.context.to_ascii_lowercase())
                .or_default()
                .push(index);
        }
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            rule_hash,
            model,
            exact_semantic_rules,
            semantic_rules_by_context,
        }
    }

    /// Returns the normalized model.
    #[must_use]
    pub const fn model(&self) -> &RulesModel {
        &self.model
    }

    /// Returns exact-key semantic rule candidates without scanning unrelated matchers.
    pub fn exact_semantic_rules(&self, key: &str) -> impl Iterator<Item = &SemanticRule> {
        case_insensitive_indices(&self.exact_semantic_rules, key)
            .into_iter()
            .flatten()
            .map(|index| &self.model.semantic.rules[*index])
    }

    /// Returns semantic rules for one context without scanning unrelated contexts.
    pub fn semantic_rules_for_context(&self, context: &str) -> impl Iterator<Item = &SemanticRule> {
        case_insensitive_indices(&self.semantic_rules_by_context, context)
            .into_iter()
            .flatten()
            .map(|index| &self.model.semantic.rules[*index])
    }

    /// Returns the matching file category.
    #[must_use]
    pub fn classify(&self, path: &LogicalPath) -> Option<&FileCategory> {
        self.model.classify(path)
    }

    /// Returns the stable game profile identity carried by this artifact.
    #[must_use]
    pub fn game_id(&self) -> &str {
        &self.model.game_id
    }

    /// Validates that this rule set can be consumed by the selected game profile.
    pub fn ensure_game(&self, expected: &str) -> Result<(), RulesError> {
        if self.game_id() == expected {
            Ok(())
        } else {
            Err(RulesError::GameMismatch {
                expected: expected.to_owned(),
                actual: self.game_id().to_owned(),
            })
        }
    }

    /// Writes a complete self-owned SQLite artifact.
    pub fn write_sqlite(&self, path: &Path) -> Result<(), RulesError> {
        let mut connection = Connection::open(path)?;
        write_connection(&mut connection, self)
    }

    /// Loads, validates, and freezes a SQLite artifact.
    pub fn load(path: &Path) -> Result<Self, RulesError> {
        let connection =
            Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        let version = metadata(&connection, "schema_version")?
            .ok_or_else(|| RulesError::MissingMetadata("schema_version".to_owned()))?
            .parse::<u32>()
            .map_err(|_| RulesError::SchemaVersion(0))?;
        if version != CURRENT_SCHEMA_VERSION {
            return Err(RulesError::SchemaVersion(version));
        }
        let stored = metadata(&connection, "rule_hash")?
            .ok_or_else(|| RulesError::MissingMetadata("rule_hash".to_owned()))?;
        let game_id = metadata(&connection, "game_id")?
            .ok_or_else(|| RulesError::MissingMetadata("game_id".to_owned()))?;
        let mut model = read_model(&connection)?;
        model.game_id = game_id;
        let mut rules = Self::from_model(model);
        rules.schema_version = version;
        let computed = rules.rule_hash.to_hex();
        if stored != computed {
            return Err(RulesError::HashMismatch { stored, computed });
        }
        Ok(rules)
    }

    /// Loads an embedded SQLite artifact without exposing a user-selectable rules path.
    ///
    /// SQLite 3.32 does not expose a safe borrowed-byte connection. The official composition
    /// root therefore materializes its compile-time bytes to a process-unique temporary file,
    /// validates the complete logical model, and removes the file before returning.
    pub fn load_embedded(bytes: &[u8]) -> Result<Self, RulesError> {
        let sequence = EMBEDDED_LOAD_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "paradoxcode-rules-{}-{sequence}.pdxrules",
            std::process::id()
        ));
        let result = (|| {
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            use std::io::Write as _;
            let mut file = options.open(&path)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            Self::load(&path)
        })();
        let cleanup = fs::remove_file(&path);
        match (result, cleanup) {
            (Ok(rules), Ok(())) => Ok(rules),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(RulesError::Io(error)),
        }
    }

    /// Returns the schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Returns the canonical content hash.
    #[must_use]
    pub const fn rule_hash(&self) -> RuleHash {
        self.rule_hash
    }
}

fn case_insensitive_indices<'a>(
    index: &'a BTreeMap<String, Vec<usize>>,
    key: &str,
) -> Option<&'a Vec<usize>> {
    if key.bytes().any(|byte| byte.is_ascii_uppercase()) {
        index.get(&key.to_ascii_lowercase())
    } else {
        index.get(key)
    }
}

fn canonical_hash(model: &RulesModel) -> RuleHash {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"paradoxcode/rules/v6\0");
    put_str(&mut bytes, &model.game_id);
    let mut categories = model.file_categories.clone();
    categories.sort_by(|left, right| left.id.cmp(&right.id));
    put_len(&mut bytes, categories.len());
    for category in categories {
        put_str(&mut bytes, &category.id);
        put_str(&mut bytes, &category.parser.as_str());
        put_str(&mut bytes, category.resolution.as_str());
        put_opt_str(&mut bytes, category.matcher.path_prefix.as_deref());
        put_len(&mut bytes, category.matcher.extensions.len());
        for extension in category.matcher.extensions {
            put_str(&mut bytes, &extension);
        }
        put_opt_str(&mut bytes, category.matcher.path_suffix.as_deref());
        bytes.push(u8::from(category.matcher.case_sensitive));
    }
    let mut descriptors = model.symbol_descriptors.clone();
    descriptors.sort_by(|left, right| left.kind_id.cmp(&right.kind_id));
    put_len(&mut bytes, descriptors.len());
    for descriptor in descriptors {
        put_str(&mut bytes, &descriptor.kind_id);
        put_str(&mut bytes, descriptor.resolution.as_str());
        bytes.push(u8::from(descriptor.case_sensitive));
    }
    let mut records = model.records.clone();
    records.sort_by(|left, right| {
        (&left.table, &left.logical_id, left.source_order).cmp(&(
            &right.table,
            &right.logical_id,
            right.source_order,
        ))
    });
    put_len(&mut bytes, records.len());
    for record in records {
        put_str(&mut bytes, &record.table);
        put_str(&mut bytes, &record.logical_id);
        bytes.extend_from_slice(&record.source_order.to_le_bytes());
        put_len(&mut bytes, record.fields.len());
        for (key, value) in record.fields {
            put_str(&mut bytes, &key);
            put_str(&mut bytes, &value);
        }
    }
    let mut semantic_rules = model.semantic.rules.clone();
    semantic_rules.sort_by(|left, right| left.id.cmp(&right.id));
    put_len(&mut bytes, semantic_rules.len());
    for rule in semantic_rules {
        put_str(&mut bytes, &rule.id);
        put_str(&mut bytes, &rule.context);
        put_len(&mut bytes, rule.parent_path.len());
        for parent in rule.parent_path {
            put_str(&mut bytes, &parent);
        }
        put_semantic_key(&mut bytes, &rule.key);
        put_opt_str(&mut bytes, rule.operator.as_deref());
        put_semantic_value(&mut bytes, &rule.value);
        put_str(&mut bytes, rule.shape.as_str());
        put_opt_str(&mut bytes, rule.child_context.as_deref());
        put_opt_str(&mut bytes, rule.alternative_id.as_deref());
        match rule.severity {
            Some(severity) => {
                bytes.push(1);
                bytes.push(severity);
            }
            None => bytes.push(0),
        }
        bytes.push(u8::from(rule.required));
        put_len(&mut bytes, rule.documentation.len());
        for documentation in &rule.documentation {
            put_str(&mut bytes, documentation);
        }
        match rule.min_occurs {
            Some(value) => {
                bytes.push(1);
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            None => bytes.push(0),
        }
        let mut allowed_scopes = rule.allowed_scopes.clone();
        allowed_scopes.sort();
        put_len(&mut bytes, allowed_scopes.len());
        for scope in allowed_scopes {
            put_str(&mut bytes, &scope);
        }
        put_opt_str(&mut bytes, rule.push_scope.as_deref());
        let mut replace_scope = rule.replace_scope.clone();
        replace_scope.sort();
        put_len(&mut bytes, replace_scope.len());
        for (register, scope) in replace_scope {
            put_str(&mut bytes, &register);
            put_str(&mut bytes, &scope);
        }
        match rule.max_occurs {
            Some(value) => {
                bytes.push(1);
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            None => bytes.push(0),
        }
        bytes.push(u8::from(rule.strict_min));
        put_str(&mut bytes, &rule.source_file);
        bytes.extend_from_slice(&rule.line.to_le_bytes());
    }
    let mut enum_names = model
        .semantic
        .enum_values
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    enum_names.sort();
    put_len(&mut bytes, enum_names.len());
    for name in enum_names {
        put_str(&mut bytes, &name);
        let mut values = model
            .semantic
            .enum_values
            .get(&name)
            .cloned()
            .unwrap_or_default();
        values.sort();
        put_len(&mut bytes, values.len());
        for value in values {
            put_str(&mut bytes, &value);
        }
    }
    let mut type_names = model
        .semantic
        .type_root_keys
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    type_names.sort();
    put_len(&mut bytes, type_names.len());
    for name in type_names {
        put_str(&mut bytes, &name);
        let mut roots = model
            .semantic
            .type_root_keys
            .get(&name)
            .cloned()
            .unwrap_or_default();
        roots.sort();
        put_len(&mut bytes, roots.len());
        for root in roots {
            put_str(&mut bytes, &root);
        }
    }
    let mut scoped_types = model
        .semantic
        .type_root_scopes
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    scoped_types.sort();
    put_len(&mut bytes, scoped_types.len());
    for type_name in scoped_types {
        put_str(&mut bytes, &type_name);
        let scopes = model
            .semantic
            .type_root_scopes
            .get(&type_name)
            .cloned()
            .unwrap_or_default();
        put_len(&mut bytes, scopes.len());
        for (root, scope) in scopes {
            put_str(&mut bytes, &root);
            put_str(&mut bytes, &scope);
        }
    }
    let mut descriptor_names = model
        .semantic
        .type_descriptors
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    descriptor_names.sort();
    put_len(&mut bytes, descriptor_names.len());
    for name in descriptor_names {
        let descriptor = model
            .semantic
            .type_descriptors
            .get(&name)
            .expect("type descriptor");
        put_str(&mut bytes, &descriptor.name);
        put_opt_str(&mut bytes, descriptor.path.as_deref());
        put_opt_str(&mut bytes, descriptor.path_file.as_deref());
        put_opt_str(&mut bytes, descriptor.path_extension.as_deref());
        bytes.push(u8::from(descriptor.path_strict));
        bytes.push(u8::from(descriptor.type_per_file));
        let mut skip_root_paths = descriptor.skip_root_paths.clone();
        skip_root_paths.sort();
        put_len(&mut bytes, skip_root_paths.len());
        for path in skip_root_paths {
            put_len(&mut bytes, path.len());
            for key in path {
                put_str(&mut bytes, &key);
            }
        }
        put_opt_str(&mut bytes, descriptor.name_field.as_deref());
        bytes.push(u8::from(descriptor.name_from_file));
        put_opt_str(&mut bytes, descriptor.starts_with.as_deref());
        match &descriptor.type_key_filter {
            Some((values, negate)) => {
                bytes.push(1);
                bytes.push(u8::from(*negate));
                let mut values = values.clone();
                values.sort();
                put_len(&mut bytes, values.len());
                for value in values {
                    put_str(&mut bytes, &value);
                }
            }
            None => bytes.push(0),
        }
    }
    let mut localisation_bindings = model.semantic.localisation_bindings.clone();
    localisation_bindings.sort_by(|left, right| {
        (
            left.type_name.as_str(),
            left.subtype.as_deref().unwrap_or_default(),
            left.field.as_str(),
            left.template.as_deref().unwrap_or_default(),
        )
            .cmp(&(
                right.type_name.as_str(),
                right.subtype.as_deref().unwrap_or_default(),
                right.field.as_str(),
                right.template.as_deref().unwrap_or_default(),
            ))
    });
    put_len(&mut bytes, localisation_bindings.len());
    for binding in localisation_bindings {
        put_str(&mut bytes, &binding.type_name);
        put_str(&mut bytes, &binding.field);
        put_opt_str(&mut bytes, binding.template.as_deref());
        bytes.push(u8::from(binding.required));
        bytes.push(u8::from(binding.optional));
        put_opt_str(&mut bytes, binding.subtype.as_deref());
        if let Some(condition) = &binding.condition {
            put_opt_str(&mut bytes, condition.field.as_deref());
            put_opt_str(&mut bytes, condition.value.as_deref());
            put_opt_str(&mut bytes, condition.key_prefix.as_deref());
        } else {
            put_opt_str(&mut bytes, None);
            put_opt_str(&mut bytes, None);
            put_opt_str(&mut bytes, None);
        }
        put_opt_str(&mut bytes, binding.explicit_field.as_deref());
    }
    let digest = Sha256::digest(bytes);
    let mut result = [0_u8; 32];
    result.copy_from_slice(&digest);
    RuleHash(result)
}

fn put_len(bytes: &mut Vec<u8>, length: usize) {
    bytes.extend_from_slice(&u64::try_from(length).unwrap_or(u64::MAX).to_le_bytes());
}
fn put_str(bytes: &mut Vec<u8>, value: &str) {
    put_len(bytes, value.len());
    bytes.extend_from_slice(value.as_bytes());
}
fn put_opt_str(bytes: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(value) => {
            bytes.push(1);
            put_str(bytes, value);
        }
        None => bytes.push(0),
    }
}

fn put_semantic_key(bytes: &mut Vec<u8>, matcher: &KeyMatcher) {
    match matcher {
        KeyMatcher::Exact(value) => {
            put_str(bytes, "exact");
            put_str(bytes, value);
        }
        KeyMatcher::Type(value) => {
            put_str(bytes, "type");
            put_str(bytes, value);
        }
        KeyMatcher::Enum(value) => {
            put_str(bytes, "enum");
            put_str(bytes, value);
        }
        KeyMatcher::AnyScalar => put_str(bytes, "any"),
        KeyMatcher::Dynamic(value) => {
            put_str(bytes, "dynamic");
            put_str(bytes, value);
        }
    }
}

fn put_semantic_value(bytes: &mut Vec<u8>, matcher: &ValueMatcher) {
    match matcher {
        ValueMatcher::AnyScalar => put_str(bytes, "any"),
        ValueMatcher::Exact(value) => {
            put_str(bytes, "exact");
            put_str(bytes, value);
        }
        ValueMatcher::Bool => put_str(bytes, "bool"),
        ValueMatcher::Int { min, max } => {
            put_str(bytes, "int");
            put_opt_str(bytes, min.map(|value| value.to_string()).as_deref());
            put_opt_str(bytes, max.map(|value| value.to_string()).as_deref());
        }
        ValueMatcher::Float { min, max } => {
            put_str(bytes, "float");
            put_opt_str(bytes, min.as_deref());
            put_opt_str(bytes, max.as_deref());
        }
        ValueMatcher::Type(value) => {
            put_str(bytes, "type");
            put_str(bytes, value);
        }
        ValueMatcher::Enum(value) => {
            put_str(bytes, "enum");
            put_str(bytes, value);
        }
        ValueMatcher::Scope(value) => {
            put_str(bytes, "scope");
            put_opt_str(bytes, value.as_deref());
        }
        ValueMatcher::Localisation => put_str(bytes, "localisation"),
        ValueMatcher::Filepath => put_str(bytes, "filepath"),
        ValueMatcher::Dynamic(value) => {
            put_str(bytes, "dynamic");
            put_str(bytes, value);
        }
        ValueMatcher::DynamicSet(value) => {
            put_str(bytes, "dynamic-set");
            put_str(bytes, value);
        }
        ValueMatcher::Opaque(value) => {
            put_str(bytes, "opaque");
            put_str(bytes, value);
        }
    }
}

fn schema(connection: &Connection) -> Result<(), RulesError> {
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;
        CREATE TABLE IF NOT EXISTS metadata (key TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS interned_names (id INTEGER PRIMARY KEY, value TEXT UNIQUE NOT NULL);
        CREATE TABLE IF NOT EXISTS file_categories (
            id TEXT PRIMARY KEY NOT NULL, parser TEXT NOT NULL, resolution TEXT NOT NULL,
            path_prefix TEXT, extensions TEXT NOT NULL, path_suffix TEXT, case_sensitive INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS symbol_descriptors (
            kind_id TEXT PRIMARY KEY NOT NULL, resolution TEXT NOT NULL, case_sensitive INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS rule_records (
            table_name TEXT NOT NULL, logical_id TEXT NOT NULL, source_order INTEGER NOT NULL,
            PRIMARY KEY (table_name, logical_id)
        );
        CREATE TABLE IF NOT EXISTS rule_fields (
            table_name TEXT NOT NULL, logical_id TEXT NOT NULL, field_name TEXT NOT NULL, field_value TEXT NOT NULL,
            PRIMARY KEY (table_name, logical_id, field_name),
            FOREIGN KEY (table_name, logical_id) REFERENCES rule_records(table_name, logical_id) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS semantic_rules (
            id TEXT PRIMARY KEY NOT NULL,
            context TEXT NOT NULL,
            parent_path TEXT NOT NULL,
            key_kind TEXT NOT NULL,
            key_value TEXT,
            operator TEXT,
            value_kind TEXT NOT NULL,
            value_arg TEXT,
            value_min TEXT,
            value_max TEXT,
            shape TEXT NOT NULL,
            child_context TEXT,
            alternative_id TEXT,
            severity INTEGER,
            required INTEGER NOT NULL DEFAULT 0,
            documentation TEXT NOT NULL DEFAULT '',
            allowed_scopes TEXT NOT NULL DEFAULT '',
            push_scope TEXT,
            replace_scope TEXT NOT NULL DEFAULT '',
            min_occurs INTEGER,
            strict_min INTEGER NOT NULL DEFAULT 1,
            max_occurs INTEGER,
            source_file TEXT NOT NULL,
            line INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS enum_values (
            enum_name TEXT NOT NULL,
            value TEXT NOT NULL,
            PRIMARY KEY (enum_name, value)
        );
        CREATE TABLE IF NOT EXISTS type_root_keys (
            type_name TEXT NOT NULL,
            root_key TEXT NOT NULL,
            PRIMARY KEY (type_name, root_key)
        );
        CREATE TABLE IF NOT EXISTS type_root_scopes (
            type_name TEXT NOT NULL,
            root_key TEXT NOT NULL,
            scope TEXT NOT NULL,
            PRIMARY KEY (type_name, root_key)
        );
        CREATE TABLE IF NOT EXISTS type_descriptors (
            type_name TEXT PRIMARY KEY NOT NULL,
            path TEXT,
            path_file TEXT,
            path_extension TEXT,
            path_strict INTEGER NOT NULL DEFAULT 0,
            type_per_file INTEGER NOT NULL DEFAULT 0,
            skip_root_keys TEXT NOT NULL DEFAULT '',
            name_field TEXT,
            name_from_file INTEGER NOT NULL DEFAULT 0,
            starts_with TEXT,
            type_key_filter TEXT NOT NULL DEFAULT '',
            type_key_filter_negate INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS localisation_bindings (
            type_name TEXT NOT NULL,
            field TEXT NOT NULL,
            template TEXT,
            required INTEGER NOT NULL DEFAULT 0,
            optional INTEGER NOT NULL DEFAULT 0,
            subtype TEXT,
            condition_field TEXT,
            condition_value TEXT,
            condition_key_prefix TEXT,
            explicit_field TEXT,
            PRIMARY KEY(type_name, field, subtype)
        );
        CREATE TABLE IF NOT EXISTS import_provenance (
            source_path TEXT PRIMARY KEY NOT NULL, source_sha256 TEXT NOT NULL, importer_version TEXT NOT NULL
        );")?;
    ensure_semantic_columns(connection)?;
    Ok(())
}

fn ensure_semantic_columns(connection: &Connection) -> Result<(), RulesError> {
    for (name, definition) in [
        ("child_context", "TEXT"),
        ("alternative_id", "TEXT"),
        ("severity", "INTEGER"),
        ("operator", "TEXT"),
        ("required", "INTEGER NOT NULL DEFAULT 0"),
        ("documentation", "TEXT NOT NULL DEFAULT ''"),
        ("allowed_scopes", "TEXT NOT NULL DEFAULT ''"),
        ("push_scope", "TEXT"),
        ("replace_scope", "TEXT NOT NULL DEFAULT ''"),
        ("min_occurs", "INTEGER"),
        ("strict_min", "INTEGER NOT NULL DEFAULT 1"),
    ] {
        let present: i64 = connection.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('semantic_rules') WHERE name = ?1",
            params![name],
            |row| row.get(0),
        )?;
        if present == 0 {
            connection.execute(
                &format!("ALTER TABLE semantic_rules ADD COLUMN {name} {definition}"),
                [],
            )?;
        }
    }
    for (name, definition) in [
        ("type_key_filter", "TEXT NOT NULL DEFAULT ''"),
        ("type_key_filter_negate", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        let present: i64 = connection.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('type_descriptors') WHERE name = ?1",
            params![name],
            |row| row.get(0),
        )?;
        if present == 0 {
            connection.execute(
                &format!("ALTER TABLE type_descriptors ADD COLUMN {name} {definition}"),
                [],
            )?;
        }
    }
    Ok(())
}

fn write_connection(connection: &mut Connection, rules: &RuleSet) -> Result<(), RulesError> {
    schema(connection)?;
    let transaction = connection.transaction()?;
    transaction.execute_batch("DELETE FROM metadata; DELETE FROM interned_names; DELETE FROM file_categories; DELETE FROM symbol_descriptors; DELETE FROM enum_values; DELETE FROM type_root_keys; DELETE FROM type_root_scopes; DELETE FROM type_descriptors; DELETE FROM localisation_bindings; DELETE FROM semantic_rules; DELETE FROM rule_fields; DELETE FROM rule_records;")?;
    transaction.execute(
        "INSERT INTO metadata(key, value) VALUES ('schema_version', ?1), ('rule_hash', ?2), ('game_id', ?3)",
        params![rules.schema_version.to_string(), rules.rule_hash.to_hex(), rules.game_id()],
    )?;
    for category in &rules.model.file_categories {
        transaction.execute("INSERT INTO file_categories(id, parser, resolution, path_prefix, extensions, path_suffix, case_sensitive) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)", params![category.id, category.parser.as_str(), category.resolution.as_str(), category.matcher.path_prefix, category.matcher.extensions.join("\u{1f}"), category.matcher.path_suffix, i64::from(category.matcher.case_sensitive)])?;
    }
    for descriptor in &rules.model.symbol_descriptors {
        transaction.execute("INSERT INTO symbol_descriptors(kind_id, resolution, case_sensitive) VALUES (?1, ?2, ?3)", params![descriptor.kind_id, descriptor.resolution.as_str(), i64::from(descriptor.case_sensitive)])?;
    }
    for record in &rules.model.records {
        transaction.execute(
            "INSERT INTO rule_records(table_name, logical_id, source_order) VALUES (?1, ?2, ?3)",
            params![record.table, record.logical_id, record.source_order],
        )?;
        for (field_name, field_value) in &record.fields {
            transaction.execute("INSERT INTO rule_fields(table_name, logical_id, field_name, field_value) VALUES (?1, ?2, ?3, ?4)", params![record.table, record.logical_id, field_name, field_value])?;
        }
    }
    for rule in &rules.model.semantic.rules {
        let (key_kind, key_value) = semantic_key_columns(&rule.key);
        let (value_kind, value_arg, value_min, value_max) = semantic_value_columns(&rule.value);
        transaction.execute(
            "INSERT INTO semantic_rules(id, context, parent_path, key_kind, key_value, operator, value_kind, value_arg, value_min, value_max, shape, child_context, alternative_id, severity, required, documentation, allowed_scopes, push_scope, replace_scope, min_occurs, strict_min, max_occurs, source_file, line) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)",
            params![
                rule.id,
                rule.context,
                rule.parent_path.join("\u{1f}"),
                key_kind,
                key_value,
                rule.operator,
                value_kind,
                value_arg,
                value_min,
                value_max,
                rule.shape.as_str(),
                rule.child_context,
                rule.alternative_id,
                rule.severity,
                i64::from(rule.required),
                rule.documentation.join("\u{1f}"),
                rule.allowed_scopes.join("\u{1f}"),
                rule.push_scope,
                encode_replace_scope(&rule.replace_scope),
                rule.min_occurs,
                i64::from(rule.strict_min),
                rule.max_occurs,
                rule.source_file,
                rule.line,
            ],
        )?;
    }
    for (name, values) in &rules.model.semantic.enum_values {
        for value in values {
            transaction.execute(
                "INSERT INTO enum_values(enum_name, value) VALUES (?1, ?2)",
                params![name, value],
            )?;
        }
    }
    for (type_name, roots) in &rules.model.semantic.type_root_keys {
        for root in roots {
            transaction.execute(
                "INSERT INTO type_root_keys(type_name, root_key) VALUES (?1, ?2)",
                params![type_name, root],
            )?;
        }
    }
    for (type_name, scopes) in &rules.model.semantic.type_root_scopes {
        for (root, scope) in scopes {
            transaction.execute(
                "INSERT INTO type_root_scopes(type_name, root_key, scope) VALUES (?1, ?2, ?3)",
                params![type_name, root, scope],
            )?;
        }
    }
    for (type_name, descriptor) in &rules.model.semantic.type_descriptors {
        transaction.execute(
            "INSERT INTO type_descriptors(type_name, path, path_file, path_extension, path_strict, type_per_file, skip_root_keys, name_field, name_from_file, starts_with, type_key_filter, type_key_filter_negate) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                type_name,
                descriptor.path,
                descriptor.path_file,
                descriptor.path_extension,
                i64::from(descriptor.path_strict),
                i64::from(descriptor.type_per_file),
                descriptor
                    .skip_root_paths
                    .iter()
                    .map(|path| path.join("\u{1e}"))
                    .collect::<Vec<_>>()
                    .join("\u{1f}"),
                descriptor.name_field,
                i64::from(descriptor.name_from_file),
                descriptor.starts_with,
                descriptor
                    .type_key_filter
                    .as_ref()
                    .map_or_else(String::new, |(values, _)| values.join("\u{1f}")),
                i64::from(
                    descriptor.type_key_filter.as_ref().is_some_and(|(_, negate)| *negate),
                ),
            ],
        )?;
    }
    for binding in &rules.model.semantic.localisation_bindings {
        transaction.execute(
            "INSERT INTO localisation_bindings(type_name, field, template, required, optional, subtype, condition_field, condition_value, condition_key_prefix, explicit_field) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                binding.type_name,
                binding.field,
                binding.template,
                i64::from(binding.required),
                i64::from(binding.optional),
                binding.subtype,
                binding.condition.as_ref().and_then(|condition| condition.field.as_deref()),
                binding.condition.as_ref().and_then(|condition| condition.value.as_deref()),
                binding.condition.as_ref().and_then(|condition| condition.key_prefix.as_deref()),
                binding.explicit_field,
            ],
        )?;
    }
    transaction.commit()?;
    Ok(())
}

fn semantic_key_columns(matcher: &KeyMatcher) -> (&'static str, Option<&str>) {
    match matcher {
        KeyMatcher::Exact(value) => ("exact", Some(value)),
        KeyMatcher::Type(value) => ("type", Some(value)),
        KeyMatcher::Enum(value) => ("enum", Some(value)),
        KeyMatcher::AnyScalar => ("any", None),
        KeyMatcher::Dynamic(value) => ("dynamic", Some(value)),
    }
}

fn semantic_value_columns(
    matcher: &ValueMatcher,
) -> (&'static str, Option<&str>, Option<String>, Option<String>) {
    match matcher {
        ValueMatcher::AnyScalar => ("any", None, None, None),
        ValueMatcher::Exact(value) => ("exact", Some(value), None, None),
        ValueMatcher::Bool => ("bool", None, None, None),
        ValueMatcher::Int { min, max } => (
            "int",
            None,
            min.map(|value| value.to_string()),
            max.map(|value| value.to_string()),
        ),
        ValueMatcher::Float { min, max } => ("float", None, min.clone(), max.clone()),
        ValueMatcher::Type(value) => ("type", Some(value), None, None),
        ValueMatcher::Enum(value) => ("enum", Some(value), None, None),
        ValueMatcher::Scope(value) => ("scope", value.as_deref(), None, None),
        ValueMatcher::Localisation => ("localisation", None, None, None),
        ValueMatcher::Filepath => ("filepath", None, None, None),
        ValueMatcher::Dynamic(value) => ("dynamic", Some(value), None, None),
        ValueMatcher::DynamicSet(value) => ("dynamic-set", Some(value), None, None),
        ValueMatcher::Opaque(value) => ("opaque", Some(value), None, None),
    }
}

fn encode_replace_scope(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(register, scope)| format!("{register}={scope}"))
        .collect::<Vec<_>>()
        .join("\u{1e}")
}

fn decode_replace_scope(value: Option<&str>) -> Vec<(String, String)> {
    value
        .unwrap_or_default()
        .split('\u{1e}')
        .filter_map(|pair| pair.split_once('='))
        .map(|(register, scope)| (register.to_owned(), scope.to_owned()))
        .collect()
}

fn metadata(connection: &Connection, key: &str) -> Result<Option<String>, RulesError> {
    connection
        .query_row(
            "SELECT value FROM metadata WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
}

fn read_model(connection: &Connection) -> Result<RulesModel, RulesError> {
    let mut categories = Vec::new();
    let mut statement = connection.prepare("SELECT id, parser, resolution, path_prefix, extensions, path_suffix, case_sensitive FROM file_categories ORDER BY id")?;
    let rows = statement.query_map([], |row| -> rusqlite::Result<FileCategory> {
        let extensions: String = row.get(4)?;
        let parser = ParserKind::parse(&row.get::<_, String>(1)?)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        let resolution = FileResolutionPolicy::parse(&row.get::<_, String>(2)?)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        Ok(FileCategory {
            id: row.get(0)?,
            parser,
            resolution,
            matcher: FileMatcher {
                path_prefix: row.get(3)?,
                extensions: if extensions.is_empty() {
                    Vec::new()
                } else {
                    extensions.split('\u{1f}').map(str::to_owned).collect()
                },
                path_suffix: row.get(5)?,
                case_sensitive: row.get::<_, i64>(6)? != 0,
            },
        })
    })?;
    for row in rows {
        categories.push(row?);
    }
    let mut descriptors = Vec::new();
    let mut statement = connection.prepare(
        "SELECT kind_id, resolution, case_sensitive FROM symbol_descriptors ORDER BY kind_id",
    )?;
    let rows = statement.query_map([], |row| -> rusqlite::Result<SymbolDescriptor> {
        let resolution = SymbolResolutionPolicy::parse(&row.get::<_, String>(1)?)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        Ok(SymbolDescriptor {
            kind_id: row.get(0)?,
            resolution,
            case_sensitive: row.get::<_, i64>(2)? != 0,
        })
    })?;
    for row in rows {
        descriptors.push(row?);
    }
    let mut records = Vec::new();
    let mut statement = connection.prepare("SELECT table_name, logical_id, source_order FROM rule_records ORDER BY table_name, logical_id")?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, u32>(2)?,
        ))
    })?;
    for row in rows {
        let (table, logical_id, source_order) = row?;
        let mut fields = BTreeMap::new();
        let mut fields_statement = connection.prepare("SELECT field_name, field_value FROM rule_fields WHERE table_name = ?1 AND logical_id = ?2 ORDER BY field_name")?;
        for field in fields_statement.query_map(params![table, logical_id], |field| {
            Ok((field.get::<_, String>(0)?, field.get::<_, String>(1)?))
        })? {
            let (key, value) = field?;
            fields.insert(key, value);
        }
        records.push(RuleRecord {
            table,
            logical_id,
            source_order,
            fields,
        });
    }
    let semantic = read_semantic_model(connection)?;
    Ok(RulesModel {
        game_id: String::new(),
        file_categories: categories,
        symbol_descriptors: descriptors,
        records,
        semantic,
    })
}

fn read_semantic_model(connection: &Connection) -> Result<SemanticModel, RulesError> {
    let mut rules = Vec::new();
    let mut statement = connection.prepare(
        "SELECT id, context, parent_path, key_kind, key_value, operator, value_kind, value_arg, value_min, value_max, shape, child_context, alternative_id, severity, required, documentation, allowed_scopes, push_scope, replace_scope, min_occurs, strict_min, max_occurs, source_file, line FROM semantic_rules ORDER BY id",
    )?;
    let rows = statement.query_map([], |row| {
        let key_kind: String = row.get(3)?;
        let key_value: Option<String> = row.get(4)?;
        let operator: Option<String> = row.get(5)?;
        let value_kind: String = row.get(6)?;
        let value_arg: Option<String> = row.get(7)?;
        let value_min: Option<String> = row.get(8)?;
        let value_max: Option<String> = row.get(9)?;
        let shape_name: String = row.get(10)?;
        let child_context: Option<String> = row.get(11)?;
        let alternative_id: Option<String> = row.get(12)?;
        let severity: Option<u8> = row.get(13)?;
        let required: bool = row.get::<_, i64>(14)? != 0;
        let documentation: String = row.get(15)?;
        let allowed_scopes: String = row.get(16)?;
        let push_scope: Option<String> = row.get(17)?;
        let replace_scope: Option<String> = row.get(18)?;
        let min_occurs: Option<u32> = row.get(19)?;
        let strict_min: bool = row.get::<_, i64>(20)? != 0;
        let key = decode_semantic_key(&key_kind, key_value.as_deref())
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        let value = decode_semantic_value(
            &value_kind,
            value_arg.as_deref(),
            value_min.as_deref(),
            value_max.as_deref(),
        )
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        let shape = RuleShape::parse(&shape_name)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        let parent_path: String = row.get(2)?;
        Ok(SemanticRule {
            id: row.get(0)?,
            context: row.get(1)?,
            parent_path: if parent_path.is_empty() {
                Vec::new()
            } else {
                parent_path.split('\u{1f}').map(str::to_owned).collect()
            },
            key,
            operator,
            value,
            shape,
            child_context,
            alternative_id,
            severity,
            required,
            documentation: if documentation.is_empty() {
                Vec::new()
            } else {
                documentation.split('\u{1f}').map(str::to_owned).collect()
            },
            min_occurs,
            strict_min,
            allowed_scopes: if allowed_scopes.is_empty() {
                Vec::new()
            } else {
                allowed_scopes.split('\u{1f}').map(str::to_owned).collect()
            },
            push_scope,
            replace_scope: decode_replace_scope(replace_scope.as_deref()),
            max_occurs: row.get(21)?,
            source_file: row.get(22)?,
            line: row.get(23)?,
        })
    })?;
    for row in rows {
        rules.push(row?);
    }

    let mut enum_values = BTreeMap::new();
    let mut statement =
        connection.prepare("SELECT enum_name, value FROM enum_values ORDER BY enum_name, value")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (name, value) = row?;
        enum_values.entry(name).or_insert_with(Vec::new).push(value);
    }
    let mut type_root_keys = BTreeMap::new();
    let mut statement = connection
        .prepare("SELECT type_name, root_key FROM type_root_keys ORDER BY type_name, root_key")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (type_name, root_key) = row?;
        type_root_keys
            .entry(type_name)
            .or_insert_with(Vec::new)
            .push(root_key);
    }
    let mut type_root_scopes = BTreeMap::new();
    let mut statement = connection.prepare(
        "SELECT type_name, root_key, scope FROM type_root_scopes ORDER BY type_name, root_key",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (type_name, root_key, scope) = row?;
        type_root_scopes
            .entry(type_name)
            .or_insert_with(BTreeMap::new)
            .insert(root_key, scope);
    }
    let mut type_descriptors = BTreeMap::new();
    let mut statement = connection.prepare(
        "SELECT type_name, path, path_file, path_extension, path_strict, type_per_file, skip_root_keys, name_field, name_from_file, starts_with, type_key_filter, type_key_filter_negate FROM type_descriptors ORDER BY type_name",
    )?;
    let rows = statement.query_map([], |row| {
        let type_name: String = row.get(0)?;
        let skip_root_paths: String = row.get(6)?;
        let type_key_filter: String = row.get(10)?;
        let type_key_filter_negate: bool = row.get::<_, i64>(11)? != 0;
        Ok(TypeDescriptor {
            name: type_name.clone(),
            path: row.get(1)?,
            path_file: row.get(2)?,
            path_extension: row.get(3)?,
            path_strict: row.get::<_, i64>(4)? != 0,
            type_per_file: row.get::<_, i64>(5)? != 0,
            skip_root_paths: if skip_root_paths.is_empty() {
                Vec::new()
            } else {
                skip_root_paths
                    .split('\u{1f}')
                    .map(|path| path.split('\u{1e}').map(str::to_owned).collect())
                    .collect()
            },
            name_field: row.get(7)?,
            name_from_file: row.get::<_, i64>(8)? != 0,
            starts_with: row.get(9)?,
            type_key_filter: if type_key_filter.is_empty() {
                None
            } else {
                Some((
                    type_key_filter.split('\u{1f}').map(str::to_owned).collect(),
                    type_key_filter_negate,
                ))
            },
        })
    })?;
    for row in rows {
        let descriptor = row?;
        type_descriptors.insert(descriptor.name.clone(), descriptor);
    }
    let mut localisation_bindings = Vec::new();
    let mut statement = connection.prepare(
        "SELECT type_name, field, template, required, optional, subtype, condition_field, condition_value, condition_key_prefix, explicit_field FROM localisation_bindings ORDER BY type_name, subtype, field",
    )?;
    let rows = statement.query_map([], |row| {
        let condition_field: Option<String> = row.get(6)?;
        let condition_value: Option<String> = row.get(7)?;
        let condition_key_prefix: Option<String> = row.get(8)?;
        Ok(LocalisationBinding {
            type_name: row.get(0)?,
            field: row.get(1)?,
            template: row.get(2)?,
            required: row.get::<_, i64>(3)? != 0,
            optional: row.get::<_, i64>(4)? != 0,
            subtype: row.get(5)?,
            condition: (condition_field.is_some()
                || condition_value.is_some()
                || condition_key_prefix.is_some())
            .then_some(LocalisationBindingCondition {
                field: condition_field,
                value: condition_value,
                key_prefix: condition_key_prefix,
            }),
            explicit_field: row.get(9)?,
        })
    })?;
    for row in rows {
        localisation_bindings.push(row?);
    }
    Ok(SemanticModel {
        rules,
        enum_values,
        type_root_keys,
        type_root_scopes,
        type_descriptors,
        localisation_bindings,
    })
}

fn decode_semantic_key(kind: &str, value: Option<&str>) -> Result<KeyMatcher, RulesError> {
    Ok(match kind {
        "exact" => KeyMatcher::Exact(value.unwrap_or_default().to_owned()),
        "type" => KeyMatcher::Type(value.unwrap_or_default().to_owned()),
        "enum" => KeyMatcher::Enum(value.unwrap_or_default().to_owned()),
        "any" => KeyMatcher::AnyScalar,
        "dynamic" => KeyMatcher::Dynamic(value.unwrap_or_default().to_owned()),
        other => return Err(RulesError::InvalidRuleShape(other.to_owned())),
    })
}

fn decode_semantic_value(
    kind: &str,
    arg: Option<&str>,
    min: Option<&str>,
    max: Option<&str>,
) -> Result<ValueMatcher, RulesError> {
    Ok(match kind {
        "any" => ValueMatcher::AnyScalar,
        "exact" => ValueMatcher::Exact(arg.unwrap_or_default().to_owned()),
        "bool" => ValueMatcher::Bool,
        "int" => ValueMatcher::Int {
            min: min.map(str::parse).transpose().map_err(|_| {
                RulesError::InvalidRuleShape("invalid integer matcher bound".to_owned())
            })?,
            max: max.map(str::parse).transpose().map_err(|_| {
                RulesError::InvalidRuleShape("invalid integer matcher bound".to_owned())
            })?,
        },
        "float" => ValueMatcher::Float {
            min: min.map(str::to_owned),
            max: max.map(str::to_owned),
        },
        "type" => ValueMatcher::Type(arg.unwrap_or_default().to_owned()),
        "enum" => ValueMatcher::Enum(arg.unwrap_or_default().to_owned()),
        "scope" => ValueMatcher::Scope(arg.map(str::to_owned)),
        "localisation" => ValueMatcher::Localisation,
        "filepath" => ValueMatcher::Filepath,
        "dynamic" => ValueMatcher::Dynamic(arg.unwrap_or_default().to_owned()),
        "dynamic-set" => ValueMatcher::DynamicSet(arg.unwrap_or_default().to_owned()),
        "opaque" => ValueMatcher::Opaque(arg.unwrap_or_default().to_owned()),
        other => return Err(RulesError::InvalidRuleShape(other.to_owned())),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        CURRENT_SCHEMA_VERSION, FileMatcher, GameProfile, KeyMatcher, ProfileMatchMode,
        ProfileTextMatcher, RuleRecord, RuleSet, RuleShape, RulesModel, SemanticRule, ValueMatcher,
    };
    use pdx_text::LogicalPath;
    use std::collections::BTreeMap;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn empty_rules_have_a_stable_schema_identity() {
        let rules = RuleSet::empty();
        assert_eq!(rules.schema_version(), CURRENT_SCHEMA_VERSION);
        assert_eq!(rules.rule_hash().as_bytes(), [0; 32]);
    }

    #[test]
    fn profile_text_matchers_are_case_insensitive_and_bounded() {
        let suffix = ProfileTextMatcher::insensitive(ProfileMatchMode::Suffix, "_event");
        let contains = ProfileTextMatcher::insensitive(ProfileMatchMode::Contains, "events/");
        let directory =
            ProfileTextMatcher::insensitive(ProfileMatchMode::Directory, "common/cultures");

        assert!(suffix.matches("COUNTRY_EVENT"));
        assert!(!suffix.matches("event_target"));
        assert!(contains.matches("COMMON/EVENTS/example.txt"));
        assert!(!contains.matches("common/event_modifiers/example.txt"));
        assert!(directory.matches("COMMON/CULTURES/example.txt"));
        assert!(!directory.matches("common/cultures/nested/example.txt"));
        assert!(ProfileTextMatcher::any().matches("anything"));
    }

    #[test]
    fn file_matcher_path_prefix_is_directory_bounded() {
        let matcher = FileMatcher {
            path_prefix: Some("localisation".to_owned()),
            extensions: vec!["yml".to_owned()],
            path_suffix: None,
            case_sensitive: false,
        };

        assert!(matcher.matches(&LogicalPath::new("localisation/main.yml")));
        assert!(matcher.matches(&LogicalPath::new("localisation/events/main.yml")));
        assert!(matcher.matches(&LogicalPath::new("LOCALISATION/events/MAIN.YML")));
        assert!(!matcher.matches(&LogicalPath::new("localisation_extra/main.yml")));
        assert!(!matcher.matches(&LogicalPath::new("common/main.yml")));
    }

    #[test]
    fn profile_scan_roots_are_directory_bounded() {
        let mut profile = GameProfile::empty("test");
        profile.scan_roots = vec!["common".to_owned(), "events".to_owned()];

        assert!(profile.allows_scan_path("common/events/example.txt"));
        assert!(profile.allows_scan_path("events/example.txt"));
        assert!(!profile.allows_scan_path("common_extra/example.txt"));
        assert!(!profile.allows_scan_path("root_level.txt"));
    }

    #[test]
    fn profile_scan_extensions_are_case_insensitive_and_directory_bounded() {
        let mut profile = GameProfile::empty("test");
        profile.scan_roots = vec!["events".to_owned()];
        profile.scan_extensions = vec!["txt".to_owned(), "gfx".to_owned(), "yml".to_owned()];

        assert!(profile.allows_scan_file("events/example.TXT"));
        assert!(profile.allows_scan_file("events/example.gfx"));
        assert!(profile.allows_scan_file("events/example.yml"));
        assert!(!profile.allows_scan_file("events/example.gui"));
        assert!(!profile.allows_scan_file("events/example.txt.bak"));
        assert!(!profile.allows_scan_file("events_extra/example.txt"));
        assert!(!profile.allows_scan_file("root_level.txt"));
    }

    #[test]
    fn canonical_hash_is_independent_of_record_insertion_order() {
        let mut first = RulesModel {
            game_id: "test-game".to_owned(),
            ..RulesModel::default()
        };
        let records = [
            RuleRecord {
                table: "types".to_owned(),
                logical_id: "a".to_owned(),
                source_order: 1,
                fields: BTreeMap::from([(String::from("name"), String::from("event"))]),
            },
            RuleRecord {
                table: "enums".to_owned(),
                logical_id: "b".to_owned(),
                source_order: 0,
                fields: BTreeMap::from([(String::from("name"), String::from("scope"))]),
            },
        ];
        first.records.extend(records.clone());
        let mut second = RulesModel {
            game_id: "test-game".to_owned(),
            ..RulesModel::default()
        };
        second.records.extend(records.into_iter().rev());
        assert_eq!(
            RuleSet::from_model(first).rule_hash(),
            RuleSet::from_model(second).rule_hash()
        );
    }

    #[test]
    fn exact_semantic_rule_index_is_case_insensitive_and_excludes_dynamic_matchers() {
        let semantic_rule = |id: &str, key: KeyMatcher| SemanticRule {
            id: id.to_owned(),
            context: "effect".to_owned(),
            parent_path: Vec::new(),
            key,
            operator: None,
            value: ValueMatcher::AnyScalar,
            shape: RuleShape::Leaf,
            child_context: None,
            alternative_id: None,
            severity: None,
            required: false,
            documentation: Vec::new(),
            allowed_scopes: Vec::new(),
            push_scope: None,
            replace_scope: Vec::new(),
            min_occurs: None,
            strict_min: false,
            max_occurs: None,
            source_file: String::new(),
            line: 0,
        };
        let mut model = RulesModel::default();
        model.semantic.rules = vec![
            semantic_rule("exact", KeyMatcher::Exact("Owner".to_owned())),
            semantic_rule("dynamic", KeyMatcher::Dynamic("scope".to_owned())),
        ];
        let rules = RuleSet::from_model(model);

        assert_eq!(
            rules
                .exact_semantic_rules("OWNER")
                .map(|rule| rule.id.as_str())
                .collect::<Vec<_>>(),
            vec!["exact"]
        );
        assert!(rules.exact_semantic_rules("missing").next().is_none());
        assert_eq!(
            rules
                .semantic_rules_for_context("EFFECT")
                .map(|rule| rule.id.as_str())
                .collect::<Vec<_>>(),
            vec!["dynamic", "exact"]
        );
        assert!(rules.semantic_rules_for_context("trigger").next().is_none());
    }

    #[test]
    fn sqlite_round_trip_validates_logical_hash() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("paradoxcode-rules-{nonce}.pdxrules"));
        let rules = RuleSet::from_model(RulesModel {
            game_id: "test-game".to_owned(),
            ..RulesModel::default()
        });
        rules.write_sqlite(&path).expect("write rules");
        let loaded = RuleSet::load(&path).expect("load rules");
        assert_eq!(loaded, rules);
        assert_eq!(loaded.game_id(), "test-game");
        assert!(loaded.ensure_game("test-game").is_ok());
        assert!(matches!(
            loaded.ensure_game("another-game"),
            Err(super::RulesError::GameMismatch { .. })
        ));
        std::fs::remove_file(path).expect("remove temporary rules");
    }
}
