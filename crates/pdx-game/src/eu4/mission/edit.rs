//! Structured editing operations on [`MissionFile`].
//!
//! All mutations go through this module so the GUI never pokes at model internals
//! directly. Every operation keeps the model consistent:
//!
//! - removing a mission removes all `required_missions` references to it;
//! - renaming a mission updates every reference in the file;
//! - adding a dependency rejects self-references and duplicates;
//! - ids are unique per file (trees) and per tree (missions).

use std::collections::HashSet;

use pdx_text::TextRange;

use super::model::{Block, Mission, MissionFile, MissionTree};

/// Failure of an edit operation. All variants are user-presentable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EditError {
    /// A tree with this id already exists in the file.
    DuplicateTreeId(String),
    /// A mission with this id already exists in the tree.
    DuplicateMissionId(String),
    /// The referenced tree does not exist.
    TreeNotFound(String),
    /// The referenced mission does not exist.
    MissionNotFound(String),
    /// A mission cannot require itself.
    SelfDependency(String),
    /// The prerequisite is not on the row directly above, nor directly above
    /// in the same column (the layouts the game renders cleanly).
    IllegalEdgePlacement { mission: String, required: String },
    /// A new mission cannot be created on a cell that already holds one.
    CellOccupied(u32, u32),
    /// The column holds more than one group, so membership is ambiguous.
    AmbiguousColumn(u32),
    /// The new id is empty.
    EmptyId,
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateTreeId(id) => write!(f, "tree id `{id}` already exists"),
            Self::DuplicateMissionId(id) => write!(f, "mission id `{id}` already exists"),
            Self::TreeNotFound(id) => write!(f, "tree `{id}` does not exist"),
            Self::MissionNotFound(id) => write!(f, "mission `{id}` does not exist"),
            Self::SelfDependency(id) => write!(f, "mission `{id}` cannot require itself"),
            Self::IllegalEdgePlacement { mission, required } => write!(
                f,
                "mission `{mission}` cannot require `{required}`: the prerequisite must be \
                 on the row directly above or directly above in the same column"
            ),
            Self::CellOccupied(column, row) => write!(
                f,
                "cell (column {column}, row {row}) already holds a mission"
            ),
            Self::AmbiguousColumn(column) => write!(
                f,
                "column {column} holds more than one group; pick a different column"
            ),
            Self::EmptyId => write!(f, "id must not be empty"),
        }
    }
}

impl std::error::Error for EditError {}

fn validate_id(id: &str) -> Result<(), EditError> {
    if id.trim().is_empty() {
        return Err(EditError::EmptyId);
    }
    Ok(())
}

impl MissionFile {
    // --- Trees ------------------------------------------------------------

    /// Creates a new empty tree with `id`.
    pub fn add_tree(&mut self, id: &str) -> Result<&mut MissionTree, EditError> {
        validate_id(id)?;
        if self.trees.iter().any(|t| t.id == id) {
            return Err(EditError::DuplicateTreeId(id.to_owned()));
        }
        self.trees.push(MissionTree {
            id: id.to_owned(),
            slot: 1,
            generic: false,
            ai: None,
            has_country_shield: None,
            potential: None,
            potential_on_load: None,
            missions: Vec::new(),
            unknown: Vec::new(),
            span: TextRange::empty(0),
        });
        Ok(self.trees.last_mut().expect("just pushed"))
    }

    /// Removes a tree and everything inside it.
    pub fn remove_tree(&mut self, tree_id: &str) -> Result<(), EditError> {
        let before = self.trees.len();
        self.trees.retain(|t| t.id != tree_id);
        if self.trees.len() == before {
            return Err(EditError::TreeNotFound(tree_id.to_owned()));
        }
        Ok(())
    }

    /// Renames a tree, keeping its missions intact.
    pub fn rename_tree(&mut self, old_id: &str, new_id: &str) -> Result<(), EditError> {
        validate_id(new_id)?;
        if self.trees.iter().any(|t| t.id == new_id) {
            return Err(EditError::DuplicateTreeId(new_id.to_owned()));
        }
        let tree = self
            .trees
            .iter_mut()
            .find(|t| t.id == old_id)
            .ok_or_else(|| EditError::TreeNotFound(old_id.to_owned()))?;
        tree.id = new_id.to_owned();
        Ok(())
    }

    // --- Missions ---------------------------------------------------------

    /// Creates a new mission inside `tree_id`.
    pub fn add_mission(&mut self, tree_id: &str, id: &str) -> Result<&mut Mission, EditError> {
        validate_id(id)?;
        let tree = self
            .trees
            .iter_mut()
            .find(|t| t.id == tree_id)
            .ok_or_else(|| EditError::TreeNotFound(tree_id.to_owned()))?;
        if tree.missions.iter().any(|m| m.id == id) {
            return Err(EditError::DuplicateMissionId(id.to_owned()));
        }
        tree.missions.push(Mission {
            id: id.to_owned(),
            icon: None,
            mission_type: None,
            provinces_to_highlight: None,
            required: Vec::new(),
            position: None,
            completed_by: None,
            trigger: None,
            effect: None,
            unknown: Vec::new(),
            span: TextRange::empty(0),
        });
        Ok(tree.missions.last_mut().expect("just pushed"))
    }

    /// Removes a mission and every `required_missions` reference to it.
    pub fn remove_mission(&mut self, tree_id: &str, mission_id: &str) -> Result<(), EditError> {
        let tree = self
            .trees
            .iter_mut()
            .find(|t| t.id == tree_id)
            .ok_or_else(|| EditError::TreeNotFound(tree_id.to_owned()))?;
        let before = tree.missions.len();
        tree.missions.retain(|m| m.id != mission_id);
        if tree.missions.len() == before {
            return Err(EditError::MissionNotFound(mission_id.to_owned()));
        }
        for mission in &mut tree.missions {
            mission.required.retain(|r| r != mission_id);
        }
        Ok(())
    }

    /// Renames a mission and updates every reference to it in the whole file.
    pub fn rename_mission(
        &mut self,
        tree_id: &str,
        old_id: &str,
        new_id: &str,
    ) -> Result<(), EditError> {
        validate_id(new_id)?;
        let tree = self
            .trees
            .iter_mut()
            .find(|t| t.id == tree_id)
            .ok_or_else(|| EditError::TreeNotFound(tree_id.to_owned()))?;
        if tree.missions.iter().any(|m| m.id == new_id) {
            return Err(EditError::DuplicateMissionId(new_id.to_owned()));
        }
        let mission = tree
            .missions
            .iter_mut()
            .find(|m| m.id == old_id)
            .ok_or_else(|| EditError::MissionNotFound(old_id.to_owned()))?;
        mission.id = new_id.to_owned();
        for other_tree in &mut self.trees {
            for other in &mut other_tree.missions {
                for required in &mut other.required {
                    if required == old_id {
                        *required = new_id.to_owned();
                    }
                }
            }
        }
        Ok(())
    }

    // --- Dependencies -----------------------------------------------------

    /// Adds `required_id` as a prerequisite of `mission_id` in `tree_id`.
    ///
    /// Rejects self-references, duplicates, and edges that violate the spatial
    /// legality rule (see [`super::graph`]): a prerequisite must sit on the row
    /// directly above its dependent or directly above it in the same column.
    pub fn add_required(
        &mut self,
        tree_id: &str,
        mission_id: &str,
        required_id: &str,
    ) -> Result<(), EditError> {
        if required_id == mission_id {
            return Err(EditError::SelfDependency(mission_id.to_owned()));
        }
        let tree = self
            .trees
            .iter()
            .find(|t| t.id == tree_id)
            .ok_or_else(|| EditError::TreeNotFound(tree_id.to_owned()))?;
        let mission = tree
            .missions
            .iter()
            .find(|m| m.id == mission_id)
            .ok_or_else(|| EditError::MissionNotFound(mission_id.to_owned()))?;
        if mission.required.contains(&required_id.to_owned()) {
            return Ok(());
        }
        self.check_edge_placement(mission_id, required_id)?;
        let tree = self
            .trees
            .iter_mut()
            .find(|t| t.id == tree_id)
            .ok_or_else(|| EditError::TreeNotFound(tree_id.to_owned()))?;
        let mission = tree
            .missions
            .iter_mut()
            .find(|m| m.id == mission_id)
            .ok_or_else(|| EditError::MissionNotFound(mission_id.to_owned()))?;
        mission.required.push(required_id.to_owned());
        Ok(())
    }

    /// Removes one prerequisite.
    pub fn remove_required(
        &mut self,
        tree_id: &str,
        mission_id: &str,
        required_id: &str,
    ) -> Result<(), EditError> {
        let tree = self
            .trees
            .iter_mut()
            .find(|t| t.id == tree_id)
            .ok_or_else(|| EditError::TreeNotFound(tree_id.to_owned()))?;
        let mission = tree
            .missions
            .iter_mut()
            .find(|m| m.id == mission_id)
            .ok_or_else(|| EditError::MissionNotFound(mission_id.to_owned()))?;
        mission.required.retain(|r| r != required_id);
        Ok(())
    }

    /// Replaces the full prerequisite list (order preserved, duplicates removed).
    pub fn set_required(
        &mut self,
        tree_id: &str,
        mission_id: &str,
        required: Vec<String>,
    ) -> Result<(), EditError> {
        let tree = self
            .trees
            .iter()
            .find(|t| t.id == tree_id)
            .ok_or_else(|| EditError::TreeNotFound(tree_id.to_owned()))?;
        tree.missions
            .iter()
            .find(|m| m.id == mission_id)
            .ok_or_else(|| EditError::MissionNotFound(mission_id.to_owned()))?;
        if required.iter().any(|r| r == mission_id) {
            return Err(EditError::SelfDependency(mission_id.to_owned()));
        }
        for required_id in &required {
            self.check_edge_placement(mission_id, required_id)?;
        }
        let tree = self
            .trees
            .iter_mut()
            .find(|t| t.id == tree_id)
            .ok_or_else(|| EditError::TreeNotFound(tree_id.to_owned()))?;
        let mission = tree
            .missions
            .iter_mut()
            .find(|m| m.id == mission_id)
            .ok_or_else(|| EditError::MissionNotFound(mission_id.to_owned()))?;
        let mut seen = HashSet::new();
        mission.required = required
            .into_iter()
            .filter(|r| seen.insert(r.clone()))
            .collect();
        Ok(())
    }

    // --- Scalar fields ----------------------------------------------------

    /// Sets a mission's `position` (game column, 1-based).
    pub fn set_mission_position(
        &mut self,
        tree_id: &str,
        mission_id: &str,
        position: Option<u32>,
    ) -> Result<(), EditError> {
        let mission = self.mission_mut(tree_id, mission_id)?;
        mission.position = position;
        Ok(())
    }

    pub fn set_mission_icon(
        &mut self,
        tree_id: &str,
        mission_id: &str,
        icon: Option<String>,
    ) -> Result<(), EditError> {
        let mission = self.mission_mut(tree_id, mission_id)?;
        mission.icon = icon.filter(|s| !s.trim().is_empty());
        Ok(())
    }

    pub fn set_mission_completed_by(
        &mut self,
        tree_id: &str,
        mission_id: &str,
        completed_by: Option<String>,
    ) -> Result<(), EditError> {
        let mission = self.mission_mut(tree_id, mission_id)?;
        mission.completed_by = completed_by.filter(|s| !s.trim().is_empty());
        Ok(())
    }

    /// Replaces a mission's opaque block (`trigger` or `effect`).
    pub fn set_mission_block(
        &mut self,
        tree_id: &str,
        mission_id: &str,
        field: BlockField,
        text: Option<String>,
    ) -> Result<(), EditError> {
        let mission = self.mission_mut(tree_id, mission_id)?;
        let block = text
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
            .map(Block::new);
        match field {
            BlockField::Trigger => mission.trigger = block,
            BlockField::Effect => mission.effect = block,
        }
        Ok(())
    }

    /// Replaces a tree's opaque block (`potential` or `potential_on_load`).
    pub fn set_tree_block(
        &mut self,
        tree_id: &str,
        field: TreeBlockField,
        text: Option<String>,
    ) -> Result<(), EditError> {
        let tree = self
            .trees
            .iter_mut()
            .find(|t| t.id == tree_id)
            .ok_or_else(|| EditError::TreeNotFound(tree_id.to_owned()))?;
        let block = text
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
            .map(Block::new);
        match field {
            TreeBlockField::Potential => tree.potential = block,
            TreeBlockField::PotentialOnLoad => tree.potential_on_load = block,
        }
        Ok(())
    }

    pub fn set_tree_slot(&mut self, tree_id: &str, slot: u32) -> Result<(), EditError> {
        let tree = self.tree_mut(tree_id)?;
        tree.slot = slot;
        Ok(())
    }

    pub fn set_tree_generic(&mut self, tree_id: &str, generic: bool) -> Result<(), EditError> {
        let tree = self.tree_mut(tree_id)?;
        tree.generic = generic;
        Ok(())
    }

    pub fn set_tree_ai(&mut self, tree_id: &str, ai: Option<bool>) -> Result<(), EditError> {
        let tree = self.tree_mut(tree_id)?;
        tree.ai = ai;
        Ok(())
    }

    pub fn set_tree_country_shield(
        &mut self,
        tree_id: &str,
        shield: Option<bool>,
    ) -> Result<(), EditError> {
        let tree = self.tree_mut(tree_id)?;
        tree.has_country_shield = shield;
        Ok(())
    }

    // --- Group membership -------------------------------------------------

    /// Moves a mission to another tree (group), preserving its id, fields and
    /// prerequisite list. References *to* the mission stay valid — the id
    /// still exists, just in a different block — so no reference cleanup runs.
    /// This is the block move behind a cross-column drag.
    pub fn move_mission(
        &mut self,
        tree_id: &str,
        mission_id: &str,
        target_tree_id: &str,
    ) -> Result<(), EditError> {
        if tree_id == target_tree_id {
            return Ok(());
        }
        let mission = {
            let tree = self
                .trees
                .iter_mut()
                .find(|t| t.id == tree_id)
                .ok_or_else(|| EditError::TreeNotFound(tree_id.to_owned()))?;
            let index = tree
                .missions
                .iter()
                .position(|m| m.id == mission_id)
                .ok_or_else(|| EditError::MissionNotFound(mission_id.to_owned()))?;
            tree.missions.remove(index)
        };
        let target = self
            .trees
            .iter_mut()
            .find(|t| t.id == target_tree_id)
            .ok_or_else(|| EditError::TreeNotFound(target_tree_id.to_owned()))?;
        if target.missions.iter().any(|m| m.id == mission.id) {
            return Err(EditError::DuplicateMissionId(mission.id.clone()));
        }
        target.missions.push(mission);
        Ok(())
    }

    // --- Helpers ----------------------------------------------------------

    /// Rejects an edge whose prerequisite is not placed legally (the row
    /// directly above, or directly above in the same column). Unknown
    /// prerequisites are allowed — they may reference another file's mission
    /// (legal EU4) and are flagged by validation instead.
    fn check_edge_placement(&self, mission_id: &str, required_id: &str) -> Result<(), EditError> {
        let locs = super::graph::effective_layout(self);
        let loc_of: std::collections::HashMap<&str, super::graph::MissionLoc> = locs
            .iter()
            .map(|loc| {
                let id = self.trees[loc.tree].missions[loc.mission].id.as_str();
                (id, *loc)
            })
            .collect();
        let (Some(&dependent), Some(&prereq)) = (loc_of.get(mission_id), loc_of.get(required_id))
        else {
            return Ok(()); // Unknown target: resolved by validation instead.
        };
        if super::graph::is_spatially_legal(prereq, dependent) {
            Ok(())
        } else {
            Err(EditError::IllegalEdgePlacement {
                mission: mission_id.to_owned(),
                required: required_id.to_owned(),
            })
        }
    }

    fn tree_mut(&mut self, tree_id: &str) -> Result<&mut MissionTree, EditError> {
        self.trees
            .iter_mut()
            .find(|t| t.id == tree_id)
            .ok_or_else(|| EditError::TreeNotFound(tree_id.to_owned()))
    }

    fn mission_mut(&mut self, tree_id: &str, mission_id: &str) -> Result<&mut Mission, EditError> {
        self.tree_mut(tree_id)?
            .missions
            .iter_mut()
            .find(|m| m.id == mission_id)
            .ok_or_else(|| EditError::MissionNotFound(mission_id.to_owned()))
    }
}

/// Opaque mission block fields.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockField {
    Trigger,
    Effect,
}

/// Opaque tree block fields.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TreeBlockField {
    Potential,
    PotentialOnLoad,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eu4::mission::load::parse_file;
    use pdx_text::TextRange;

    fn empty_file() -> MissionFile {
        MissionFile { trees: Vec::new() }
    }

    fn sample_tree() -> MissionFile {
        let mut file = empty_file();
        file.add_tree("t1").unwrap();
        file.add_mission("t1", "a").unwrap();
        file.add_mission("t1", "b").unwrap();
        file.add_required("t1", "b", "a").unwrap();
        file
    }

    fn span_of(file: &MissionFile, tree_id: &str) -> TextRange {
        file.trees.iter().find(|t| t.id == tree_id).unwrap().span
    }

    #[test]
    fn add_and_remove_tree() {
        let mut file = empty_file();
        file.add_tree("x").unwrap();
        assert_eq!(file.trees.len(), 1);
        assert_eq!(file.trees[0].slot, 1);
        assert!(file.add_tree("x").is_err());
        file.remove_tree("x").unwrap();
        assert!(file.trees.is_empty());
        assert!(file.remove_tree("x").is_err());
    }

    #[test]
    fn remove_mission_cleans_references() {
        let mut file = sample_tree();
        file.remove_mission("t1", "a").unwrap();
        assert!(file.trees[0].missions.iter().all(|m| m.required.is_empty()));
        assert!(file.remove_mission("t1", "ghost").is_err());
    }

    #[test]
    fn rename_mission_updates_references_file_wide() {
        let mut file = sample_tree();
        file.add_tree("t2").unwrap();
        file.add_mission("t2", "c").unwrap();
        // c sits one row below a (same column) so the edge is legal.
        file.set_mission_position("t2", "c", Some(2)).unwrap();
        file.add_required("t2", "c", "a").unwrap();
        file.rename_mission("t1", "a", "a2").unwrap();
        assert_eq!(file.trees[0].missions[1].required, vec!["a2"]);
        assert_eq!(file.trees[1].missions[0].required, vec!["a2"]);
        assert!(file.rename_mission("t1", "a", "b").is_err());
        assert!(file.rename_mission("t1", "ghost", "z").is_err());
    }

    #[test]
    fn dependencies_reject_self_and_duplicates() {
        let mut file = sample_tree();
        assert!(file.add_required("t1", "b", "b").is_err());
        file.add_required("t1", "b", "a").unwrap();
        file.add_required("t1", "b", "a").unwrap();
        assert_eq!(file.trees[0].missions[1].required, vec!["a"]);
        file.remove_required("t1", "b", "a").unwrap();
        assert!(file.trees[0].missions[1].required.is_empty());
        file.set_required("t1", "b", vec!["a".into(), "a".into()])
            .unwrap();
        assert_eq!(file.trees[0].missions[1].required, vec!["a"]);
        assert!(file.set_required("t1", "b", vec!["b".into()]).is_err());
    }

    #[test]
    fn dependencies_reject_spatially_illegal_edges() {
        let mut file = empty_file();
        file.add_tree("t1").unwrap();
        file.add_mission("t1", "a").unwrap();
        file.add_mission("t1", "b").unwrap();
        // a below b -> illegal (prerequisite must sit above).
        file.set_mission_position("t1", "a", Some(2)).unwrap();
        file.set_mission_position("t1", "b", Some(1)).unwrap();
        assert!(matches!(
            file.add_required("t1", "b", "a"),
            Err(EditError::IllegalEdgePlacement { .. })
        ));
        assert!(file.trees[0].missions[1].required.is_empty());
        // a on the row directly above b -> legal.
        file.set_mission_position("t1", "a", Some(1)).unwrap();
        file.set_mission_position("t1", "b", Some(2)).unwrap();
        file.add_required("t1", "b", "a").unwrap();
        // Cross-column edges need the row directly above.
        file.add_tree("t2").unwrap();
        file.set_tree_slot("t2", 2).unwrap();
        file.add_mission("t2", "c").unwrap();
        file.set_mission_position("t2", "c", Some(2)).unwrap();
        file.add_required("t2", "c", "a").unwrap();
        // Same row, cross column: illegal.
        file.set_mission_position("t2", "c", Some(1)).unwrap();
        file.remove_required("t2", "c", "a").unwrap();
        assert!(matches!(
            file.add_required("t2", "c", "a"),
            Err(EditError::IllegalEdgePlacement { .. })
        ));
        // Unknown ids are allowed (they may be cross-file refs).
        file.add_required("t1", "b", "elsewhere").unwrap();
        assert_eq!(file.trees[0].missions[1].required, vec!["a", "elsewhere"]);
    }

    #[test]
    fn move_mission_moves_block_without_cleaning_references() {
        let mut file = sample_tree();
        file.add_tree("t2").unwrap();
        file.set_tree_slot("t2", 2).unwrap();
        // b requires a; x in t2 also requires a. Moving a must not touch any
        // reference to it (the id still exists in the file).
        file.add_mission("t2", "x").unwrap();
        file.set_mission_position("t2", "x", Some(2)).unwrap();
        file.add_required("t2", "x", "a").unwrap();
        file.move_mission("t1", "a", "t2").unwrap();
        assert!(file.trees[0].mission("a").is_none());
        assert!(file.trees[1].mission("a").is_some());
        // b (t1) and x (t2) still reference a — the id still exists.
        assert_eq!(file.trees[0].missions[0].required, vec!["a"]);
        assert_eq!(file.trees[1].missions[0].required, vec!["a"]);
        // Moving to the same tree is a no-op; unknown ids error.
        file.move_mission("t2", "a", "t2").unwrap();
        assert!(file.move_mission("t2", "ghost", "t1").is_err());
        assert!(file.move_mission("t2", "a", "ghost").is_err());
    }

    #[test]
    fn scalar_and_block_fields() {
        let mut file = sample_tree();
        file.set_mission_icon("t1", "a", Some("mission_x".into()))
            .unwrap();
        file.set_mission_position("t1", "a", Some(3)).unwrap();
        file.set_mission_completed_by("t1", "a", Some("1500.1.1".into()))
            .unwrap();
        file.set_mission_block(
            "t1",
            "a",
            BlockField::Trigger,
            Some("{\n\talways = yes\n}".into()),
        )
        .unwrap();
        file.set_tree_slot("t1", 4).unwrap();
        file.set_tree_generic("t1", true).unwrap();
        file.set_tree_ai("t1", Some(false)).unwrap();
        file.set_tree_block("t1", TreeBlockField::Potential, Some("{ tag = T1 }".into()))
            .unwrap();

        let mission = &file.trees[0].missions[0];
        assert_eq!(mission.icon.as_deref(), Some("mission_x"));
        assert_eq!(mission.position, Some(3));
        assert_eq!(mission.completed_by.as_deref(), Some("1500.1.1"));
        assert_eq!(
            mission.trigger.as_ref().unwrap().text,
            "{\n\talways = yes\n}"
        );
        assert!(mission.effect.is_none());
        let tree = &file.trees[0];
        assert_eq!(tree.slot, 4);
        assert!(tree.generic);
        assert_eq!(tree.ai, Some(false));
        assert_eq!(tree.potential.as_ref().unwrap().text, "{ tag = T1 }");

        // Empty text clears the block.
        file.set_mission_block("t1", "a", BlockField::Trigger, None)
            .unwrap();
        assert!(file.trees[0].missions[0].trigger.is_none());
    }

    #[test]
    fn edited_file_round_trips_through_writer() {
        // The full pipeline: load fixture -> edit -> render -> parse -> compare.
        let fixture = include_str!("../../../tests/fixtures/sample_missions.txt");
        let loaded = parse_file(fixture);
        let mut file = loaded.file.clone();
        let style = crate::eu4::mission::write::detect_style(fixture);

        file.add_mission("sam_main_tree", "sam_brand_new").unwrap();
        file.add_required("sam_main_tree", "sam_brand_new", "sam_first_mission")
            .unwrap();
        file.remove_mission("sam_main_tree", "sam_second_mission")
            .unwrap();
        file.rename_mission("sam_main_tree", "sam_first_mission", "sam_first_renamed")
            .unwrap();

        let tree = file.trees.iter().find(|t| t.id == "sam_main_tree").unwrap();
        let rendered = crate::eu4::mission::write::render_tree(tree, &style);
        let reparsed = parse_file(&rendered).file;
        assert_eq!(reparsed.trees.len(), 1);

        let edited = &reparsed.trees[0];
        let ids: Vec<&str> = edited.missions.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["sam_first_renamed", "sam_brand_new"]);
        let brand_new = edited.mission("sam_brand_new").unwrap();
        assert_eq!(brand_new.required, vec!["sam_first_renamed"]);
        // The removed mission must be gone and un-referenced.
        assert!(edited.mission("sam_second_mission").is_none());
        assert!(
            edited
                .missions
                .iter()
                .all(|m| !m.required.iter().any(|r| r == "sam_second_mission"))
        );
        let _ = span_of(&file, "sam_main_tree");
    }
}
