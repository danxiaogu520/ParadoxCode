use pdx_text::TextRange;
use unicode_width::UnicodeWidthChar;

const LINE_WIDTH: usize = 120;
const TAB_WIDTH: usize = 4;

/// Why formatting did not produce edits.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FormatSkipReason {
    /// The parser reported an error, so a rewrite cannot be proven safe.
    UnsafeSyntax,
    /// The canonical output failed structural, token, or idempotence validation.
    SafetyValidationFailed,
}

/// A single source edit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextEdit {
    /// Source range to replace.
    pub range: TextRange,
    /// Replacement text.
    pub replacement: String,
}

/// Formatter result that can safely contain no edits.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FormatResult {
    /// Safe, non-overlapping edits in source order.
    pub edits: Vec<TextEdit>,
    /// Explicit reason when formatting was skipped.
    pub skipped: Option<FormatSkipReason>,
}

pub(super) fn skipped(reason: FormatSkipReason) -> FormatResult {
    FormatResult {
        edits: Vec::new(),
        skipped: Some(reason),
    }
}

pub(super) fn indent(depth: usize) -> String {
    "\t".repeat(depth)
}

pub(super) fn fits_line(line: &str) -> bool {
    !contains_line_break(line) && display_width(line) <= LINE_WIDTH
}

fn display_width(text: &str) -> usize {
    text.chars().fold(0_usize, |column, character| {
        if character == '\t' {
            column.saturating_add(TAB_WIDTH - column % TAB_WIDTH)
        } else {
            column.saturating_add(UnicodeWidthChar::width(character).unwrap_or(0))
        }
    })
}

pub(super) fn contains_line_break(text: &str) -> bool {
    text.contains(['\r', '\n'])
}
