//! Workspace asset catalog backing `.gfx` path references.
//!
//! The semantic rules type texture/effect references as `texture_path` values; the catalog
//! is the filesystem ground truth those matchers resolve against. It walks each source
//! root's asset directories once per root generation and answers normalized-path lookups,
//! mirroring the game's own resolution: any source root may provide the file, DLC packs
//! resolve pack-relative, and a `.tga`/`.dds` extension drift is accepted because the
//! engine falls back between the two.

use std::collections::BTreeMap;

use text::AbsPath;
use vfs::{SourceRoot, SourceRootKind};

/// Top-level directories of each source root harvested into the catalog.
const CATALOG_DIRECTORIES: &[&str] = &["gfx", "tutorial"];
/// DLC pack container directory; its packs re-base catalog keys pack-relative.
const CATALOG_PACK_DIRECTORY: &str = "builtin_dlc";
/// File extensions harvested into the catalog.
const CATALOG_EXTENSIONS: &[&str] = &["dds", "tga", "png", "jpg", "jpeg", "ddr", "lua", "mesh"];
/// Hard cap on walked entries so a pathological tree cannot stall a snapshot build.
const MAX_CATALOG_ENTRIES: usize = 200_000;
/// Maximum completion labels served for one prefix query.
const MAX_PREFIX_RESULTS: usize = 200;

/// One source-root hit for a normalized asset path.
#[derive(Clone, Debug)]
pub struct TextureCatalogHit {
    /// Kind of the source root providing the file.
    pub root_kind: SourceRootKind,
    /// Physical path of the file, absolute.
    pub path: AbsPath,
}

/// A successful asset-path resolution.
#[derive(Clone, Debug)]
pub struct TextureResolution {
    /// The root that provides the file.
    pub hit: TextureCatalogHit,
    /// The extension drift fallback (`.tga` referenced while `.dds` ships, or the
    /// reverse) was applied; the referenced spelling itself has no file.
    pub extension_fallback: bool,
}

/// Host-side lazily built catalog slot: the `roots` slice address it was built
/// from plus the catalog itself.
pub type TextureCatalogCache = std::sync::Mutex<Option<(usize, std::sync::Arc<TextureCatalog>)>>;

/// Normalized asset-path index over the workspace source roots.
#[derive(Clone, Debug, Default)]
pub struct TextureCatalog {
    /// Normalized (lowercased, forward-slash, collapsed) asset path -> hits.
    entries: BTreeMap<String, Vec<TextureCatalogHit>>,
}

impl TextureCatalog {
    /// Walks the asset directories of every root and indexes the collected files.
    ///
    /// `builtin_dlc` pack files are keyed pack-relative (the spelling the pack's own
    /// `.gfx` files reference), so a pack-provided `gfx/x.dds` is found by the plain
    /// `gfx/x.dds` alongside the game-root entry of the same name when both exist.
    #[must_use]
    pub fn build(roots: &[SourceRoot]) -> Self {
        let mut catalog = Self::default();
        for root in roots {
            for directory in CATALOG_DIRECTORIES {
                collect_directory(
                    &mut catalog,
                    root,
                    &root.path.as_path().join(directory),
                    &format!("{directory}/"),
                );
            }
            let packs = root.path.as_path().join(CATALOG_PACK_DIRECTORY);
            if let Ok(entries) = std::fs::read_dir(&packs) {
                for pack in entries.flatten() {
                    collect_directory(&mut catalog, root, &pack.path(), "");
                }
            }
        }
        catalog
    }

    /// Returns the number of indexed distinct asset paths.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns whether the catalog indexed any asset path.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Resolves one raw `.gfx` path value against the catalog and, as a fallback
    /// for paths outside the walked directories, against the roots directly.
    ///
    /// Root priority follows the workspace order (current mod first); the first
    /// providing root wins so hover provenance reflects load order.
    #[must_use]
    pub fn resolve(&self, roots: &[SourceRoot], raw: &str) -> Option<TextureResolution> {
        let normalized = normalize_asset_path(raw);
        if normalized.is_empty() {
            return None;
        }
        let catalog_hit = |key: &str| {
            self.entries
                .get(key)
                .and_then(|hits| hits.first())
                .cloned()
                .map(|hit| TextureResolution {
                    hit,
                    extension_fallback: key != normalized,
                })
        };
        if let Some(resolution) = catalog_hit(&normalized) {
            return Some(resolution);
        }
        if let Some(fallback) =
            extension_fallback(&normalized).and_then(|candidate| catalog_hit(&candidate))
        {
            return Some(fallback);
        }
        // Paths outside the harvested directories (a modder may reference any
        // game-root-relative file) fall through to a direct probe.
        let mut probe = normalized.clone();
        for _ in 0..2 {
            let hit = roots
                .iter()
                .find(|root| root.path.as_path().join(&probe).is_file())
                .map(|root| TextureCatalogHit {
                    root_kind: root.kind,
                    path: AbsPath::normalize(&root.path.as_path().join(&probe)),
                });
            if let Some(hit) = hit {
                return Some(TextureResolution {
                    hit,
                    extension_fallback: probe != normalized,
                });
            }
            probe = extension_fallback(&probe)?;
        }
        None
    }

    /// Returns whether one raw `.gfx` path value resolves to an existing file.
    #[must_use]
    pub fn exists(&self, roots: &[SourceRoot], raw: &str) -> bool {
        self.resolve(roots, raw).is_some()
    }

    /// Returns whether a physical path carries a catalog-harvested extension, used
    /// to invalidate the cache when disk events touch asset files.
    #[must_use]
    pub fn is_catalog_extension(extension: &str) -> bool {
        CATALOG_EXTENSIONS
            .iter()
            .any(|accepted| accepted.eq_ignore_ascii_case(extension))
    }

    /// Returns up to 200 catalog paths starting with `prefix`.
    #[must_use]
    pub fn paths_with_prefix(&self, prefix: &str) -> Vec<&str> {
        let normalized = normalize_asset_path(prefix);
        self.entries
            .range(normalized.clone()..)
            .map_while(|(path, _)| path.starts_with(&normalized).then_some(path.as_str()))
            .take(MAX_PREFIX_RESULTS)
            .collect()
    }

    /// Returns catalog paths sharing the directory prefix of `raw`, used for
    /// did-you-mean suggestions on a missing texture.
    #[must_use]
    pub fn sibling_paths(&self, raw: &str, limit: usize) -> Vec<&str> {
        let normalized = normalize_asset_path(raw);
        let directory = match normalized.rfind('/') {
            Some(index) => normalized[..=index].to_owned(),
            None => String::new(),
        };
        self.entries
            .range(directory.clone()..)
            .map_while(|(path, _)| path.starts_with(&directory).then_some(path.as_str()))
            .filter(|path| **path != normalized)
            .take(limit)
            .collect()
    }

    fn insert(&mut self, key: String, hit: TextureCatalogHit) {
        self.entries.entry(key).or_default().push(hit);
    }
}

/// Normalizes a raw `.gfx` path value: quote-free, forward slashes, doubled
/// separators collapsed, lowercased, without a leading slash.
#[must_use]
pub fn normalize_asset_path(raw: &str) -> String {
    let trimmed = raw.trim_matches('"');
    let mut normalized = String::with_capacity(trimmed.len());
    let mut previous_slash = false;
    for character in trimmed.chars() {
        let lowered = match character {
            '\\' => '/',
            _ => character.to_ascii_lowercase(),
        };
        if lowered == '/' {
            if previous_slash {
                continue;
            }
            previous_slash = true;
        } else {
            previous_slash = false;
        }
        normalized.push(lowered);
    }
    normalized.trim_start_matches('/').to_owned()
}

/// Returns the `.tga`/`.dds` extension-drift spelling of a normalized path.
fn extension_fallback(normalized: &str) -> Option<String> {
    let extension = normalized.rsplit_once('.')?.1;
    let replacement = match extension {
        "tga" => ".dds",
        "dds" => ".tga",
        _ => return None,
    };
    let stem = &normalized[..normalized.len() - extension.len() - 1];
    Some(format!("{stem}{replacement}"))
}

/// Recursively collects catalog-extension files under `directory`, keying each by
/// `prefix` + its in-directory path.
fn collect_directory(
    catalog: &mut TextureCatalog,
    root: &SourceRoot,
    directory: &std::path::Path,
    prefix: &str,
) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        if catalog.len() >= MAX_CATALOG_ENTRIES {
            return;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let key = format!("{prefix}{name}");
        if file_type.is_dir() {
            collect_directory(catalog, root, &entry.path(), &format!("{key}/"));
        } else if file_type.is_file()
            && name.rsplit_once('.').is_some_and(|(_, extension)| {
                CATALOG_EXTENSIONS
                    .iter()
                    .any(|accepted| accepted.eq_ignore_ascii_case(extension))
            })
        {
            catalog.insert(
                key.to_ascii_lowercase(),
                TextureCatalogHit {
                    root_kind: root.kind,
                    path: AbsPath::normalize(&entry.path()),
                },
            );
        }
    }
}
