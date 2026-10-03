//! Capability boundary for the first supported structured view.
//!
//! View presence, source selection, symbol namespace, field spellings, and write order come
//! from the active package. Its grid geometry stays in the game package. This intentionally
//! does not introduce a general tree-view language before a second view exists.
use rules::{GameProfile, ProfileMissionViewSpec};
use std::collections::HashSet;

pub use crate::eu4::mission::geometry;
pub use crate::eu4::mission::{
    Diagnostic, LoadedFile, Mission, MissionFile, MissionTree, Severity,
};

/// The mission view declared by a game package, independent of its game identity.
#[derive(Clone, Copy)]
pub struct MissionView<'a> {
    /// Declared facts used by this view.
    pub spec: &'a ProfileMissionViewSpec,
}
/// Returns the optional structured-view capability exposed by this package.
#[must_use]
pub fn view(profile: &GameProfile) -> Option<MissionView<'_>> {
    profile
        .mission_view
        .as_ref()
        .map(|spec| MissionView { spec })
}
impl MissionView<'_> {
    /// Whether the view serves a logical source path.
    #[must_use]
    pub fn matches(&self, path: &str) -> bool {
        self.spec.path.matches(path)
    }
    /// Loss-aware extraction using package field spellings.
    #[must_use]
    pub fn parse(&self, source: &str) -> LoadedFile {
        crate::eu4::mission::load::parse_file_with_spec(source, self.spec)
    }
    /// Writes a tree with this package's field spellings and stable role order.
    #[must_use]
    pub fn render(&self, tree: &MissionTree, style: &crate::eu4::mission::WriteStyle) -> String {
        crate::eu4::mission::write::render_tree_with_spec(tree, style, self.spec)
    }
    /// Structural findings resolved against reachable workspace node names.
    #[must_use]
    pub fn validate(&self, file: &MissionFile, universe: &HashSet<String>) -> Vec<Diagnostic> {
        crate::eu4::mission::validate_with_universe_ids(file, universe)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn another_game_and_field_layout_work_without_engine_or_ide_changes() {
        let mut profile = crate::eu4::first_party_ir().unwrap().game.profile.clone();
        profile.game_id = "synthetic".to_owned();
        let spec = profile.mission_view.as_mut().unwrap();
        spec.path.pattern = "graphs/".to_owned();
        spec.symbol_kind = "node".to_owned();
        spec.tree_fields.slot = "column".to_owned();
        spec.node_fields.position = "row".to_owned();
        spec.node_fields.required_missions = "after".to_owned();
        let view = view(&profile).unwrap();
        assert!(view.matches("graphs/example.txt"));
        assert!(!view.matches("missions/example.txt"));
        let loaded = view.parse(
            "tree = { column = 2 a = { row = 1 after = { b } } b = { row = 2 after = { a } } }",
        );
        assert_eq!(loaded.file.trees[0].slot, 2);
        assert_eq!(loaded.file.trees[0].missions[0].position, Some(1));
        assert_eq!(loaded.file.trees[0].missions[0].required, ["b"]);
        assert!(
            view.validate(&loaded.file, &HashSet::new())
                .iter()
                .any(|finding| finding.code == "dependency-cycle")
        );
        let rendered = view.render(
            &loaded.file.trees[0],
            &crate::eu4::mission::detect_style(""),
        );
        assert!(rendered.contains("column = 2"));
        assert!(rendered.contains("row = 1"));
        assert!(rendered.contains("after = { b }"));
        assert!(!rendered.contains("required_missions"));
        assert_eq!(
            view.parse(&rendered).file.trees[0].missions[0].required,
            ["b"]
        );
        profile.mission_view = None;
        assert!(super::view(&profile).is_none());
    }
}
