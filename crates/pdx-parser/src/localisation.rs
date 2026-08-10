use super::{CstKind, CstNode, ParseParts, SyntaxError, SyntaxErrorKind, SyntaxToken, TokenKind};

pub(crate) fn parse(source: &str) -> ParseParts {
    let mut parser = Parser::new(source);
    let children = parser.parse_lines();
    let root = parser.node(CstKind::LocalisationDocument, 0, source.len(), children);
    ParseParts {
        root,
        tokens: parser.tokens,
        errors: parser.errors,
    }
}

struct Parser<'source> {
    source: &'source str,
    tokens: Vec<SyntaxToken>,
    errors: Vec<SyntaxError>,
}

impl<'source> Parser<'source> {
    fn new(source: &'source str) -> Self {
        Self {
            source,
            tokens: Vec::new(),
            errors: Vec::new(),
        }
    }

    fn parse_lines(&mut self) -> Vec<CstNode> {
        let mut children = Vec::new();
        let mut line_start = 0;
        let bytes = self.source.as_bytes();
        let mut first_line = true;
        while line_start <= bytes.len() {
            let line_end = bytes[line_start..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(bytes.len(), |offset| line_start + offset);
            let content_end = if line_end > line_start && bytes[line_end - 1] == b'\r' {
                line_end - 1
            } else {
                line_end
            };
            let line = &self.source[line_start..content_end];
            if first_line && line.starts_with('\u{feff}') {
                let range = super::range(line_start, line_start + '\u{feff}'.len_utf8());
                self.tokens.push(SyntaxToken::new(TokenKind::Bom, range));
                children.push(self.node(CstKind::Bom, line_start, line_start + 3, Vec::new()));
            }
            let offset = if first_line && line.starts_with('\u{feff}') {
                3
            } else {
                0
            };
            let content = &line[offset..];
            let leading = content.len() - content.trim_start_matches([' ', '\t']).len();
            let start = line_start + offset + leading;
            let trimmed = &content[leading..];
            if !trimmed.is_empty() {
                if trimmed.starts_with('#') {
                    self.tokens.push(SyntaxToken::new(
                        TokenKind::Comment,
                        super::range(start, content_end),
                    ));
                    children.push(self.node(CstKind::Comment, start, content_end, Vec::new()));
                } else if let Some(node) = self.parse_line(start, content_end, trimmed) {
                    children.push(node);
                }
            }
            if line_end == bytes.len() {
                break;
            }
            line_start = line_end + 1;
            first_line = false;
        }
        children
    }

    fn parse_line(&mut self, start: usize, line_end: usize, line: &str) -> Option<CstNode> {
        let key_len = line
            .bytes()
            .take_while(|byte| matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'$' | b'.' | b'-'))
            .count();
        if key_len == 0 {
            self.error(
                SyntaxErrorKind::InvalidLocalisationEntry,
                start,
                line_end,
                "localisation line is missing a key",
            );
            return Some(self.node(CstKind::Error, start, line_end, Vec::new()));
        }
        let key_end = start + key_len;
        self.tokens.push(SyntaxToken::new(
            TokenKind::Bare,
            super::range(start, key_end),
        ));
        let key = self.node(CstKind::LocalisationKey, start, key_end, Vec::new());
        let mut position = key_len;
        while line
            .as_bytes()
            .get(position)
            .is_some_and(u8::is_ascii_whitespace)
        {
            position += 1;
        }
        let mut children = vec![key];
        if line.as_bytes().get(position) == Some(&b':') {
            let colon_start = start + position;
            position += 1;
            if line[..key_len].starts_with("l_")
                && line[position..].trim_matches([' ', '\t']).is_empty()
            {
                self.tokens.push(SyntaxToken::new(
                    TokenKind::Colon,
                    super::range(colon_start, colon_start + 1),
                ));
                return Some(self.node(CstKind::LanguageHeader, start, line_end, children));
            }
            self.tokens.push(SyntaxToken::new(
                TokenKind::Colon,
                super::range(colon_start, colon_start + 1),
            ));
            while line
                .as_bytes()
                .get(position)
                .is_some_and(u8::is_ascii_whitespace)
            {
                position += 1;
            }
            // The version digits after the colon are optional: the game also accepts entries
            // written as `key: "value"` without a version number.
            if line
                .as_bytes()
                .get(position)
                .is_some_and(u8::is_ascii_digit)
            {
                let version_start = position;
                while line
                    .as_bytes()
                    .get(position)
                    .is_some_and(u8::is_ascii_digit)
                {
                    position += 1;
                }
                let version_end = start + position;
                self.tokens.push(SyntaxToken::new(
                    TokenKind::Bare,
                    super::range(start + version_start, version_end),
                ));
                children.push(self.node(
                    CstKind::Version,
                    start + version_start,
                    version_end,
                    Vec::new(),
                ));
                while line
                    .as_bytes()
                    .get(position)
                    .is_some_and(u8::is_ascii_whitespace)
                {
                    position += 1;
                }
            }
        }
        if position >= line.len() || line.as_bytes().get(position) == Some(&b'#') {
            self.error(
                SyntaxErrorKind::InvalidLocalisationEntry,
                line_end,
                line_end,
                "localisation entry is missing a value",
            );
            if line.as_bytes().get(position) == Some(&b'#') {
                let comment_start = start + position;
                self.tokens.push(SyntaxToken::new(
                    TokenKind::Comment,
                    super::range(comment_start, line_end),
                ));
                children.push(self.node(CstKind::Comment, comment_start, line_end, Vec::new()));
            }
            children.push(self.node(CstKind::Error, line_end, line_end, Vec::new()));
            return Some(self.node(CstKind::LocalisationEntry, start, line_end, children));
        }

        let value_start = start + position;
        let (value, comment) = if line.as_bytes()[position] == b'"' {
            self.parse_quoted(value_start, line_end)
        } else {
            self.parse_unquoted(value_start, line_end)
        };
        children.push(value);
        if let Some(comment) = comment {
            children.push(comment);
        }
        Some(self.node(CstKind::LocalisationEntry, start, line_end, children))
    }

    fn parse_quoted(&mut self, start: usize, line_end: usize) -> (CstNode, Option<CstNode>) {
        let mut position = start + 1;
        let mut closed = false;
        while position < line_end {
            let byte = self.source.as_bytes()[position];
            if byte == b'"' {
                position += 1;
                closed = true;
                break;
            }
            if byte == b'\\' {
                position += 1;
                if position < line_end {
                    position += self.source[position..]
                        .chars()
                        .next()
                        .map_or(1, char::len_utf8);
                }
            } else {
                position += self.source[position..]
                    .chars()
                    .next()
                    .map_or(1, char::len_utf8);
            }
        }
        let end = position;
        self.tokens.push(SyntaxToken::new(
            TokenKind::Quoted,
            super::range(start, end),
        ));
        if !closed {
            self.error(
                SyntaxErrorKind::UnterminatedLocalisationString,
                start,
                line_end,
                "localisation string is missing `\"`",
            );
        }
        let comment = closed.then(|| self.inline_comment(end, line_end)).flatten();
        (
            self.node(CstKind::LocalisationString, start, end, Vec::new()),
            comment,
        )
    }

    fn parse_unquoted(&mut self, start: usize, line_end: usize) -> (CstNode, Option<CstNode>) {
        let mut end = line_end;
        let mut comment_node = None;
        if let Some(comment) = self.source[start..line_end].find('#') {
            end = start + comment;
            while end > start && self.source.as_bytes()[end - 1].is_ascii_whitespace() {
                end -= 1;
            }
            let comment_start = start + comment;
            self.tokens.push(SyntaxToken::new(
                TokenKind::Comment,
                super::range(comment_start, line_end),
            ));
            comment_node = Some(self.node(CstKind::Comment, comment_start, line_end, Vec::new()));
        }
        while end > start && self.source.as_bytes()[end - 1].is_ascii_whitespace() {
            end -= 1;
        }
        self.tokens
            .push(SyntaxToken::new(TokenKind::Bare, super::range(start, end)));
        (
            self.node(CstKind::UnquotedValue, start, end, Vec::new()),
            comment_node,
        )
    }

    fn inline_comment(&mut self, start: usize, line_end: usize) -> Option<CstNode> {
        let relative = self.source[start..line_end].find('#')?;
        let comment_start = start + relative;
        self.tokens.push(SyntaxToken::new(
            TokenKind::Comment,
            super::range(comment_start, line_end),
        ));
        Some(self.node(CstKind::Comment, comment_start, line_end, Vec::new()))
    }

    fn node(&self, kind: CstKind, start: usize, end: usize, children: Vec<CstNode>) -> CstNode {
        CstNode::new(kind, super::range(start, end), children)
    }

    fn error(&mut self, kind: SyntaxErrorKind, start: usize, end: usize, message: &'static str) {
        self.errors
            .push(SyntaxError::new(kind, super::range(start, end), message));
    }
}
