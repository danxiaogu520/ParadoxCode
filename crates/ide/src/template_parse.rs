//! Bounded decoding and CST parsing of quoted carriers consumed by a Template.
use parser::{CstNode, QuotedScript};

use crate::types::{CancellationToken, Cancelled};

pub(crate) const MAX_TEMPLATE_PARSE_DEPTH: usize = 32;
pub(crate) const MAX_TEMPLATE_PARSE_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_TEMPLATE_PARSE_TOTAL_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_TEMPLATE_PARSE_NODES: usize = 50_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TemplateParseLimit {
    Depth,
    CarrierBytes,
    TotalBytes,
    Nodes,
}

impl TemplateParseLimit {
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::Depth => "quoted script nesting exceeds the analysis depth limit",
            Self::CarrierBytes => "quoted script exceeds the analysis payload limit",
            Self::TotalBytes => "quoted scripts exceed the analysis byte budget",
            Self::Nodes => "quoted scripts exceed the analysis node budget",
        }
    }
}

pub(crate) enum TemplateParse {
    Parsed(QuotedScript),
    Opaque,
    Limited(TemplateParseLimit),
}

/// Query-local budget for secondary Script parses. The same policy is shared by diagnostics,
/// completion, hover and navigation so malformed editor input cannot take an unbounded path in
/// one feature while remaining bounded in another.
pub(crate) struct TemplateParseSession<'cancel> {
    cancellation: &'cancel CancellationToken,
    parsed_bytes: usize,
    parsed_nodes: usize,
}

impl<'cancel> TemplateParseSession<'cancel> {
    pub(crate) const fn new(cancellation: &'cancel CancellationToken) -> Self {
        Self {
            cancellation,
            parsed_bytes: 0,
            parsed_nodes: 0,
        }
    }

    pub(crate) fn parse(&mut self, source: &str, depth: usize) -> Result<TemplateParse, Cancelled> {
        self.cancellation.checkpoint()?;
        if depth >= MAX_TEMPLATE_PARSE_DEPTH {
            return Ok(TemplateParse::Limited(TemplateParseLimit::Depth));
        }
        if source.len() > MAX_TEMPLATE_PARSE_BYTES {
            return Ok(TemplateParse::Limited(TemplateParseLimit::CarrierBytes));
        }
        self.parsed_bytes = self.parsed_bytes.saturating_add(source.len());
        if self.parsed_bytes > MAX_TEMPLATE_PARSE_TOTAL_BYTES {
            return Ok(TemplateParse::Limited(TemplateParseLimit::TotalBytes));
        }
        let script = parser::parse_quoted_script_bounded(
            source,
            parser::ScriptParseBudget {
                bytes: MAX_TEMPLATE_PARSE_BYTES,
                nodes: MAX_TEMPLATE_PARSE_NODES.saturating_sub(self.parsed_nodes),
                ..Default::default()
            },
            &mut || self.cancellation.checkpoint(),
        )?;
        let script = match script {
            Ok(Some(script)) => script,
            Ok(None) => return Ok(TemplateParse::Opaque),
            Err(parser::ScriptParseLimit::Bytes) => {
                return Ok(TemplateParse::Limited(TemplateParseLimit::CarrierBytes));
            }
            Err(parser::ScriptParseLimit::Nodes) => {
                return Ok(TemplateParse::Limited(TemplateParseLimit::Nodes));
            }
            Err(parser::ScriptParseLimit::Depth) => {
                return Ok(TemplateParse::Limited(TemplateParseLimit::Depth));
            }
        };
        self.parsed_nodes = self
            .parsed_nodes
            .saturating_add(cst_node_count(script.parsed().root()));
        if self.parsed_nodes > MAX_TEMPLATE_PARSE_NODES {
            return Ok(TemplateParse::Limited(TemplateParseLimit::Nodes));
        }
        self.cancellation.checkpoint()?;
        Ok(TemplateParse::Parsed(script))
    }
}

fn cst_node_count(node: CstNode<'_>) -> usize {
    let mut count = 0usize;
    let mut pending = vec![node];
    while let Some(current) = pending.pop() {
        count = count.saturating_add(1);
        pending.extend(current.children());
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_budget_limits_depth_and_accumulated_bytes() {
        let cancellation = CancellationToken::new();
        let mut session = TemplateParseSession::new(&cancellation);
        assert!(matches!(
            session
                .parse("\"foo = yes\"", MAX_TEMPLATE_PARSE_DEPTH)
                .expect("parse"),
            TemplateParse::Limited(TemplateParseLimit::Depth)
        ));

        let large = format!("\"{}\"", "a".repeat(MAX_TEMPLATE_PARSE_TOTAL_BYTES / 2));
        assert!(matches!(
            session.parse(&large, 0).expect("first parse"),
            TemplateParse::Parsed(_)
        ));
        assert!(matches!(
            session.parse(&large, 0).expect("second parse"),
            TemplateParse::Limited(TemplateParseLimit::TotalBytes)
        ));
    }
}
