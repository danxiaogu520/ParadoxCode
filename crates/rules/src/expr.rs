//! The type-expression mini-syntax (see `docs/rules-language.md`).
//!
//! Type expressions are spelled as short strings in rule sources —
//! `'yes'`, `int[1..10]`, `ref<event.country>`, `'monthly_{ref<governor>}'`,
//! `a | b` — and appear wherever a key or value type is expected. This module
//! owns the grammar and the parser; it is deliberately independent of the
//! source model so parse errors can be reported with the caller's provenance
//! (source file + JSON pointer + the expression-internal column below).
//!
//! Lexical rules:
//!
//! - Identifiers are `[A-Za-z_][A-Za-z0-9_]*`; `impl` is reserved as the
//!   trait-argument introducer.
//! - Numbers (range bounds) are `-?[0-9]+(\.[0-9]+)?`, without exponents.
//! - Whitespace outside literals is insignificant between tokens and at the
//!   ends of the expression.
//! - Inside `'...'` literals the backslash escapes `\'`, `\{`, `\}`, `\\`;
//!   `{` opens a hole holding a nested expression; every other character,
//!   including `}`, is literal text. A literal without holes is a constant,
//!   a literal with holes is a template.

use std::fmt;

/// A parsed type expression: the `|`-separated alternatives of `expr` in the
/// grammar, tried in written order at match time.
#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    pub alternatives: Vec<Primary>,
}

/// One alternative of an [`Expr`]: one `prim [range]`, `ctor<arg>`, `path`,
/// literal, or bare parameter.
#[derive(Clone, Debug, PartialEq)]
pub enum Primary {
    /// `scalar`, `int[1..]`, `float`, `bool`, `date`, `loc`, `link`, `opaque`.
    Scalar {
        kind: ScalarKind,
        range: Option<Range>,
    },
    /// `ref<event>`, `ref<event.country>`, `ref<event.$S>`.
    Ref(Argument),
    /// `def<country_flag>`: defines a symbol at this position.
    Def(Argument),
    /// `enum<country_tags>`: one member of a named enum.
    Enum(Argument),
    /// `scope<country>`: a scope expression.
    Scope(Argument),
    /// `quoted<trigger>`: a quoted script parsed with the named schema.
    Quoted(Argument),
    /// `path` or `path<gfx>`: a file-path scalar, optionally a path category.
    Path { category: Option<String> },
    /// `'yes'` (constant) or `'monthly_{ref<power>}'` (template).
    Literal(Vec<LiteralPart>),
    /// `$S`: a formal parameter of the enclosing parameterized schema.
    Param(Param),
}

/// The scalar spellings accepted by `prim` in the grammar.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScalarKind {
    /// `scalar`: any scalar.
    Scalar,
    /// `int`: an integer, optionally range-bounded.
    Int,
    /// `float`: a floating-point number, optionally range-bounded.
    Float,
    /// `bool`: `yes` / `no`.
    Bool,
    /// `date`: a campaign date.
    Date,
    /// `loc`: a localisation key.
    Loc,
    /// `link`: any scope link, register, or prefixed link (keys only).
    Link,
    /// `opaque`: unchecked text.
    Opaque,
}

impl ScalarKind {
    fn from_keyword(keyword: &str) -> Option<Self> {
        Some(match keyword {
            "scalar" => Self::Scalar,
            "int" => Self::Int,
            "float" => Self::Float,
            "bool" => Self::Bool,
            "date" => Self::Date,
            "loc" => Self::Loc,
            "link" => Self::Link,
            "opaque" => Self::Opaque,
            _ => return None,
        })
    }
}

/// An inclusive numeric range `[min..max]`; either bound may be omitted.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Range {
    pub min: Option<Number>,
    pub max: Option<Number>,
}

/// A range bound. Stored as `f64`; integer ranges additionally require
/// whole-number bounds (checked by the parser).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Number(pub f64);

impl Number {
    #[must_use]
    pub fn is_integral(self) -> bool {
        self.0.fract() == 0.0
    }
}

/// The `<...>` argument of `ref` / `def` / `enum` / `scope` / `quoted`.
#[derive(Clone, Debug, PartialEq)]
pub enum Argument {
    /// `event`, `event.country`, `event.$S`: a dotted path of names and parameters.
    Path(Vec<Segment>),
    /// `estate strip_prefix estate_`: a path whose member name loses an affix
    /// before it is substituted (the legacy template `strip_prefix`).
    Stripped {
        /// The dotted path.
        segments: Vec<Segment>,
        /// Affix removed from the resolved member name.
        strip_prefix: String,
    },
}

impl Argument {
    /// The dotted path, for both path-carrying forms.
    #[must_use]
    pub fn segments(&self) -> Option<&[Segment]> {
        match self {
            Self::Path(segments) | Self::Stripped { segments, .. } => Some(segments),
        }
    }
}

/// One dot-separated segment of an [`Argument::Path`].
#[derive(Clone, Debug, PartialEq)]
pub enum Segment {
    Name(String),
    Param(Param),
}

/// `$name`: a formal parameter of the enclosing parameterized schema.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Param {
    pub name: String,
}

/// One piece of a literal: literal text or a `{expr}` hole.
#[derive(Clone, Debug, PartialEq)]
pub enum LiteralPart {
    Text(String),
    Hole(Expr),
}

/// A parse failure with the byte offset and the 1-based character column of
/// the offending position inside the expression string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseError {
    /// Byte offset into the expression string.
    pub offset: usize,
    /// 1-based character column of `offset`.
    pub column: usize,
    /// Human-readable description of the problem.
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} (at column {})", self.message, self.column)
    }
}

impl std::error::Error for ParseError {}

/// Parses one type expression.
///
/// # Errors
///
/// Returns a [`ParseError`] carrying the expression-internal column of the
/// first offending character.
pub fn parse(source: &str) -> Result<Expr, ParseError> {
    let mut cursor = Cursor::new(source);
    let expression = cursor.parse_expr()?;
    cursor.skip_whitespace();
    if let Some(rest) = cursor.peek() {
        return Err(cursor.error(
            cursor.position,
            format!("unexpected trailing input starting at `{rest}`"),
        ));
    }
    Ok(expression)
}

/// Parses a bare template — literal text with `{expr}` holes and no enclosing
/// quotes — as used for scope-link keys such as `event_target:{ref<event_target>}`.
///
/// # Errors
///
/// Returns a [`ParseError`] like [`parse`].
pub fn parse_template(source: &str) -> Result<Vec<LiteralPart>, ParseError> {
    let mut cursor = Cursor::new(source);
    let parts = cursor.parse_literal_parts(false)?;
    cursor.skip_whitespace();
    if !cursor.is_at_end() {
        return Err(cursor.error(cursor.position, "unexpected trailing input"));
    }
    Ok(parts)
}

struct Cursor<'source> {
    source: &'source str,
    position: usize,
}

impl<'source> Cursor<'source> {
    fn new(source: &'source str) -> Self {
        Self {
            source,
            position: 0,
        }
    }

    fn is_at_end(&self) -> bool {
        self.position >= self.source.len()
    }

    fn peek(&self) -> Option<char> {
        self.source[self.position..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let next = self.peek()?;
        self.position += next.len_utf8();
        Some(next)
    }

    fn skip_whitespace(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.position += self.peek().map_or(0, char::len_utf8);
        }
    }

    fn error(&self, offset: usize, message: impl Into<String>) -> ParseError {
        ParseError {
            offset,
            column: self.source[..offset].chars().count() + 1,
            message: message.into(),
        }
    }

    fn expect(&mut self, expected: char) -> Result<(), ParseError> {
        self.skip_whitespace();
        match self.peek() {
            Some(found) if found == expected => {
                self.bump();
                Ok(())
            }
            Some(found) => Err(self.error(
                self.position,
                format!("expected `{expected}`, found `{found}`"),
            )),
            None => Err(self.error(
                self.position,
                format!("expected `{expected}`, found end of input"),
            )),
        }
    }

    fn read_ident(&mut self) -> Result<String, ParseError> {
        self.skip_whitespace();
        let start = self.position;
        match self.peek() {
            Some(first) if first.is_ascii_alphabetic() || first == '_' => {
                self.bump();
            }
            Some(found) => {
                return Err(self.error(start, format!("expected an identifier, found `{found}`")));
            }
            None => {
                return Err(self.error(start, "expected an identifier, found end of input"));
            }
        }
        while self
            .peek()
            .is_some_and(|next| next.is_ascii_alphanumeric() || next == '_')
        {
            self.bump();
        }
        Ok(self.source[start..self.position].to_owned())
    }

    fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        let mut alternatives = vec![self.parse_primary()?];
        loop {
            self.skip_whitespace();
            if self.peek() != Some('|') {
                break;
            }
            self.bump();
            alternatives.push(self.parse_primary()?);
        }
        Ok(Expr { alternatives })
    }

    fn parse_primary(&mut self) -> Result<Primary, ParseError> {
        self.skip_whitespace();
        let start = self.position;
        match self.peek() {
            None => Err(self.error(start, "expected a type expression, found end of input")),
            Some('$') => Ok(Primary::Param(self.parse_param()?)),
            Some('\'') => Ok(Primary::Literal(self.parse_literal_parts(true)?)),
            Some(first) if first.is_ascii_alphabetic() || first == '_' => {
                let keyword = self.read_ident()?;
                if let Some(kind) = ScalarKind::from_keyword(&keyword) {
                    let range = self.parse_range(kind)?;
                    return Ok(Primary::Scalar { kind, range });
                }
                match keyword.as_str() {
                    "path" => {
                        self.skip_whitespace();
                        let category = if self.peek() == Some('<') {
                            self.bump();
                            let name = self.read_ident()?;
                            self.expect('>')?;
                            Some(name)
                        } else {
                            None
                        };
                        Ok(Primary::Path { category })
                    }
                    "ref" | "def" | "enum" | "scope" | "quoted" => {
                        self.expect('<')?;
                        let argument = self.parse_argument()?;
                        self.expect('>')?;
                        Ok(match keyword.as_str() {
                            "ref" => Primary::Ref(argument),
                            "def" => Primary::Def(argument),
                            "enum" => Primary::Enum(argument),
                            "scope" => Primary::Scope(argument),
                            _ => Primary::Quoted(argument),
                        })
                    }
                    _ => Err(self.error(start, format!("unknown type expression `{keyword}`"))),
                }
            }
            Some(found) => Err(self.error(
                start,
                format!("expected a type expression, found `{found}`"),
            )),
        }
    }

    fn parse_range(&mut self, kind: ScalarKind) -> Result<Option<Range>, ParseError> {
        self.skip_whitespace();
        if self.peek() != Some('[') {
            return Ok(None);
        }
        if !matches!(kind, ScalarKind::Int | ScalarKind::Float) {
            return Err(self.error(
                self.position,
                "range bounds are only allowed on `int` and `float`",
            ));
        }
        let range_offset = self.position;
        self.bump();
        let min = self.parse_number()?;
        self.expect_dots()?;
        let max = self.parse_number()?;
        self.expect(']')?;
        if kind == ScalarKind::Int {
            for bound in [min, max] {
                if let Some((number, offset)) = bound
                    && !number.is_integral()
                {
                    return Err(self.error(offset, "integer range bounds must be whole numbers"));
                }
            }
        }
        if let (Some((lower, _)), Some((upper, _))) = (min, max)
            && lower.0 > upper.0
        {
            return Err(self.error(
                range_offset,
                format!(
                    "range lower bound {} exceeds upper bound {}",
                    lower.0, upper.0
                ),
            ));
        }
        Ok(Some(Range {
            min: min.map(|(number, _)| number),
            max: max.map(|(number, _)| number),
        }))
    }

    fn expect_dots(&mut self) -> Result<(), ParseError> {
        self.skip_whitespace();
        if self.peek() == Some('.') && self.source[self.position..].starts_with("..") {
            self.bump();
            self.bump();
            return Ok(());
        }
        Err(self.error(self.position, "expected `..`"))
    }

    /// Parses one optional range bound. A `.` only starts a fraction when a
    /// digit follows, so `int[1..10]` does not read `1.` as a broken decimal.
    fn parse_number(&mut self) -> Result<Option<(Number, usize)>, ParseError> {
        self.skip_whitespace();
        let start = self.position;
        if self.peek() == Some('-') {
            self.bump();
            if !self.peek().is_some_and(|next| next.is_ascii_digit()) {
                return Err(self.error(self.position, "expected digits after `-`"));
            }
        } else if !self.peek().is_some_and(|next| next.is_ascii_digit()) {
            return Ok(None);
        }
        while self.peek().is_some_and(|next| next.is_ascii_digit()) {
            self.bump();
        }
        if self.peek() == Some('.')
            && self.source[self.position + 1..]
                .chars()
                .next()
                .is_some_and(|next| next.is_ascii_digit())
        {
            self.bump();
            while self.peek().is_some_and(|next| next.is_ascii_digit()) {
                self.bump();
            }
        }
        let parsed = self.source[start..self.position]
            .parse::<f64>()
            .map_err(|_| self.error(start, "invalid number"))?;
        Ok(Some((Number(parsed), start)))
    }

    fn parse_argument(&mut self) -> Result<Argument, ParseError> {
        self.skip_whitespace();
        if self.peek() == Some('$') {
            // `arg = ... | param`: a bare parameter instantiates the argument.
            return Ok(Argument::Path(vec![Segment::Param(self.parse_param()?)]));
        }
        let first = self.read_ident()?;
        if first == "impl" {
            return Err(self.error(self.position, "trait references are not supported"));
        }
        let mut segments = vec![Segment::Name(first)];
        loop {
            self.skip_whitespace();
            if self.peek() != Some('.') {
                break;
            }
            self.bump();
            segments.push(self.parse_segment()?);
        }
        // `arg = name ["." name] | "impl" name | name "strip_prefix" name`
        self.skip_whitespace();
        if self
            .peek()
            .is_some_and(|found| found.is_ascii_alphabetic() || found == '_')
        {
            let marker = self.read_ident()?;
            if marker == "strip_prefix" {
                let strip_prefix = self.read_ident()?;
                return Ok(Argument::Stripped {
                    segments,
                    strip_prefix,
                });
            }
            return Err(self.error(
                self.position,
                format!("expected `strip_prefix`, found `{marker}`"),
            ));
        }
        Ok(Argument::Path(segments))
    }

    fn parse_segment(&mut self) -> Result<Segment, ParseError> {
        self.skip_whitespace();
        if self.peek() == Some('$') {
            Ok(Segment::Param(self.parse_param()?))
        } else {
            Ok(Segment::Name(self.read_ident()?))
        }
    }

    fn parse_param(&mut self) -> Result<Param, ParseError> {
        self.skip_whitespace();
        if self.peek() != Some('$') {
            return Err(self.error(self.position, "expected `$`"));
        }
        self.bump();
        let name = self.read_ident()?;
        self.skip_whitespace();
        if self.peek() == Some('.') {
            return Err(self.error(self.position, "parameter attributes are not supported"));
        }
        Ok(Param { name })
    }

    /// Parses the body of a `'...'` literal (`quoted`), or a bare template
    /// (link keys) when `quoted` is false.
    fn parse_literal_parts(&mut self, quoted: bool) -> Result<Vec<LiteralPart>, ParseError> {
        let opening = self.position;
        if quoted {
            self.expect('\'')?;
        }
        let mut parts: Vec<LiteralPart> = Vec::new();
        let mut text = String::new();
        loop {
            let Some(next) = self.peek() else {
                if quoted {
                    return Err(self.error(opening, "unterminated literal: missing closing `'`"));
                }
                break;
            };
            match next {
                '\'' if quoted => {
                    self.bump();
                    break;
                }
                '\\' => {
                    self.bump();
                    let Some(escaped) = self.bump() else {
                        return Err(self.error(self.position, "unterminated escape"));
                    };
                    match escaped {
                        '\'' | '{' | '}' | '\\' => text.push(escaped),
                        other => {
                            return Err(self.error(
                                self.position - other.len_utf8() - 1,
                                format!("invalid escape `\\{other}`"),
                            ));
                        }
                    }
                }
                '{' => {
                    self.bump();
                    if !text.is_empty() {
                        parts.push(LiteralPart::Text(std::mem::take(&mut text)));
                    }
                    let hole = self.parse_expr()?;
                    self.expect('}')?;
                    parts.push(LiteralPart::Hole(hole));
                }
                other => {
                    self.bump();
                    text.push(other);
                }
            }
        }
        if !text.is_empty() {
            parts.push(LiteralPart::Text(text));
        }
        Ok(parts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alts(source: &str) -> Expr {
        parse(source).expect("parses")
    }

    fn one(source: &str) -> Primary {
        let expression = alts(source);
        assert_eq!(
            expression.alternatives.len(),
            1,
            "{source} is one alternative"
        );
        expression.alternatives.into_iter().next().expect("one")
    }

    fn error(source: &str) -> ParseError {
        parse(source).expect_err("fails")
    }

    #[test]
    fn scalar_primaries_parse() {
        assert_eq!(
            one("scalar"),
            Primary::Scalar {
                kind: ScalarKind::Scalar,
                range: None
            }
        );
        assert_eq!(
            one("bool"),
            Primary::Scalar {
                kind: ScalarKind::Bool,
                range: None
            }
        );
        assert_eq!(
            one("date"),
            Primary::Scalar {
                kind: ScalarKind::Date,
                range: None
            }
        );
        assert_eq!(
            one("loc"),
            Primary::Scalar {
                kind: ScalarKind::Loc,
                range: None
            }
        );
        assert_eq!(
            one("link"),
            Primary::Scalar {
                kind: ScalarKind::Link,
                range: None
            }
        );
        assert_eq!(
            one("opaque"),
            Primary::Scalar {
                kind: ScalarKind::Opaque,
                range: None
            }
        );
    }

    /// The legacy template parameter's `strip_prefix` is spelled on the hole.
    #[test]
    fn template_holes_may_strip_a_member_affix() {
        assert_eq!(
            one("ref<estate strip_prefix estate_>"),
            Primary::Ref(Argument::Stripped {
                segments: vec![Segment::Name("estate".to_owned())],
                strip_prefix: "estate_".to_owned(),
            })
        );
        assert_eq!(
            one("'{ref<estate strip_prefix estate_>}_loyalty_modifier'"),
            Primary::Literal(vec![
                LiteralPart::Hole(Expr {
                    alternatives: vec![Primary::Ref(Argument::Stripped {
                        segments: vec![Segment::Name("estate".to_owned())],
                        strip_prefix: "estate_".to_owned(),
                    })],
                }),
                LiteralPart::Text("_loyalty_modifier".to_owned()),
            ])
        );
    }

    #[test]
    fn numeric_ranges_parse_with_open_bounds() {
        assert_eq!(
            one("int[1..10]"),
            Primary::Scalar {
                kind: ScalarKind::Int,
                range: Some(Range {
                    min: Some(Number(1.0)),
                    max: Some(Number(10.0))
                }),
            }
        );
        assert_eq!(
            one("float[0..]"),
            Primary::Scalar {
                kind: ScalarKind::Float,
                range: Some(Range {
                    min: Some(Number(0.0)),
                    max: None
                }),
            }
        );
        assert_eq!(
            one("int[..5]"),
            Primary::Scalar {
                kind: ScalarKind::Int,
                range: Some(Range {
                    min: None,
                    max: Some(Number(5.0))
                }),
            }
        );
        assert_eq!(
            one("int[-6..-1]"),
            Primary::Scalar {
                kind: ScalarKind::Int,
                range: Some(Range {
                    min: Some(Number(-6.0)),
                    max: Some(Number(-1.0))
                }),
            }
        );
    }

    #[test]
    fn ctor_arguments_parse() {
        assert_eq!(
            one("ref<event>"),
            Primary::Ref(Argument::Path(vec![Segment::Name("event".to_owned())]))
        );
        assert_eq!(
            one("ref<event.country>"),
            Primary::Ref(Argument::Path(vec![
                Segment::Name("event".to_owned()),
                Segment::Name("country".to_owned()),
            ]))
        );
        assert_eq!(
            one("ref<event.$S>"),
            Primary::Ref(Argument::Path(vec![
                Segment::Name("event".to_owned()),
                Segment::Param(Param {
                    name: "S".to_owned(),
                }),
            ]))
        );
        assert!(parse("ref<impl ModifierSource>").is_err());
        assert_eq!(
            one("def<country_flag>"),
            Primary::Def(Argument::Path(vec![Segment::Name(
                "country_flag".to_owned()
            )]))
        );
        assert_eq!(
            one("enum<country_tags>"),
            Primary::Enum(Argument::Path(vec![Segment::Name(
                "country_tags".to_owned()
            )]))
        );
        assert_eq!(
            one("scope<any>"),
            Primary::Scope(Argument::Path(vec![Segment::Name("any".to_owned())]))
        );
        assert_eq!(
            one("quoted<trigger>"),
            Primary::Quoted(Argument::Path(vec![Segment::Name("trigger".to_owned())]))
        );
    }

    #[test]
    fn path_categories_parse() {
        assert_eq!(one("path"), Primary::Path { category: None });
        assert_eq!(
            one("path<gfx>"),
            Primary::Path {
                category: Some("gfx".to_owned())
            }
        );
    }

    #[test]
    fn parameter_arguments_parse() {
        assert_eq!(
            one("ref<$S>"),
            Primary::Ref(Argument::Path(vec![Segment::Param(Param {
                name: "S".to_owned(),
            })]))
        );
        assert_eq!(
            one("quoted<$body>"),
            Primary::Quoted(Argument::Path(vec![Segment::Param(Param {
                name: "body".to_owned(),
            })]))
        );
    }

    #[test]
    fn literals_split_into_text_and_holes() {
        assert_eq!(
            one("'yes'"),
            Primary::Literal(vec![LiteralPart::Text("yes".to_owned())])
        );
        assert_eq!(one("''"), Primary::Literal(Vec::new()));
        assert_eq!(
            one("'monthly_{ref<government_mechanic_power>}'"),
            Primary::Literal(vec![
                LiteralPart::Text("monthly_".to_owned()),
                LiteralPart::Hole(Expr {
                    alternatives: vec![Primary::Ref(Argument::Path(vec![Segment::Name(
                        "government_mechanic_power".to_owned(),
                    )]))],
                }),
            ])
        );
    }

    #[test]
    fn literal_escapes_and_nested_literals() {
        assert_eq!(
            one(r"'it\'s \{fine\} \\ here'"),
            Primary::Literal(vec![LiteralPart::Text("it's {fine} \\ here".to_owned())])
        );
        assert_eq!(
            one("'outer_{'inner_{ref<x>}'}'"),
            Primary::Literal(vec![
                LiteralPart::Text("outer_".to_owned()),
                LiteralPart::Hole(Expr {
                    alternatives: vec![Primary::Literal(vec![
                        LiteralPart::Text("inner_".to_owned()),
                        LiteralPart::Hole(Expr {
                            alternatives: vec![Primary::Ref(Argument::Path(vec![Segment::Name(
                                "x".to_owned(),
                            )]))],
                        }),
                    ])],
                }),
            ])
        );
    }

    #[test]
    fn unions_keep_written_order() {
        let expression = alts("scalar | '0' | int[1..]");
        assert_eq!(expression.alternatives.len(), 3);
        assert_eq!(
            expression.alternatives[0],
            Primary::Scalar {
                kind: ScalarKind::Scalar,
                range: None
            }
        );
        assert_eq!(
            expression.alternatives[1],
            Primary::Literal(vec![LiteralPart::Text("0".to_owned())])
        );
    }

    #[test]
    fn whitespace_between_tokens_is_insignificant() {
        assert_eq!(one("int[ 1 .. 10 ]"), one("int[1..10]"));
        assert_eq!(one("ref< event . country >"), one("ref<event.country>"));
        assert_eq!(alts("scalar | bool"), alts("scalar|bool"));
    }

    #[test]
    fn errors_report_expression_columns() {
        let failure = error("it");
        assert_eq!(failure.column, 1);
        assert!(
            failure.message.contains("unknown type expression `it`"),
            "{failure}"
        );

        let failure = error("scalar | | bool");
        assert_eq!(failure.column, 10);
        assert!(
            failure.message.contains("expected a type expression"),
            "{failure}"
        );

        let failure = error("ref<event");
        assert_eq!(failure.column, 10);
        assert!(failure.message.contains("expected `>`"), "{failure}");

        let failure = error("int[1..10] oops");
        assert_eq!(failure.column, 12);
        assert!(
            failure.message.contains("unexpected trailing input"),
            "{failure}"
        );
    }

    #[test]
    fn range_misuse_reports_columns() {
        let failure = error("bool[0..1]");
        assert_eq!(failure.column, 5);
        assert!(
            failure
                .message
                .contains("only allowed on `int` and `float`"),
            "{failure}"
        );

        let failure = error("int[1.5..2]");
        assert_eq!(failure.column, 5);
        assert!(failure.message.contains("whole numbers"), "{failure}");

        let failure = error("int[5..1]");
        assert_eq!(failure.column, 4);
        assert!(failure.message.contains("exceeds upper bound"), "{failure}");

        let failure = error("int[1...5]");
        assert_eq!(failure.column, 8);
        assert!(failure.message.contains("expected `]`"), "{failure}");
    }

    #[test]
    fn literal_errors_report_the_opening_quote() {
        let failure = error("'unterminated");
        assert_eq!(failure.column, 1);
        assert!(
            failure.message.contains("unterminated literal"),
            "{failure}"
        );

        let failure = error("'bad \\q'");
        assert_eq!(failure.column, 6);
        assert!(failure.message.contains("invalid escape"), "{failure}");

        let failure = error("'empty_{}'");
        assert_eq!(failure.column, 9);
        assert!(
            failure.message.contains("expected a type expression"),
            "{failure}"
        );

        let failure = error("'unclosed_{ref<x>'");
        assert_eq!(failure.column, 18);
        assert!(failure.message.contains("expected `}`"), "{failure}");
    }

    #[test]
    fn formal_parameters_parse_and_attributes_are_rejected() {
        assert_eq!(
            one("$S"),
            Primary::Param(Param {
                name: "S".to_owned()
            })
        );
        assert_eq!(
            one("ref<event.$S>"),
            Primary::Ref(Argument::Path(vec![
                Segment::Name("event".to_owned()),
                Segment::Param(Param {
                    name: "S".to_owned()
                })
            ]))
        );
        for removed in ["$key.scope", "$S.scope", "ref<impl ModifierSource>"] {
            assert!(parse(removed).is_err(), "{removed}");
        }
    }

    #[test]
    fn parameter_errors_report_columns() {
        let failure = error("$");
        assert_eq!(failure.column, 2);
        assert!(
            failure.message.contains("expected an identifier"),
            "{failure}"
        );

        let failure = error("$key.");
        assert_eq!(failure.column, 5);
        assert!(
            failure
                .message
                .contains("parameter attributes are not supported"),
            "{failure}"
        );
    }

    #[test]
    fn link_key_templates_parse_without_quotes() {
        assert_eq!(
            parse_template("event_target:{ref<event_target>}").expect("parses"),
            vec![
                LiteralPart::Text("event_target:".to_owned()),
                LiteralPart::Hole(Expr {
                    alternatives: vec![Primary::Ref(Argument::Path(vec![Segment::Name(
                        "event_target".to_owned(),
                    )]))],
                }),
            ]
        );
        assert_eq!(
            parse_template("owner").expect("parses"),
            vec![LiteralPart::Text("owner".to_owned())]
        );
    }
}
