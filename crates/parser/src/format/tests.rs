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
        "list = {\n\tone\n\ttwo\n\tthree\n}\n",
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
    let expected = "ROOT = { # header\n\t# second\n\tchild = yes # tail\n}\n";
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
fn single_line_quoted_for_effect_expands_nested_blocks() {
    let source = concat!(
        "hidden_effect = {\n\tfor = {\n\t\tamount = 4\n",
        "\t\teffect = \"random_owned_province = { limit = { and = { is_city = yes ",
        "or = { has_building = workshop has_building = counting_house } ",
        "not = { devastation = 10 } } not = { has_province_modifier = example_modifier } } ",
        "add_province_modifier = { name = example_modifier duration = 7300 } }\"\n",
        "\t}\n}\n",
    );
    let expected = concat!(
        "hidden_effect = {\n\tfor = {\n\t\tamount = 4\n",
        "\t\teffect = \"\n",
        "\t\t\trandom_owned_province = {\n",
        "\t\t\t\tlimit = {\n",
        "\t\t\t\t\tAND = {\n",
        "\t\t\t\t\t\tis_city = yes\n",
        "\t\t\t\t\t\tOR = {\n",
        "\t\t\t\t\t\t\thas_building = workshop\n",
        "\t\t\t\t\t\t\thas_building = counting_house\n",
        "\t\t\t\t\t\t}\n",
        "\t\t\t\t\t\tNOT = { devastation = 10 }\n",
        "\t\t\t\t\t}\n",
        "\t\t\t\t\tNOT = { has_province_modifier = example_modifier }\n",
        "\t\t\t\t}\n",
        "\t\t\t\tadd_province_modifier = {\n",
        "\t\t\t\t\tname = example_modifier\n",
        "\t\t\t\t\tduration = 7300\n",
        "\t\t\t\t}\n",
        "\t\t\t}\n",
        "\t\t\"\n\t}\n}\n",
    );
    let output = formatted(FileFormat::Script, source);
    assert_eq!(output, expected);
    assert_eq!(formatted(FileFormat::Script, &output), output);
}

#[test]
fn single_line_quotes_without_a_valid_script_block_stay_opaque() {
    let source = r#"description = "Use fallback={left right}"
label = "state=ROOT"
example = "text=\"{x}\""
broken = "trigger={ value=yes"
"#;
    assert_eq!(formatted(FileFormat::Script, source), source);
}

#[test]
fn quoted_script_with_multiple_lines_of_layout_survives_compact_parameter_blocks() {
    let source = "[[name]\neffect=\"if={a=yes b=yes}\"\nnext=yes\n]\n";
    let expected = "[[name]effect = \"if={a=yes b=yes}\" next = yes]\n";
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
        "\u{feff}ROOT = { child = yes }\n"
    );
}

#[test]
fn line_width_expands_properties_and_unclassified_lists() {
    let long_key = "界".repeat(58);
    let property_source = format!("root = {{ {long_key} = yes }}\n");
    let property_expected = format!("ROOT = {{\n\t{long_key} = yes\n}}\n");
    assert_eq!(
        formatted(FileFormat::Script, &property_source),
        property_expected
    );

    let scalar = "界".repeat(56);
    let scalar_source = format!("list = {{ {scalar} end }}\n");
    assert_eq!(
        formatted(FileFormat::Script, &scalar_source),
        format!("list = {{\n\t{scalar}\n\tend\n}}\n")
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
        "ROOT = {\n\ta = 1\n\tb = 2\n}\n"
    );
}

#[test]
fn script_keyword_spelling_is_canonicalized_to_capitals() {
    let source = concat!(
        "trigger = { and = { not = { root = this } } }\n",
        "effect = { or = { from = prev } }\n",
        "name = root\n",
        "description = \"root or this stays quoted\"\n",
        "and = { value = yes }\n",
    );
    let expected = concat!(
        "trigger = { AND = { NOT = { ROOT = THIS } } }\n",
        "effect = { OR = { FROM = PREV } }\n",
        "name = ROOT\n",
        "description = \"root or this stays quoted\"\n",
        "AND = { value = yes }\n",
    );
    let output = formatted(FileFormat::Script, source);
    assert_eq!(output, expected);
    // The canonical form is already uppercase: formatting is idempotent and emits no edits.
    assert!(format(&parse(FileFormat::Script, &output)).edits.is_empty());
}

#[test]
fn asset_path_separators_canonicalize_to_single_forward_slashes() {
    let source = r#"sprite = {
	texturefile = "gfx\\interface\\alerticon_dismiss.dds"
	second = "gfx//interface//hre_bg.tga"
	mixed = "gfx\\interface\choose_language_bg.dds"
	file = "gfx\\models\ships\barque.mesh"
	bare = gfx\interface\plain.dds
	upper = "gfx\\a.DDS"
	clean = "gfx/interface/plain.dds"
}
"#;
    let expected = r#"sprite = {
	texturefile = "gfx/interface/alerticon_dismiss.dds"
	second = "gfx/interface/hre_bg.tga"
	mixed = "gfx/interface/choose_language_bg.dds"
	file = "gfx/models/ships/barque.mesh"
	bare = gfx/interface/plain.dds
	upper = "gfx/a.DDS"
	clean = "gfx/interface/plain.dds"
}
"#;
    let output = formatted(FileFormat::Script, source);
    assert_eq!(output, expected);
    assert!(format(&parse(FileFormat::Script, &output)).edits.is_empty());
}

#[test]
fn non_asset_path_scalars_keep_their_spelling() {
    let source = r#"window = {
	spaces = "gfx//dlc//king of kings//x.dds"
	no_extension = "gfx\interface"
	plain_text = "notes\a.txt"
	empty = ""
	number = "999"
	escaped_quote = "a\"b.dds"
	description = "readme\\notes"
}
"#;
    assert_eq!(formatted(FileFormat::Script, source), source);
}

#[test]
fn quoted_script_payloads_normalize_nested_asset_paths() {
    let source = r#"effect = "
	texturefile = \"gfx\\a.dds\"
"
"#;
    let expected = concat!(r#"effect = "texturefile = \"gfx/a.dds\"""#, "\n",);
    let output = formatted(FileFormat::Script, source);
    assert_eq!(output, expected);
    assert!(format(&parse(FileFormat::Script, &output)).edits.is_empty());
}
