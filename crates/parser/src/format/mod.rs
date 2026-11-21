//! Canonical, editor-neutral formatting for the PDX text frontends.
//!
//! Script uses tabs, LF line endings, recursive block layout, and no layout blank lines.
//! Ordinary scalar spelling is preserved;
//! the fixed keyword spelling (`AND`/`OR`/`NOT`, `ROOT`/`FROM`/`PREV`/`THIS`) is
//! canonicalized to capitals, and asset-path separators (`\`, `\\`, `//`) collapse to single
//! forward slashes — the game accepts every separator spelling and vanilla ships all of
//! them. Quoted strings with valid Script payloads are formatted recursively when they span
//! multiple lines or contain a top-level block assignment with no loose scalars.

use std::collections::BTreeMap;

use crate::{FileFormat, ParsedFile, parse};
use common::skipped;
use equivalence::{equivalent, minimal_edits};
use text::TextRange;

mod common;
mod equivalence;
mod localisation;
mod paths;
mod script;

pub use common::{FormatResult, FormatSkipReason, TextEdit};
pub use paths::is_asset_path_normalization;
pub use script::canonical_keyword;

/// A layout hint for the bare items in a Script block.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListLayout {
    /// Respect existing line breaks and expand a long single line.
    #[default]
    Auto,
    /// Keep the components of a logical value together.
    Inline,
    /// Place each item on its own line when there are multiple items.
    OnePerLine,
    /// Preserve existing rows and pack multiple items per line.
    Wrap,
    /// Pack multiple pairs per line without splitting a pair.
    WrapPairs,
}

#[cfg(test)]
mod tests;

/// Formats a parsed document using the fixed canonical style.
#[must_use]
pub fn format(file: &ParsedFile) -> FormatResult {
    format_with_list_layouts(file, |_| BTreeMap::new())
}

/// Formats with a caller-supplied map of Script block ranges to layout hints.
/// The resolver also receives the reparsed output so ranges follow moved blocks.
#[must_use]
pub fn format_with_list_layouts(
    file: &ParsedFile,
    resolve_layouts: impl Fn(&ParsedFile) -> BTreeMap<TextRange, ListLayout>,
) -> FormatResult {
    if !file.errors().is_empty() {
        return skipped(FormatSkipReason::UnsafeSyntax);
    }
    let formatted = canonical_text(file, &resolve_layouts);
    if formatted == file.source() {
        return FormatResult {
            edits: Vec::new(),
            skipped: None,
        };
    }

    let reparsed = parse(file.format(), &formatted);
    if !reparsed.errors().is_empty()
        || !equivalent(file, &reparsed, 0)
        || canonical_text(&reparsed, &resolve_layouts) != formatted
    {
        return skipped(FormatSkipReason::SafetyValidationFailed);
    }

    let Some(edits) = minimal_edits(file, &reparsed) else {
        return skipped(FormatSkipReason::SafetyValidationFailed);
    };
    FormatResult {
        edits,
        skipped: None,
    }
}

fn canonical_text(
    file: &ParsedFile,
    resolve_layouts: &impl Fn(&ParsedFile) -> BTreeMap<TextRange, ListLayout>,
) -> String {
    match file.format() {
        FileFormat::Script => script::format_script(file, &resolve_layouts(file)),
        FileFormat::Localisation => localisation::format_localisation(file),
    }
}
