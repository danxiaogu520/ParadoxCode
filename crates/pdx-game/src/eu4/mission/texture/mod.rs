//! Texture assets for the EMT-style mission-tree preview.
//!
//! This module turns EU4 interface textures (DDS files referenced by
//! `interface/*.gfx`) into `data:image/png;base64,` URLs a renderer can draw
//! directly. All decoding is pure; the only I/O happens in [`assets`].

pub mod assets;
pub mod dds;
pub mod gfx;
pub mod png;

pub use assets::{FRAME_SPRITE, TextureAssets, arrow_sprite_name};
pub use dds::{DdsError, DecodedImage, decode_dds};
pub use gfx::{SpriteEntry, build_sprite_index, parse_gfx_sprites};
pub use png::{base64_encode, encode_png, png_data_url};
