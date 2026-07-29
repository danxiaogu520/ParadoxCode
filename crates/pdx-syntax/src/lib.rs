//! Loss-aware Paradox text syntax boundary.
//!
//! The Phase 3 facade exposes grammar-shaped, loss-aware CST nodes and stable syntax errors for
//! reusable Paradox script, localisation, and CSV frontends. A game profile classifies files and
//! selects the appropriate frontend.

use pdx_text::TextRange;

mod cst;
mod localisation;
mod script;

pub use cst::{CstKind, CstNode, SyntaxError, SyntaxErrorKind, SyntaxToken, TokenKind};

/// The independent CSV frontend used for supported EU4 record files.
pub mod csv {
    use pdx_text::{TextRange, TextSize};

    /// Delimiters accepted by the Phase 1 CSV facade.
    #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
    pub enum CsvDialect {
        /// Comma-separated values.
        Comma,
        /// Semicolon-separated values, common in EU4 data exports.
        Semicolon,
        /// Tab-separated values.
        Tab,
    }

    impl CsvDialect {
        const fn byte(self) -> u8 {
            match self {
                Self::Comma => b',',
                Self::Semicolon => b';',
                Self::Tab => b'\t',
            }
        }
    }

    /// A recoverable CSV syntax error.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct CsvError {
        /// Source range associated with the error.
        pub range: TextRange,
        /// Human-readable explanation.
        pub message: String,
    }

    /// One cell in a CSV record.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct CsvCell {
        /// Full source range, including surrounding quotes when present.
        pub range: TextRange,
        /// Range of the cell content, excluding surrounding quotes.
        pub value_range: TextRange,
        /// Zero-based row number.
        pub row: u32,
        /// Zero-based column number.
        pub column: u32,
        /// Whether the cell was enclosed in double quotes.
        pub quoted: bool,
    }

    /// One CSV record and its cells.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct CsvRecord {
        /// Full source range, excluding the line ending.
        pub range: TextRange,
        /// Zero-based row number.
        pub row: u32,
        /// Cells in source order.
        pub cells: Vec<CsvCell>,
    }

    /// Loss-aware result of parsing one CSV source.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct CsvParse {
        /// Selected delimiter dialect.
        pub dialect: CsvDialect,
        /// Parsed records in source order.
        pub records: Vec<CsvRecord>,
        /// Recoverable errors in source order.
        pub errors: Vec<CsvError>,
    }

    fn text_size(offset: usize) -> TextSize {
        TextSize::try_from(offset).unwrap_or(TextSize::MAX)
    }

    fn range(start: usize, end: usize) -> TextRange {
        TextRange::new(text_size(start), text_size(end))
            .unwrap_or(TextRange::empty(text_size(start)))
    }

    struct CellInput {
        start: usize,
        end: usize,
        value_start: usize,
        quoted: bool,
        row: u32,
        column: u32,
    }

    fn push_cell(source: &[u8], cells: &mut Vec<CsvCell>, input: CellInput) {
        let value_end = if input.quoted && input.end > input.start && source[input.end - 1] == b'"'
        {
            input.end - 1
        } else {
            input.end
        };
        cells.push(CsvCell {
            range: range(input.start, input.end),
            value_range: range(input.value_start, value_end),
            row: input.row,
            column: input.column,
            quoted: input.quoted,
        });
    }

    /// Parses CSV while preserving source ranges and recovering from malformed records.
    ///
    /// The facade supports quoted cells, doubled quotes, delimiters inside quoted cells,
    /// CRLF/LF line endings, and quoted newlines. It deliberately does not guess a delimiter;
    /// file classification supplies the selected [`CsvDialect`].
    #[must_use]
    pub fn parse(source: &str, dialect: CsvDialect) -> CsvParse {
        let bytes = source.as_bytes();
        let delimiter = dialect.byte();
        let mut records = Vec::new();
        let mut errors = Vec::new();
        let mut cells = Vec::new();
        let mut record_start = 0_usize;
        let mut cell_start = 0_usize;
        let mut value_start = 0_usize;
        let mut row = 0_u32;
        let mut column = 0_u32;
        let mut quoted = false;
        let mut cell_quoted = false;
        let mut closed_quote = false;
        let mut index = 0_usize;

        while index < bytes.len() {
            match bytes[index] {
                b'"' if !quoted && index == cell_start => {
                    quoted = true;
                    cell_quoted = true;
                    closed_quote = false;
                    value_start = index + 1;
                    index += 1;
                }
                b'"' if quoted => {
                    if bytes.get(index + 1) == Some(&b'"') {
                        index += 2;
                    } else {
                        quoted = false;
                        closed_quote = true;
                        index += 1;
                    }
                }
                byte if quoted => {
                    index += 1;
                    if byte == b'\0' {
                        errors.push(CsvError {
                            range: range(index - 1, index),
                            message: "NUL byte inside quoted CSV cell".to_owned(),
                        });
                    }
                }
                byte if byte == delimiter => {
                    push_cell(
                        source.as_bytes(),
                        &mut cells,
                        CellInput {
                            start: cell_start,
                            end: index,
                            value_start,
                            quoted: cell_quoted,
                            row,
                            column,
                        },
                    );
                    column = column.saturating_add(1);
                    cell_start = index + 1;
                    value_start = cell_start;
                    cell_quoted = false;
                    closed_quote = false;
                    index += 1;
                }
                b'\n' | b'\r' => {
                    if closed_quote && index > cell_start + 1 && bytes[index - 1] != b'"' {
                        errors.push(CsvError {
                            range: range(cell_start, index),
                            message: "characters after a closing quote".to_owned(),
                        });
                    }
                    push_cell(
                        source.as_bytes(),
                        &mut cells,
                        CellInput {
                            start: cell_start,
                            end: index,
                            value_start,
                            quoted: cell_quoted,
                            row,
                            column,
                        },
                    );
                    let line_end = if bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n')
                    {
                        index + 1
                    } else {
                        index
                    };
                    records.push(CsvRecord {
                        range: range(record_start, index),
                        row,
                        cells: std::mem::take(&mut cells),
                    });
                    row = row.saturating_add(1);
                    column = 0;
                    record_start = line_end + 1;
                    cell_start = line_end + 1;
                    value_start = cell_start;
                    cell_quoted = false;
                    closed_quote = false;
                    index = line_end + 1;
                }
                _ => {
                    if closed_quote {
                        errors.push(CsvError {
                            range: range(index, index + 1),
                            message: "characters after a closing quote".to_owned(),
                        });
                        closed_quote = false;
                    }
                    index += 1;
                }
            }
        }

        if quoted {
            errors.push(CsvError {
                range: range(cell_start, bytes.len()),
                message: "unterminated quoted CSV cell".to_owned(),
            });
        }
        if cell_start < bytes.len() || !cells.is_empty() {
            push_cell(
                source.as_bytes(),
                &mut cells,
                CellInput {
                    start: cell_start,
                    end: bytes.len(),
                    value_start,
                    quoted: cell_quoted,
                    row,
                    column,
                },
            );
            records.push(CsvRecord {
                range: range(record_start, bytes.len()),
                row,
                cells: std::mem::take(&mut cells),
            });
        }

        CsvParse { dialect, records, errors }
    }
}

/// One of the reusable Paradox text frontends.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FileFormat {
    /// Paradox key/value script.
    Script,
    /// EU4 localisation YAML-like text.
    Localisation,
    /// A supported EU4 CSV file.
    Csv,
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
        Self { range: None, replacement: replacement.into() }
    }

    /// Creates a ranged edit.
    #[must_use]
    pub fn ranged(range: TextRange, replacement: impl Into<String>) -> Self {
        Self { range: Some(range), replacement: replacement.into() }
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
                write!(formatter, "invalid syntax edit range {}..{}", range.start(), range.end())
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
#[derive(Clone, Debug)]
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
        Ok(parse_with_revision(self.format, &source, self.revision.saturating_add(1)))
    }
}

/// A typed CSV parse result with a common CST root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CsvParsedFile {
    source: String,
    dialect: csv::CsvDialect,
    parse: csv::CsvParse,
    root: CstNode,
    errors: Vec<SyntaxError>,
}

impl CsvParsedFile {
    /// Returns the source text.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Returns the selected dialect.
    #[must_use]
    pub const fn dialect(&self) -> csv::CsvDialect {
        self.dialect
    }

    /// Returns the record-oriented CSV parse.
    #[must_use]
    pub const fn parse(&self) -> &csv::CsvParse {
        &self.parse
    }

    /// Returns the typed CSV CST root.
    #[must_use]
    pub const fn root(&self) -> &CstNode {
        &self.root
    }

    /// Returns CSV parser errors mapped to stable syntax diagnostics.
    #[must_use]
    pub fn errors(&self) -> &[SyntaxError] {
        &self.errors
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
        FileFormat::Csv => ParseParts {
            root: CstNode::new(CstKind::CsvDocument, range(0, source.len()), Vec::new()),
            tokens: Vec::new(),
            errors: Vec::new(),
        },
    };
    ParsedFile::from_parts(format, source, parts, revision)
}

/// Parses a supported EU4 CSV using its independent record-oriented facade.
#[must_use]
pub fn parse_csv(source: &str, dialect: csv::CsvDialect) -> csv::CsvParse {
    csv::parse(source, dialect)
}

/// Parses a supported EU4 CSV and exposes its rows/cells as typed CST nodes.
#[must_use]
pub fn parse_csv_file(source: &str, dialect: csv::CsvDialect) -> CsvParsedFile {
    let parse = csv::parse(source, dialect);
    let records = parse
        .records
        .iter()
        .map(|record| {
            let cells = record
                .cells
                .iter()
                .map(|cell| CstNode::new(CstKind::CsvCell, cell.range, Vec::new()))
                .collect();
            CstNode::new(CstKind::CsvRecord, record.range, cells)
        })
        .collect();
    let root = CstNode::new(CstKind::CsvDocument, range(0, source.len()), records);
    let errors = parse
        .errors
        .iter()
        .map(|error| {
            SyntaxError::new(SyntaxErrorKind::CsvRecoverable, error.range, error.message.clone())
        })
        .collect();
    CsvParsedFile { source: source.to_owned(), dialect, parse, root, errors }
}

pub(crate) fn range(start: usize, end: usize) -> TextRange {
    let start = u32::try_from(start).unwrap_or(u32::MAX);
    let end = u32::try_from(end).unwrap_or(u32::MAX).max(start);
    TextRange::new(start, end).unwrap_or_else(|| TextRange::empty(start))
}

#[cfg(test)]
mod tests {
    use pdx_text::TextRange;

    use super::{
        CstKind, FileFormat, SyntaxEdit, SyntaxErrorKind, csv::CsvDialect, parse, parse_csv,
        parse_csv_file,
    };

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
        assert_eq!(parsed.text(parsed.root().children()[0].range()), Some("# note"));
    }

    #[test]
    fn syntax_errors_have_stable_codes_and_safe_ranges() {
        let parsed = parse(FileFormat::Script, "key = { value = \"unfinished");
        assert!(parsed.errors().iter().any(|error| {
            error.kind == SyntaxErrorKind::UnterminatedString
                && error.code() == "pdx-syntax-unterminated-string"
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
                && error.code() == "pdx-syntax-unexpected-token"
        }));
    }

    #[test]
    fn localisation_and_csv_have_independent_typed_facades() {
        let localisation = parse(
            FileFormat::Localisation,
            "l_english:\nhello:0 \"Hello $NAME$\"\nother:0 text # note\n",
        );
        assert!(localisation.errors().is_empty(), "errors: {:?}", localisation.errors());
        assert_eq!(localisation.root().children().len(), 3);
        assert_eq!(localisation.root().children()[1].kind(), CstKind::LocalisationEntry);

        let csv = parse_csv_file("id;name\n1;two\n", CsvDialect::Semicolon);
        assert!(csv.errors().is_empty());
        assert_eq!(csv.root().children()[1].children().len(), 2);
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

    #[test]
    fn csv_facade_preserves_records_cells_and_quotes() {
        let parsed =
            parse_csv("id;name;note\n1;\"A;B\";\"say \"\"hi\"\"\"\n", CsvDialect::Semicolon);
        assert_eq!(parsed.records.len(), 2);
        assert_eq!(parsed.records[1].cells.len(), 3);
        assert!(parsed.records[1].cells[1].quoted);
        assert_eq!(parsed.records[1].cells[1].value_range.start(), 16);
        assert_eq!(parsed.records[1].cells[1].value_range.end(), 19);
        assert!(parsed.errors.is_empty());
    }

    #[test]
    fn csv_facade_recovers_unterminated_quotes() {
        let parsed = parse_csv("id;name\n1;\"unfinished", CsvDialect::Semicolon);
        assert_eq!(parsed.records.len(), 2);
        assert_eq!(parsed.errors.len(), 1);
        assert_eq!(parsed.errors[0].message, "unterminated quoted CSV cell");
    }

    #[test]
    fn csv_facade_supports_tabs_without_guessing() {
        let parsed = parse_csv("a\tb\n", CsvDialect::Tab);
        assert_eq!(parsed.records[0].cells.len(), 2);
        assert_eq!(parsed.records[0].cells[1].column, 1);
    }
}
