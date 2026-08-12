use super::{CstKind, CstNode, ParseParts, SyntaxError, SyntaxErrorKind, SyntaxToken, TokenKind};

pub(crate) fn parse(source: &str) -> ParseParts {
    let mut parser = Parser::new(source);
    let children = parser.parse_container(None);
    let root = parser.node(CstKind::Document, 0, source.len(), children);
    ParseParts {
        root,
        tokens: parser.tokens,
        errors: parser.errors,
    }
}

struct Parser<'source> {
    source: &'source str,
    position: usize,
    tokens: Vec<SyntaxToken>,
    errors: Vec<SyntaxError>,
}

impl<'source> Parser<'source> {
    fn new(source: &'source str) -> Self {
        Self {
            source,
            position: 0,
            tokens: Vec::new(),
            errors: Vec::new(),
        }
    }

    fn node(&self, kind: CstKind, start: usize, end: usize, children: Vec<CstNode>) -> CstNode {
        CstNode::new(kind, super::range(start, end), children)
    }

    fn parse_container(&mut self, terminator: Option<u8>) -> Vec<CstNode> {
        let mut children = Vec::new();
        loop {
            self.skip_whitespace();
            if self.position >= self.source.len() {
                break;
            }
            if terminator.is_some_and(|value| self.peek() == Some(value)) {
                break;
            }
            if self.peek() == Some(b'#') {
                children.push(self.parse_comment());
                continue;
            }
            if matches!(self.peek(), Some(b'}' | b']')) {
                let start = self.position;
                self.position += 1;
                self.error(
                    SyntaxErrorKind::UnexpectedToken,
                    start,
                    self.position,
                    "unexpected closing delimiter",
                );
                children.push(self.node(CstKind::Error, start, self.position, Vec::new()));
                continue;
            }

            let start = self.position;
            let Some(first) = self.parse_bare() else {
                if self.peek() == Some(b'{') {
                    // Clausewitz data occasionally uses anonymous nested vectors, for example
                    // `position = { { 0 0 0 } { 1 1 1 } }` in GFX configuration.
                    children.push(self.parse_block());
                } else if self.peek() == Some(b'"') {
                    let quoted = self.parse_quoted(CstKind::QuotedString);
                    self.skip_whitespace();
                    if self.operator_starts_here() {
                        children.push(self.parse_property(start, quoted));
                    } else if self.peek() == Some(b'{') {
                        let block = self.parse_block();
                        let end = block.range().end() as usize;
                        children.push(self.node(
                            CstKind::HeaderBlock,
                            start,
                            end,
                            vec![quoted, block],
                        ));
                    } else {
                        if terminator.is_none() {
                            self.error(
                                SyntaxErrorKind::UnexpectedToken,
                                start,
                                quoted.range().end() as usize,
                                "a top-level script value must be assigned to a key",
                            );
                        }
                        children.push(quoted);
                    }
                } else if self.starts_parameter_block() {
                    children.push(self.parse_parameter_block());
                } else {
                    let end = self.position.saturating_add(1).min(self.source.len());
                    self.position = end;
                    self.error(
                        SyntaxErrorKind::UnexpectedToken,
                        start,
                        end,
                        "unexpected token in script document",
                    );
                    children.push(self.node(CstKind::Error, start, end, Vec::new()));
                }
                continue;
            };

            let after_first = self.position;
            self.skip_whitespace();
            if self.operator_starts_here() {
                children.push(self.parse_property(start, first));
            } else if self.peek() == Some(b'{') {
                let block = self.parse_block();
                let end = block.range().end() as usize;
                children.push(self.node(CstKind::HeaderBlock, start, end, vec![first, block]));
            } else {
                // Whitespace after a scalar belongs to the container, not to the scalar node.
                // Keeping the cursor after it is what lets a mixed block continue parsing.
                let _ = after_first;
                children.push(first);
            }
        }
        children
    }

    fn parse_property(&mut self, start: usize, key: CstNode) -> CstNode {
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
                "invalid script operator",
            );
            self.node(CstKind::Error, operator_start, end, Vec::new())
        };

        self.skip_whitespace();
        let value = if self.position >= self.source.len()
            || matches!(self.peek(), Some(b'#' | b'}' | b']'))
        {
            let at = self.position;
            self.error(
                SyntaxErrorKind::MissingValue,
                at,
                at,
                "property operator is missing a value",
            );
            self.node(CstKind::Error, at, at, Vec::new())
        } else {
            self.parse_value()
        };
        let end = value.range().end() as usize;
        self.node(
            CstKind::Property,
            start,
            end,
            vec![
                self.node(
                    CstKind::Key,
                    key.range().start() as usize,
                    key.range().end() as usize,
                    vec![key],
                ),
                operator,
                self.node(
                    CstKind::Value,
                    value.range().start() as usize,
                    end,
                    vec![value],
                ),
            ],
        )
    }

    fn parse_value(&mut self) -> CstNode {
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
                "expected a script value",
            );
            return self.node(CstKind::Error, start, end, Vec::new());
        };
        self.skip_whitespace();
        if self.peek() == Some(b'{') {
            let block = self.parse_block();
            let end = block.range().end() as usize;
            self.node(CstKind::HeaderBlock, start, end, vec![value, block])
        } else {
            value
        }
    }

    fn parse_block(&mut self) -> CstNode {
        let start = self.position;
        self.consume_expected(b'{');
        let children = self.parse_container(Some(b'}'));
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

    fn parse_parameter_block(&mut self) -> CstNode {
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
            self.node(CstKind::Error, condition_start, condition_end, Vec::new())
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
        let condition_node = self.node(
            CstKind::ParameterCondition,
            condition_start,
            condition_end,
            vec![condition],
        );
        let mut children = vec![condition_node];
        children.extend(self.parse_container(Some(b']')));
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

    fn parse_comment(&mut self) -> CstNode {
        let start = self.position;
        while self.position < self.source.len()
            && !matches!(self.source.as_bytes()[self.position], b'\r' | b'\n')
        {
            self.position += 1;
        }
        let range = super::range(start, self.position);
        self.tokens
            .push(SyntaxToken::new(TokenKind::Comment, range));
        self.node(CstKind::Comment, start, self.position, Vec::new())
    }

    fn parse_quoted(&mut self, kind: CstKind) -> CstNode {
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
        self.node(kind, start, end, Vec::new())
    }

    fn parse_bare(&mut self) -> Option<CstNode> {
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
        Some(self.node(CstKind::BareValue, start, self.position, Vec::new()))
    }

    fn parse_operator(&mut self) -> Option<(usize, CstNode)> {
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
            self.node(CstKind::Operator, start, self.position, Vec::new()),
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

    fn error(&mut self, kind: SyntaxErrorKind, start: usize, end: usize, message: &'static str) {
        self.errors
            .push(SyntaxError::new(kind, super::range(start, end), message));
    }
}
