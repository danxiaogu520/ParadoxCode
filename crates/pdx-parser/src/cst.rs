use std::fmt;

use pdx_text::TextRange;
use serde::{Deserialize, Serialize};

/// A loss-aware CST node kind shared by the EU4 syntax frontends.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub enum CstKind {
    /// Root node for a Script document.
    Document,
    /// A `key operator value` property.
    Property,
    /// Property key.
    Key,
    /// One of the eight supported Script operators.
    Operator,
    /// A value wrapper used by property nodes.
    Value,
    /// A mixed Script block.
    Block,
    /// An unquoted scalar.
    BareValue,
    /// A quoted scalar.
    QuotedString,
    /// A header followed by a block, such as `rgb { 1 2 3 }`.
    HeaderBlock,
    /// A conditional parameter block such as `[[!country] ... ]`.
    ParameterBlock,
    /// The condition inside a parameter block.
    ParameterCondition,
    /// A line comment.
    Comment,
    /// A UTF-8 BOM.
    Bom,
    /// Root node for a localisation document.
    LocalisationDocument,
    /// A localisation language header.
    LanguageHeader,
    /// A localisation entry.
    LocalisationEntry,
    /// A localisation key.
    LocalisationKey,
    /// A localisation entry version.
    Version,
    /// A quoted localisation value.
    LocalisationString,
    /// A recoverable unquoted localisation value.
    UnquotedValue,
    /// A parser recovery node.
    Error,
}

/// A typed, range-only CST node.
///
/// Node text is always read from the owning [`crate::ParsedFile`] source. The node therefore
/// stores no copied scalar or comment text and remains loss-aware for formatting and diagnostics.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CstNode {
    kind: CstKind,
    range: TextRange,
    children: Vec<CstNode>,
}

impl CstNode {
    pub(crate) fn new(kind: CstKind, range: TextRange, children: Vec<CstNode>) -> Self {
        Self {
            kind,
            range,
            children,
        }
    }

    /// Returns the node kind.
    #[must_use]
    pub const fn kind(&self) -> CstKind {
        self.kind
    }

    /// Returns the half-open UTF-8 source range.
    #[must_use]
    pub const fn range(&self) -> TextRange {
        self.range
    }

    /// Returns direct children in source order.
    #[must_use]
    pub fn children(&self) -> &[CstNode] {
        &self.children
    }

    /// Returns whether this node represents parser recovery.
    #[must_use]
    pub const fn is_error(&self) -> bool {
        matches!(self.kind, CstKind::Error)
    }
}

/// A lexical token retained by a parsed file for token-preservation checks.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub enum TokenKind {
    /// An unquoted scalar.
    Bare,
    /// A quoted scalar, including its quotes.
    Quoted,
    /// An operator.
    Operator,
    /// An opening delimiter.
    OpenDelimiter,
    /// A closing delimiter.
    CloseDelimiter,
    /// A colon in a localisation header or entry.
    Colon,
    /// A line comment.
    Comment,
    /// A UTF-8 BOM.
    Bom,
}

impl TokenKind {
    /// Returns whether this token is trivia for token-preservation comparisons.
    #[must_use]
    pub const fn is_trivia(self) -> bool {
        matches!(self, Self::Comment | Self::Bom)
    }
}

/// A token range in a parsed source.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SyntaxToken {
    kind: TokenKind,
    range: TextRange,
}

impl SyntaxToken {
    pub(crate) const fn new(kind: TokenKind, range: TextRange) -> Self {
        Self { kind, range }
    }

    /// Returns the token kind.
    #[must_use]
    pub const fn kind(self) -> TokenKind {
        self.kind
    }

    /// Returns the token range.
    #[must_use]
    pub const fn range(self) -> TextRange {
        self.range
    }
}

/// Stable categories of syntax errors.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub enum SyntaxErrorKind {
    /// A token cannot be interpreted at the current grammar position.
    UnexpectedToken,
    /// A property has an operator but no value.
    MissingValue,
    /// A quoted string has no closing quote.
    UnterminatedString,
    /// A `{` block has no closing `}`.
    UnterminatedBlock,
    /// A `[[... ] ... ]` block has no closing delimiter.
    UnterminatedParameterBlock,
    /// A localisation entry is missing a key, value, or version digits.
    InvalidLocalisationEntry,
    /// A localisation string has no closing quote.
    UnterminatedLocalisationString,
}

impl SyntaxErrorKind {
    /// Returns a stable diagnostic code suitable for later LSP mapping.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnexpectedToken => "pdx-parser-unexpected-token",
            Self::MissingValue => "pdx-parser-missing-value",
            Self::UnterminatedString => "pdx-parser-unterminated-string",
            Self::UnterminatedBlock => "pdx-parser-unterminated-block",
            Self::UnterminatedParameterBlock => "pdx-parser-unterminated-parameter-block",
            Self::InvalidLocalisationEntry => "pdx-localisation-invalid-entry",
            Self::UnterminatedLocalisationString => "pdx-localisation-unterminated-string",
        }
    }
}

/// A stable, source-ranged syntax diagnostic candidate.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SyntaxError {
    /// Stable error category.
    pub kind: SyntaxErrorKind,
    /// Source range associated with the error.
    pub range: TextRange,
    /// Human-readable explanation.
    pub message: String,
}

impl SyntaxError {
    pub(crate) fn new(kind: SyntaxErrorKind, range: TextRange, message: impl Into<String>) -> Self {
        Self {
            kind,
            range,
            message: message.into(),
        }
    }

    /// Returns the stable diagnostic code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.kind.code()
    }
}

impl fmt::Display for SyntaxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code(), self.message)
    }
}

impl std::error::Error for SyntaxError {}
