//! Source-ranged replacement templates shared by lowering and workspace facts.

use std::sync::Arc;
use text::TextRange;

/// One source token retained by a dynamic-definition template.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TemplateToken {
    /// Exact definition-side token range, including quotes when present.
    pub range: TextRange,
    /// Whether the source token was quoted.
    pub quoted: bool,
    /// Literal and parameter fragments in source order, excluding surrounding quotes.
    pub fragments: Vec<TemplateFragment>,
}

/// One literal or parameter fragment within a dynamic template token.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TemplateFragment {
    /// Definition-side text copied without interpretation.
    Literal(String),
    /// One owner-local parameter slot.
    Parameter {
        /// Parameter spelling without delimiters.
        name: Arc<str>,
        /// Exact definition-side range of the delimited occurrence.
        range: TextRange,
    },
}

/// The value attached to a property in a dynamic template.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TemplateValue {
    /// One scalar token.
    Scalar(TemplateToken),
    /// One ordered script block.
    Block {
        /// Exact definition-side block range.
        range: TextRange,
        /// Properties, bare values, and conditional blocks in source order.
        items: Vec<TemplateItem>,
    },
}

/// One property retained in a dynamic template.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TemplateProperty {
    /// Token supplying the property key.
    pub key: TemplateToken,
    /// Full definition-side property range.
    pub range: TextRange,
    /// Operator spelling recovered by the parser.
    pub operator: Option<Arc<str>>,
    /// Scalar or block value.
    pub value: TemplateValue,
}

/// One conditional block retained in a dynamic template.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TemplateConditional {
    /// Parameter spelling without `!`.
    pub name: Arc<str>,
    /// Whether the body is active when the parameter is absent.
    pub negated: bool,
    /// Full definition-side conditional range.
    pub range: TextRange,
    /// Ordered body items.
    pub items: Vec<TemplateItem>,
}

/// One ordered item in a dynamic template container.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TemplateItem {
    /// A key/operator/value property.
    Property(TemplateProperty),
    /// A standalone scalar in a mixed block.
    BareValue(TemplateToken),
    /// A supplied/absent parameter conditional.
    Conditional(TemplateConditional),
}

/// Reusable, source-ranged body of one scripted effect or trigger definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Template {
    /// Dynamic symbol kind, such as `scripted_effect`.
    pub kind: Arc<str>,
    /// Definition name as written in source.
    pub name: String,
    /// Full owning definition range.
    pub definition_range: TextRange,
    /// Exact body block range.
    pub body_range: TextRange,
    /// Ordered body items.
    pub items: Vec<TemplateItem>,
}
