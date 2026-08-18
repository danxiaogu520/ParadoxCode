//! World-space mission-tree geometry shared by every renderer.
//!
//! The grid is a literal mapping of the file's coordinates onto a canvas —
//! **X = `slot` - 1, Y = `position` - 1** — exactly what the game renders, so
//! `what you see is exactly what the file says`. All units are world pixels;
//! a renderer applies its own pan/zoom transform on top.
//!
//! Dependency arrows replicate `EMT.MissionTreeView.DrawArrows` (the sprite
//! runs assembled by the editor): vertical runs inside a slot column,
//! horizontal runs across columns, with skip tiles for multi-row / multi-slot
//! jumps. The geometry is texture-agnostic: it emits [`ArrowGlyph`] kinds and
//! offsets, and the renderer maps kinds to its own sprites or drawings.

use crate::model::MissionFile;

/// Canvas node size and spacing in world pixels. The node is the in-game
/// mission frame texture (103x123) with EMT's logical 104x122 box; columns sit
/// flush next to each other (EMT `spaceHorizontal = 0`) and rows are spaced by
/// 30px (EMT `spaceVertical = 30`), exactly like the game UI.
pub const NODE_WIDTH: f32 = 104.0;
pub const NODE_HEIGHT: f32 = 122.0;
pub const GAP_X: f32 = 0.0;
pub const GAP_Y: f32 = 30.0;
/// Origin offset of the grid inside the canvas (a small margin around the
/// EMT origin).
pub const ORIGIN: (f32, f32) = (16.0, 56.0);

/// Grid position of one mission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodePosition {
    /// Index into `MissionFile::trees`.
    pub tree_index: usize,
    /// Index into `MissionTree::missions`.
    pub mission_index: usize,
    /// 0-based slot column (`slot` - 1, sparse slots leave empty columns).
    pub column: u32,
    /// 0-based row (`position` - 1; missions without a `position` follow the
    /// file order).
    pub row: u32,
}

/// Computes grid positions for all missions of `file`: the literal
/// `x = slot - 1, y = position - 1` mapping. Missions without a `position`
/// take the previous mission's `position` + 1 (starting at 1), which is the
/// game's own convention for position-less trees like `00_Generic`.
#[must_use]
pub fn layout_file(file: &MissionFile) -> Vec<NodePosition> {
    let mut positions = Vec::new();
    for (tree_index, tree) in file.trees.iter().enumerate() {
        if tree.missions.is_empty() {
            continue;
        }
        let column = tree.slot.saturating_sub(1);
        let mut next_row = 1u32;
        for (mission_index, mission) in tree.missions.iter().enumerate() {
            let row = mission.position.unwrap_or(next_row);
            next_row = row + 1;
            positions.push(NodePosition {
                tree_index,
                mission_index,
                column,
                row: row.saturating_sub(1),
            });
        }
    }
    positions
}

/// World-space top-left corner of a grid cell.
#[must_use]
pub fn world_position(pos: &NodePosition) -> (f32, f32) {
    world_position_at(pos.column, pos.row)
}

/// World-space top-left corner of the cell at `column`/`row`.
#[must_use]
pub fn world_position_at(column: u32, row: u32) -> (f32, f32) {
    (
        ORIGIN.0 + column as f32 * (NODE_WIDTH + GAP_X),
        ORIGIN.1 + row as f32 * (NODE_HEIGHT + GAP_Y),
    )
}

/// One arrow texture glyph, as placed by the EMT-compatible geometry.
///
/// The variants mirror the game's arrow textures (`countrymissionsview.gfx`);
/// renderers map each kind to their own sprite table or drawing primitive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArrowGlyph {
    /// Repeating vertical tile of a same-column run.
    VerticalTile,
    /// Repeating tile down the gap for multi-row jumps.
    VerticalSkipTier,
    /// Repeating tile across the gap for multi-slot runs.
    HorizontalSkipSlot,
    /// Arrow head exiting the prerequisite's left edge.
    LeftOut,
    /// Arrow head entering the dependent's left edge.
    LeftIn,
    /// Arrow head exiting the prerequisite's right edge.
    RightOut,
    /// Arrow head entering the dependent's right edge.
    RightIn,
    /// Final arrow end marker at the dependent's row.
    End,
}

/// One arrow texture placement, in world coordinates (EMT's `AddIcon` calls).
#[derive(Clone, Debug, PartialEq)]
pub struct ArrowSegment {
    pub glyph: ArrowGlyph,
    pub x: f32,
    pub y: f32,
}

/// Computes the arrow texture placements for all dependencies, replicating
/// `EMT.MissionTreeView.DrawArrows`: vertical arrows inside a slot column and
/// horizontal arrow runs across slot columns. All coordinates are world-space.
#[must_use]
pub fn arrow_geometry(file: &MissionFile, layout: &[NodePosition]) -> Vec<ArrowSegment> {
    let mut segments = Vec::new();
    // World constants mirror EMT's layout math.
    let w = NODE_WIDTH;
    let h = NODE_HEIGHT;
    let row_step = h + GAP_Y;

    for pos in layout {
        let tree = &file.trees[pos.tree_index];
        let mission = &tree.missions[pos.mission_index];
        for required in &mission.required {
            let Some(req_pos) = layout
                .iter()
                .find(|p| file.trees[p.tree_index].missions[p.mission_index].id == *required)
            else {
                continue;
            };
            let (src_x, src_y) = world_position(req_pos);
            let h_diff = pos.column as i32 - req_pos.column as i32;
            // Literal rows can put a dependent above its prerequisite (the
            // file says so, e.g. positions written out of order); the arrow
            // sprites cannot bend upward, so clamp the run to the
            // prerequisite's row boundary like an adjacent-row arrow instead
            // of emitting tiles far above the source.
            let v_diff = (pos.row as i32 - req_pos.row as i32).max(1);

            if h_diff == 0 {
                // Same column: vertical run from the prerequisite's bottom edge.
                segments.push(ArrowSegment {
                    glyph: ArrowGlyph::VerticalTile,
                    x: src_x + 46.0,
                    y: src_y + h - 1.0,
                });
                for i in 0..v_diff.saturating_sub(1) {
                    segments.push(ArrowSegment {
                        glyph: ArrowGlyph::VerticalSkipTier,
                        x: src_x + 46.0,
                        y: src_y + h - 1.0 + i as f32 * row_step,
                    });
                }
                if v_diff > 1 {
                    segments.push(ArrowSegment {
                        glyph: ArrowGlyph::VerticalTile,
                        x: src_x + 46.0,
                        y: src_y + h - 1.0 + (v_diff - 1) as f32 * row_step,
                    });
                }
                segments.push(ArrowSegment {
                    glyph: ArrowGlyph::End,
                    x: src_x + 38.0,
                    y: src_y + h + 19.0 + (v_diff - 1) as f32 * row_step,
                });
            } else if h_diff > 0 {
                // Prerequisite left of the dependent: arrow exits right.
                segments.push(ArrowSegment {
                    glyph: ArrowGlyph::RightOut,
                    x: src_x + 60.0,
                    y: src_y + h,
                });
                for i in 0..h_diff.saturating_sub(1) {
                    segments.push(ArrowSegment {
                        glyph: ArrowGlyph::HorizontalSkipSlot,
                        x: src_x + (i + 1) as f32 * w - 6.0,
                        y: src_y + h + 5.0,
                    });
                }
                segments.push(ArrowSegment {
                    glyph: ArrowGlyph::RightIn,
                    x: src_x + w * h_diff as f32 - 5.0,
                    y: src_y + h + 5.0 + (v_diff - 1) as f32 * row_step,
                });
                segments.push(ArrowSegment {
                    glyph: ArrowGlyph::End,
                    x: src_x + 15.0 + w * h_diff as f32,
                    y: src_y + h + 19.0 + (v_diff - 1) as f32 * row_step,
                });
            } else {
                // Prerequisite right of the dependent: arrow exits left.
                segments.push(ArrowSegment {
                    glyph: ArrowGlyph::LeftOut,
                    x: src_x + 4.0,
                    y: src_y + h,
                });
                let mut i = 0i32;
                while i > h_diff + 1 {
                    segments.push(ArrowSegment {
                        glyph: ArrowGlyph::HorizontalSkipSlot,
                        x: src_x + (i - 1) as f32 * w - 6.0,
                        y: src_y + h + 5.0,
                    });
                    i -= 1;
                }
                segments.push(ArrowSegment {
                    glyph: ArrowGlyph::LeftIn,
                    x: src_x + w * h_diff as f32 + 69.0,
                    y: src_y + h + 3.0 + (v_diff - 1) as f32 * row_step,
                });
                segments.push(ArrowSegment {
                    glyph: ArrowGlyph::End,
                    x: src_x + 61.0 + w * h_diff as f32,
                    y: src_y + h + 19.0 + (v_diff - 1) as f32 * row_step,
                });
            }
        }
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Mission, MissionTree};
    use crate::parse_file;
    use pdx_text::TextRange;
    use std::collections::HashMap;

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

    #[test]
    fn columns_follow_tree_slot() {
        // Columns are the raw 1-based slot value (slot 1 leftmost); sparse
        // slots leave empty columns, exactly like the game and EMT.
        let file = MissionFile {
            trees: vec![
                tree("a", 2, vec![mission("a1", &[], Some(1))]),
                tree("b", 1, vec![mission("b1", &[], Some(1))]),
                tree("c", 4, vec![mission("c1", &[], Some(1))]),
            ],
        };
        let layout = layout_file(&file);
        let by_id: HashMap<&str, &NodePosition> = layout
            .iter()
            .map(|p| {
                (
                    file.trees[p.tree_index].missions[p.mission_index]
                        .id
                        .as_str(),
                    p,
                )
            })
            .collect();
        assert_eq!(by_id["b1"].column, 0);
        assert_eq!(by_id["a1"].column, 1);
        assert_eq!(
            by_id["c1"].column, 3,
            "slot 4 keeps its raw column; slot 3 stays empty"
        );
    }

    #[test]
    fn rows_follow_position_literally() {
        // The canvas row is exactly `position - 1`; prerequisites do NOT
        // rearrange the tree (no EMT-style real-position recalculation).
        let file = MissionFile {
            trees: vec![tree(
                "t",
                1,
                vec![
                    mission("root", &[], Some(1)),
                    mission("mid", &["root"], Some(1)),
                    mission("leaf", &["mid"], Some(1)),
                    mission("sibling", &["root"], Some(2)),
                ],
            )],
        };
        let layout = layout_file(&file);
        let by_id: HashMap<&str, &NodePosition> = layout
            .iter()
            .map(|p| {
                (
                    file.trees[p.tree_index].missions[p.mission_index]
                        .id
                        .as_str(),
                    p,
                )
            })
            .collect();
        assert_eq!(by_id["root"].row, 0);
        assert_eq!(by_id["mid"].row, 0); // position 1, even with a prereq
        assert_eq!(by_id["leaf"].row, 0); // position 1, even with a prereq
        assert_eq!(by_id["sibling"].row, 1); // position 2
    }

    #[test]
    fn own_position_lifts_above_prerequisites() {
        // A mission with position 4 stays at row 3 even with no prereqs.
        let file = MissionFile {
            trees: vec![tree(
                "t",
                1,
                vec![mission("a", &[], Some(1)), mission("b", &[], Some(4))],
            )],
        };
        let layout = layout_file(&file);
        let by_id: HashMap<&str, &NodePosition> = layout
            .iter()
            .map(|p| {
                (
                    file.trees[p.tree_index].missions[p.mission_index]
                        .id
                        .as_str(),
                    p,
                )
            })
            .collect();
        assert_eq!(by_id["a"].row, 0);
        assert_eq!(by_id["b"].row, 3); // position 4 -> row 3, gap preserved
    }

    #[test]
    fn cross_tree_prerequisites_do_not_affect_rows() {
        // Branching missions: the dependent keeps its written position even
        // when its prerequisite lives in another tree.
        let file = MissionFile {
            trees: vec![
                tree("main", 1, vec![mission("eng_mighty_army", &[], Some(1))]),
                tree(
                    "branch",
                    2,
                    vec![mission("conquer_ireland", &["eng_mighty_army"], Some(2))],
                ),
            ],
        };
        let layout = layout_file(&file);
        let branch = layout
            .iter()
            .find(|p| file.trees[p.tree_index].missions[p.mission_index].id == "conquer_ireland")
            .unwrap();
        assert_eq!(branch.row, 1); // position 2 -> row 1, literally
        assert_eq!(branch.column, 1);
    }

    #[test]
    fn shared_slot_trees_overlap() {
        // Trees sharing a slot (conditional alternates) are drawn at the same
        // column and simply overlap — the canvas shows the file as it is.
        let file = MissionFile {
            trees: vec![
                tree("alt1", 1, vec![mission("a", &[], Some(1))]),
                tree("alt2", 1, vec![mission("b", &[], Some(1))]),
            ],
        };
        let layout = layout_file(&file);
        let a = layout
            .iter()
            .find(|p| file.trees[p.tree_index].missions[p.mission_index].id == "a")
            .unwrap();
        let b = layout
            .iter()
            .find(|p| file.trees[p.tree_index].missions[p.mission_index].id == "b")
            .unwrap();
        assert_eq!(a.column, b.column);
        assert_eq!(a.row, b.row);
    }

    #[test]
    fn cycles_do_not_hang() {
        let file = MissionFile {
            trees: vec![tree(
                "t",
                1,
                vec![mission("a", &["b"], None), mission("b", &["a"], None)],
            )],
        };
        let layout = layout_file(&file);
        assert_eq!(layout.len(), 2);
    }

    #[test]
    fn missions_without_position_form_compact_chains() {
        // 00_Generic style: no positions at all.
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
        let layout = layout_file(&file);
        let by_id: HashMap<&str, &NodePosition> = layout
            .iter()
            .map(|p| {
                (
                    file.trees[p.tree_index].missions[p.mission_index]
                        .id
                        .as_str(),
                    p,
                )
            })
            .collect();
        assert_eq!(by_id["m1"].row, 0);
        assert_eq!(by_id["m2"].row, 1);
        assert_eq!(by_id["m3"].row, 2);
    }

    /// Arrow texture placements must follow the exact EMT `DrawArrows` anchor
    /// math (game coordinates + ORIGIN): vertical runs inside a slot column,
    /// horizontal runs across columns, left/right mirrors, and skip-tier tiles
    /// for multi-row jumps.
    #[test]
    fn arrow_geometry_follows_emt_anchors() {
        let source = r#"
arrow_a_tree = {
    slot = 1
    a1 = { position = 1 }
    a2 = { position = 2 required_missions = { a1 } }
    a4 = { position = 4 required_missions = { c1 } }
    a3 = { position = 5 required_missions = { a1 } }
}
arrow_b_tree = {
    slot = 2
    b1 = { position = 1 required_missions = { a1 } }
}
arrow_c_tree = {
    slot = 3
    c1 = { position = 1 required_missions = { b1 } }
}
"#;
        let file = parse_file(source).file;
        let layout = layout_file(&file);
        let segments = arrow_geometry(&file, &layout);

        let find = |glyph: ArrowGlyph| {
            segments
                .iter()
                .filter(|s| s.glyph == glyph)
                .map(|s| (s.x, s.y))
                .collect::<Vec<_>>()
        };

        // a1 (16,56) -> a2 (16,208): same column, one row down. The
        // a1 -> a3 run (4 rows) repeats the head tile, then skip-tier tiles
        // down the gap, then one closing tile — exactly like EMT `DrawArrows`.
        assert_eq!(
            find(ArrowGlyph::VerticalTile),
            vec![(62.0, 177.0), (62.0, 177.0), (62.0, 633.0)],
            "vertical tiles at src.x+46 / src.y+121"
        );
        assert_eq!(
            find(ArrowGlyph::VerticalSkipTier),
            vec![(62.0, 177.0), (62.0, 329.0), (62.0, 481.0)],
            "skip-tier tiles tile down the gap between rows"
        );
        // Ends in segment order: a1 -> a2, c1 -> a4, a1 -> a3, a1 -> b1,
        // b1 -> c1 (trees/missions are visited in file order). Rows are the
        // literal `position - 1`, so b1/c1 sit on row 0 and the horizontal
        // runs clamp to the source row like adjacent-row arrows.
        assert_eq!(
            find(ArrowGlyph::End),
            vec![
                (54.0, 197.0),
                (77.0, 501.0),
                (54.0, 653.0),
                (135.0, 197.0),
                (239.0, 197.0)
            ],
            "arrow ends at src.x+38 / src.y+141 (vertical) or +15/+61 + w*hdiff (horizontal)"
        );
        // a1 -> b1: right out at src.x+60, right in at src.x+w-5, both on
        // row 0 (b1's written position is 1, same as a1's).
        assert_eq!(
            find(ArrowGlyph::RightOut),
            vec![(76.0, 178.0), (180.0, 178.0)]
        );
        assert_eq!(
            find(ArrowGlyph::RightIn),
            vec![(115.0, 183.0), (219.0, 183.0)]
        );
        // c1 -> a4: left mirror (left out at src.x+4, skip slots, left in at
        // src.x + w*hdiff + 69); c1 is on row 0, a4 on row 3.
        assert_eq!(find(ArrowGlyph::LeftOut), vec![(228.0, 178.0)]);
        assert_eq!(find(ArrowGlyph::HorizontalSkipSlot), vec![(114.0, 183.0)]);
        assert_eq!(find(ArrowGlyph::LeftIn), vec![(85.0, 485.0)]);
    }

    /// Literal rows can put a dependent above its prerequisite (the file
    /// says so); the arrow sprites cannot bend upward, so the run clamps to
    /// the prerequisite's row boundary instead of emitting tiles far above
    /// the source.
    #[test]
    fn cross_column_arrows_clamp_inverted_rows() {
        let source = r#"
arrow_a_tree = {
    slot = 1
    a1 = { position = 1 }
    low = { position = 4 }
}
arrow_b_tree = {
    slot = 2
    b1 = { position = 1 required_missions = { low } }
}
"#;
        let file = parse_file(source).file;
        let layout = layout_file(&file);
        let segments = arrow_geometry(&file, &layout);

        let find = |glyph: ArrowGlyph| {
            segments
                .iter()
                .filter(|s| s.glyph == glyph)
                .map(|s| (s.x, s.y))
                .collect::<Vec<_>>()
        };

        // low sits at row 3 (position 4); b1 at row 0 depends on it, so the
        // raw row difference is -3. The unclamped formula would put the
        // in/end tiles at y = 183/197, far above the source; the clamped run
        // stays at low's row boundary (y 634/639/653).
        assert_eq!(find(ArrowGlyph::RightOut), vec![(76.0, 634.0)]);
        assert_eq!(find(ArrowGlyph::RightIn), vec![(115.0, 639.0)]);
        assert_eq!(find(ArrowGlyph::End), vec![(135.0, 653.0)]);
        assert!(find(ArrowGlyph::HorizontalSkipSlot).is_empty());
    }

    /// An empty file produces no arrow placements at all.
    #[test]
    fn arrow_geometry_empty_file_is_empty() {
        let file = MissionFile { trees: Vec::new() };
        assert!(arrow_geometry(&file, &[]).is_empty());
    }
}
