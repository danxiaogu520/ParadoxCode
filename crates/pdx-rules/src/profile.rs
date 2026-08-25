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
    /// Property keys that must not be interpreted as this reference kind.
    pub excluded_keys: Vec<String>,
    /// Logical paths where the property key must not be interpreted as this kind
    /// (for example history files where `name` is literal text, not localisation).
    pub excluded_paths: Vec<ProfileTextMatcher>,
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

/// One block-valued property whose nested name field declares a workspace symbol.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileContainerValueDefinitionRule {
    /// Property-key selector.
    pub key: ProfileTextMatcher,
    /// Nested scalar field that supplies the declared symbol name.
    pub name_field: String,
    /// Stable declared symbol kind.
    pub kind: String,
}

/// One suffix appended to member names when validating a dynamic value kind.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileMemberNameSuffixRule {
    /// Value kinds whose members also accept the suffixed spelling.
    pub kinds: Vec<String>,
    /// Suffix appended to a candidate name before the membership lookup.
    pub suffix: String,
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
    /// Optional maximum number of directory levels below each scan root.
    ///
    /// A missing entry keeps the root recursive. A value of `0` authorizes only files directly
    /// inside that root, while larger values authorize that many nested directory levels.
    pub scan_root_max_depths: BTreeMap<String, usize>,
    /// Optional exact relative-file-path whitelist for a scan root.
    ///
    /// Entries are relative to the root and use `/` separators. When a root has an entry here,
    /// only those exact paths are accepted. This supports fixed nested paths such as EU4's
    /// `map/lakes/00_lakes.txt` without falling back to an unbounded recursive glob.
    pub scan_root_files: BTreeMap<String, Vec<String>>,
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
    /// Ordered block value-definition rules; the first match wins.
    pub container_value_definitions: Vec<ProfileContainerValueDefinitionRule>,
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
    /// Additional first-party rule contexts inherited by a semantic container.
    ///
    /// This keeps inline game constructs data-driven; for example a game profile can declare
    /// that one type accepts the keys from its generic modifier context.
    pub semantic_context_inheritance: BTreeMap<String, Vec<String>>,
    /// Quoted scalar keys whose payload may declare workspace symbols as embedded Script.
    pub quoted_script_definition_keys: Vec<ProfileTextMatcher>,
    /// Prefixes for runtime-named scope expressions such as `event_target:name`.
    pub dynamic_scope_prefixes: Vec<String>,
    /// Scalar prefixes that make any value rule accept a runtime reference.
    pub dynamic_value_prefixes: Vec<String>,
    /// Dynamic value kinds that cannot be proven complete from workspace declarations.
    pub open_world_value_kinds: Vec<String>,
    /// semantic type/enum spellings mapped to workspace symbol kinds.
    pub member_kind_aliases: BTreeMap<String, String>,
    /// Value kinds whose member lookups also try a suffixed spelling.
    pub member_name_suffixes: Vec<ProfileMemberNameSuffixRule>,
    /// Profile fallback keys used when no imported semantic rule selects a property.
    pub fallback_keys: Vec<String>,
    /// Control-flow keys highlighted as keywords (for example `if`, `limit`, `not`).
    pub control_flow_keys: Vec<String>,
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
            scan_root_max_depths: BTreeMap::new(),
            scan_root_files: BTreeMap::new(),
            scan_extensions: Vec::new(),
            definitions: Vec::new(),
            references: Vec::new(),
            value_definitions: Vec::new(),
            container_value_definitions: Vec::new(),
            container_definitions: Vec::new(),
            conditional_definitions: Vec::new(),
            token_definitions: Vec::new(),
            scope_names: Vec::new(),
            scope_completions: Vec::new(),
            root_scopes: Vec::new(),
            scope_compatibilities: Vec::new(),
            transparent_scope_wrappers: Vec::new(),
            semantic_context_inheritance: BTreeMap::new(),
            quoted_script_definition_keys: Vec::new(),
            dynamic_scope_prefixes: Vec::new(),
            dynamic_value_prefixes: Vec::new(),
            open_world_value_kinds: Vec::new(),
            member_kind_aliases: BTreeMap::new(),
            member_name_suffixes: Vec::new(),
            fallback_keys: Vec::new(),
            control_flow_keys: Vec::new(),
            enum_extra_members: BTreeMap::new(),
        }
    }

    /// Returns additional rule contexts inherited by `context`.
    #[must_use]
    pub fn inherited_semantic_contexts(&self, context: &str) -> &[String] {
        self.semantic_context_inheritance
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(context))
            .map_or(&[], |(_, inherited)| inherited.as_slice())
    }

    /// Returns whether `context` directly or transitively inherits `ancestor`.
    ///
    /// For example `imperial_incident_option_modifier` inherits `trigger`, so trigger
    /// wrappers and scope links stay valid inside incident options.
    #[must_use]
    pub fn semantic_context_inherits(&self, context: &str, ancestor: &str) -> bool {
        let mut pending = self.inherited_semantic_contexts(context).to_vec();
        let mut visited = std::collections::BTreeSet::new();
        while let Some(candidate) = pending.pop() {
            if candidate.eq_ignore_ascii_case(ancestor) {
                return true;
            }
            if visited.insert(candidate.to_ascii_lowercase()) {
                pending.extend(self.inherited_semantic_contexts(&candidate).iter().cloned());
            }
        }
        false
    }

    /// Returns whether a quoted scalar key carries Script whose definitions enter the index.
    #[must_use]
    pub fn indexes_quoted_script_definitions(&self, key: &str) -> bool {
        self.quoted_script_definition_keys
            .iter()
            .any(|matcher| matcher.matches(key))
    }

    /// Returns whether a spelling is a runtime-named scope expression.
    #[must_use]
    pub fn is_dynamic_scope_expression(&self, value: &str) -> bool {
        value.split_once(':').is_some_and(|(prefix, name)| {
            !name.is_empty()
                && self
                    .dynamic_scope_prefixes
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(prefix))
        })
    }

    /// Returns whether a scalar is a runtime value reference such as `variable:name`.
    #[must_use]
    pub fn is_dynamic_value_reference(&self, value: &str) -> bool {
        value.split_once(':').is_some_and(|(prefix, name)| {
            !name.is_empty()
                && self
                    .dynamic_value_prefixes
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(prefix))
        })
    }

    /// Returns the first block value-definition rule matching a property key.
    #[must_use]
    pub fn container_value_definition(
        &self,
        key: &str,
    ) -> Option<&ProfileContainerValueDefinitionRule> {
        self.container_value_definitions
            .iter()
            .find(|rule| rule.key.matches(key))
    }

    /// Returns the suffixes appended when looking up members of `kind`.
    #[must_use]
    pub fn member_name_suffixes_for(&self, kind: &str) -> Vec<&str> {
        self.member_name_suffixes
            .iter()
            .filter(|rule| {
                rule.kinds
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(kind))
            })
            .map(|rule| rule.suffix.as_str())
            .collect()
    }

    /// Returns whether declarations of a dynamic value kind are necessarily incomplete.
    #[must_use]
    pub fn is_open_world_value_kind(&self, kind: &str) -> bool {
        self.open_world_value_kinds
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(kind))
    }

    /// Returns the logical directory whitelist used for source-root discovery.
    #[must_use]
    pub fn scan_roots(&self) -> &[String] {
        &self.scan_roots
    }

    /// Returns the optional maximum directory depth for one scan root.
    #[must_use]
    pub fn scan_root_max_depth(&self, root: &str) -> Option<usize> {
        self.scan_root_max_depths.get(root).copied()
    }

    /// Returns the optional exact relative-file-path whitelist for one scan root.
    #[must_use]
    pub fn scan_root_files(&self, root: &str) -> Option<&[String]> {
        self.scan_root_files.get(root).map(Vec::as_slice)
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

    /// Returns whether a logical file path has an allowed root, relative path, and directory
    /// depth. This deliberately ignores the extension whitelist so parser selection can still
    /// recognize formats such as `.gui` that are not part of the source-index scan set.
    #[must_use]
    pub fn allows_profile_path(&self, logical_path: &str) -> bool {
        self.scan_roots
            .iter()
            .any(|root| self.allows_profile_path_in_root(root, logical_path))
    }

    /// Returns whether a path is a direct file below a root with an explicit filename whitelist
    /// and is not one of those filenames. This is used by parser selection to prevent a generic
    /// extension fallback from reopening an otherwise closed root while preserving the generic
    /// parser behavior for documents supplied through a prefixed editor URI.
    #[must_use]
    pub fn rejects_unlisted_root_file(&self, logical_path: &str) -> bool {
        self.scan_roots.iter().any(|root| {
            let Some(allowed_files) = self.scan_root_files(root) else {
                return false;
            };
            let remainder = if root.is_empty() {
                logical_path
            } else {
                let Some(remainder) = logical_path
                    .strip_prefix(root)
                    .and_then(|remainder| remainder.strip_prefix('/'))
                else {
                    return false;
                };
                remainder
            };
            !remainder.is_empty()
                && !remainder.contains('/')
                && !allowed_files
                    .iter()
                    .any(|allowed| allowed.eq_ignore_ascii_case(remainder))
        })
    }

    fn allows_profile_path_in_root(&self, root: &str, logical_path: &str) -> bool {
        let remainder = if root.is_empty() {
            logical_path
        } else {
            if logical_path == root {
                return false;
            }
            let Some(remainder) = logical_path
                .strip_prefix(root)
                .and_then(|remainder| remainder.strip_prefix('/'))
            else {
                return false;
            };
            remainder
        };
        let components = remainder
            .split('/')
            .filter(|component| !component.is_empty())
            .collect::<Vec<_>>();
        if components.is_empty() {
            return false;
        }
        let directory_depth = components.len().saturating_sub(1);
        if self.scan_root_files(root).is_some_and(|allowed_files| {
            !allowed_files
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(remainder))
        }) {
            return false;
        }
        self.scan_root_max_depth(root)
            .is_none_or(|max_depth| directory_depth <= max_depth)
    }

    /// Returns whether a logical file path belongs to the directory and extension whitelists.
    #[must_use]
    pub fn allows_scan_file(&self, logical_path: &str) -> bool {
        let allowed_by_root = self.allows_profile_path(logical_path);
        if !allowed_by_root || self.scan_extensions.is_empty() {
            return allowed_by_root;
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
        self.reference_rule(key).map(|rule| rule.kind.as_str())
    }

    /// Returns the first reference rule whose key selector accepts `key`.
    #[must_use]
    pub fn reference_rule(&self, key: &str) -> Option<&ProfileReferenceRule> {
        self.references.iter().find(|rule| {
            rule.key.matches(key)
                && !rule
                    .excluded_keys
                    .iter()
                    .any(|excluded| excluded.eq_ignore_ascii_case(key))
        })
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

    /// Returns whether a key is a profile control-flow keyword.
    #[must_use]
    pub fn is_control_flow_key(&self, key: &str) -> bool {
        self.control_flow_keys
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(key))
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
