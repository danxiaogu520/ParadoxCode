//! CST → model extraction.
//!
//! Uses the loss-aware [`pdx_parser`] CST so syntactically broken files still load:
//! recoverable nodes are skipped or kept as unknown fields, and issues are reported
//! in [`LoadedFile::warnings`] instead of failing the whole load.

use pdx_parser::{CstKind, CstNode, ParsedFile};

use super::model::{Block, Mission, MissionFile, MissionTree, RawField};

/// Result of loading a mission file.
#[derive(Clone, Debug)]
pub struct LoadedFile {
    pub file: MissionFile,
    /// Parser-level syntax errors (file still loads loss-aware).
    pub syntax_errors: Vec<pdx_parser::SyntaxError>,
    /// Extraction issues that did not prevent loading.
    pub warnings: Vec<String>,
}

/// Parses `source` as an EU4 mission file.
#[must_use]
pub fn parse_file(source: &str) -> LoadedFile {
    let parsed = pdx_parser::parse(pdx_parser::FileFormat::Script, source);
    let mut warnings = Vec::new();
    let trees = extract_trees(&parsed, &mut warnings);
    LoadedFile {
        file: MissionFile { trees },
        syntax_errors: parsed.errors().to_vec(),
        warnings,
    }
}

fn extract_trees(parsed: &ParsedFile, warnings: &mut Vec<String>) -> Vec<MissionTree> {
    let mut trees = Vec::new();
    for node in parsed.root().children() {
        if node.kind() != CstKind::Property {
            continue;
        }
        let (key, value) = match prop_parts(parsed, node) {
            Some(parts) => parts,
            None => {
                warnings.push("skipped malformed top-level property".to_owned());
                continue;
            }
        };
        let Some(block) = as_block(value) else {
            // Top-level scalars (e.g. stray assignments) are not trees.
            continue;
        };
        let tree = parse_tree(parsed, key, block, node.range(), warnings);
        trees.push(tree);
    }
    trees
}

fn parse_tree(
    parsed: &ParsedFile,
    key: &CstNode,
    value: &CstNode,
    span: pdx_text::TextRange,
    warnings: &mut Vec<String>,
) -> MissionTree {
    let mut tree = MissionTree {
        id: scalar(parsed, key),
        slot: 1,
        generic: false,
        ai: None,
        has_country_shield: None,
        potential: None,
        potential_on_load: None,
        missions: Vec::new(),
        unknown: Vec::new(),
        span,
    };
    for prop in block_props(value) {
        let (k, v) = match prop_parts(parsed, prop) {
            Some(parts) => parts,
            None => continue,
        };
        let name = scalar(parsed, k);
        match name.as_str() {
            "slot" => match parse_u32(parsed, v) {
                Some(n) => tree.slot = n,
                None => push_unknown(&mut tree.unknown, &name, value_text(parsed, v), warnings),
            },
            "generic" => match parse_bool(parsed, v) {
                Some(b) => tree.generic = b,
                None => push_unknown(&mut tree.unknown, &name, value_text(parsed, v), warnings),
            },
            "ai" => match parse_bool(parsed, v) {
                Some(b) => tree.ai = Some(b),
                None => push_unknown(&mut tree.unknown, &name, value_text(parsed, v), warnings),
            },
            "has_country_shield" => match parse_bool(parsed, v) {
                Some(b) => tree.has_country_shield = Some(b),
                None => push_unknown(&mut tree.unknown, &name, value_text(parsed, v), warnings),
            },
            "potential" if as_block(v).is_some() => {
                tree.potential = as_block(v).map(|b| block(parsed, b));
            }
            "potential_on_load" if as_block(v).is_some() => {
                tree.potential_on_load = as_block(v).map(|b| block(parsed, b));
            }
            "potential" | "potential_on_load" => {
                push_unknown(&mut tree.unknown, &name, value_text(parsed, v), warnings);
            }
            _ if as_block(v).is_some() => {
                let block = as_block(v).expect("checked above");
                let mission = parse_mission(parsed, k, block, prop.range(), warnings);
                tree.missions.push(mission);
            }
            _ => {
                tree.unknown.push(RawField {
                    name,
                    value: value_text(parsed, v),
                });
            }
        }
    }
    tree
}

fn parse_mission(
    parsed: &ParsedFile,
    key: &CstNode,
    value: &CstNode,
    span: pdx_text::TextRange,
    warnings: &mut Vec<String>,
) -> Mission {
    let mut mission = Mission {
        id: scalar(parsed, key),
        icon: None,
        mission_type: None,
        provinces_to_highlight: None,
        required: Vec::new(),
        position: None,
        completed_by: None,
        trigger: None,
        effect: None,
        unknown: Vec::new(),
        span,
    };
    for prop in block_props(value) {
        let (k, v) = match prop_parts(parsed, prop) {
            Some(parts) => parts,
            None => continue,
        };
        let name = scalar(parsed, k);
        match name.as_str() {
            "icon" => mission.icon = Some(scalar(parsed, v)),
            "type" => mission.mission_type = Some(scalar(parsed, v)),
            "provinces_to_highlight" if as_block(v).is_some() => {
                mission.provinces_to_highlight = as_block(v).map(|b| block(parsed, b));
            }
            "required_missions" if as_block(v).is_some() => {
                mission.required = block_scalars(parsed, as_block(v).expect("checked above"));
            }
            "required_missions" => {
                push_unknown(&mut mission.unknown, &name, value_text(parsed, v), warnings);
            }
            "position" => match parse_u32(parsed, v) {
                Some(n) => mission.position = Some(n),
                None => push_unknown(&mut mission.unknown, &name, value_text(parsed, v), warnings),
            },
            "completed_by" => mission.completed_by = Some(scalar(parsed, v)),
            "trigger" if as_block(v).is_some() => {
                mission.trigger = as_block(v).map(|b| block(parsed, b));
            }
            "effect" if as_block(v).is_some() => {
                mission.effect = as_block(v).map(|b| block(parsed, b));
            }
            "trigger" | "effect" => {
                push_unknown(&mut mission.unknown, &name, value_text(parsed, v), warnings);
            }
            _ => mission.unknown.push(RawField {
                name,
                value: value_text(parsed, v),
            }),
        }
    }
    mission
}

fn push_unknown(
    unknown: &mut Vec<RawField>,
    name: &str,
    value: String,
    warnings: &mut Vec<String>,
) {
    warnings.push(format!(
        "field `{name}` has an unexpected value type; kept verbatim"
    ));
    unknown.push(RawField {
        name: name.to_owned(),
        value,
    });
}

// --- CST helpers -----------------------------------------------------------

/// Returns the block node of a property value, unwrapping the `Value` wrapper.
fn as_block(node: &CstNode) -> Option<&CstNode> {
    match node.kind() {
        CstKind::Block | CstKind::HeaderBlock => Some(node),
        CstKind::Value => node
            .children()
            .iter()
            .find(|c| matches!(c.kind(), CstKind::Block | CstKind::HeaderBlock)),
        _ => None,
    }
}

fn prop_parts<'a>(_parsed: &ParsedFile, prop: &'a CstNode) -> Option<(&'a CstNode, &'a CstNode)> {
    let mut key = None;
    let mut value = None;
    for child in prop.children() {
        match child.kind() {
            CstKind::Key => key = Some(child),
            CstKind::Value => value = Some(child),
            _ => {}
        }
    }
    Some((key?, value?))
}

/// Properties directly inside a block, in source order.
fn block_props(block: &CstNode) -> Vec<&CstNode> {
    block
        .children()
        .iter()
        .filter(|c| c.kind() == CstKind::Property)
        .collect()
}

/// Unquoted scalar text of a node.
fn scalar(parsed: &ParsedFile, node: &CstNode) -> String {
    unquote(parsed.text(node.range()).unwrap_or_default().trim())
}

/// Full raw value text (scalar or block, braces included).
fn value_text(parsed: &ParsedFile, node: &CstNode) -> String {
    parsed
        .text(node.range())
        .unwrap_or_default()
        .trim()
        .to_owned()
}

fn unquote(text: &str) -> String {
    text.strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(text)
        .to_owned()
}

fn block(parsed: &ParsedFile, node: &CstNode) -> Block {
    Block::new(value_text(parsed, node))
}

/// Scalars inside a block, in source order.
fn block_scalars(parsed: &ParsedFile, block: &CstNode) -> Vec<String> {
    block
        .children()
        .iter()
        .filter(|c| matches!(c.kind(), CstKind::BareValue | CstKind::QuotedString))
        .map(|c| scalar(parsed, c))
        .filter(|s| !s.is_empty())
        .collect()
}

fn parse_u32(parsed: &ParsedFile, node: &CstNode) -> Option<u32> {
    scalar(parsed, node).parse().ok()
}

fn parse_bool(parsed: &ParsedFile, node: &CstNode) -> Option<bool> {
    match scalar(parsed, node).as_str() {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    }
}
