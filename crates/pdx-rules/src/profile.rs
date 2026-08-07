use std::collections::BTreeMap;
/// Text encoding policy selected by a game profile for source files.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SourceEncoding {
    /// Strict UTF-8 source, which is the generic engine default.
    #[default]
    Utf8,
    /// Legacy Windows-1252 source decoded to UTF-8 before parsing.
    Windows1252,
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
