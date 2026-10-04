//! Immutable scanning catalog derived from the compiled IR.
use crate::{FileCategory, GameProfile, RuleHash};
use rustc_hash::FxHashMap;
use sha2::{Digest, Sha256};
use std::fmt;
use text::LogicalPath;
/// Errors from rule construction, validation, or first-party source compilation.
#[derive(Debug)]
pub enum RulesError {
    /// The rules belong to a different game profile.
    GameMismatch { expected: String, actual: String },
    /// The first-party JSON source could not be compiled into a runtime rule set.
    Source(String),
}

impl fmt::Display for RulesError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GameMismatch { expected, actual } => {
                write!(
                    formatter,
                    "rules game mismatch: expected {expected}, found {actual}"
                )
            }
            Self::Source(message) => write!(formatter, "first-party rule source error: {message}"),
        }
    }
}

impl std::error::Error for RulesError {}

/// Scanning metadata; semantic consumers use `RulesIr` directly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuleSet {
    rule_hash: RuleHash,
    game_id: String,
    file_categories: Vec<FileCategory>,
    profile: GameProfile,
    template_contexts: FxHashMap<Box<str>, Box<str>>,
}
impl RuleSet {
    /// Empty scanning catalog for syntax-only hosts.
    #[must_use]
    pub fn empty() -> Self {
        Self::from_catalog(String::new(), Vec::new(), GameProfile::default())
    }
    /// Constructs a catalog for source-scanning fixtures and nonsemantic providers.
    #[must_use]
    pub fn from_catalog(
        game_id: String,
        mut file_categories: Vec<FileCategory>,
        profile: GameProfile,
    ) -> Self {
        file_categories.sort_by(|a, b| a.id.cmp(&b.id));
        let bytes = serde_json::to_vec(&(&game_id, &file_categories, &profile))
            .expect("catalog serialization");
        let rule_hash = RuleHash::from_bytes(Sha256::digest(&bytes).into());
        Self {
            rule_hash,
            game_id,
            file_categories,
            profile,
            template_contexts: FxHashMap::default(),
        }
    }
    /// Extracts scanning and Template metadata without flattening semantic fields.
    #[must_use]
    pub fn from_ir_catalog(ir: &crate::ir::RulesIr) -> Self {
        use crate::ir::{DocumentParser, FileResolution};
        let files = ir
            .files
            .iter()
            .map(|file| FileCategory {
                id: ir.strings.resolve(file.name).to_owned(),
                matcher: file.matcher.clone(),
                parser: match file.parser {
                    DocumentParser::Script => crate::ParserKind::Script,
                    DocumentParser::Localisation => crate::ParserKind::Localisation,
                    DocumentParser::Asset => crate::ParserKind::Asset,
                    DocumentParser::SyntaxOnly => crate::ParserKind::SyntaxOnly,
                },
                resolution: match file.resolution {
                    FileResolution::ReplaceByPath => {
                        crate::FileResolutionPolicy::ReplaceByRelativePath
                    }
                    FileResolution::Merge => crate::FileResolutionPolicy::Merge,
                },
            })
            .collect();
        let mut rules = Self::from_catalog(ir.game_id().to_owned(), files, ir.game.profile.clone());
        rules.rule_hash = ir.rule_hash();
        if let Some(template) = ir.trait_by_name("Template") {
            for ty in &ir.types {
                if let Some(implementation) = ty.trait_impls.iter().find(|i| i.trait_id == template)
                    && let Some(body) = implementation.arguments.iter().find_map(|(name, value)| {
                        if !ir.strings.resolve(*name).eq_ignore_ascii_case("body") {
                            return None;
                        }
                        match value {
                            crate::ir::TraitArgument::Text(body) => Some(ir.strings.resolve(*body)),
                            _ => None,
                        }
                    })
                {
                    rules
                        .template_contexts
                        .insert(ir.strings.resolve(ty.name).into(), body.into());
                }
            }
        }
        rules
    }
    /// Configuration supplied by the compiled game package.
    #[must_use]
    pub const fn profile(&self) -> &GameProfile {
        &self.profile
    }
    /// Stable file catalog, without semantic rules.
    #[must_use]
    pub fn file_categories(&self) -> &[FileCategory] {
        &self.file_categories
    }
    /// Declared Template body context, derived from trait implementation arguments.
    #[must_use]
    pub fn dynamic_definition_context(&self, kind: &str) -> Option<&str> {
        self.template_contexts
            .get(kind.to_ascii_lowercase().as_str())
            .map(AsRef::as_ref)
    }
    /// Most specific matching category in deterministic catalog order.
    #[must_use]
    pub fn classify(&self, path: &LogicalPath) -> Option<&FileCategory> {
        self.file_categories
            .iter()
            .filter(|c| c.matcher.matches(path))
            .max_by_key(|c| c.matcher.specificity())
    }
    /// Selected game identity.
    #[must_use]
    pub fn game_id(&self) -> &str {
        &self.game_id
    }
    /// Derives a logical path for a document addressed by URI without a physical
    /// path, such as an editor scratch buffer.
    ///
    /// URIs carry arbitrary prefixes (`file:///`, percent-encoded drive letters,
    /// scheme names), while game directories are identified by their trailing
    /// structure. Every trailing run of path segments is tried as a candidate,
    /// but only candidates a *directory-aware* file category recognizes count:
    /// an extension-only category proves nothing about directories, so a path
    /// that only classifies that way keeps the caller's bare-file-name fallback
    /// instead of preserving URI junk segments. Among recognized candidates the
    /// most specific category wins, so `file:///tmp/common/scripted_effects/
    /// 00_a.txt` derives `common/scripted_effects/00_a.txt`.
    #[must_use]
    pub fn logical_path_for_uri(&self, uri: &str) -> Option<LogicalPath> {
        let body = uri.split_once("://").map_or(uri, |(_, rest)| rest);
        let segments: Vec<&str> = body
            .split(['/', '\\'])
            .filter(|segment| !segment.is_empty())
            .collect();
        if !segments.last()?.contains('.') {
            return None;
        }
        let mut best: Option<((u8, usize), LogicalPath)> = None;
        for start in (0..segments.len()).rev() {
            let candidate = segments[start..].join("/");
            let Ok(path) = LogicalPath::parse(&candidate) else {
                continue;
            };
            let Some(category) = self.classify(&path) else {
                continue;
            };
            let score @ (rank, _) = category.matcher.specificity();
            if rank == 0 {
                continue;
            }
            if best
                .as_ref()
                .is_none_or(|(best_score, _)| *best_score < score)
            {
                best = Some((score, path));
            }
        }
        best.map(|(_, path)| path)
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
    /// Returns the canonical content hash.
    #[must_use]
    pub const fn rule_hash(&self) -> RuleHash {
        self.rule_hash
    }
}
