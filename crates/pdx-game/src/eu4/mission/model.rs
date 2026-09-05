//! Mission-tree data model.
//!
//! The model mirrors the EU4 1.35+ mission file format. Fields the editor does not
//! model yet are kept as [`RawField`] entries so nothing is lost on write-back.

use pdx_text::TextRange;

/// A parsed mission file: an ordered list of top-level mission trees.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MissionFile {
    pub trees: Vec<MissionTree>,
}

/// One top-level mission tree (`tree_id = { ... }`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MissionTree {
    /// Tree id (the top-level property key).
    pub id: String,
    /// Byte range of the id token in the source file.
    pub id_range: TextRange,
    /// In-game slot (column position, 1-based).
    pub slot: u32,
    /// Whether the tree is a generic tree (`generic = yes`).
    pub generic: bool,
    /// `ai = yes/no`, absent when not written.
    pub ai: Option<bool>,
    /// `has_country_shield`, absent when not written.
    pub has_country_shield: Option<bool>,
    /// `potential` block, preserved verbatim.
    pub potential: Option<Block>,
    /// `potential_on_load` block, preserved verbatim.
    pub potential_on_load: Option<Block>,
    /// Missions in source order.
    pub missions: Vec<Mission>,
    /// Fields this editor does not model, in source order.
    pub unknown: Vec<RawField>,
    /// Byte span of the whole `id = { ... }` block in the source file.
    pub span: TextRange,
}

/// One mission inside a tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mission {
    /// Mission id (property key inside the tree).
    pub id: String,
    /// Byte range of the id token in the source file.
    pub id_range: TextRange,
    /// `icon = mission_...`
    pub icon: Option<String>,
    /// `type = conquest` etc.
    pub mission_type: Option<String>,
    /// `provinces_to_highlight` block, preserved verbatim.
    pub provinces_to_highlight: Option<Block>,
    /// Prerequisite missions (`required_missions = { ... }`), in source order.
    pub required: Vec<String>,
    /// Byte range of each prerequisite token inside `required_missions`,
    /// parallel to [`Mission::required`].
    pub required_ranges: Vec<TextRange>,
    /// `position = n` (in-game column). `None` when absent.
    pub position: Option<u32>,
    /// Byte range of the written `position` value, when present.
    pub position_range: Option<TextRange>,
    /// `completed_by = date`
    pub completed_by: Option<String>,
    /// `trigger` block, preserved verbatim.
    pub trigger: Option<Block>,
    /// `effect` block, preserved verbatim.
    pub effect: Option<Block>,
    /// Fields this editor does not model (e.g. custom fields), in source order.
    pub unknown: Vec<RawField>,
    /// Byte span of the whole `mission_id = { ... }` block in the source file.
    pub span: TextRange,
}

/// An opaque block value (`{ ... }`) preserved byte-for-byte.
///
/// The editor treats `trigger`, `effect`, `potential` and similar blocks as plain text
/// for now: the user edits them in the property panel and the text is written back
/// verbatim. No inner reformatting happens.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Block {
    /// Full raw block text including braces.
    pub text: String,
}

/// A field this editor does not model, kept verbatim and written back unchanged.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawField {
    pub name: String,
    /// Raw value text: scalar, quoted string, or full block.
    pub value: String,
}

impl MissionFile {
    /// All mission ids in this file, keyed by id (first occurrence wins).
    pub fn mission_ids(&self) -> std::collections::HashMap<&str, (&MissionTree, &Mission)> {
        let mut map = std::collections::HashMap::new();
        for tree in &self.trees {
            for mission in &tree.missions {
                map.entry(mission.id.as_str())
                    .or_insert_with(|| (tree, mission));
            }
        }
        map
    }
}

impl MissionTree {
    /// Returns the mission with `id` in this tree, if any.
    pub fn mission(&self, id: &str) -> Option<&Mission> {
        self.missions.iter().find(|m| m.id == id)
    }
}

impl Block {
    /// Creates a block from raw text. The text should include the braces.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}
