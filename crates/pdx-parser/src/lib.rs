//! Loss-aware Paradox text syntax boundary.
//!
//! The Phase 3 facade exposes grammar-shaped, loss-aware CST nodes and stable syntax errors for
//! reusable Paradox script and localisation frontends. A game profile classifies files and
//! selects the appropriate frontend.

use pdx_text::TextRange;
use serde::{Deserialize, Serialize};

mod cst;
pub mod format;
mod localisation;
mod quoted_script;
mod script;

pub use cst::{CstKind, CstNode, SyntaxError, SyntaxErrorKind, SyntaxToken, TokenKind};
pub use quoted_script::{
    QuotedScript, QuotedScriptSourceMap, encode_quoted_script_text, parse_quoted_script,
};

/// One of the reusable Paradox text frontends.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum FileFormat {
    /// Paradox key/value script.
    Script,
    /// EU4 localisation YAML-like text.
    Localisation,
}

/// A source edit applied to a parsed syntax document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyntaxEdit {
    /// UTF-8 byte range to replace. `None` means a full-document replacement.
    pub range: Option<TextRange>,
    /// Replacement text.
    pub replacement: String,
}

impl SyntaxEdit {
    /// Creates a full-document edit.
    #[must_use]
    pub fn full(replacement: impl Into<String>) -> Self {
        Self {
            range: None,
            replacement: replacement.into(),
        }
    }

    /// Creates a ranged edit.
    #[must_use]
    pub fn ranged(range: TextRange, replacement: impl Into<String>) -> Self {
        Self {
            range: Some(range),
            replacement: replacement.into(),
        }
    }
}

/// Failure to apply a syntax edit without guessing at offsets.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EditError {
    /// The requested range is outside the source or splits a UTF-8 code point.
    InvalidRange(TextRange),
}

impl std::fmt::Display for EditError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRange(range) => {
                write!(
                    formatter,
                    "invalid syntax edit range {}..{}",
                    range.start(),
                    range.end()
                )
            }
        }
    }
}

impl std::error::Error for EditError {}

pub(crate) struct ParseParts {
    root: CstNode,
    tokens: Vec<SyntaxToken>,
    errors: Vec<SyntaxError>,
}

/// A loss-aware parsed EU4 document.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ParsedFile {
    format: FileFormat,
    source: String,
    root: CstNode,
    tokens: Vec<SyntaxToken>,
    errors: Vec<SyntaxError>,
    revision: u64,
}

impl PartialEq for ParsedFile {
    fn eq(&self, other: &Self) -> bool {
        self.format == other.format
            && self.source == other.source
            && self.root == other.root
            && self.tokens == other.tokens
            && self.errors == other.errors
            && self.revision == other.revision
    }
}

impl Eq for ParsedFile {}

impl ParsedFile {
    fn from_parts(format: FileFormat, source: &str, parts: ParseParts, revision: u64) -> Self {
        Self {
            format,
            source: source.to_owned(),
            root: parts.root,
            tokens: parts.tokens,
            errors: parts.errors,
            revision,
        }
    }

    /// Returns the selected frontend kind.
    #[must_use]
    pub const fn format(&self) -> FileFormat {
        self.format
    }

    /// Returns the lossless source text retained by this parse handle.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Returns the typed CST root.
    #[must_use]
    pub const fn root(&self) -> &CstNode {
        &self.root
    }

    /// Returns lexical tokens in source order.
    #[must_use]
    pub fn tokens(&self) -> &[SyntaxToken] {
        &self.tokens
    }

    /// Returns the source text covered by a token or node range.
    #[must_use]
    pub fn text(&self, range: TextRange) -> Option<&str> {
        let start = usize::try_from(range.start()).ok()?;
        let end = usize::try_from(range.end()).ok()?;
        self.source.get(start..end)
    }

    /// Returns syntax errors, in source order.
    #[must_use]
    pub fn errors(&self) -> &[SyntaxError] {
        &self.errors
    }

    /// Returns the monotonically increasing parse revision.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Returns whether a deserialized parse can safely be reused for the supplied source.
    ///
    /// Persistent parse caches are untrusted input. Besides matching the source fingerprint,
    /// callers use this structural check to reject out-of-range or cross-parent CST ranges before
    /// the tree reaches lowering and editor queries.
    #[must_use]
    pub fn is_valid_for(&self, format: FileFormat, source: &str) -> bool {
        self.format == format
            && self.source == source
            && matches!(
                (format, self.root.kind()),
                (FileFormat::Script, CstKind::Document)
                    | (FileFormat::Localisation, CstKind::LocalisationDocument)
            )
            && valid_node(&self.root, None, source)
            && self
                .tokens
                .iter()
                .all(|token| valid_range(token.range(), source))
            && self
                .errors
                .iter()
                .all(|error| valid_range(error.range, source))
    }

    /// Applies one edit and reparses the affected frontend.
    ///
    /// The public operation is deliberately edit-shaped so the workspace can later replace the
    /// implementation can later add subtree reuse without changing callers.
    /// The Phase 3 correctness boundary is that its root, token sequence, and diagnostics are
    /// identical to a full parse of the resulting source.
    pub fn apply_edit(&self, edit: &SyntaxEdit) -> Result<Self, EditError> {
        let mut source = self.source.clone();
        if let Some(range) = edit.range {
            let start = usize::try_from(range.start()).ok();
            let end = usize::try_from(range.end()).ok();
            let valid = start
                .zip(end)
                .is_some_and(|(start, end)| start <= end && source.get(start..end).is_some());
            if !valid {
                return Err(EditError::InvalidRange(range));
            }
            if let (Some(start), Some(end)) = (start, end) {
                source.replace_range(start..end, &edit.replacement);
            }
        } else {
            source = edit.replacement.clone();
        }
        Ok(parse_with_revision(
            self.format,
            &source,
            self.revision.saturating_add(1),
        ))
    }
}

/// Creates a typed parse handle for one EU4 text frontend.
#[must_use]
pub fn parse(format: FileFormat, source: &str) -> ParsedFile {
    parse_with_revision(format, source, 0)
}

fn parse_with_revision(format: FileFormat, source: &str, revision: u64) -> ParsedFile {
    let parts = match format {
        FileFormat::Script => script::parse(source),
        FileFormat::Localisation => localisation::parse(source),
    };
    ParsedFile::from_parts(format, source, parts, revision)
}

fn valid_node(node: &CstNode, parent: Option<TextRange>, source: &str) -> bool {
    valid_range(node.range(), source)
        && parent.is_none_or(|parent| {
            parent.start() <= node.range().start() && node.range().end() <= parent.end()
        })
        && node
            .children()
            .iter()
            .all(|child| valid_node(child, Some(node.range()), source))
}

fn valid_range(range: TextRange, source: &str) -> bool {
    let Some(start) = usize::try_from(range.start()).ok() else {
        return false;
    };
    let Some(end) = usize::try_from(range.end()).ok() else {
        return false;
    };
    start <= end && end <= source.len() && source.get(start..end).is_some()
}

pub(crate) fn range(start: usize, end: usize) -> TextRange {
    let start = u32::try_from(start).unwrap_or(u32::MAX);
    let end = u32::try_from(end).unwrap_or(u32::MAX).max(start);
    TextRange::new(start, end).unwrap_or_else(|| TextRange::empty(start))
}

#[cfg(test)]
mod tests {
    use pdx_text::TextRange;

    use super::{CstKind, FileFormat, SyntaxEdit, SyntaxErrorKind, parse};

    #[test]
    fn phase0_parse_preserves_source() {
        let parsed = parse(FileFormat::Script, "# comment\nkey = value");
        assert_eq!(parsed.source(), "# comment\nkey = value");
        assert!(parsed.errors().is_empty());
        assert_eq!(parsed.root().kind(), CstKind::Document);
    }

    #[test]
    fn script_cst_preserves_duplicate_properties_comments_and_headers() {
        let parsed = parse(
            FileFormat::Script,
            "# note\nname = one\nname = two\nrgb { 1 2 3 }\n[[!country] value = yes]",
        );
        assert!(parsed.errors().is_empty(), "errors: {:?}", parsed.errors());
        assert_eq!(parsed.root().children().len(), 5);
        assert_eq!(parsed.root().children()[1].kind(), CstKind::Property);
        assert_eq!(parsed.root().children()[3].kind(), CstKind::HeaderBlock);
        assert_eq!(parsed.root().children()[4].kind(), CstKind::ParameterBlock);
        assert_eq!(
            parsed.text(parsed.root().children()[0].range()),
            Some("# note")
        );
    }

    #[test]
    fn script_accepts_quoted_property_keys() {
        let parsed = parse(
            FileFormat::Script,
            "monarch_names = { \"Friedrich #0\" = 100 }\n",
        );
        assert!(parsed.errors().is_empty(), "errors: {:?}", parsed.errors());
        let monarch_names = &parsed.root().children()[0];
        let block = monarch_names
            .children()
            .iter()
            .find(|child| child.kind() == CstKind::Value)
            .and_then(|value| {
                value
                    .children()
                    .iter()
                    .find(|child| child.kind() == CstKind::Block)
            })
            .expect("monarch names block");
        assert_eq!(block.children().len(), 1);
        assert_eq!(block.children()[0].kind(), CstKind::Property);
        let key = block.children()[0]
            .children()
            .iter()
            .find(|child| child.kind() == CstKind::Key)
            .expect("quoted key");
        assert_eq!(parsed.text(key.range()), Some("\"Friedrich #0\""));
    }

    #[test]
    fn script_accepts_anonymous_nested_vector_blocks() {
        let parsed = parse(
            FileFormat::Script,
            "position = { { 0.0 -5.0 0.0 } { 0.0 -6.5 0.0 } }\n",
        );
        assert!(parsed.errors().is_empty(), "errors: {:?}", parsed.errors());
        let value = parsed.root().children()[0]
            .children()
            .iter()
            .find(|child| child.kind() == CstKind::Value)
            .expect("position value");
        let outer = value
            .children()
            .iter()
            .find(|child| child.kind() == CstKind::Block)
            .expect("outer block");
        assert_eq!(
            outer
                .children()
                .iter()
                .filter(|child| child.kind() == CstKind::Block)
                .count(),
            2
        );
    }

    #[test]
    fn syntax_errors_have_stable_codes_and_safe_ranges() {
        let parsed = parse(FileFormat::Script, "key = { value = \"unfinished");
        assert!(parsed.errors().iter().any(|error| {
            error.kind == SyntaxErrorKind::UnterminatedString
                && error.code() == "pdx-parser-unterminated-string"
        }));
        assert!(parsed.errors().iter().all(|error| {
            parsed.text(error.range).is_some()
                && usize::try_from(error.range.end()).unwrap_or(usize::MAX) <= parsed.source().len()
        }));
    }

    #[test]
    fn rust_parser_recovery_nodes_map_to_stable_errors() {
        let parsed = parse(FileFormat::Script, "\"top_level_string\"");
        assert!(parsed.errors().iter().any(|error| {
            error.kind == SyntaxErrorKind::UnexpectedToken
                && error.code() == "pdx-parser-unexpected-token"
        }));
    }

    #[test]
    fn localisation_has_typed_facade() {
        let localisation = parse(
            FileFormat::Localisation,
            "l_english:\nhello:0 \"Hello $NAME$\"\nother:0 text # note\n",
        );
        assert!(
            localisation.errors().is_empty(),
            "errors: {:?}",
            localisation.errors()
        );
        assert_eq!(localisation.root().children().len(), 3);
        assert_eq!(
            localisation.root().children()[1].kind(),
            CstKind::LocalisationEntry
        );
    }

    #[test]
    fn localisation_entries_allow_missing_version_digits() {
        let localisation = parse(
            FileFormat::Localisation,
            "l_english:\nhello: \"text\"\nother:0 \"text\"\nlast: text\n",
        );
        assert!(
            localisation.errors().is_empty(),
            "errors: {:?}",
            localisation.errors()
        );
        let entries = localisation
            .root()
            .children()
            .iter()
            .filter(|child| child.kind() == CstKind::LocalisationEntry)
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn incremental_edits_match_full_reparse() {
        let mut current = parse(FileFormat::Script, "name = one\nvalue = yes\n");
        let edits = [
            SyntaxEdit::ranged(TextRange::new(8, 11).expect("range"), "two"),
            SyntaxEdit::ranged(TextRange::new(0, 4).expect("range"), "title"),
            SyntaxEdit::full("title = final\n"),
        ];
        for edit in edits {
            let next = current.apply_edit(&edit).expect("edit should apply");
            let expected = parse(FileFormat::Script, next.source());
            assert_eq!(next.root(), expected.root());
            assert_eq!(next.tokens(), expected.tokens());
            assert_eq!(next.errors(), expected.errors());
            current = next;
        }
        assert_eq!(current.revision(), 3);
    }
}
