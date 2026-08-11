use pdx_parser::{CstNode, QuotedScript, parse_quoted_script};

use crate::types::{CancellationToken, Cancelled};

pub(crate) const MAX_QUOTED_SCRIPT_DEPTH: usize = 32;
pub(crate) const MAX_QUOTED_SCRIPT_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_QUOTED_SCRIPT_TOTAL_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_QUOTED_SCRIPT_NODES: usize = 50_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum QuotedScriptLimit {
    Depth,
    PayloadBytes,
    TotalBytes,
    Nodes,
}

impl QuotedScriptLimit {
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::Depth => "quoted Script exceeds the semantic nesting depth limit",
            Self::PayloadBytes => "quoted Script exceeds the semantic payload size limit",
            Self::TotalBytes => "quoted Script exceeds the semantic query byte budget",
            Self::Nodes => "quoted Script exceeds the semantic query node budget",
        }
    }
}

pub(crate) enum QuotedScriptParse {
    Parsed(QuotedScript),
    Opaque,
    Limited(QuotedScriptLimit),
}

/// Query-local budget for secondary Script parses. The same policy is shared by diagnostics,
/// completion, hover and navigation so malformed editor input cannot take an unbounded path in
/// one feature while remaining bounded in another.
pub(crate) struct QuotedScriptSession<'cancel> {
    cancellation: &'cancel CancellationToken,
    parsed_bytes: usize,
    parsed_nodes: usize,
}

impl<'cancel> QuotedScriptSession<'cancel> {
    pub(crate) const fn new(cancellation: &'cancel CancellationToken) -> Self {
        Self {
            cancellation,
            parsed_bytes: 0,
            parsed_nodes: 0,
        }
    }

    pub(crate) const fn cancellation(&self) -> &'cancel CancellationToken {
        self.cancellation
    }

    pub(crate) fn parse(
        &mut self,
        source: &str,
        depth: usize,
    ) -> Result<QuotedScriptParse, Cancelled> {
        self.cancellation.checkpoint()?;
        if depth >= MAX_QUOTED_SCRIPT_DEPTH {
            return Ok(QuotedScriptParse::Limited(QuotedScriptLimit::Depth));
        }
        if source.len() > MAX_QUOTED_SCRIPT_BYTES {
            return Ok(QuotedScriptParse::Limited(QuotedScriptLimit::PayloadBytes));
        }
        self.parsed_bytes = self.parsed_bytes.saturating_add(source.len());
        if self.parsed_bytes > MAX_QUOTED_SCRIPT_TOTAL_BYTES {
            return Ok(QuotedScriptParse::Limited(QuotedScriptLimit::TotalBytes));
        }
        let Some(script) = parse_quoted_script(source) else {
            return Ok(QuotedScriptParse::Opaque);
        };
        self.parsed_nodes = self
            .parsed_nodes
            .saturating_add(cst_node_count(script.parsed().root()));
        if self.parsed_nodes > MAX_QUOTED_SCRIPT_NODES {
            return Ok(QuotedScriptParse::Limited(QuotedScriptLimit::Nodes));
        }
        self.cancellation.checkpoint()?;
        Ok(QuotedScriptParse::Parsed(script))
    }
}

fn cst_node_count(node: &CstNode) -> usize {
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
        let mut session = QuotedScriptSession::new(&cancellation);
        assert!(matches!(
            session
                .parse("\"foo = yes\"", MAX_QUOTED_SCRIPT_DEPTH)
                .expect("parse"),
            QuotedScriptParse::Limited(QuotedScriptLimit::Depth)
        ));

        let large = format!("\"{}\"", "a".repeat(MAX_QUOTED_SCRIPT_TOTAL_BYTES / 2));
        assert!(matches!(
            session.parse(&large, 0).expect("first parse"),
            QuotedScriptParse::Parsed(_)
        ));
        assert!(matches!(
            session.parse(&large, 0).expect("second parse"),
            QuotedScriptParse::Limited(QuotedScriptLimit::TotalBytes)
        ));
    }
}
