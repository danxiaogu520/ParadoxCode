use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Text encoding policy selected by a game profile for source files.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceEncoding {
    /// Strict UTF-8 source, which is the generic engine default.
    #[default]
    Utf8,
    /// Legacy Windows-1252 source decoded to UTF-8 before parsing.
    #[serde(rename = "windows-1252")]
    Windows1252,
}
/// Matching mode for small, data-only game-profile selectors.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
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
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
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
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
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
    /// Whether the index retains the definition body's direct attribute keys
    /// so analysis can classify them without re-reading the file.
    #[serde(default)]
    pub retain_attributes: bool,
}

/// One scalar-reference interpretation supplied by a game profile.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
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
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileValueDefinitionRule {
    /// Property-key selector.
    pub key: ProfileTextMatcher,
    /// Optional immediate parent-key selector.
    pub parent_key: Option<ProfileTextMatcher>,
    /// Stable declared symbol kind.
    pub kind: String,
}

/// One block-valued property whose nested name field declares a workspace symbol.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileContainerValueDefinitionRule {
    /// Property-key selector.
    pub key: ProfileTextMatcher,
    /// Nested scalar field that supplies the declared symbol name.
    pub name_field: String,
    /// Stable declared symbol kind.
    pub kind: String,
}

/// One suffix appended to member names when validating a dynamic value kind.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileMemberNameSuffixRule {
    /// Value kinds whose members also accept the suffixed spelling.
    pub kinds: Vec<String>,
    /// Suffix appended to a candidate name before the membership lookup.
    pub suffix: String,
}

/// One block whose direct child keys declare workspace symbols.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileContainerDefinitionRule {
    /// Logical-path selector.
    pub path: ProfileTextMatcher,
    /// Top-level container-key selector.
    pub key: ProfileTextMatcher,
    /// Stable child symbol kind.
    pub kind: String,
}

/// One definition emitted when nested scalar conditions are satisfied.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
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
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
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
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileRootScopeRule {
    /// Root property-key selector.
    pub key: ProfileTextMatcher,
    /// Initial root/current scope.
    pub scope: String,
}

/// One asymmetric scope compatibility accepted by a game profile.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileScopeCompatibility {
    /// Actual current scope.
    pub actual: String,
    /// Rule-expected scope accepted for the actual scope.
    pub expected: String,
}

/// A root entry source tells the generic completion engine where a file-root type's legal entry
/// names come from.  First-party rules already describe the file path and semantic container;
/// this small profile-level discriminator covers the cases where entries are static enum values
/// or workspace-defined members rather than a `type_root_keys` list.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProfileRootEntrySource {
    /// Read the names from the type descriptor's `type_root_keys` declaration.
    TypeRootKeys,
    /// Read the names from a semantic enum, including profile/workspace extensions.
    Enum { enum_name: String },
    /// Read the names from workspace definitions of the given semantic type.
    Workspace { type_name: String },
}

/// Text shape inserted for a file-root completion item.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileRootEntryInsertion {
    /// Insert `name = { ... }`.
    #[default]
    Block,
    /// Insert a bare scalar such as `westerngfx`.
    Bare,
    /// Insert `name = ` and leave the value to the user.
    Assignment,
    /// Insert `name = "$0"` for a quoted scalar mapping.
    QuotedAssignment,
}

/// Completion metadata for one `TypeDescriptor.root_entries` container.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileRootEntrySpec {
    /// Source of legal root entry names.
    pub source: ProfileRootEntrySource,
    /// Snippet shape used when inserting a selected entry.
    #[serde(default)]
    pub insertion: ProfileRootEntryInsertion,
    /// Whether the same root entry may be declared more than once in a file.
    #[serde(default)]
    pub repeatable: bool,
}

/// Data-only game-specific interpretation selected by the composition root.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
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
    /// Directory names whose top-level `name` fields declare scripted-localisation commands.
    ///
    /// The match is path-segment based and case-insensitive so a game profile can support
    /// spellings such as `scripted_localisation`, `scripted_localization`, and `scripted_loc`
    /// without teaching the generic engine a concrete game layout.
    pub scripted_localisation_directories: Vec<String>,
    /// Ordered top-level definition rules; the first match wins.
    pub definitions: Vec<ProfileDefinitionRule>,
    /// Ordered scalar-reference rules; the first match wins.
    pub references: Vec<ProfileReferenceRule>,
    /// Ordered scalar value-definition rules; the first match wins.
    pub value_definitions: Vec<ProfileValueDefinitionRule>,
    /// Ordered block value-definition rules; the first match wins.
    pub container_value_definitions: Vec<ProfileContainerValueDefinitionRule>,
    /// Lazily built exact-key lookup over the rule lists above, mirroring each
    /// list's first-match order. Populated on first per-key query because
    /// lowering performs millions of these lookups per workspace scan and the
    /// linear `find` over every rule dominated scan CPU.
    #[serde(skip)]
    pub key_index: std::sync::OnceLock<ProfileKeyIndex>,
    /// Blocks whose direct child keys declare symbols.
    pub container_definitions: Vec<ProfileContainerDefinitionRule>,
    /// Additional definitions gated by nested fields.
    pub conditional_definitions: Vec<ProfileConditionalDefinitionRule>,
    /// Delimited identifiers embedded in parser tokens.
    pub token_definitions: Vec<ProfileTokenDefinitionRule>,
    /// Known concrete scopes and scope expressions.
    pub scope_names: Vec<String>,
    /// Scope-valued intrinsic expressions supplied by the game profile.
    ///
    /// Some games expose keywords such as `owner` or `capital_scope` that resolve to a concrete
    /// scope without appearing as ordinary `push_scope` rules. Keeping the mapping in profile
    /// data lets the generic semantic engine remain game-agnostic.
    pub scope_member_aliases: BTreeMap<String, String>,
    /// Derived scope name/alias lookup; see [`ScopeLookup`].
    #[serde(skip)]
    scope_lookup: std::sync::OnceLock<ScopeLookup>,
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
    /// Optional sources and insertion styles for non-`type_root_keys` file-root entries.
    ///
    /// The map is keyed by the `root_entries` context name from a type descriptor.  An absent
    /// entry preserves the legacy behavior: semantic rules and `type_root_keys` are used as-is.
    pub root_entry_specs: BTreeMap<String, ProfileRootEntrySpec>,
}

/// Exact-key lookup accelerator over a game profile's ordered rule lists.
///
/// Rules with an exact key selector are bucketed by their pattern
/// (case-insensitive patterns are stored lowercased); every other selector
/// mode stays in a short scan list. Both buckets keep rule order, and queries
/// take the earliest matching index across the two, preserving each list's
/// documented first-match semantics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileKeyIndex {
    references_exact: ExactKeyBuckets,
    references_scan: Vec<usize>,
    value_exact: ExactKeyBuckets,
    value_scan: Vec<usize>,
    container_value_exact: ExactKeyBuckets,
    container_value_scan: Vec<usize>,
}

/// Lazily built lookup for scope names and member aliases.
///
/// `is_scope` and `scope_member_alias` run inside per-rule value matching, so alias probing
/// must not rebuild underscore-stripped spellings per call.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ScopeLookup {
    /// Lowercased known scope spellings.
    names: rustc_hash::FxHashSet<Box<str>>,
    /// Lowercased, underscore-stripped alias -> concrete scope spelling.
    aliases: rustc_hash::FxHashMap<Box<str>, Box<str>>,
}

/// Case-split exact buckets for one rule list.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ExactKeyBuckets {
    /// Case-insensitive exact patterns, stored lowercased.
    caseless: std::collections::HashMap<String, Vec<usize>>,
    /// Case-sensitive exact patterns, stored verbatim.
    cased: std::collections::HashMap<String, Vec<usize>>,
}

impl ExactKeyBuckets {
    fn insert(&mut self, matcher: &ProfileTextMatcher, index: usize) -> bool {
        if matcher.mode != ProfileMatchMode::Exact {
            return false;
        }
        if matcher.case_sensitive {
            self.cased
                .entry(matcher.pattern.clone())
                .or_default()
                .push(index);
        } else {
            self.caseless
                .entry(matcher.pattern.to_ascii_lowercase())
                .or_default()
                .push(index);
        }
        true
    }

    /// Bucket contents for `candidate` across both case variants.
    fn lookup(&self, candidate: &str) -> impl Iterator<Item = usize> + '_ {
        let cased = self.cased.get(candidate).map(Vec::as_slice).unwrap_or(&[]);
        let caseless = normalized_ascii_query(candidate);
        let caseless = self
            .caseless
            .get(caseless.as_ref())
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        cased
            .iter()
            .chain(caseless)
            .copied()
            .collect::<Vec<_>>()
            .into_iter()
    }
}

fn normalized_ascii_query(value: &str) -> std::borrow::Cow<'_, str> {
    if value.bytes().all(|byte| byte.is_ascii_lowercase()) {
        std::borrow::Cow::Borrowed(value)
    } else {
        std::borrow::Cow::Owned(value.to_ascii_lowercase())
    }
}

impl ProfileKeyIndex {
    fn build(profile: &GameProfile) -> Self {
        let mut references_exact = ExactKeyBuckets::default();
        let mut references_scan = Vec::new();
        for (index, rule) in profile.references.iter().enumerate() {
            if !references_exact.insert(&rule.key, index) {
                references_scan.push(index);
            }
        }
        let mut value_exact = ExactKeyBuckets::default();
        let mut value_scan = Vec::new();
        for (index, rule) in profile.value_definitions.iter().enumerate() {
            if !value_exact.insert(&rule.key, index) {
                value_scan.push(index);
            }
        }
        let mut container_value_exact = ExactKeyBuckets::default();
        let mut container_value_scan = Vec::new();
        for (index, rule) in profile.container_value_definitions.iter().enumerate() {
            if !container_value_exact.insert(&rule.key, index) {
                container_value_scan.push(index);
            }
        }
        Self {
            references_exact,
            references_scan,
            value_exact,
            value_scan,
            container_value_exact,
            container_value_scan,
        }
    }

    /// Candidate rule indices for `candidate` in ascending rule order.
    fn candidates<'a>(
        exact: &'a ExactKeyBuckets,
        scan: &'a [usize],
        candidate: &str,
    ) -> impl Iterator<Item = usize> + 'a {
        let mut indices = exact.lookup(candidate).collect::<Vec<_>>();
        indices.extend_from_slice(scan);
        indices.sort_unstable();
        indices.dedup();
        indices.into_iter()
    }
}

impl GameProfile {
    /// Returns the shared exact-key index, building it on first use.
    fn resolved_key_index(&self) -> &ProfileKeyIndex {
        self.key_index.get_or_init(|| ProfileKeyIndex::build(self))
    }

    fn resolved_scope_lookup(&self) -> &ScopeLookup {
        self.scope_lookup.get_or_init(|| {
            let fold = |value: &str| value.to_ascii_lowercase().replace('_', "").into_boxed_str();
            ScopeLookup {
                // Scope names compare case-insensitively with underscores intact.
                names: self
                    .scope_names
                    .iter()
                    .map(|name| name.to_ascii_lowercase().into_boxed_str())
                    .collect(),
                // Alias identity additionally ignores underscores, matching the original
                // `alias.replace('_', "") == value.replace('_', "")` fallback.
                aliases: self
                    .scope_member_aliases
                    .iter()
                    .map(|(alias, scope)| (fold(alias), fold(scope)))
                    .collect(),
            }
        })
    }

    /// Creates an identity-only profile with no game-specific interpretation.
    #[must_use]
    pub fn empty(game_id: impl Into<String>) -> Self {
        Self {
            game_id: game_id.into(),
            source_encoding: SourceEncoding::Utf8,
            key_index: std::sync::OnceLock::new(),
            scan_roots: Vec::new(),
            scan_root_max_depths: BTreeMap::new(),
            scan_root_files: BTreeMap::new(),
            scan_extensions: Vec::new(),
            scripted_localisation_directories: Vec::new(),
            definitions: Vec::new(),
            references: Vec::new(),
            value_definitions: Vec::new(),
            container_value_definitions: Vec::new(),
            container_definitions: Vec::new(),
            conditional_definitions: Vec::new(),
            token_definitions: Vec::new(),
            scope_names: Vec::new(),
            scope_member_aliases: BTreeMap::new(),
            scope_lookup: std::sync::OnceLock::new(),
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
            root_entry_specs: BTreeMap::new(),
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

    /// Returns root-entry completion metadata using case-insensitive context matching.
    #[must_use]
    pub fn root_entry_spec(&self, context: &str) -> Option<&ProfileRootEntrySpec> {
        self.root_entry_specs
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(context))
            .map(|(_, spec)| spec)
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
        let index = self.resolved_key_index();
        ProfileKeyIndex::candidates(
            &index.container_value_exact,
            &index.container_value_scan,
            key,
        )
        .map(|rule_index| &self.container_value_definitions[rule_index])
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

    /// Returns whether a logical path is inside one of the profile-declared scripted
    /// localisation directories.
    #[must_use]
    pub fn is_scripted_localisation_path(&self, logical_path: &str) -> bool {
        let path = logical_path.replace('\\', "/");
        path.split('/').any(|segment| {
            self.scripted_localisation_directories
                .iter()
                .any(|directory| segment.eq_ignore_ascii_case(directory))
        })
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
        let index = self.resolved_key_index();
        ProfileKeyIndex::candidates(&index.references_exact, &index.references_scan, key)
            .map(|rule_index| &self.references[rule_index])
            .find(|rule| {
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
        let index = self.resolved_key_index();
        ProfileKeyIndex::candidates(&index.value_exact, &index.value_scan, key)
            .map(|rule_index| &self.value_definitions[rule_index])
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
        let lookup = self.resolved_scope_lookup();
        if value.bytes().any(|byte| byte.is_ascii_uppercase()) {
            lookup.names.contains(value.to_ascii_lowercase().as_str())
        } else {
            lookup.names.contains(value)
        }
    }

    /// Returns the concrete scope selected by a profile-defined intrinsic expression.
    ///
    /// The alias identity is lowercase with underscores removed, matching the original
    /// equality of `alias` and `alias.replace('_', "") == value.replace('_', "")` without
    /// allocating stripped spellings per call.
    #[must_use]
    pub fn scope_member_alias(&self, value: &str) -> Option<&str> {
        let folded = folded_scope_key(value);
        self.resolved_scope_lookup()
            .aliases
            .get(folded.as_ref())
            .map(|scope| scope.as_ref())
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

/// Lowercases and strips underscores, borrowing the input when neither is needed.
fn folded_scope_key(value: &str) -> std::borrow::Cow<'_, str> {
    let needs_fold = value
        .bytes()
        .any(|byte| byte.is_ascii_uppercase() || byte == b'_');
    if needs_fold {
        std::borrow::Cow::Owned(value.to_ascii_lowercase().replace('_', ""))
    } else {
        std::borrow::Cow::Borrowed(value)
    }
}
