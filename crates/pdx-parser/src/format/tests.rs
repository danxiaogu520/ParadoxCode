use crate::{FileFormat, SyntaxErrorKind, parse};

use super::{FormatSkipReason, TextEdit, format};

fn apply(source: &str, edits: &[TextEdit]) -> String {
    let mut output = source.to_owned();
    for edit in edits.iter().rev() {
        let start = usize::try_from(edit.range.start()).expect("start");
        let end = usize::try_from(edit.range.end()).expect("end");
        output.replace_range(start..end, &edit.replacement);
    }
    output
}

fn formatted(format_kind: FileFormat, source: &str) -> String {
    let parsed = parse(format_kind, source);
    assert!(parsed.errors().is_empty(), "errors: {:?}", parsed.errors());
    let result = format(&parsed);
    assert!(result.skipped.is_none(), "skipped: {:?}", result.skipped);
    apply(source, &result.edits)
}

#[test]
fn canonical_block_layout_is_recursive_and_idempotent() {
    let source = "outer={inner={factor=1}}\nlist={one\ntwo\nthree}\nmany={a=1 b=2}\n";
    let expected = concat!(
        "outer = { inner = { factor = 1 } }\n",
        "list = { one two three }\n",
        "many = {\n",
        "\ta = 1\n",
        "\tb = 2\n",
        "}\n",
    );
    let output = formatted(FileFormat::Script, source);
    assert_eq!(output, expected);
    assert!(format(&parse(FileFormat::Script, &output)).edits.is_empty());
}

#[test]
fn comments_expand_blocks_and_first_leading_comment_joins_opener() {
    let source = "root = {\n\n# header\n# second\nchild=yes # tail\n}\n";
    let expected = "root = { # header\n\t# second\n\tchild = yes # tail\n}\n";
    assert_eq!(formatted(FileFormat::Script, source), expected);
}

#[test]
fn parameter_blocks_are_compact_unless_they_contain_comments() {
    let compact = "[[name]\na=1\nb={x=1 y=2}\n]\n";
    assert_eq!(
        formatted(FileFormat::Script, compact),
        "[[name]a = 1 b = { x = 1 y = 2 }]\n"
    );
    let commented = "[[name]\n# note\na=1\n]\n";
    assert_eq!(
        formatted(FileFormat::Script, commented),
        "[[name]\n\t# note\n\ta = 1\n]\n"
    );
}

#[test]
fn quoted_script_collapses_or_expands_and_opaque_text_is_unchanged() {
    let source = concat!(
        "first_limit = \"\n\thas_disaster = example\n\"\n",
        "first_effect = \"\n\ta = yes\n\tb = { x = 1 }\n\"\n",
        "description = \"first prose line\n\nsecond prose line\"\n",
    );
    let expected = concat!(
        "first_limit = \"has_disaster = example\"\n",
        "first_effect = \"\n",
        "\ta = yes\n",
        "\tb = { x = 1 }\n",
        "\"\n",
        "description = \"first prose line\n\nsecond prose line\"\n",
    );
    assert_eq!(formatted(FileFormat::Script, source), expected);
}

#[test]
fn quoted_script_supports_escaped_nested_quotes() {
    let source = "effect = \"\n\tname = \\\"quoted\\\"\n\tvalue = yes\n\"\n";
    let expected = "effect = \"\n\tname = \\\"quoted\\\"\n\tvalue = yes\n\"\n";
    assert_eq!(formatted(FileFormat::Script, source), expected);
}

#[test]
fn formatting_uses_tabs_lf_no_blank_lines_and_one_final_newline() {
    let source = "\u{feff}root = {\r\n  child = yes\r\n\r\n}\r\n\r\n";
    assert_eq!(
        formatted(FileFormat::Script, source),
        "\u{feff}root = { child = yes }\n"
    );
}

#[test]
fn line_width_expands_properties_but_never_scalar_only_blocks() {
    let long_key = "界".repeat(58);
    let property_source = format!("root = {{ {long_key} = yes }}\n");
    let property_expected = format!("root = {{\n\t{long_key} = yes\n}}\n");
    assert_eq!(
        formatted(FileFormat::Script, &property_source),
        property_expected
    );

    let scalar = "value".repeat(30);
    let scalar_source = format!("list = {{\n{scalar}\n}}\n");
    assert_eq!(
        formatted(FileFormat::Script, &scalar_source),
        format!("list = {{ {scalar} }}\n")
    );
}

#[test]
fn opaque_multiline_scalar_preserves_internal_crlf_and_blank_lines() {
    let source = "description = \"first\r\n\r\nsecond\"\r\n";
    assert_eq!(
        formatted(FileFormat::Script, source),
        "description = \"first\r\n\r\nsecond\"\n"
    );
}

#[test]
fn localisation_is_canonical_but_values_remain_opaque() {
    let source = "\u{feff}  l_english:  \r\n\r\n hello:0 \"  text  \"   \r\n# note\r\n";
    let expected = "\u{feff}l_english:\nhello:0 \"  text  \"\n# note\n";
    assert_eq!(formatted(FileFormat::Localisation, source), expected);
}

#[test]
fn unsafe_syntax_does_not_generate_edits() {
    let parsed = parse(FileFormat::Script, "broken = \"unfinished");
    assert!(
        parsed
            .errors()
            .iter()
            .any(|error| error.kind == SyntaxErrorKind::UnterminatedString)
    );
    let result = format(&parsed);
    assert!(result.edits.is_empty());
    assert_eq!(result.skipped, Some(FormatSkipReason::UnsafeSyntax));
}

#[test]
fn formatter_emits_precise_non_overlapping_edits() {
    let source = "root={a=1 b=2}";
    let parsed = parse(FileFormat::Script, source);
    let result = format(&parsed);
    assert!(result.skipped.is_none());
    assert!(result.edits.len() > 1);
    assert!(
        result
            .edits
            .windows(2)
            .all(|pair| pair[0].range.end() <= pair[1].range.start())
    );
    assert_eq!(
        apply(source, &result.edits),
        "root = {\n\ta = 1\n\tb = 2\n}\n"
    );
}
