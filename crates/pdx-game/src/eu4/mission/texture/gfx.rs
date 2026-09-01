//! `interface/*.gfx` spriteType index.
//!
//! EU4 maps sprite names to texture files through `spriteType = { name = ...
//! texturefile = "gfx//interface//..." }` entries in the `interface/*.gfx`
//! files. This module finds those pairs with the loss-aware `pdx-parser` CST
//! so even partially broken files still yield usable entries.

use std::collections::HashMap;

use pdx_parser::{CstKind, FileFormat, ParsedFile};

/// One parsed sprite mapping: `name` -> normalized `texturefile` path
/// (relative to the game root, forward slashes, leading slash stripped).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpriteEntry {
    pub name: String,
    pub texture_file: String,
}

/// Parses one `.gfx` file and returns its sprite entries in source order.
pub fn parse_gfx_sprites(source: &str) -> Vec<SpriteEntry> {
    let parsed = pdx_parser::parse(FileFormat::Script, source);
    let mut entries = Vec::new();
    for sprite in sprite_type_nodes(&parsed) {
        let Some(block) = property_block(sprite) else {
            continue;
        };
        let mut name = None;
        let mut texture_file = None;
        for prop in block_properties(block) {
            match scalar_key(&parsed, prop).as_deref() {
                Some("name") => name = scalar_value(&parsed, prop),
                Some("texturefile") => texture_file = scalar_value(&parsed, prop),
                _ => {}
            }
        }
        if let (Some(name), Some(texture_file)) = (name, texture_file) {
            let texture_file = normalize_texture_path(&texture_file);
            if !texture_file.is_empty() {
                entries.push(SpriteEntry { name, texture_file });
            }
        }
    }
    entries
}

/// Builds a name -> normalized texture path map from the parsed entries of
/// several `.gfx` files. Earlier files win; repeated names (mod overrides)
/// keep their first occurrence, matching the game's `spriteType` lookup.
pub fn build_sprite_index(files: &[&str]) -> HashMap<String, String> {
    let mut index = HashMap::new();
    for source in files {
        for entry in parse_gfx_sprites(source) {
            index.entry(entry.name).or_insert(entry.texture_file);
        }
    }
    index
}

/// Normalizes an EU4 texture path (`gfx//interface//missions//x.dds`,
/// `gfx/interface/...`) into a clean root-relative path with `/` separators.
fn normalize_texture_path(path: &str) -> String {
    let trimmed = path.trim().trim_matches('"').trim();
    let cleaned = trimmed.replace("//", "/");
    let cleaned = cleaned.trim_start_matches('/');
    // Reject escaping or absolute paths defensively.
    if cleaned.starts_with("..") || cleaned.contains('\\') || cleaned.contains('\0') {
        return String::new();
    }
    cleaned.to_owned()
}

// --- CST traversal helpers (mirroring `load.rs`) ---------------------------

/// Returns every `spriteType` property node, including those wrapped in a
/// top-level `spriteTypes = { ... }` container (the common EU4 layout).
fn sprite_type_nodes(parsed: &ParsedFile) -> Vec<pdx_parser::CstNode<'_>> {
    let mut nodes = Vec::new();
    for prop in parsed.root().children().filter(is_property) {
        match scalar_key(parsed, prop).as_deref() {
            Some("spriteType") if property_block(prop).is_some() => nodes.push(prop),
            Some("spriteTypes") => {
                if let Some(block) = property_block(prop) {
                    for nested in block_properties(block) {
                        if scalar_key(parsed, nested).as_deref() == Some("spriteType")
                            && property_block(nested).is_some()
                        {
                            nodes.push(nested);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    nodes
}

fn is_property(node: &pdx_parser::CstNode<'_>) -> bool {
    node.kind() == CstKind::Property
}

fn block_properties(node: pdx_parser::CstNode<'_>) -> Vec<pdx_parser::CstNode<'_>> {
    node.children()
        .filter(|child| child.kind() == CstKind::Property)
        .collect()
}

fn property_block(prop: pdx_parser::CstNode<'_>) -> Option<pdx_parser::CstNode<'_>> {
    prop.children().find_map(|child| match child.kind() {
        CstKind::Block | CstKind::HeaderBlock => Some(child),
        CstKind::Value => child
            .children()
            .find(|c| matches!(c.kind(), CstKind::Block | CstKind::HeaderBlock)),
        _ => None,
    })
}

fn scalar_key(parsed: &ParsedFile, prop: pdx_parser::CstNode<'_>) -> Option<String> {
    prop.children()
        .find(|c| c.kind() == CstKind::Key)
        .map(|key| unquote(parsed.text(key.range()).unwrap_or_default().trim()))
        .filter(|value| !value.is_empty())
}

fn scalar_value(parsed: &ParsedFile, prop: pdx_parser::CstNode<'_>) -> Option<String> {
    prop.children()
        .find(|c| c.kind() == CstKind::Value)
        .map(|value| unquote(parsed.text(value.range()).unwrap_or_default().trim()))
        .filter(|value| !value.is_empty())
}

fn unquote(text: &str) -> String {
    text.strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(text)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
spriteTypes = {
    spriteType = {
        name = "GFX_mission_icons_frame"
        texturefile = "gfx//interface//missions//mission_icons_frame.dds"
    }
    spriteType = {
        name = "gfx_arrow_end"
        texturefile = "gfx//interface//missions//arrow_end.dds"
    }
}
"#;

    #[test]
    fn parses_nested_sprite_types() {
        let entries = parse_gfx_sprites(SAMPLE);
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0],
            SpriteEntry {
                name: "GFX_mission_icons_frame".to_owned(),
                texture_file: "gfx/interface/missions/mission_icons_frame.dds".to_owned(),
            }
        );
        assert_eq!(entries[1].name, "gfx_arrow_end");
    }

    #[test]
    fn parses_flat_sprite_types() {
        let source = r#"
spriteType = {
    name = mission_x
    texturefile = gfx/interface/missions/mission_x.dds
}
"#;
        let entries = parse_gfx_sprites(source);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "mission_x");
        assert_eq!(
            entries[0].texture_file,
            "gfx/interface/missions/mission_x.dds"
        );
    }

    #[test]
    fn rejects_escaping_paths() {
        assert_eq!(normalize_texture_path("../evil.dds"), "");
        assert_eq!(normalize_texture_path("C:\\evil.dds"), "");
        assert_eq!(
            normalize_texture_path("gfx//interface//missions//a.dds"),
            "gfx/interface/missions/a.dds"
        );
    }

    #[test]
    fn first_occurrence_wins_in_index() {
        let index = build_sprite_index(&[
            "spriteTypes = { spriteType = { name = a texturefile = \"one.dds\" } }",
            "spriteTypes = { spriteType = { name = a texturefile = \"two.dds\" } }",
        ]);
        assert_eq!(index.get("a").map(String::as_str), Some("one.dds"));
    }
}
