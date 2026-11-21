//! EU4 list layout hints from the file category and the syntax's block path.

use std::collections::{BTreeMap, HashMap};
use std::sync::LazyLock;

use parser::format::{FormatResult, ListLayout, format, format_with_list_layouts};
use parser::{CstKind, CstNode, ParsedFile, decode_quoted_script};
use rules::ir::{FieldId, Matcher, RootRule, RulesIr, SchemaId, Shape, SymbolFacts};
use serde::Deserialize;
use text::{LogicalPath, TextRange};

#[derive(Deserialize)]
struct Layouts {
    fields: HashMap<String, ListLayout>,
    schemas: HashMap<String, ListLayout>,
}

static LAYOUTS: LazyLock<Layouts> = LazyLock::new(|| {
    toml::from_str(include_str!("../../../../rules/eu4/list-layout.toml"))
        .expect("first-party EU4 list layouts")
});

struct SyntaxOnlyFacts;

impl SymbolFacts for SyntaxOnlyFacts {
    fn facts_complete(&self) -> bool {
        false
    }
}

/// Formats a document, using the selected EU4 schema to lay out known list blocks.
#[must_use]
pub fn format_document(
    file: &ParsedFile,
    path: Option<&LogicalPath>,
    ir: &RulesIr,
) -> FormatResult {
    let schema = path.and_then(|path| match ir.root_rule(path)? {
        RootRule::Schema(schema) => Some(*schema),
        RootRule::Instance { body, .. } => *body,
        RootRule::Opaque => None,
    });
    let Some(root) = schema.filter(|_| ir.game_id() == super::GAME_ID) else {
        return format(file);
    };
    format_with_list_layouts(file, |file| {
        let mut layouts = BTreeMap::new();
        collect_layouts(file, file.root(), root, ir, &mut layouts);
        layouts
    })
}

fn collect_layouts(
    file: &ParsedFile,
    container: CstNode<'_>,
    schema: SchemaId,
    ir: &RulesIr,
    layouts: &mut BTreeMap<TextRange, ListLayout>,
) {
    for node in container.children() {
        if node.kind() == CstKind::ParameterBlock {
            collect_layouts(file, node, schema, ir, layouts);
            continue;
        }
        let Some((key, block)) = keyed_block(node) else {
            continue;
        };
        let Some(raw_key) = file.text(key.range()) else {
            continue;
        };
        let raw_key = raw_key.trim_start_matches('\u{feff}');
        let decoded = decode_quoted_script(raw_key);
        let key = decoded.as_ref().map_or(raw_key, |(key, _)| key);
        let Some(field) = block_field(ir, schema, key, block) else {
            continue;
        };
        let Some(child) = ir.child(field, schema) else {
            continue;
        };
        let child_schema = ir.schema(child);
        if child_schema.items.is_some() {
            let field_layout = ir
                .provenance_of(field)
                .and_then(|origin| LAYOUTS.fields.get(ir.strings().resolve(origin.pointer)));
            let layout = field_layout
                .or_else(|| LAYOUTS.schemas.get(ir.strings().resolve(child_schema.name)));
            if let Some(&layout) = layout {
                layouts.insert(block.range(), layout);
            }
        }
        collect_layouts(file, block, child, ir, layouts);
    }
}

fn keyed_block(node: CstNode<'_>) -> Option<(CstNode<'_>, CstNode<'_>)> {
    let (key, mut block) = match node.kind() {
        CstKind::Property => (node.child(0)?, node.child(2)?.child(0)?),
        CstKind::HeaderBlock => (node.child(0)?, node.child(1)?),
        _ => return None,
    };
    if block.kind() == CstKind::HeaderBlock {
        block = block.child(1)?;
    }
    (block.kind() == CstKind::Block).then_some((key, block))
}

fn block_field(ir: &RulesIr, schema: SchemaId, key: &str, block: CstNode<'_>) -> Option<FieldId> {
    let mut exact = ir.lookup_exact(schema, key, Shape::Block);
    if let Some(first) = exact.next() {
        let accepts_items = |field| {
            ir.child(field, schema)
                .is_some_and(|id| ir.schema(id).items.is_some())
        };
        if !accepts_items(first)
            && block
                .children()
                .any(|node| matches!(node.kind(), CstKind::BareValue | CstKind::QuotedString))
        {
            return exact.find(|&field| accepts_items(field)).or(Some(first));
        }
        return Some(first);
    }
    let mut possible = None;
    for field in ir.lookup(schema, key, Shape::Block) {
        let matcher = ir.field(field).key;
        let matches = if matches!(ir.matcher(matcher), Matcher::Def { .. }) {
            Some(true)
        } else {
            ir.scalar_outcome(matcher, key, &SyntaxOnlyFacts)
        };
        match matches {
            Some(false) => {}
            Some(true) if possible.is_none() => return Some(field),
            _ if possible.is_some() => return None,
            _ => possible = Some(field),
        }
    }
    // A symbol-backed key can still have an unambiguous layout in a dedicated file.
    possible
}

#[cfg(test)]
mod tests {
    use super::*;

    fn formatted(path: &str, source: &str) -> String {
        let ir = super::super::first_party_ir().expect("EU4 rules");
        let path = LogicalPath::parse(path).expect("logical path");
        let run = |text| {
            format_document(
                &parser::parse(parser::FileFormat::Script, text),
                Some(&path),
                &ir,
            )
        };
        let result = run(source);
        assert_eq!(result.skipped, None);
        let mut output = source.to_owned();
        for edit in result.edits.into_iter().rev() {
            output.replace_range(
                edit.range.start() as usize..edit.range.end() as usize,
                &edit.replacement,
            );
        }
        let second_pass = run(&output);
        assert_eq!(second_pass.skipped, None);
        assert!(second_pass.edits.is_empty());
        output
    }

    #[test]
    fn mission_lists_use_their_schema_and_keep_singletons_inline() {
        let source = concat!(
            "series = {\n",
            "\tone = { required_missions = { a b a } }\n",
            "\ttwo = { required_missions = { a } }\n",
            "\tthree = { required_missions = { } }\n",
            "}\n",
        );
        let output = formatted("missions/test.txt", source);
        assert!(output.contains("required_missions = {\n\t\t\ta\n\t\t\tb\n\t\t\ta\n\t\t}"));
        assert!(output.contains("required_missions = { a }"));
        assert!(output.contains("required_missions = { }"));
        assert_eq!(formatted("events/test.txt", source), source);
    }

    #[test]
    fn list_layout_survives_conditional_blocks_and_block_overloads() {
        let source = "series = { task = { [[dlc]required_missions={a b}] } }\n";
        let output = formatted("missions/test.txt", source);
        assert!(output.contains("[[dlc]\n\t\t\trequired_missions = {\n\t\t\t\ta\n\t\t\t\tb\n"));

        let source = "test_reform = { factions = { faction_a faction_b } }\n";
        let output = formatted("common/government_reforms/test.txt", source);
        assert_eq!(
            output,
            "test_reform = {\n\tfactions = {\n\t\tfaction_a\n\t\tfaction_b\n\t}\n}\n"
        );
    }

    #[test]
    fn fixed_vectors_stay_inline_and_area_members_keep_their_rows() {
        let value = ["0.0000000000"; 12].join(" ");
        let source = format!("10 = {{ position = {{\n1 {value}\n2\n}} }}\n");
        let output = formatted("map/positions.txt", &source);
        assert_eq!(
            output,
            format!("10 = {{\n\tposition = {{ 1 {value} 2 }}\n}}\n")
        );

        let source =
            "demo_area = {\n# first group\n1 2 2\ncolor={\n1 2 3\n}\n4 5 # second group\n}\n";
        let expected = "demo_area = {\n\t# first group\n\t1 2 2\n\tcolor = { 1 2 3 }\n\t4 5 # second group\n}\n";
        assert_eq!(formatted("map/area.txt", source), expected);
    }

    #[test]
    fn list_hints_work_for_symbol_keys_and_parameterized_schemas() {
        let source = "17 = {\n \"Local Name\"\n \"Capital Name\"\n}\n";
        assert_eq!(
            formatted("common/province_names/test.txt", source),
            "17 = { \"Local Name\" \"Capital Name\" }\n"
        );
        let source = "on_startup = { events = { a.1 b.2 } }\n";
        assert_eq!(
            formatted("common/on_actions/test.txt", source),
            "on_startup = {\n\tevents = {\n\t\ta.1\n\t\tb.2\n\t}\n}\n"
        );
    }

    #[test]
    fn comments_keep_their_list_rows_when_items_expand() {
        let source =
            "series = { task = { required_missions = {\n# prerequisite\na b # last\n} } }\n";
        let output = formatted("missions/test.txt", source);
        assert!(
            output
                .contains("required_missions = {\n\t\t\t# prerequisite\n\t\t\ta\n\t\t\tb # last\n")
        );
    }

    #[test]
    fn long_map_lists_wrap_and_keep_source_groups_and_comments() {
        let wide = (1000..1035).map(|n| n.to_string()).collect::<Vec<_>>();
        let source = format!("sea_starts = {{\n{} # offshore\n9 8\n}}\n", wide.join(" "));
        let mut output = formatted("map/default.map", &source);
        let expected = format!(
            "sea_starts = {{\n\t{}\n\t{} # offshore\n\t9 8\n}}\n",
            wide[..23].join(" "),
            wide[23..].join(" "),
        );
        assert_eq!(output, expected);

        let control = (1000..1030).map(|n| n.to_string()).collect::<Vec<_>>();
        let source = format!(
            "node = {{ outgoing = {{ control = {{ {} }} }} }}\n",
            control.join(" ")
        );
        output = formatted("common/tradenodes/test.txt", &source);
        let lines = output
            .lines()
            .filter(|line| line.trim_start().starts_with("10"));
        let counts = lines
            .map(|line| line.split_whitespace().count())
            .collect::<Vec<_>>();
        assert_eq!(counts, [20, 10]);
    }
}
