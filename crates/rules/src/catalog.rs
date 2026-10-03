//! File classification and parser selection for source scanning.
use crate::FileMatcher;
use serde::{Deserialize, Serialize};
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
    pub fn as_str(&self) -> String {
        match self {
            Self::Script => "script".to_owned(),
            Self::Localisation => "localisation".to_owned(),
            Self::Asset => "asset".to_owned(),
            Self::SyntaxOnly => "syntax-only".to_owned(),
        }
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
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReplaceByRelativePath => "replace-by-relative-path",
            Self::Merge => "merge",
            Self::ReplaceDirectory => "replace-directory",
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
