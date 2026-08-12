use crate::canonical::{RuleHash, canonical_hash};
use crate::matcher::KeyMatcher;
use crate::model::{FileCategory, RulesModel, SemanticModel, SemanticRule};
use crate::{CURRENT_SCHEMA_VERSION, sqlite};
use pdx_text::LogicalPath;

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
    pub(crate) exact_semantic_rules: BTreeMap<String, Vec<usize>>,
    pub(crate) semantic_rules_by_context: BTreeMap<String, Vec<usize>>,
    /// Lowercased context -> (lowercased exact key -> rule indices).
    pub(crate) semantic_exact_rules_by_context_key: BTreeMap<String, BTreeMap<String, Vec<usize>>>,
    /// Lowercased context -> rule indices whose key is not exact.
    pub(crate) semantic_non_exact_rules_by_context: BTreeMap<String, Vec<usize>>,
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
            semantic_exact_rules_by_context_key: BTreeMap::new(),
            semantic_non_exact_rules_by_context: BTreeMap::new(),
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
        let mut semantic_exact_rules_by_context_key =
            BTreeMap::<String, BTreeMap<String, Vec<usize>>>::new();
        let mut semantic_non_exact_rules_by_context = BTreeMap::<String, Vec<usize>>::new();
        for (index, rule) in model.semantic.rules.iter().enumerate() {
            match &rule.key {
                KeyMatcher::Exact(key) => {
                    exact_semantic_rules
                        .entry(key.to_ascii_lowercase())
                        .or_default()
                        .push(index);
                    semantic_exact_rules_by_context_key
                        .entry(rule.context.to_ascii_lowercase())
                        .or_default()
                        .entry(key.to_ascii_lowercase())
                        .or_default()
                        .push(index);
                }
                _ => {
                    semantic_non_exact_rules_by_context
                        .entry(rule.context.to_ascii_lowercase())
                        .or_default()
                        .push(index);
                }
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
            semantic_exact_rules_by_context_key,
            semantic_non_exact_rules_by_context,
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

    /// Returns semantic rules for one context and property key without scanning unrelated rules.
    ///
    /// Exact-key rules are indexed per context and key, so per-property lookups stay proportional
    /// to the few matching rules plus the context's non-exact matchers (type, enum, dynamic).
    pub fn semantic_rules_for_context_key(
        &self,
        context: &str,
        key: &str,
    ) -> impl Iterator<Item = &SemanticRule> {
        let context_key = context.to_ascii_lowercase();
        let exact = self
            .semantic_exact_rules_by_context_key
            .get(&context_key)
            .and_then(|by_key| by_key.get(&key.to_ascii_lowercase()))
            .into_iter()
            .flatten();
        let non_exact = self
            .semantic_non_exact_rules_by_context
            .get(&context_key)
            .into_iter()
            .flatten();
        exact
            .chain(non_exact)
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
    index: &'a BTreeMap<String, Vec<usize>>,
    key: &str,
) -> Option<&'a Vec<usize>> {
    if key.bytes().any(|byte| byte.is_ascii_uppercase()) {
        index.get(&key.to_ascii_lowercase())
    } else {
        index.get(key)
    }
}
