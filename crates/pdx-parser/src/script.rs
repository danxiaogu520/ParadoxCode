use pdx_text::TextRange;

use super::{
    CstKind, ParseParts, SyntaxError, SyntaxErrorKind, SyntaxToken, SyntaxTreeBuilder, TokenKind,
};

pub(crate) fn parse(source: &str) -> ParseParts {
    let mut parser = Parser::new(source);
    let mark = parser.tree.child_mark();
    parser.parse_container(None);
    let children = parser.tree.children_since(mark);
    parser.node(CstKind::Document, 0, source.len(), children);
    ParseParts {
        tree: parser.tree.finish(),
        tokens: parser.tokens,
        errors: parser.errors,
    }
}

struct Parser<'source> {
    source: &'source str,
    position: usize,
    tokens: Vec<SyntaxToken>,
    errors: Vec<SyntaxError>,
    tree: SyntaxTreeBuilder,
}

impl<'source> Parser<'source> {
    fn new(source: &'source str) -> Self {
        Self {
            source,
            position: 0,
            tokens: Vec::new(),
            errors: Vec::new(),
            tree: SyntaxTreeBuilder::default(),
        }
    }

    /// Emits one node whose children are the most recently pushed `child_count` indices.
    fn node(&mut self, kind: CstKind, start: usize, end: usize, child_count: usize) -> u32 {
        self.tree.node(kind, super::range(start, end), child_count)
    }

    /// Queues one emitted node as a child of the parent under construction.
    fn push(&mut self, index: u32) {
        self.tree.push_child(index);
    }

    fn children_mark(&self) -> usize {
        self.tree.child_mark()
    }

    fn children_since(&self, mark: usize) -> usize {
        self.tree.children_since(mark)
    }

    fn node_range(&self, index: u32) -> TextRange {
        self.tree.node_range(index)
    }

    /// Returns the source text of a byte range, for quoting the offending
    /// token in a diagnostic message.
    fn slice(&self, start: usize, end: usize) -> &str {
        let end = end.min(self.source.len());
        let start = start.min(end);
        self.source
            .get(start..end)
            .filter(|text| !text.is_empty())
            .unwrap_or("")
    }

    /// Truncates a quoted token so pathological input cannot flood the message.
    fn snippet(&self, start: usize, end: usize) -> String {
        let text = self.slice(start, end);
        if text.chars().count() > 24 {
            let prefix: String = text.chars().take(24).collect();
            format!("{prefix}...")
        } else {
            text.to_owned()
        }
    }

    fn parse_container(&mut self, terminator: Option<u8>) {
        loop {
            self.skip_whitespace();
            if self.position >= self.source.len() || self.tree.is_saturated() {
                break;
            }
            if terminator.is_some_and(|value| self.peek() == Some(value)) {
                break;
            }
            if self.peek() == Some(b'#') {
                let comment = self.parse_comment();
                self.push(comment);
                continue;
            }
            if matches!(self.peek(), Some(b'}' | b']')) {
                let start = self.position;
                let delimiter = self.peek().unwrap_or(b'}') as char;
                self.position += 1;
                self.error(
                    SyntaxErrorKind::UnexpectedToken,
                    start,
                    self.position,
                    format!("unexpected `{delimiter}`"),
                );
                let recovery = self.node(CstKind::Error, start, self.position, 0);
                self.push(recovery);
                continue;
            }

            let start = self.position;
            let Some(first) = self.parse_bare() else {
                if self.peek() == Some(b'{') {
                    // Clausewitz data occasionally uses anonymous nested vectors, for example
                    // `position = { { 0 0 0 } { 1 1 1 } }` in GFX configuration.
                    let block = self.parse_block();
                    self.push(block);
                } else if self.peek() == Some(b'"') {
                    let quoted = self.parse_quoted(CstKind::QuotedString);
                    self.skip_whitespace();
                    if self.operator_starts_here() {
                        let property = self.parse_property(start, quoted);
                        self.push(property);
                    } else if self.peek() == Some(b'{') {
                        let block = self.parse_block();
                        let end = self.node_range(block).end() as usize;
                        self.push(quoted);
                        self.push(block);
                        let header = self.node(CstKind::HeaderBlock, start, end, 2);
                        self.push(header);
                    } else {
                        if terminator.is_none() {
                            self.error(
                                SyntaxErrorKind::UnexpectedToken,
                                start,
                                self.node_range(quoted).end() as usize,
                                "a top-level script value must be assigned to a key",
                            );
                        }
                        self.push(quoted);
                    }
                } else if self.starts_parameter_block() {
                    let parameter = self.parse_parameter_block();
                    self.push(parameter);
                } else {
                    let end = self.position.saturating_add(1).min(self.source.len());
                    self.position = end;
                    self.error(
                        SyntaxErrorKind::UnexpectedToken,
                        start,
                        end,
                        format!("unexpected character `{}`", self.snippet(start, end)),
                    );
                    let recovery = self.node(CstKind::Error, start, end, 0);
                    self.push(recovery);
                }
                continue;
            };

            self.skip_whitespace();
            if self.operator_starts_here() {
                let property = self.parse_property(start, first);
                self.push(property);
            } else if self.peek() == Some(b'{') {
                let block = self.parse_block();
                let end = self.node_range(block).end() as usize;
                self.push(first);
                self.push(block);
                let header = self.node(CstKind::HeaderBlock, start, end, 2);
                self.push(header);
            } else {
                // Whitespace after a scalar belongs to the container, not to the scalar node.
                // Keeping the cursor after it is what lets a mixed block continue parsing.
                self.push(first);
            }
        }
    }

    fn parse_property(&mut self, start: usize, key: u32) -> u32 {
        let operator_start = self.position;
        let operator = if let Some((_end, token)) = self.parse_operator() {
            token
        } else {
            let end = self.position.saturating_add(1).min(self.source.len());
            self.position = end;
            self.error(
                SyntaxErrorKind::UnexpectedToken,
                operator_start,
                end,
                format!("invalid operator `{}`", self.snippet(operator_start, end)),
            );
            self.node(CstKind::Error, operator_start, end, 0)
        };

        self.skip_whitespace();
        let value = if self.position >= self.source.len()
            || matches!(self.peek(), Some(b'#' | b'}' | b']'))
        {
            let at = self.position;
            // Underline the key and operator that never received a value;
            // a zero-width caret at the gap hides what is incomplete.
            let key_range = self.node_range(key);
            self.error(
                SyntaxErrorKind::MissingValue,
                key_range.start() as usize,
                at,
                format!(
                    "`{}` is missing a value",
                    self.snippet(key_range.start() as usize, key_range.end() as usize)
                ),
            );
            self.node(CstKind::Error, at, at, 0)
        } else {
            self.parse_value()
        };
        let key_range = self.node_range(key);
        self.push(key);
        let key_node = self.node(
            CstKind::Key,
            key_range.start() as usize,
            key_range.end() as usize,
            1,
        );
        let value_range = self.node_range(value);
        let end = value_range.end() as usize;
        self.push(value);
        let value_node = self.node(CstKind::Value, value_range.start() as usize, end, 1);
        self.push(key_node);
        self.push(operator);
        self.push(value_node);
        self.node(CstKind::Property, start, end, 3)
    }

    fn parse_value(&mut self) -> u32 {
        let start = self.position;
        if self.peek() == Some(b'"') {
            return self.parse_quoted(CstKind::QuotedString);
        }
        if self.peek() == Some(b'{') {
            return self.parse_block();
        }
        if self.starts_parameter_block() {
            return self.parse_parameter_block();
        }
        let Some(value) = self.parse_bare() else {
            let end = self.position.saturating_add(1).min(self.source.len());
            self.position = end;
            self.error(
                SyntaxErrorKind::UnexpectedToken,
                start,
                end,
                format!("expected a value, found `{}`", self.snippet(start, end)),
            );
            return self.node(CstKind::Error, start, end, 0);
        };
        self.skip_whitespace();
        if self.peek() == Some(b'{') {
            let block = self.parse_block();
            let end = self.node_range(block).end() as usize;
            self.push(value);
            self.push(block);
            self.node(CstKind::HeaderBlock, start, end, 2)
        } else {
            value
        }
    }

    fn parse_block(&mut self) -> u32 {
        let start = self.position;
        self.consume_expected(b'{');
        let mark = self.children_mark();
        self.parse_container(Some(b'}'));
        let children = self.children_since(mark);
        if self.peek() == Some(b'}') {
            self.consume_delimiter(TokenKind::CloseDelimiter);
        } else {
            self.error(
                SyntaxErrorKind::UnterminatedBlock,
                start,
                self.source.len(),
                "script block is missing `}`",
            );
        }
        self.node(CstKind::Block, start, self.position.max(start), children)
    }

    fn parse_parameter_block(&mut self) -> u32 {
        let start = self.position;
        self.consume_delimiter(TokenKind::OpenDelimiter);
        self.consume_delimiter(TokenKind::OpenDelimiter);
        let condition_start = self.position;
        if self.peek() == Some(b'!') {
            self.position += 1;
        }
        let condition = self.parse_bare();
        let condition_end = self.position;
        let condition = condition.unwrap_or_else(|| {
            self.error(
                SyntaxErrorKind::UnexpectedToken,
                condition_start,
                condition_end,
                "parameter block is missing a condition",
            );
            self.node(CstKind::Error, condition_start, condition_end, 0)
        });
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.consume_delimiter(TokenKind::CloseDelimiter);
        } else {
            let at = self.position;
            self.error(
                SyntaxErrorKind::UnexpectedToken,
                at,
                at,
                "parameter condition is missing `]`",
            );
        }
        self.push(condition);
        let condition_node = self.node(
            CstKind::ParameterCondition,
            condition_start,
            condition_end,
            1,
        );
        self.push(condition_node);
        let mark = self.children_mark();
        self.parse_container(Some(b']'));
        let children = self.children_since(mark) + 1;
        if self.peek() == Some(b']') {
            self.consume_delimiter(TokenKind::CloseDelimiter);
        } else {
            self.error(
                SyntaxErrorKind::UnterminatedParameterBlock,
                start,
                self.source.len(),
                "parameter block is missing `]`",
            );
        }
        self.node(
            CstKind::ParameterBlock,
            start,
            self.position.max(start),
            children,
        )
    }

    fn parse_comment(&mut self) -> u32 {
        let start = self.position;
        while self.position < self.source.len()
            && !matches!(self.source.as_bytes()[self.position], b'\r' | b'\n')
        {
            self.position += 1;
        }
        let range = super::range(start, self.position);
        self.tokens
            .push(SyntaxToken::new(TokenKind::Comment, range));
        self.node(CstKind::Comment, start, self.position, 0)
    }

    fn parse_quoted(&mut self, kind: CstKind) -> u32 {
        let start = self.position;
        self.position += 1;
        let mut closed = false;
        while self.position < self.source.len() {
            let byte = self.source.as_bytes()[self.position];
            if byte == b'"' {
                self.position += 1;
                closed = true;
                break;
            }
            if byte == b'\\' {
                self.position += 1;
                if self.position < self.source.len()
                    && let Some(character) = self.source[self.position..].chars().next()
                {
                    self.position += character.len_utf8();
                }
            } else if let Some(character) = self.source[self.position..].chars().next() {
                self.position += character.len_utf8();
            } else {
                break;
            }
        }
        let end = self.position;
        self.tokens.push(SyntaxToken::new(
            TokenKind::Quoted,
            super::range(start, end),
        ));
        if !closed {
            self.error(
                SyntaxErrorKind::UnterminatedString,
                start,
                end,
                "quoted string is missing `\"`",
            );
        }
        self.node(kind, start, end, 0)
    }

    fn parse_bare(&mut self) -> Option<u32> {
        let start = self.position;
        while self.position < self.source.len() {
            let byte = self.source.as_bytes()[self.position];
            if byte.is_ascii_whitespace()
                || matches!(
                    byte,
                    b'{' | b'}' | b'[' | b']' | b'#' | b'=' | b'<' | b'>' | b'!' | b'?' | b'"'
                )
            {
                break;
            }
            self.position += 1;
        }
        if self.position == start {
            return None;
        }
        let range = super::range(start, self.position);
        self.tokens.push(SyntaxToken::new(TokenKind::Bare, range));
        Some(self.node(CstKind::BareValue, start, self.position, 0))
    }

    fn parse_operator(&mut self) -> Option<(usize, u32)> {
        let start = self.position;
        let rest = &self.source.as_bytes()[start..];
        let length = if rest.starts_with(b">=")
            || rest.starts_with(b"<=")
            || rest.starts_with(b"!=")
            || rest.starts_with(b"==")
            || rest.starts_with(b"?=")
        {
            2
        } else if rest.starts_with(b"=") || rest.starts_with(b">") || rest.starts_with(b"<") {
            1
        } else {
            return None;
        };
        self.position += length;
        let range = super::range(start, self.position);
        self.tokens
            .push(SyntaxToken::new(TokenKind::Operator, range));
        Some((
            self.position,
            self.node(CstKind::Operator, start, self.position, 0),
        ))
    }

    fn operator_starts_here(&self) -> bool {
        matches!(self.peek(), Some(b'=' | b'<' | b'>' | b'!' | b'?'))
    }

    fn starts_parameter_block(&self) -> bool {
        self.source
            .as_bytes()
            .get(self.position..self.position.saturating_add(2))
            == Some(b"[[")
    }

    fn skip_whitespace(&mut self) {
        while self.position < self.source.len()
            && self.source.as_bytes()[self.position].is_ascii_whitespace()
        {
            self.position += 1;
        }
    }

    fn consume_expected(&mut self, expected: u8) {
        if self.peek() == Some(expected) {
            let kind = if expected == b'{' {
                TokenKind::OpenDelimiter
            } else {
                TokenKind::CloseDelimiter
            };
            self.consume_delimiter(kind);
        }
    }

    fn consume_delimiter(&mut self, kind: TokenKind) {
        let start = self.position;
        self.position = self.position.saturating_add(1).min(self.source.len());
        self.tokens
            .push(SyntaxToken::new(kind, super::range(start, self.position)));
    }

    fn peek(&self) -> Option<u8> {
        self.source.as_bytes().get(self.position).copied()
    }

    fn error(
        &mut self,
        kind: SyntaxErrorKind,
        start: usize,
        end: usize,
        message: impl Into<String>,
    ) {
        self.errors
            .push(SyntaxError::new(kind, super::range(start, end), message));
    }
}
