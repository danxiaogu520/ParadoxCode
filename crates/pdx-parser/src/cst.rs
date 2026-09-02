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

/// Flat arena holding one document's typed CST.
///
/// Node text is always read from the owning [`crate::ParsedFile`] source, so the arena stores
/// only kinds, ranges, and child links. Children of one node are contiguous in `edges`, which
/// keeps whole-tree memory proportional to node count (17 bytes per node plus 4 bytes per
/// child link) instead of one heap allocation per interior node.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SyntaxTree {
    /// Node kind by node index.
    kinds: Vec<CstKind>,
    /// Node range and child-link span by node index.
    cores: Vec<NodeCore>,
    /// Child node indices; each node's children are one contiguous run.
    edges: Vec<u32>,
    /// Index of the document root. Builders emit parents after children, so the root is the
    /// final pushed node.
    root: u32,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct NodeCore {
    range: TextRange,
    first_edge: u32,
    edge_count: u32,
}

impl SyntaxTree {
    /// Returns the document root as a borrowed handle.
    #[must_use]
    pub fn root(&self) -> CstNode<'_> {
        CstNode {
            tree: self,
            index: self.root,
        }
    }

    /// Returns the number of nodes in the tree, including the root.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.cores.len()
    }
}

/// A borrowed handle to one node of a [`SyntaxTree`].
///
/// Handles are `Copy` and borrow only the immutable arena, so walking a document never
/// mutates or clones tree storage.
#[derive(Clone, Copy)]
pub struct CstNode<'tree> {
    tree: &'tree SyntaxTree,
    index: u32,
}

impl<'tree> CstNode<'tree> {
    pub(crate) fn new(tree: &'tree SyntaxTree, index: u32) -> Self {
        Self { tree, index }
    }

    /// Returns the node kind.
    #[must_use]
    pub fn kind(&self) -> CstKind {
        self.tree.kinds[self.index as usize]
    }

    /// Returns the half-open UTF-8 source range.
    #[must_use]
    pub fn range(&self) -> TextRange {
        self.tree.cores[self.index as usize].range
    }

    /// Returns direct children in source order.
    #[must_use]
    pub fn children(&self) -> CstChildren<'tree> {
        let core = self.tree.cores[self.index as usize];
        let first = core.first_edge as usize;
        let end = first + core.edge_count as usize;
        CstChildren {
            tree: self.tree,
            edges: self.tree.edges.get(first..end).unwrap_or_default().iter(),
        }
    }

    /// Returns the number of direct children.
    #[must_use]
    pub fn child_count(&self) -> usize {
        self.tree.cores[self.index as usize].edge_count as usize
    }

    /// Returns the child at one position, like indexing the children slice.
    #[must_use]
    pub fn child(&self, position: usize) -> Option<Self> {
        let core = self.tree.cores[self.index as usize];
        if position >= core.edge_count as usize {
            return None;
        }
        self.tree
            .edges
            .get(core.first_edge as usize + position)
            .map(|index| Self {
                tree: self.tree,
                index: *index,
            })
    }

    /// Returns whether this node represents parser recovery.
    #[must_use]
    pub fn is_error(&self) -> bool {
        matches!(self.kind(), CstKind::Error)
    }
}

impl fmt::Debug for CstNode<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CstNode")
            .field("kind", &self.kind())
            .field("range", &self.range())
            .field("child_count", &self.child_count())
            .finish()
    }
}

/// Structural equality across trees, comparing kind, range, and children recursively.
/// Array-level identity for trees built identically lives on [`SyntaxTree`] instead.
impl<'a, 'b> PartialEq<CstNode<'b>> for CstNode<'a> {
    fn eq(&self, other: &CstNode<'b>) -> bool {
        self.kind() == other.kind()
            && self.range() == other.range()
            && self.child_count() == other.child_count()
            && self
                .children()
                .zip(other.children())
                .all(|(left, right)| left == right)
    }
}

impl Eq for CstNode<'_> {}

/// Iterator over one node's direct children.
#[derive(Clone, Debug)]
pub struct CstChildren<'tree> {
    tree: &'tree SyntaxTree,
    edges: std::slice::Iter<'tree, u32>,
}

impl<'tree> Iterator for CstChildren<'tree> {
    type Item = CstNode<'tree>;

    fn next(&mut self) -> Option<Self::Item> {
        self.edges
            .next()
            .map(|index| CstNode::new(self.tree, *index))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.edges.size_hint()
    }
}

impl DoubleEndedIterator for CstChildren<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.edges
            .next_back()
            .map(|index| CstNode::new(self.tree, *index))
    }
}

impl ExactSizeIterator for CstChildren<'_> {}
impl std::iter::FusedIterator for CstChildren<'_> {}

/// Incremental arena builder used by the syntax frontends.
///
/// Parsers push child node indices onto a shared stack and then emit the parent with
/// `child_count`; the builder splices that stack segment into the flat edge array, so tree
/// construction performs no per-node allocations.
#[derive(Default)]
pub(crate) struct SyntaxTreeBuilder {
    tree: SyntaxTree,
    /// Pending child indices below the current construction mark.
    children: Vec<u32>,
    /// Set once the arena's u32 index space is exhausted; parsing then stops gracefully.
    saturated: bool,
}

impl SyntaxTreeBuilder {
    /// Emits a node whose children are the top `child_count` pending indices.
    pub(crate) fn node(&mut self, kind: CstKind, range: TextRange, child_count: usize) -> u32 {
        debug_assert!(self.children.len() >= child_count);
        let first_edge = self.tree.edges.len();
        let mark = self.children.len() - child_count;
        self.tree.edges.extend_from_slice(&self.children[mark..]);
        self.children.truncate(mark);
        let index = self.tree.cores.len() as u32;
        self.tree.kinds.push(kind);
        self.tree.cores.push(NodeCore {
            range,
            first_edge: first_edge as u32,
            edge_count: child_count as u32,
        });
        // The arena is index-addressed by u32. Reaching this bound needs a source on the
        // order of gigabytes; saturating stops the parse instead of corrupting the tree.
        self.saturated = self.tree.cores.len() >= u32::MAX as usize;
        index
    }

    /// Queues one emitted node as a child of the parent under construction.
    pub(crate) fn push_child(&mut self, index: u32) {
        self.children.push(index);
    }

    /// Returns the current pending-child mark for span computations.
    pub(crate) fn child_mark(&self) -> usize {
        self.children.len()
    }

    /// Returns how many pending children were queued since a mark.
    pub(crate) fn children_since(&self, mark: usize) -> usize {
        self.children.len() - mark
    }

    /// Returns whether the arena index space is exhausted.
    pub(crate) fn is_saturated(&self) -> bool {
        self.saturated
    }

    /// Returns the emitted range of one node.
    pub(crate) fn node_range(&self, index: u32) -> TextRange {
        self.tree.cores[index as usize].range
    }

    /// Finishes the tree, marking the last emitted node as the root.
    pub(crate) fn finish(mut self) -> SyntaxTree {
        self.tree.root = (self.tree.cores.len() as u32).wrapping_sub(1);
        self.tree
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

#[cfg(test)]
mod tests {
    use super::{CstKind, SyntaxTreeBuilder};
    use crate::range;

    #[test]
    fn builder_emits_post_order_tree_with_contiguous_children() {
        let mut builder = SyntaxTreeBuilder::default();
        // key leaf, operator leaf, value leaf
        let key = builder.node(CstKind::BareValue, range(0, 3), 0);
        let operator = builder.node(CstKind::Operator, range(4, 5), 0);
        let value = builder.node(CstKind::BareValue, range(6, 11), 0);
        builder.push_child(key);
        let key_node = builder.node(CstKind::Key, range(0, 3), 1);
        builder.push_child(value);
        let value_node = builder.node(CstKind::Value, range(6, 11), 1);
        builder.push_child(key_node);
        builder.push_child(operator);
        builder.push_child(value_node);
        let property = builder.node(CstKind::Property, range(0, 11), 3);
        builder.push_child(property);
        let root = builder.node(CstKind::Document, range(0, 11), 1);
        let tree = builder.finish();
        assert_eq!(tree.root(), tree.root());
        assert_eq!(root, tree.node_count() as u32 - 1);
        let document = tree.root();
        assert_eq!(document.kind(), CstKind::Document);
        assert_eq!(document.child_count(), 1);
        let parsed_property = document.child(0).expect("property child");
        assert_eq!(parsed_property.kind(), CstKind::Property);
        let kinds: Vec<_> = parsed_property
            .children()
            .map(|child| child.kind())
            .collect();
        assert_eq!(kinds, [CstKind::Key, CstKind::Operator, CstKind::Value]);
        assert_eq!(parsed_property.child(3), None);
        let last = parsed_property.children().next_back().expect("value");
        assert_eq!(last.kind(), CstKind::Value);
        assert_eq!(last.range().start(), 6);
    }
}
