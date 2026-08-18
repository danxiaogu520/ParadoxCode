//! Game-resource texture loading for the mission preview.
//!
//! Loads the sprite index from the game's `interface/*.gfx` files, resolves
//! each sprite name to a DDS file under the game root, decodes it to RGBA8,
//! and serves it as a `data:image/png;base64,` URL. Results are cached per
//! sprite name and invalidated on mtime changes so repeated preview refreshes
//! stay cheap. Every failure degrades to `None`; previews must never fail
//! because a texture is missing.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::dds::decode_dds;
use super::gfx::build_sprite_index;
use super::png::png_data_url;

/// Maximum sprite file size this loader reads (guards hostile files).
const MAX_SPRITE_BYTES: u64 = 8 * 1024 * 1024;
/// Maximum number of sprites retained in memory per preview session.
const MAX_CACHED_SPRITES: usize = 512;

/// The sprite names the mission-tree renderer needs beyond node icons.
pub const FRAME_SPRITE: &str = "GFX_mission_icons_frame";

/// Mirror of the glyph-to-sprite mapping used by the arrow geometry.
pub fn arrow_sprite_name(glyph: &str) -> Option<&'static str> {
    Some(match glyph {
        "verticalTile" => "gfx_arrow_verticall_tile",
        "verticalSkipTier" => "gfx_arrow_verticall_skip_tier",
        "horizontalSkipSlot" => "gfx_arrow_horizontal_skip_slot",
        "leftOut" => "gfx_arrow_left_out",
        "leftIn" => "gfx_arrow_left_in",
        "rightOut" => "gfx_arrow_right_out",
        "rightIn" => "gfx_arrow_right_in",
        "end" => "gfx_arrow_end",
        _ => return None,
    })
}

/// Loads and caches game textures for one game installation.
///
/// Shared across snapshot requests; all mutation is interior (`Mutex`) so the
/// value can be cloned cheaply and used from concurrent workers.
#[derive(Debug)]
pub struct TextureAssets {
    sprite_paths: HashMap<String, PathBuf>,
    cache: Mutex<HashMap<String, Option<(u64, String)>>>,
}

impl TextureAssets {
    /// Builds the sprite index from every `interface/*.gfx` file under
    /// `game_dir`. Returns `None` when the game directory is unusable, so
    /// callers can fall back to a texture-less preview.
    pub fn load(game_dir: &Path) -> Option<Self> {
        let interface_dir = game_dir.join("interface");
        let mut files = vec![];
        let read_dir = std::fs::read_dir(&interface_dir).ok()?;
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("gfx")
                && entry.file_type().map_or(true, |kind| kind.is_file())
            {
                let source = fs::read_to_string(&path).ok()?;
                files.push(source);
            }
        }
        if files.is_empty() {
            return None;
        }
        let mut sprite_paths = HashMap::new();
        for entry in build_sprite_index(&files.iter().map(String::as_str).collect::<Vec<_>>()) {
            let path = game_dir.join(&entry.1);
            sprite_paths.insert(entry.0, path);
        }
        Some(Self {
            sprite_paths,
            cache: Mutex::new(HashMap::new()),
        })
    }

    /// Returns the data URL for a sprite name, decoding and caching on first
    /// use. `None` for unknown names, unreadable files, or decode failures.
    pub fn data_url(&self, name: &str) -> Option<String> {
        let path = self.sprite_paths.get(name)?;
        let mtime = fs::metadata(path).and_then(|meta| meta.modified()).ok()?;
        let mtime = mtime
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(cached) = cache.get(name) {
            if cached.as_ref().is_none_or(|(known, _)| *known == mtime) {
                return cached.as_ref().map(|(_, url)| url.clone());
            }
            cache.remove(name);
        }
        let url = self.decode_to_url(path).map(|url| (mtime, url));
        if cache.len() >= MAX_CACHED_SPRITES {
            cache.clear();
        }
        let value = url.clone();
        cache.insert(name.to_owned(), value);
        url.map(|(_, url)| url)
    }

    fn decode_to_url(&self, path: &Path) -> Option<String> {
        let metadata = fs::metadata(path).ok()?;
        if metadata.len() > MAX_SPRITE_BYTES {
            return None;
        }
        let bytes = fs::read(path).ok()?;
        decode_dds(&bytes).ok().map(|image| png_data_url(&image))
    }

    /// The set of sprite names this asset store knows about.
    pub fn sprite_names(&self) -> impl Iterator<Item = &str> {
        self.sprite_paths.keys().map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrow_sprite_names_cover_every_glyph() {
        for glyph in [
            "verticalTile",
            "verticalSkipTier",
            "horizontalSkipSlot",
            "leftOut",
            "leftIn",
            "rightOut",
            "rightIn",
            "end",
        ] {
            assert!(
                arrow_sprite_name(glyph).is_some(),
                "glyph {glyph} must map to a texture"
            );
        }
        assert_eq!(arrow_sprite_name("bogus"), None);
    }

    #[test]
    fn loads_index_and_round_trips_a_sprite() {
        let root = std::env::temp_dir().join(format!(
            "pdx-mission-textures-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let interface = root.join("interface");
        let missions = root.join("gfx/interface/missions");
        std::fs::create_dir_all(&interface).expect("interface dir");
        std::fs::create_dir_all(&missions).expect("missions dir");
        std::fs::write(
            interface.join("test.gfx"),
            "spriteTypes = { spriteType = { name = \"test_icon\" texturefile = \"gfx//interface//missions//t.dds\" } }",
        )
        .expect("gfx file");
        // A 2x1 BGRA32 DDS with one red pixel.
        let mut dds = vec![0u8; 128 + 8];
        dds[..4].copy_from_slice(b"DDS ");
        dds[4..8].copy_from_slice(&124u32.to_le_bytes());
        dds[12..16].copy_from_slice(&1u32.to_le_bytes());
        dds[16..20].copy_from_slice(&2u32.to_le_bytes());
        dds[20..24].copy_from_slice(&8u32.to_le_bytes());
        dds[80..84].copy_from_slice(&0x41u32.to_le_bytes());
        dds[88..92].copy_from_slice(&32u32.to_le_bytes());
        dds[92..96].copy_from_slice(&0x00ff_0000u32.to_le_bytes());
        dds[96..100].copy_from_slice(&0x0000_ff00u32.to_le_bytes());
        dds[100..104].copy_from_slice(&0x0000_00ffu32.to_le_bytes());
        dds[104..108].copy_from_slice(&0xff00_0000u32.to_le_bytes());
        dds.extend_from_slice(&[0, 0, 255, 255, 0, 255, 0, 255]);
        std::fs::write(missions.join("t.dds"), dds).expect("dds file");

        let assets = TextureAssets::load(&root).expect("assets load");
        let url = assets.data_url("test_icon").expect("decoded sprite");
        assert!(url.starts_with("data:image/png;base64,"));
        // Cached second fetch is identical.
        assert_eq!(assets.data_url("test_icon").as_deref(), Some(url.as_str()));
        // Unknown and missing sprites degrade to None.
        assert!(assets.data_url("missing").is_none());
        std::fs::remove_dir_all(root).expect("cleanup");
    }
}
