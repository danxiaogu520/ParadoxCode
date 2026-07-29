//! Conservative, trivia-preserving formatting for the EU4 text frontends.
//!
//! The formatter only rewrites whitespace. It does not reorder properties, change operators,
//! add quotes, normalize scalar spelling, or format CSV. Any syntax error causes a safe no-edit
//! result because a recovered tree may not provide enough boundaries to prove a rewrite safe.

use pdx_syntax::{FileFormat, ParsedFile};
use pdx_text::TextRange;

/// Indentation style used for generated leading whitespace.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum IndentStyle {
    /// Insert spaces.
    Spaces,
    /// Insert tabs.
    Tabs,
}

/// Conservative formatting options.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FormatOptions {
    /// Indentation representation.
    pub indent_style: IndentStyle,
    /// Number of spaces per indentation level when [`IndentStyle::Spaces`] is selected.
    pub indent_width: u8,
    /// Maximum number of consecutive blank lines.
    pub max_blank_lines: u8,
    /// Whether supported PdxScript operators get one space on both sides.
    pub space_around_operator: bool,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            indent_style: IndentStyle::Spaces,
            indent_width: 4,
            max_blank_lines: 2,
            space_around_operator: true,
        }
    }
}

/// Why formatting did not produce edits.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FormatSkipReason {
    /// The parser reported an error, so a trivia rewrite cannot be proven safe.
    UnsafeSyntax,
    /// CSV and binary/resource files do not have a generic full-document formatter.
    UnsupportedFormat,
}

/// A single source edit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextEdit {
    /// Source range to replace.
    pub range: TextRange,
    /// Replacement text.
    pub replacement: String,
}

/// Formatter result that can safely contain no edits.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FormatResult {
    /// Safe edits to apply.
    pub edits: Vec<TextEdit>,
    /// Explicit reason when no edits are available.
    pub skipped: Option<FormatSkipReason>,
}

/// Formats a parsed EU4 document using the default conservative style.
#[must_use]
pub fn format(file: &ParsedFile) -> FormatResult {
    format_with_options(file, FormatOptions::default())
}

/// Formats a parsed EU4 document without changing its non-trivia tokens.
#[must_use]
pub fn format_with_options(file: &ParsedFile, options: FormatOptions) -> FormatResult {
    if !file.errors().is_empty() {
        return FormatResult { edits: Vec::new(), skipped: Some(FormatSkipReason::UnsafeSyntax) };
    }
    let formatted = match file.format() {
        FileFormat::PdxScript => format_pdx_script(file.source(), options),
        FileFormat::Localisation => format_localisation(file.source(), options),
        FileFormat::Csv => {
            return FormatResult {
                edits: Vec::new(),
                skipped: Some(FormatSkipReason::UnsupportedFormat),
            };
        }
    };
    if formatted == file.source() {
        return FormatResult { edits: Vec::new(), skipped: None };
    }
    FormatResult {
        edits: vec![TextEdit {
            range: TextRange::new(0, u32::try_from(file.source().len()).unwrap_or(u32::MAX))
                .unwrap_or_else(|| TextRange::empty(0)),
            replacement: formatted,
        }],
        skipped: None,
    }
}

fn format_pdx_script(source: &str, options: FormatOptions) -> String {
    let mut result = String::with_capacity(source.len());
    let mut stack = Vec::new();
    let mut blank_lines = 0_u8;
    for (content, ending) in split_lines(source) {
        let trimmed = content.trim_matches([' ', '\t']);
        if trimmed.is_empty() {
            if blank_lines < options.max_blank_lines {
                result.push_str(ending);
                blank_lines = blank_lines.saturating_add(1);
            }
            continue;
        }
        blank_lines = 0;
        let leading_closes = leading_closes(trimmed);
        let depth = stack.len().saturating_sub(leading_closes);
        result.push_str(&indent(depth, options));
        result.push_str(&normalize_pdx_line(trimmed, options.space_around_operator));
        result.push_str(ending);
        update_delimiters(trimmed, &mut stack);
    }
    result
}

fn format_localisation(source: &str, options: FormatOptions) -> String {
    let mut result = String::with_capacity(source.len());
    let mut blank_lines = 0_u8;
    for (content, ending) in split_lines(source) {
        let trimmed = content.trim_matches([' ', '\t']);
        if trimmed.is_empty() {
            if blank_lines < options.max_blank_lines {
                result.push_str(ending);
                blank_lines = blank_lines.saturating_add(1);
            }
            continue;
        }
        blank_lines = 0;
        result.push_str(trimmed);
        result.push_str(ending);
    }
    let _ = options;
    result
}

fn indent(depth: usize, options: FormatOptions) -> String {
    match options.indent_style {
        IndentStyle::Tabs => "\t".repeat(depth),
        IndentStyle::Spaces => " ".repeat(depth.saturating_mul(usize::from(options.indent_width))),
    }
}

fn normalize_pdx_line(line: &str, space_around_operator: bool) -> String {
    let mut output = String::with_capacity(line.len());
    let bytes = line.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'"' {
            let start = index;
            index += 1;
            while index < bytes.len() {
                if bytes[index] == b'\\' {
                    index = index.saturating_add(2).min(bytes.len());
                } else {
                    let closed = bytes[index] == b'"';
                    index += 1;
                    if closed {
                        break;
                    }
                }
            }
            output.push_str(&line[start..index]);
            continue;
        }
        if bytes[index] == b'#' {
            if !output.is_empty() && !output.ends_with([' ', '\t']) {
                output.push(' ');
            }
            output.push_str(&line[index..]);
            break;
        }
        if let Some(length) = operator_length(&bytes[index..]) {
            while output.ends_with([' ', '\t']) {
                output.pop();
            }
            if space_around_operator && !output.is_empty() {
                output.push(' ');
            }
            output.push_str(&line[index..index + length]);
            index += length;
            while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
                index += 1;
            }
            if space_around_operator && index < bytes.len() {
                output.push(' ');
            }
            continue;
        }
        if let Some(character) = line[index..].chars().next() {
            output.push(character);
            index += character.len_utf8();
        } else {
            break;
        }
    }
    output.trim_end_matches([' ', '\t']).to_owned()
}

fn operator_length(bytes: &[u8]) -> Option<usize> {
    if bytes.starts_with(b">=")
        || bytes.starts_with(b"<=")
        || bytes.starts_with(b"!=")
        || bytes.starts_with(b"==")
        || bytes.starts_with(b"?=")
    {
        Some(2)
    } else if bytes.starts_with(b"=") || bytes.starts_with(b">") || bytes.starts_with(b"<") {
        Some(1)
    } else {
        None
    }
}

fn leading_closes(line: &str) -> usize {
    line.bytes().take_while(|byte| matches!(byte, b'}' | b']')).count()
}

fn update_delimiters(line: &str, stack: &mut Vec<u8>) {
    let bytes = line.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'"' {
            index += 1;
            while index < bytes.len() {
                if bytes[index] == b'\\' {
                    index = index.saturating_add(2).min(bytes.len());
                } else {
                    let closed = bytes[index] == b'"';
                    index += 1;
                    if closed {
                        break;
                    }
                }
            }
            continue;
        }
        if bytes[index] == b'#' {
            break;
        }
        match bytes[index] {
            b'{' => stack.push(b'}'),
            b'}' => pop_delimiter(stack, b'}'),
            b'[' if bytes.get(index + 1) == Some(&b'[') => {
                stack.push(b']');
                index += 1;
            }
            b']' => pop_delimiter(stack, b']'),
            _ => {}
        }
        index += 1;
    }
}

fn pop_delimiter(stack: &mut Vec<u8>, delimiter: u8) {
    if stack.last() == Some(&delimiter) {
        stack.pop();
    }
}

fn split_lines(source: &str) -> Vec<(&str, &str)> {
    let mut lines = Vec::new();
    let bytes = source.as_bytes();
    let mut start = 0;
    for index in 0..bytes.len() {
        if bytes[index] != b'\n' {
            continue;
        }
        let (content_end, ending_start) = if index > start && bytes[index - 1] == b'\r' {
            (index - 1, index - 1)
        } else {
            (index, index)
        };
        lines.push((&source[start..content_end], &source[ending_start..index + 1]));
        start = index + 1;
    }
    if start < source.len() || source.is_empty() {
        lines.push((&source[start..], ""));
    }
    lines
}

#[cfg(test)]
mod tests {
    use pdx_syntax::{FileFormat, SyntaxErrorKind, TokenKind, parse};
    use pdx_text::TextRange;

    use super::{FormatOptions, FormatSkipReason, IndentStyle, format, format_with_options};

    fn apply(result: &super::FormatResult, source: &str) -> String {
        assert!(result.skipped.is_none());
        match result.edits.as_slice() {
            [] => source.to_owned(),
            [edit] => {
                assert_eq!(edit.range, TextRange::new(0, source.len() as u32).expect("range"));
                edit.replacement.clone()
            }
            _ => panic!("formatter currently emits one merged edit"),
        }
    }

    fn semantic_tokens(parsed: &pdx_syntax::ParsedFile) -> Vec<(TokenKind, String)> {
        parsed
            .tokens()
            .iter()
            .copied()
            .filter(|token| !token.kind().is_trivia())
            .map(|token| {
                (token.kind(), parsed.text(token.range()).expect("token range").to_owned())
            })
            .collect()
    }

    #[test]
    fn pdx_script_format_is_idempotent_and_only_changes_trivia() {
        let source = "# top\nroot={\nchild=1 # tail\n\n\n}\nroot = \"same\"\n";
        let parsed = parse(FileFormat::PdxScript, source);
        assert!(parsed.errors().is_empty(), "errors: {:?}", parsed.errors());
        let formatted = apply(&format(&parsed), source);
        assert_eq!(formatted, "# top\nroot = {\n    child = 1 # tail\n\n\n}\nroot = \"same\"\n");
        let reparsed = parse(FileFormat::PdxScript, &formatted);
        assert!(reparsed.errors().is_empty());
        assert_eq!(semantic_tokens(&parsed), semantic_tokens(&reparsed));
        assert_eq!(formatted, apply(&format(&reparsed), &formatted));
    }

    #[test]
    fn grammar_fixtures_format_idempotently() {
        let pdx_fixtures = [
            "country = FRA\ncapital ?= 144\nname = \"A quoted value\"\noperator_lte <= 10\n",
            "country = {\n  primary_culture = french\n  1444.11.11\n  rgb { 12 34 56 }\n  duplicate = one\n  duplicate = two\n}\n",
            "# leading comment\ntitle = \"escaped \\\"quote\\\" and $PARAM$\"\nvalue = @root:scope|leaf\n",
            "effect = {\n  [[name] value = yes ]\n  [[!other_name] nested = { enabled = yes } ]\n}\n",
        ];
        for source in pdx_fixtures {
            let parsed = parse(FileFormat::PdxScript, source);
            assert!(parsed.errors().is_empty(), "errors: {:?}", parsed.errors());
            let formatted = apply(&format(&parsed), source);
            let reparsed = parse(FileFormat::PdxScript, &formatted);
            assert!(reparsed.errors().is_empty(), "formatted errors: {:?}", reparsed.errors());
            assert_eq!(formatted, apply(&format(&reparsed), &formatted));
            assert_eq!(semantic_tokens(&parsed), semantic_tokens(&reparsed));
        }

        let localisation_fixtures = [
            "\u{feff}l_english:\n# fixture comment\ngreeting:0 \"Hello $NAME$ [Root.GetName] §YGold§!\"\nduplicate:1 \"First\"\nduplicate:2 \"Second\"\n",
            "l_german:\nlegacy:0 legacy value\n",
        ];
        for source in localisation_fixtures {
            let parsed = parse(FileFormat::Localisation, source);
            assert!(parsed.errors().is_empty(), "errors: {:?}", parsed.errors());
            let formatted = apply(&format(&parsed), source);
            let reparsed = parse(FileFormat::Localisation, &formatted);
            assert!(reparsed.errors().is_empty(), "formatted errors: {:?}", reparsed.errors());
            assert_eq!(formatted, apply(&format(&reparsed), &formatted));
            assert_eq!(semantic_tokens(&parsed), semantic_tokens(&reparsed));
        }
    }

    #[test]
    fn formatter_preserves_comments_and_supports_tabs() {
        let source = "a = {\n  # keep me\n  b = yes\n}\n";
        let parsed = parse(FileFormat::PdxScript, source);
        let options = FormatOptions { indent_style: IndentStyle::Tabs, ..FormatOptions::default() };
        let formatted = apply(&format_with_options(&parsed, options), source);
        assert!(formatted.contains("\t# keep me\n"));
        assert!(formatted.contains("\tb = yes\n"));
        assert!(formatted.contains("# keep me"));
    }

    #[test]
    fn unsafe_syntax_does_not_generate_a_destructive_edit() {
        let parsed = parse(FileFormat::PdxScript, "broken = \"unfinished");
        assert!(
            parsed.errors().iter().any(|error| error.kind == SyntaxErrorKind::UnterminatedString)
        );
        let result = format(&parsed);
        assert!(result.edits.is_empty());
        assert_eq!(result.skipped, Some(FormatSkipReason::UnsafeSyntax));
    }

    #[test]
    fn localisation_formatter_only_normalizes_layout() {
        let source = "  l_english:  \n\n\nhello:0 \"  text  \"   \n";
        let parsed = parse(FileFormat::Localisation, source);
        assert!(parsed.errors().is_empty(), "errors: {:?}", parsed.errors());
        let formatted = apply(&format(&parsed), source);
        assert_eq!(formatted, "l_english:\n\n\nhello:0 \"  text  \"\n");
    }

    #[test]
    fn csv_formatter_is_explicitly_unsupported() {
        let parsed = parse(FileFormat::Csv, "a,b\n1,2\n");
        let result = format(&parsed);
        assert!(result.edits.is_empty());
        assert_eq!(result.skipped, Some(FormatSkipReason::UnsupportedFormat));
    }
}
