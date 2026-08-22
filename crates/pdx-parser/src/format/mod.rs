//! Canonical, editor-neutral formatting for the PDX text frontends.
//!
//! The formatter is intentionally non-configurable. Script uses tabs, LF line endings,
//! recursive block layout, and no layout blank lines. Ordinary scalar spelling is preserved;
//! the fixed keyword spelling (`AND`/`OR`/`NOT`, `ROOT`/`FROM`/`PREV`/`THIS`) is
//! canonicalized to capitals. Multiline quoted strings are formatted recursively only when
//! their decoded payload is demonstrably valid, non-empty Script.

use crate::{FileFormat, ParsedFile, parse};
use common::skipped;
use equivalence::{equivalent, minimal_edits};

mod common;
mod equivalence;
mod localisation;
mod script;

pub use common::{FormatResult, FormatSkipReason, TextEdit};

#[cfg(test)]
mod tests;

/// Formats a parsed document using the fixed canonical style.
#[must_use]
pub fn format(file: &ParsedFile) -> FormatResult {
    if !file.errors().is_empty() {
        return skipped(FormatSkipReason::UnsafeSyntax);
    }
    let formatted = canonical_text(file);
    if formatted == file.source() {
        return FormatResult {
            edits: Vec::new(),
            skipped: None,
        };
    }

    let reparsed = parse(file.format(), &formatted);
    if !reparsed.errors().is_empty()
        || !equivalent(file, &reparsed, 0)
        || canonical_text(&reparsed) != formatted
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

fn canonical_text(file: &ParsedFile) -> String {
    match file.format() {
        FileFormat::Script => script::format_script(file),
        FileFormat::Localisation => localisation::format_localisation(file),
    }
}
