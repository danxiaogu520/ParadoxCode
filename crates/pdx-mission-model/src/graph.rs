//! Flat "tasks + edges" view of a mission file.
//!
//! This module is the concept layer of the reframed editor: users work with
//! missions (nodes) and dependency edges, never with trees. Trees are an
//! implementation detail of the file format, so every operation here either
//! works on missions/edges directly or derives tree membership from the flat
//! view. The tree model in [`crate::model`] is untouched; this module only
//! reads it.
//!
//! Two concepts are central:
//!
//! - **Effective layout**: every mission has an effective position — the
//!   written `position`, or the file-order imputation (previous mission's
//!   `position` + 1, the game's own convention for position-less trees such
//!   as `00_Generic`). All spatial rules evaluate on effective positions.
//! - **Spatial legality** of an edge A→B (A is a prerequisite of B):
//!   legal iff `position_A == position_B − 1` (the row directly above, any
//!   column) or `slot_A == slot_B && position_A < position_B` (directly above
//!   in the same column, skipping rows). This is exactly the set of layouts
//!   the game can render cleanly; anything else is rejected at creation and
//!   flagged with a warning when loaded from an existing file.
//!
//! A corollary: positions strictly decrease along legal edges, so cycles and
//! self-loops are geometrically impossible for created edges.

use std::collections::HashMap;

use crate::model::MissionFile;

/// Effective grid location of one mission in a file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MissionLoc {
    /// Index into `MissionFile::trees`.
    pub tree: usize,
    /// Index into `MissionTree::missions`.
    pub mission: usize,
    /// Tree slot (column, 1-based).
    pub slot: u32,
    /// Effective position (row, 1-based): written `position`, or the
    /// file-order imputation (`previous mission's position + 1`).
    pub position: u32,
}

/// Computes the effective layout of every mission in `file`.
///
/// Mirrors the editor's literal grid mapping (`X = slot - 1`, `Y = position - 1`);
/// missions without a written `position` take the previous mission's
/// `position + 1` within their tree, starting at 1.
#[must_use]
pub fn effective_layout(file: &MissionFile) -> Vec<MissionLoc> {
    let mut locs = Vec::new();
    for (tree_index, tree) in file.trees.iter().enumerate() {
        if tree.missions.is_empty() {
            continue;
        }
        let mut next_position = 1u32;
        for (mission_index, mission) in tree.missions.iter().enumerate() {
            let position = mission.position.unwrap_or(next_position);
            next_position = position + 1;
            locs.push(MissionLoc {
                tree: tree_index,
                mission: mission_index,
                slot: tree.slot,
                position,
            });
        }
    }
    locs
}

/// Whether an edge `prereq → dependent` is spatially legal.
///
/// Legal iff the prerequisite sits on the row directly above the dependent
/// (any column) or directly above it in the same column (any distance).
#[must_use]
pub fn is_spatially_legal(prereq: MissionLoc, dependent: MissionLoc) -> bool {
    prereq.position == dependent.position.saturating_sub(1)
        || (prereq.slot == dependent.slot && prereq.position < dependent.position)
}

/// One dependency edge that violates the spatial legality rule.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpatialViolation {
    pub tree: usize,
    pub mission: usize,
    pub required: String,
}

/// Finds in-file dependency edges that violate the spatial legality rule.
///
/// Edges whose prerequisite is not in this file (cross-file references, which
/// are legal EU4) cannot be evaluated and are not reported.
#[must_use]
pub fn spatial_violations(file: &MissionFile) -> Vec<SpatialViolation> {
    let loc_of: HashMap<&str, MissionLoc> = effective_layout(file)
        .into_iter()
        .map(|loc| {
            let id = file.trees[loc.tree].missions[loc.mission].id.as_str();
            (id, loc)
        })
        .collect();
    let mut violations = Vec::new();
    for loc in effective_layout(file) {
        let mission = &file.trees[loc.tree].missions[loc.mission];
        for required in &mission.required {
            let Some(&prereq) = loc_of.get(required.as_str()) else {
                continue; // Not in this file; cross-file refs are legal.
            };
            if !is_spatially_legal(prereq, loc) {
                violations.push(SpatialViolation {
                    tree: loc.tree,
                    mission: loc.mission,
                    required: required.clone(),
                });
            }
        }
    }
    violations
}

/// Why a creation/move target cell cannot be used.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CreateError {
    /// The cell already holds a mission (same slot and effective position).
    OccupiedCell,
    /// The column holds more than one group, so a new mission's membership
    /// would be ambiguous. Only possible in files loaded from disk.
    MultipleGroups(u32),
}

/// Where a new mission created at `column`/`row` (0-based) belongs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CreateTarget {
    /// The column is empty: create a new group (tree) with `slot = column + 1`.
    NewGroup,
    /// Join the existing group at this tree index.
    JoinGroup(usize),
    Rejected(CreateError),
}

/// Resolves the group a new mission at `column`/`row` (0-based) belongs to.
///
/// The cell must be free; the column must contain exactly one group (empty
/// trees still count — a new mission fills the existing block instead of
/// creating a duplicate-slot group).
#[must_use]
pub fn creation_target(file: &MissionFile, column: u32, row: u32) -> CreateTarget {
    let column = column.saturating_add(1);
    let row = row.saturating_add(1);
    if effective_layout(file)
        .iter()
        .any(|loc| loc.slot == column && loc.position == row)
    {
        return CreateTarget::Rejected(CreateError::OccupiedCell);
    }
    match group_target(file, column - 1) {
        GroupTarget::Existing(tree) => CreateTarget::JoinGroup(tree),
        GroupTarget::NewGroup => CreateTarget::NewGroup,
        GroupTarget::Rejected(error) => CreateTarget::Rejected(error),
    }
}

/// The target group for a mission moved to `column` (0-based).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GroupTarget {
    /// The column is empty: a new group (tree) with `slot = column + 1`.
    NewGroup,
    /// The existing group at this tree index.
    Existing(usize),
    Rejected(CreateError),
}

/// Resolves the group a mission moved into `column` (0-based) would join.
/// Moving onto an occupied cell is allowed (the file may overlap), so only
/// the column's group count matters.
#[must_use]
pub fn group_target(file: &MissionFile, column: u32) -> GroupTarget {
    let slot = column.saturating_add(1);
    let mut trees: Vec<usize> = file
        .trees
        .iter()
        .enumerate()
        .filter(|(_, t)| t.slot == slot)
        .map(|(i, _)| i)
        .collect();
    match trees.len() {
        0 => GroupTarget::NewGroup,
        1 => GroupTarget::Existing(trees.remove(0)),
        _ => GroupTarget::Rejected(CreateError::MultipleGroups(column)),
    }
}

/// Indices of all missions sharing a group (tree).
#[must_use]
pub fn group_members(file: &MissionFile, tree_index: usize) -> Vec<usize> {
    file.trees
        .get(tree_index)
        .map(|t| (0..t.missions.len()).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Mission, MissionTree};
    use pdx_text::TextRange;

    fn tree(id: &str, slot: u32, missions: Vec<Mission>) -> MissionTree {
        MissionTree {
            id: id.into(),
            slot,
            generic: false,
            ai: None,
            has_country_shield: None,
            potential: None,
            potential_on_load: None,
            missions,
            unknown: Vec::new(),
            span: TextRange::empty(0),
        }
    }

    fn mission(id: &str, required: &[&str], position: Option<u32>) -> Mission {
        Mission {
            id: id.into(),
            icon: None,
            mission_type: None,
            provinces_to_highlight: None,
            required: required.iter().map(|s| s.to_string()).collect(),
            position,
            completed_by: None,
            trigger: None,
            effect: None,
            unknown: Vec::new(),
            span: TextRange::empty(0),
        }
    }

    fn locs(file: &MissionFile) -> HashMap<&str, MissionLoc> {
        effective_layout(file)
            .into_iter()
            .map(|loc| (file.trees[loc.tree].missions[loc.mission].id.as_str(), loc))
            .collect()
    }

    /// The Q7 legality matrix, exactly as agreed: legal iff the prerequisite
    /// is on the row directly above (any column) or directly above in the
    /// same column (any distance).
    #[test]
    fn spatial_legality_matrix() {
        // Case helper: (a_slot, a_pos, b_slot, b_pos) with a = prerequisite.
        let legal = |a_slot: u32, a_pos: u32, b_slot: u32, b_pos: u32| {
            let file = MissionFile {
                trees: vec![
                    tree("a", a_slot, vec![mission("a", &[], Some(a_pos))]),
                    tree("b", b_slot, vec![mission("b", &[], Some(b_pos))]),
                ],
            };
            let l = locs(&file);
            is_spatially_legal(l["a"], l["b"])
        };
        // 1: adjacent rows, same column -> legal.
        assert!(legal(1, 1, 1, 2));
        // 2: adjacent rows, different columns -> legal.
        assert!(legal(1, 1, 2, 2));
        // 3: two rows above, different column -> illegal.
        assert!(!legal(1, 1, 2, 3));
        // 4: same column, several rows above -> legal.
        assert!(legal(1, 1, 1, 4));
        // 5: prerequisite below dependent -> illegal.
        assert!(!legal(1, 4, 1, 1));
        // 6: same cell -> illegal.
        assert!(!legal(1, 2, 1, 2));
        // 7: same row, different column -> illegal.
        assert!(!legal(1, 1, 2, 1));
    }

    #[test]
    fn imputed_positions_follow_file_order() {
        // 00_Generic style: no positions at all; imputation is prev + 1.
        let file = MissionFile {
            trees: vec![tree(
                "t",
                1,
                vec![
                    mission("m1", &[], None),
                    mission("m2", &["m1"], None),
                    mission("m3", &["m2"], None),
                ],
            )],
        };
        let l = locs(&file);
        assert_eq!(l["m1"].position, 1);
        assert_eq!(l["m2"].position, 2);
        assert_eq!(l["m3"].position, 3);
        // Script-order chains are adjacent rows -> legal.
        assert!(is_spatially_legal(l["m1"], l["m2"]));
        assert!(is_spatially_legal(l["m2"], l["m3"]));
        assert!(is_spatially_legal(l["m1"], l["m3"])); // same column, above
    }

    #[test]
    fn spatial_violations_find_only_illegal_in_file_edges() {
        let file = MissionFile {
            trees: vec![
                tree(
                    "main",
                    1,
                    vec![
                        mission("root", &[], Some(1)),
                        mission("mid", &["root"], Some(2)),
                        mission("leaf", &["mid"], Some(3)),
                        // Same row as root, cross-tree: illegal.
                        mission("stray", &["root"], Some(1)),
                        // Not in this file: skipped (legal cross-file ref).
                        mission("foreign", &["elsewhere"], Some(2)),
                    ],
                ),
                tree(
                    "branch",
                    2,
                    vec![
                        // root (slot 1, pos 1) is directly above -> legal.
                        mission("dep", &["root"], Some(2)),
                        // root is not on the row above (pos 3 != 1) -> illegal.
                        mission("far", &["root"], Some(3)),
                    ],
                ),
            ],
        };
        let violations = spatial_violations(&file);
        let ids: Vec<(String, String)> = violations
            .iter()
            .map(|v| {
                let mission = &file.trees[v.tree].missions[v.mission];
                (mission.id.clone(), v.required.clone())
            })
            .collect();
        assert!(ids.contains(&("stray".into(), "root".into())), "{ids:?}");
        assert!(ids.contains(&("far".into(), "root".into())), "{ids:?}");
        assert!(
            ids.iter()
                .all(|(m, r)| !(m == "foreign" && r == "elsewhere")),
            "cross-file refs are not spatial violations: {ids:?}"
        );
        assert!(
            ids.iter()
                .all(|(m, _)| m != "mid" && m != "leaf" && m != "dep"),
            "{ids:?}"
        );
    }

    #[test]
    fn creation_target_follows_group_rules() {
        let file = MissionFile {
            trees: vec![
                tree("only", 1, vec![mission("a", &[], Some(1))]),
                tree("empty", 2, vec![]),
                tree("alt1", 3, vec![mission("x", &[], Some(1))]),
                tree("alt2", 3, vec![mission("y", &[], Some(2))]),
            ],
        };
        // Empty column -> new group.
        assert_eq!(creation_target(&file, 4, 0), CreateTarget::NewGroup);
        // Occupied cell -> rejected.
        assert_eq!(
            creation_target(&file, 0, 0),
            CreateTarget::Rejected(CreateError::OccupiedCell)
        );
        // Single-group column -> join (empty trees still count).
        assert_eq!(creation_target(&file, 1, 0), CreateTarget::JoinGroup(1));
        // Multi-group column -> rejected.
        assert_eq!(
            creation_target(&file, 2, 2),
            CreateTarget::Rejected(CreateError::MultipleGroups(2))
        );
        // group_target for moves: occupancy is irrelevant.
        assert_eq!(group_target(&file, 4), GroupTarget::NewGroup);
        assert_eq!(group_target(&file, 1), GroupTarget::Existing(1));
        assert_eq!(
            group_target(&file, 2),
            GroupTarget::Rejected(CreateError::MultipleGroups(2))
        );
    }
}
