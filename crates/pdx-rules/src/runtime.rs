use crate::canonical::{RuleHash, canonical_hash};
use crate::matcher::KeyMatcher;
use crate::model::{FileCategory, RulesModel, SemanticModel, SemanticRule, TypeRootScope};
use crate::{CURRENT_SCHEMA_VERSION, sqlite};
use pdx_text::LogicalPath;

use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static EMBEDDED_LOAD_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
    /// The first-party JSON source could not be compiled into a runtime rule set.
    Source(String),
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
            Self::Source(message) => write!(formatter, "first-party rule source error: {message}"),
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
    pub(crate) schema_version: u32,
    pub(crate) rule_hash: RuleHash,
    pub(crate) model: RulesModel,
    pub(crate) exact_semantic_rules: FxHashMap<Box<str>, Vec<usize>>,
    pub(crate) semantic_rules_by_context: FxHashMap<Box<str>, Vec<usize>>,
    /// Lowercased context -> (lowercased exact key -> rule indices).
    pub(crate) semantic_exact_rules_by_context_key:
        FxHashMap<Box<str>, FxHashMap<Box<str>, Vec<usize>>>,
    /// Lowercased context -> rule indices whose key is not exact.
    pub(crate) semantic_non_exact_rules_by_context: FxHashMap<Box<str>, Vec<usize>>,
    /// Lowercased type names whose `root:<name>` semantic context holds at least one rule.
    ///
    /// Root-context selection probes this per descriptor during lowering; building the
    /// `root:<name>` string and scanning rules per probe dominated context resolution.
    pub(crate) root_context_types: FxHashSet<Box<str>>,
    /// Whether each rule's context is `effect` or `trigger`, precomputed once so scope
    /// link resolution stops re-lowercasing rule contexts per lookup.
    pub(crate) effect_trigger_contexts: Vec<bool>,
}

impl RuleSet {
    /// Creates an empty rule set for bootstrapping the crate graph.
    #[must_use]
    pub fn empty() -> Self {
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
                profile: crate::GameProfile::default(),
            },
            exact_semantic_rules: FxHashMap::default(),
            semantic_rules_by_context: FxHashMap::default(),
            semantic_exact_rules_by_context_key: FxHashMap::default(),
            semantic_non_exact_rules_by_context: FxHashMap::default(),
            root_context_types: FxHashSet::default(),
            effect_trigger_contexts: Vec::new(),
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
        let mut exact_semantic_rules = FxHashMap::<Box<str>, Vec<usize>>::default();
        let mut semantic_rules_by_context = FxHashMap::<Box<str>, Vec<usize>>::default();
        let mut semantic_exact_rules_by_context_key =
            FxHashMap::<Box<str>, FxHashMap<Box<str>, Vec<usize>>>::default();
        let mut semantic_non_exact_rules_by_context = FxHashMap::<Box<str>, Vec<usize>>::default();
        let mut root_context_types = FxHashSet::<Box<str>>::default();
        let mut effect_trigger_contexts = Vec::with_capacity(model.semantic.rules.len());
        for (index, rule) in model.semantic.rules.iter().enumerate() {
            let context_key: Box<str> = rule.context.to_ascii_lowercase().into_boxed_str();
            effect_trigger_contexts
                .push(context_key.as_ref() == "effect" || context_key.as_ref() == "trigger");
            match &rule.key {
                KeyMatcher::Exact(key) => {
                    let key: Box<str> = key.to_ascii_lowercase().into_boxed_str();
                    exact_semantic_rules
                        .entry(key.clone())
                        .or_default()
                        .push(index);
                    semantic_exact_rules_by_context_key
                        .entry(context_key.clone())
                        .or_default()
                        .entry(key)
                        .or_default()
                        .push(index);
                }
                _ => {
                    semantic_non_exact_rules_by_context
                        .entry(context_key.clone())
                        .or_default()
                        .push(index);
                }
            }
            semantic_rules_by_context
                .entry(context_key)
                .or_default()
                .push(index);
        }
        for context_key in semantic_rules_by_context.keys() {
            if let Some(type_name) = context_key.strip_prefix("root:") {
                root_context_types.insert(type_name.into());
            }
        }
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            rule_hash,
            model,
            exact_semantic_rules,
            semantic_rules_by_context,
            semantic_exact_rules_by_context_key,
            semantic_non_exact_rules_by_context,
            root_context_types,
            effect_trigger_contexts,
        }
    }

    /// Returns the normalized model.
    #[must_use]
    pub const fn model(&self) -> &RulesModel {
        &self.model
    }

    /// Returns the data-only profile carried by this rules artifact.
    #[must_use]
    pub const fn profile(&self) -> &crate::GameProfile {
        &self.model.profile
    }

    /// Returns exact-key semantic rule candidates without scanning unrelated matchers.
    pub fn exact_semantic_rules(&self, key: &str) -> impl Iterator<Item = &SemanticRule> {
        case_insensitive_indices(&self.exact_semantic_rules, key)
            .into_iter()
            .flatten()
            .map(|index| &self.model.semantic.rules[*index])
    }

    /// Returns the rule indices behind [`Self::exact_semantic_rules`] without borrowing the
    /// rules themselves, so callers can pair indices with precomputed per-rule facts.
    #[must_use]
    pub fn exact_semantic_rule_indices(&self, key: &str) -> &[usize] {
        case_insensitive_indices(&self.exact_semantic_rules, key)
            .map_or(&[], |indices| indices.as_slice())
    }

    /// Returns whether the rule at `index` is declared in the `effect` or `trigger` context.
    ///
    /// Precomputed at load time; scope link resolution consults it per exact-key lookup and
    /// must not re-fold rule context strings on the hot path.
    #[must_use]
    pub fn semantic_rule_is_effect_or_trigger(&self, index: usize) -> bool {
        self.effect_trigger_contexts
            .get(index)
            .is_some_and(|flag| *flag)
    }

    /// Returns rule indices whose key matcher is not exact for one context.
    ///
    /// These are the rules a key-indexed lookup must still scan; callers that
    /// memoize them per container avoid repeating the scan per property.
    pub fn semantic_non_exact_rules_for_context(
        &self,
        context: &str,
    ) -> impl Iterator<Item = usize> + '_ {
        let context_key = normalized_ascii_query(context);
        self.semantic_non_exact_rules_by_context
            .get(context_key.as_ref())
            .into_iter()
            .flatten()
            .copied()
    }

    /// Returns exact-key rule indices for one (context, key) pair.
    pub fn semantic_exact_rules_for_context_key(
        &self,
        context: &str,
        key: &str,
    ) -> impl Iterator<Item = usize> + '_ {
        let context_key = normalized_ascii_query(context);
        let key = normalized_ascii_query(key);
        self.semantic_exact_rules_by_context_key
            .get(context_key.as_ref())
            .and_then(|by_key| by_key.get(key.as_ref()))
            .into_iter()
            .flatten()
            .copied()
    }

    /// Returns rule indices for one context (both exact and non-exact keys).
    ///
    /// Lets callers memoize filtered index lists without recovering indices
    /// from references.
    pub fn semantic_rule_indices_for_context(
        &self,
        context: &str,
    ) -> impl Iterator<Item = usize> + '_ {
        let context_key = normalized_ascii_query(context);
        self.semantic_rules_by_context
            .get(context_key.as_ref())
            .into_iter()
            .flatten()
            .copied()
    }

    /// Returns the semantic rule at one index from `semantic_rule_indices_for_context`.
    #[must_use]
    pub fn semantic_rule_at(&self, index: usize) -> Option<&SemanticRule> {
        self.model.semantic.rules.get(index)
    }

    /// Iterates every compiled semantic rule regardless of context.
    pub fn semantic_rules(&self) -> impl Iterator<Item = &SemanticRule> {
        self.model.semantic.rules.iter()
    }

    /// Returns whether the `root:<type_name>` semantic context holds at least one rule.
    ///
    /// Equivalent to `semantic_rules_for_context(&format!("root:{type_name}")).next().is_some()`
    /// or to finding any rule whose context equals `root:<type_name>` case-insensitively, but
    /// probes a precomputed set without building the context string.
    #[must_use]
    pub fn has_root_context_rules(&self, type_name: &str) -> bool {
        let type_name = normalized_ascii_query(type_name);
        self.root_context_types.contains(type_name.as_ref())
    }

    /// Returns semantic rules for one context without scanning unrelated contexts.
    pub fn semantic_rules_for_context(&self, context: &str) -> impl Iterator<Item = &SemanticRule> {
        case_insensitive_indices(&self.semantic_rules_by_context, context)
            .into_iter()
            .flatten()
            .map(|index| &self.model.semantic.rules[*index])
    }

    /// Returns semantic rules for one context and property key without scanning unrelated rules.
    ///
    /// Exact-key rules are indexed per context and key, so per-property lookups stay proportional
    /// to the few matching rules plus the context's non-exact matchers (type, enum, dynamic).
    pub fn semantic_rules_for_context_key(
        &self,
        context: &str,
        key: &str,
    ) -> impl Iterator<Item = &SemanticRule> {
        let context_key = normalized_ascii_query(context);
        let key = normalized_ascii_query(key);
        let exact = self
            .semantic_exact_rules_by_context_key
            .get(context_key.as_ref())
            .and_then(|by_key| by_key.get(key.as_ref()))
            .into_iter()
            .flatten();
        let non_exact = self
            .semantic_non_exact_rules_by_context
            .get(context_key.as_ref())
            .into_iter()
            .flatten();
        exact
            .chain(non_exact)
            .map(|index| &self.model.semantic.rules[*index])
    }

    /// Returns the profile-declared initial scope for a type root key.
    ///
    /// Type and root-key identities are logical names, so callers should not have to know how
    /// the source compiler cased them. The fast path serves canonical source spelling while the
    /// fallback keeps hand-authored or legacy artifacts case-insensitive.
    #[must_use]
    pub fn type_root_scope(&self, type_name: &str, root_key: &str) -> Option<&str> {
        self.type_root_scope_registers(type_name, root_key)
            .map(|scopes| scopes.root.as_str())
    }

    /// Returns the initial `ROOT`, `THIS`, and `FROM` scope registers for a type root key.
    ///
    /// Legacy scalar declarations are normalized by the source compiler to `THIS = ROOT` and
    /// `FROM = any`, so callers can rely on all three fields being populated.
    #[must_use]
    pub fn type_root_scope_registers(
        &self,
        type_name: &str,
        root_key: &str,
    ) -> Option<&TypeRootScope> {
        let scopes = self
            .model
            .semantic
            .type_root_scopes
            .get(type_name)
            .or_else(|| {
                self.model
                    .semantic
                    .type_root_scopes
                    .iter()
                    .find(|(candidate, _)| candidate.eq_ignore_ascii_case(type_name))
                    .map(|(_, scopes)| scopes)
            })?;
        scopes.get(root_key).or_else(|| {
            scopes
                .iter()
                .find(|(candidate, _)| candidate.eq_ignore_ascii_case(root_key))
                .map(|(_, scope)| scope)
        })
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
        sqlite::write(path, self)
    }

    /// Loads, validates, and freezes a SQLite artifact.
    pub fn load(path: &Path) -> Result<Self, RulesError> {
        sqlite::load(path)
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
    index: &'a FxHashMap<Box<str>, Vec<usize>>,
    key: &str,
) -> Option<&'a Vec<usize>> {
    let key = normalized_ascii_query(key);
    index.get(key.as_ref())
}

fn normalized_ascii_query(value: &str) -> std::borrow::Cow<'_, str> {
    if value.bytes().any(|byte| byte.is_ascii_uppercase()) {
        std::borrow::Cow::Owned(value.to_ascii_lowercase())
    } else {
        std::borrow::Cow::Borrowed(value)
    }
}
