//! Workspace asset catalog backing `.gfx` path references.
//!
//! The semantic rules type texture/effect references as `texture_path` values; the catalog
//! is the filesystem ground truth those matchers resolve against. It walks each source
//! root's asset directories once per root generation and answers normalized-path lookups,
//! mirroring the game's own resolution: any source root may provide the file, DLC packs
//! resolve pack-relative, and a `.tga`/`.dds` extension drift is accepted because the
//! engine falls back between the two.

use std::collections::{BTreeMap, BTreeSet};

use text::AbsPath;
use vfs::{SourceRoot, SourceRootKind};

/// Top-level directories of each source root harvested into the catalog.
const CATALOG_DIRECTORIES: &[&str] = &["gfx", "tutorial"];
/// DLC pack container directory; its packs re-base catalog keys pack-relative.
const CATALOG_PACK_DIRECTORY: &str = "builtin_dlc";
/// DLC archive container directory; its `*.zip` central directories are
/// harvested name-only, without extraction.
const CATALOG_DLC_DIRECTORY: &str = "dlc";
/// File extensions harvested into the catalog.
const CATALOG_EXTENSIONS: &[&str] = &["dds", "tga", "png", "jpg", "jpeg", "ddr", "lua", "mesh"];
/// Hard cap on walked entries so a pathological tree cannot stall a snapshot build.
const MAX_CATALOG_ENTRIES: usize = 200_000;
/// Maximum completion labels served for one prefix query.
pub(crate) const MAX_PREFIX_RESULTS: usize = 200;
/// End-of-central-directory signature (`PK\x05\x06`).
const ZIP_EOCD_SIGNATURE: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];
/// Central-directory file-header signature (`PK\x01\x02`).
const ZIP_CENTRAL_SIGNATURE: [u8; 4] = [0x50, 0x4b, 0x01, 0x02];
/// Fixed byte length of one central-directory file header (before the name).
const ZIP_CENTRAL_HEADER: usize = 46;
/// Fixed byte length of the end-of-central-directory record.
const ZIP_EOCD_HEADER: usize = 22;
/// Zip64 sentinel spellings in the classic end-of-central-directory record.
const ZIP64_SENTINEL16: u16 = 0xffff;
const ZIP64_SENTINEL32: u32 = 0xffff_ffff;
/// Upper bound on a central directory read into memory (roughly 100k entries).
const MAX_ZIP_CENTRAL_DIRECTORY: u32 = 16 * 1024 * 1024;

/// One source-root hit for a normalized asset path.
#[derive(Clone, Debug)]
pub struct TextureCatalogHit {
    /// Kind of the source root providing the file.
    pub root_kind: SourceRootKind,
    /// Physical path of the file, absolute. For a DLC archive member this is
    /// the `.zip` itself; `archive_member` names the entry inside it.
    pub path: AbsPath,
    /// In-archive spelling of the serving entry when the hit is a DLC zip
    /// member rather than a file on disk.
    pub archive_member: Option<String>,
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
    /// Every ancestor directory of an entry key, spelled with a trailing slash.
    directories: BTreeSet<String>,
}

/// Drill-down completion children of one catalog directory.
#[derive(Clone, Debug, Default)]
pub struct TextureCatalogChildren<'a> {
    /// Subdirectory keys, each spelled with a trailing slash so selecting one
    /// continues the browse.
    pub directories: Vec<&'a str>,
    /// File keys sitting directly in the browsed directory.
    pub files: Vec<&'a str>,
}

impl TextureCatalog {
    /// Walks the asset directories of every root and indexes the collected files.
    ///
    /// `builtin_dlc` pack files are keyed pack-relative (the spelling the pack's own
    /// `.gfx` files reference), so a pack-provided `gfx/x.dds` is found by the plain
    /// `gfx/x.dds` alongside the game-root entry of the same name when both exist.
    /// `dlc` archives contribute their members the same way, keyed by the in-archive
    /// spelling the main interface `.gfx` files reference DLC assets by; member names
    /// are read from each zip's central directory and nothing is ever extracted.
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
            let archives = root.path.as_path().join(CATALOG_DLC_DIRECTORY);
            if let Ok(entries) = std::fs::read_dir(&archives) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        // The shipped layout nests one zip per dlc directory.
                        if let Ok(zips) = std::fs::read_dir(&path) {
                            for zip in zips.flatten() {
                                collect_zip_archive(&mut catalog, root, &zip.path());
                            }
                        }
                    } else {
                        collect_zip_archive(&mut catalog, root, &path);
                    }
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
    /// Root priority follows the workspace order (project first); the first
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
                    archive_member: None,
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

    /// Returns the immediate children of the directory a completion prefix points into.
    ///
    /// The prefix's directory portion (everything up to and including its last `/`)
    /// selects the browsed directory; the remainder filters child names. Subdirectory
    /// labels keep their trailing slash so a selected directory continues the browse,
    /// while files stay capped at 200 per directory. This keeps a
    /// shallow prefix (even the empty one after an opening quote) useful on catalogs
    /// with thousands of entries, where the flat prefix scan would show only its
    /// alphabetical head.
    #[must_use]
    pub fn children_with_prefix(&self, prefix: &str) -> TextureCatalogChildren<'_> {
        let normalized = normalize_asset_path(prefix);
        let (directory, fragment) = match normalized.rfind('/') {
            Some(index) => normalized.split_at(index + 1),
            None => ("", normalized.as_str()),
        };
        let mut directories = Vec::new();
        for dir in self.directories.range(directory.to_owned()..) {
            if !dir.starts_with(directory) {
                break;
            }
            let remainder = &dir[directory.len()..];
            if let Some(name) = remainder.strip_suffix('/')
                && !name.contains('/')
                && name.starts_with(fragment)
            {
                directories.push(dir.as_str());
            }
        }
        let mut files = Vec::new();
        for (path, _) in self.entries.range(directory.to_owned()..) {
            if !path.starts_with(directory) || files.len() >= MAX_PREFIX_RESULTS {
                break;
            }
            let name = &path[directory.len()..];
            if !name.contains('/') && name.starts_with(fragment) {
                files.push(path.as_str());
            }
        }
        TextureCatalogChildren { directories, files }
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
        let mut boundary = 0;
        while let Some(slash) = key[boundary..].find('/') {
            boundary += slash + 1;
            self.directories.insert(key[..boundary].to_owned());
        }
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
                    archive_member: None,
                },
            );
        }
    }
}

/// Harvests the catalog-extension members of one DLC zip archive.
///
/// Keys keep the in-archive spelling (game-relative — the spelling the main
/// interface `.gfx` files reference DLC assets by, lowercased like every other
/// catalog key) and the hit points at the archive through `archive_member`,
/// so hover provenance can show both the pack and the entry inside it. Files
/// shipped on disk always win because archives are harvested last per root.
fn collect_zip_archive(catalog: &mut TextureCatalog, root: &SourceRoot, path: &std::path::Path) {
    if !path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
    {
        return;
    }
    if catalog.len() >= MAX_CATALOG_ENTRIES {
        return;
    }
    for name in zip_member_names(path) {
        if catalog.len() >= MAX_CATALOG_ENTRIES {
            return;
        }
        let Some((_, extension)) = name.rsplit_once('.') else {
            continue;
        };
        if !CATALOG_EXTENSIONS
            .iter()
            .any(|accepted| accepted.eq_ignore_ascii_case(extension))
        {
            continue;
        }
        let normalized = name.replace('\\', "/").to_ascii_lowercase();
        let normalized = normalized.trim_start_matches('/');
        if normalized.is_empty() {
            continue;
        }
        catalog.insert(
            normalized.to_owned(),
            TextureCatalogHit {
                root_kind: root.kind,
                path: AbsPath::normalize(path),
                archive_member: Some(name),
            },
        );
    }
}

/// Reads one zip's member names from its central directory, nothing more.
///
/// The last 64 KiB plus the 22-byte record locate the end-of-central-directory
/// entry; the directory table itself is then read with one bounded seek. Every
/// failure mode — a truncated tail, a comment that swallows the record, a
/// zip64 sentinel, an implausible table size, a malformed entry — yields an
/// empty list, so an unreadable archive simply contributes nothing.
fn zip_member_names(path: &std::path::Path) -> Vec<String> {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let Ok(size) = file.metadata().map(|metadata| metadata.len()) else {
        return Vec::new();
    };
    if size < ZIP_EOCD_HEADER as u64 {
        return Vec::new();
    }
    let window_start = size.saturating_sub(65_536 + ZIP_EOCD_HEADER as u64);
    let window_len = usize::try_from(size - window_start).unwrap_or(0);
    let mut window = vec![0_u8; window_len];
    if file.seek(SeekFrom::Start(window_start)).is_err() || file.read_exact(&mut window).is_err() {
        return Vec::new();
    }
    // Scan backwards for the record; a candidate only counts when its comment
    // length exactly spans the remaining bytes, which rejects a signature that
    // merely appears inside a comment.
    let mut eocd = None;
    if window_len >= ZIP_EOCD_HEADER {
        let mut index = window_len - ZIP_EOCD_HEADER;
        loop {
            if window[index..].starts_with(&ZIP_EOCD_SIGNATURE) {
                let comment_len = usize::from(le16(&window, index + 20));
                if index + ZIP_EOCD_HEADER + comment_len == window_len {
                    eocd = Some(index);
                    break;
                }
            }
            if index == 0 {
                break;
            }
            index -= 1;
        }
    }
    let Some(eocd) = eocd else {
        return Vec::new();
    };
    let entries = le16(&window, eocd + 10);
    let table_size = le32(&window, eocd + 12);
    let table_offset = le32(&window, eocd + 16);
    if entries == ZIP64_SENTINEL16
        || table_size == ZIP64_SENTINEL32
        || table_offset == ZIP64_SENTINEL32
    {
        return Vec::new();
    }
    if table_size > MAX_ZIP_CENTRAL_DIRECTORY
        || u64::from(table_offset.saturating_add(table_size)) > size
    {
        return Vec::new();
    }
    let mut directory = vec![0_u8; table_size as usize];
    if file.seek(SeekFrom::Start(u64::from(table_offset))).is_err()
        || file.read_exact(&mut directory).is_err()
    {
        return Vec::new();
    }
    let mut names = Vec::new();
    let mut offset = 0_usize;
    while offset + ZIP_CENTRAL_HEADER <= directory.len() {
        if !directory[offset..].starts_with(&ZIP_CENTRAL_SIGNATURE) {
            break;
        }
        let name_len = usize::from(le16(&directory, offset + 28));
        let extra_len = usize::from(le16(&directory, offset + 30));
        let comment_len = usize::from(le16(&directory, offset + 32));
        let Some(name_end) = (offset + ZIP_CENTRAL_HEADER)
            .checked_add(name_len)
            .filter(|end| *end <= directory.len())
        else {
            break;
        };
        if let Ok(name) = std::str::from_utf8(&directory[offset + ZIP_CENTRAL_HEADER..name_end])
            && !name.ends_with('/')
        {
            names.push(name.to_owned());
        }
        let Some(next) = name_end
            .checked_add(extra_len)
            .and_then(|end| end.checked_add(comment_len))
            .filter(|end| *end <= directory.len())
        else {
            break;
        };
        offset = next;
    }
    names
}

/// Reads one little-endian `u16` at `offset`.
fn le16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

/// Reads one little-endian `u32` at `offset`.
fn le32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}
