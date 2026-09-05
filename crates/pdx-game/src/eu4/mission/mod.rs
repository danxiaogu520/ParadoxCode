//! Structured EU4 mission-tree model shared by the language server's mission
//! preview (`pdx-lsp` `pdx/missionPreview`) and future editing surfaces.
//!
//! This module owns the boundary between raw Paradox script text and the
//! structured mission model:
//!
//! - [`load`] extracts a [`MissionFile`] from a loss-aware [`pdx_parser::ParsedFile`];
//! - [`geometry`] computes the literal grid layout and EMT-compatible
//!   dependency-arrow placements any renderer consumes;
//! - [`mod@write`] renders a tree back to script text with a stable field order;
//! - [`validate()`] reports structural problems (duplicate ids, dangling or cyclic
//!   `required_missions` references).
//!
//! The model is deliberately EU4-shaped: top-level blocks are mission trees, mission
//! dependencies come from `required_missions`, and layout is driven by the game's
//! `slot` / `position` fields. GUI logic must not live here.

pub mod edit;
pub mod encoding;
pub mod geometry;
pub mod graph;
pub mod load;
pub mod model;
pub mod texture;
pub mod validate;
pub mod write;

pub use edit::{BlockField, EditError, TreeBlockField};
pub use encoding::{EncodingError, FileEncoding, decode_bytes, encode_text};
pub use geometry::{
    ArrowGlyph, ArrowSegment, NodePosition, arrow_geometry, layout_file, world_position,
    world_position_at,
};
pub use graph::{
    CreateError, CreateTarget, GroupTarget, MissionLoc, SpatialViolation, creation_target,
    effective_layout, group_members, group_target, is_spatially_legal, spatial_violations,
};
pub use load::{LoadedFile, parse_file};
pub use model::{Block, Mission, MissionFile, MissionTree, RawField};
pub use texture::{
    FRAME_SPRITE, TextureAssets, arrow_sprite_name, decode_dds, parse_gfx_sprites, png_data_url,
};
pub use validate::{Diagnostic, Severity, validate, validate_in, validate_with_universe_ids};
pub use write::{
    BlockSpacing, Indent, WriteStyle, apply_tree_edit, detect_style, render_mission_block,
    render_tree,
};
