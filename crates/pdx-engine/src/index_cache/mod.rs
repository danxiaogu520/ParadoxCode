//! Index cache model, metadata, and public lifecycle facade.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use pdx_rules::{GameProfile, RuleSet};
use sha2::{Digest, Sha256};

use crate::index::{LocalisationPreviewMap, PositionMap, WorkspaceIndex};
use crate::{
    AnalysisSnapshot, SourceFile, SourceFileId, SourceRoot, SourceRootKind, WorkspaceScanToken,
};

mod codec;
mod position_codec;
mod preview;
mod read;
mod refresh;
mod template_codec;
mod write;

/// Current on-disk cache schema.
///
/// Schema 10 persists the exact selection range of definitions. Schema 9 invalidated indexes built
/// by the old encoding-recovery sanitizer, which could expose braces from malformed comments as
/// active syntax. Older caches are rebuilt once by the CLI or LSP, the same way a rules update
/// triggers a rebuild; no legacy reader is retained.
pub const CURRENT_CACHE_SCHEMA_VERSION: u32 = 11;

/// Oldest on-disk cache schema this executable can still load.
pub const MIN_SUPPORTED_CACHE_SCHEMA_VERSION: u32 = CURRENT_CACHE_SCHEMA_VERSION;

const APPLICATION_ID: i32 = 0x5044_5856;
const MAX_CACHE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_CACHE_FILES: usize = 100_000;
const MAX_CACHE_SYMBOLS: usize = 5_000_000;
const MAX_TEXT_FIELD_BYTES: usize = 1024 * 1024;
const MAX_MACRO_TEMPLATE_BYTES: usize = 16 * 1024 * 1024;
const MAX_MACRO_TEMPLATE_NODES: usize = 1_000_000;
/// One file can hold at most this many encoded navigation position bytes.
const MAX_POSITION_PAYLOAD_BYTES: u64 = 256 * 1024 * 1024;
/// Free pages are reclaimed from databases only when they hold at least this many bytes and
/// this share of the file, so small rebuilds stay cheap.
const FREELIST_TRIM_THRESHOLD_BYTES: i64 = 1024 * 1024;
const FREELIST_TRIM_MIN_PERCENT: i64 = 5;

type IndexParts = (
    IndexCacheMetadata,
    SourceRoot,
    BTreeMap<SourceFileId, SourceFile>,
    WorkspaceIndex,
    PositionMap,
    LocalisationPreviewMap,
);

type LoadedIndex = (
    BTreeMap<SourceFileId, SourceFile>,
    WorkspaceIndex,
    PositionMap,
    LocalisationPreviewMap,
    BTreeMap<SourceFileId, String>,
    BTreeMap<SourceFileId, Option<String>>,
);

/// Observable metadata recorded when an index cache is built manually.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexCacheMetadata {
    /// Cache format version.
    pub schema_version: u32,
    /// Stable game identity carried by the rules artifact.
    pub game_id: String,
    /// Rules hash used to create the cache. Loading never rejects a mismatch; callers (the CLI
    /// or LSP) compare it and decide whether a full reindex is warranted.
    pub rule_hash: String,
    /// Human-readable source directory identity.
    pub source_identity: String,
    /// SHA-256 over indexed logical paths and per-file content fingerprints at build time.
    /// Opaque assets intentionally contribute no source bytes because they are never read by
    /// indexing.
    pub source_fingerprint: String,
    /// Cache creation time as Unix seconds.
    pub created_unix_seconds: u64,
    /// Number of indexed files stored in the cache.
    pub indexed_files: usize,
}

/// A validated persistent index cache containing metadata, semantic shards, per-file content
/// fingerprints, and bounded derived localisation previews, but no source text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexCache {
    metadata: IndexCacheMetadata,
    root: SourceRoot,
    source_files: BTreeMap<SourceFileId, SourceFile>,
    index: WorkspaceIndex,
    localisation_previews: LocalisationPreviewMap,
    file_fingerprints: BTreeMap<SourceFileId, String>,
    file_metadata_fingerprints: BTreeMap<SourceFileId, Option<String>>,
}

impl IndexCache {
    /// Consumes a validated cache so installation can move its large semantic index.
    pub(crate) fn into_parts(mut self) -> IndexParts {
        // Move the position table out instead of cloning it; the caller rebuilds the index.
        let positions = std::mem::take(&mut self.index.position_ranges);
        (
            self.metadata,
            self.root,
            self.source_files,
            self.index,
            positions,
            self.localisation_previews,
        )
    }

    /// Builds a cache from a dedicated single-root workspace snapshot.
    ///
    /// The snapshot must contain exactly one source root; its identity is recorded in the cache
    /// and restored on load, so the same format serves the Vanilla installation and dependency
    /// layers.
    pub fn from_snapshot(snapshot: &AnalysisSnapshot) -> Result<Self, IndexCacheError> {
        let [root] = snapshot.source_roots() else {
            return Err(IndexCacheError::InvalidData(
                "a cache must be built from exactly one source root".to_owned(),
            ));
        };
        if let Some(file) = snapshot.source_files().values().find(|file| {
            !snapshot
                .game_profile()
                .allows_scan_file(file.logical_path.as_str())
        }) {
            return Err(IndexCacheError::InvalidData(format!(
                "cache file {} is outside the active profile scan whitelist",
                file.logical_path.as_str()
            )));
        }
        if snapshot.source_files().len() > MAX_CACHE_FILES {
            return Err(IndexCacheError::LimitExceeded("file", MAX_CACHE_FILES));
        }
        validate_cache_limits(
            snapshot.index().definitions_iter().count(),
            snapshot.index().references_iter().count(),
            snapshot
                .index()
                .shards
                .values()
                .map(|shard| shard.macro_definitions.len())
                .sum::<usize>(),
            snapshot
                .index()
                .shards
                .values()
                .flat_map(|shard| shard.macro_definitions.iter())
                .map(|summary| summary.parameters.len())
                .sum::<usize>(),
        )?;
        let mut hasher = Sha256::new();
        hasher.update(b"paradoxcode/vanilla-source/v2\0");
        let mut file_fingerprints = BTreeMap::new();
        let mut file_metadata_fingerprints = BTreeMap::new();
        for (id, file) in snapshot.source_files() {
            if file.root_id != root.id {
                return Err(IndexCacheError::InvalidData(format!(
                    "file {} belongs to a different source root",
                    id.get()
                )));
            }
            let state = snapshot.file_state(*id).ok_or_else(|| {
                IndexCacheError::InvalidData(format!(
                    "cached file {} has no materialized file state",
                    file.logical_path.as_str()
                ))
            })?;
            put_fingerprint_field(&mut hasher, file.logical_path.as_str().as_bytes());
            let digest = content_fingerprint(state.source());
            put_fingerprint_field(&mut hasher, digest.as_bytes());
            file_fingerprints.insert(*id, digest);
            file_metadata_fingerprints
                .insert(*id, source_metadata_fingerprint(&file.physical_path));
        }
        let source_fingerprint: String = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let localisation_previews = preview::collect_localisation_previews(snapshot)?;
        let created_unix_seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| IndexCacheError::InvalidData(error.to_string()))?
            .as_secs();
        let metadata = IndexCacheMetadata {
            schema_version: CURRENT_CACHE_SCHEMA_VERSION,
            game_id: snapshot.rules().game_id().to_owned(),
            rule_hash: snapshot.rules().rule_hash().to_hex(),
            source_identity: root.path.display().to_string(),
            source_fingerprint,
            created_unix_seconds,
            indexed_files: snapshot.source_files().len(),
        };
        Ok(Self {
            metadata,
            root: root.clone(),
            source_files: snapshot.source_files().clone(),
            index: snapshot.index().clone(),
            localisation_previews,
            file_fingerprints,
            file_metadata_fingerprints,
        })
    }
}

impl IndexCache {
    /// Reindexes this cache against its recorded source directory, avoiding reads for files whose
    /// recorded filesystem metadata is unchanged and parsing only files whose content changed.
    ///
    /// The rules must match the hash recorded in the cache: shard contents (kinds, macro
    /// summaries, references) depend on the rules, so a different hash needs a full rebuild.
    pub fn refresh(&self, rules: &RuleSet, profile: &GameProfile) -> Result<Self, IndexCacheError> {
        refresh::refresh_cancellable(self, rules, profile, &WorkspaceScanToken::new(), None)
    }

    /// [`Self::refresh`] with cooperative cancellation and per-file `(done, total)` progress.
    pub fn refresh_cancellable(
        &self,
        rules: &RuleSet,
        profile: &GameProfile,
        cancellation: &WorkspaceScanToken,
        progress: Option<&(dyn Fn(usize, usize) + Sync)>,
    ) -> Result<Self, IndexCacheError> {
        refresh::refresh_cancellable(self, rules, profile, cancellation, progress)
    }
}

impl IndexCache {
    /// Loads a cache without reading or scanning its original source directory.
    pub fn load(path: &Path) -> Result<Self, IndexCacheError> {
        Self::load_cancellable(path, &WorkspaceScanToken::new())
    }

    /// Loads a cache while allowing an initialization worker to interrupt SQLite work.
    pub fn load_cancellable(
        path: &Path,
        cancellation: &WorkspaceScanToken,
    ) -> Result<Self, IndexCacheError> {
        read::load_cancellable(path, cancellation)
    }

    /// Loads a cache for immediate installation, skipping the derivation of lookup maps.
    ///
    /// [`crate::AnalysisHost::install_index_cache`] merges the cache shards with the workspace and
    /// rebuilds the maps once, so the maps a full load derives would be discarded. Validation
    /// is identical to [`Self::load_cancellable`]; the returned cache must not be used for
    /// symbol queries before installation.
    pub fn load_cancellable_for_install(
        path: &Path,
        cancellation: &WorkspaceScanToken,
    ) -> Result<Self, IndexCacheError> {
        read::load_cancellable_for_install(path, cancellation)
    }

    /// [`Self::load_cancellable_for_install`] with `(done, total)` row-level progress reports.
    ///
    /// The totals are derived from the table-limit validation pass, so the first report fires
    /// before any row is materialized and the final report lands after cross-table validation.
    pub fn load_cancellable_for_install_with_progress(
        path: &Path,
        cancellation: &WorkspaceScanToken,
        progress: Option<&(dyn Fn(usize, usize) + Sync)>,
    ) -> Result<Self, IndexCacheError> {
        read::load_cancellable_for_install_with_progress(path, cancellation, progress)
    }

    /// Atomically replaces a recognized cache database in one SQLite transaction.
    ///
    /// An existing non-cache SQLite file is never overwritten.
    pub fn save(&self, path: &Path) -> Result<(), IndexCacheError> {
        write::save(self, path)
    }

    /// [`Self::save`] with per-source-file `(done, total)` progress reports.
    ///
    /// The total is the cached source-file count, matching the scan progress that precedes the
    /// save during a background rebuild; the position and preview tables are written after the
    /// final report.
    pub fn save_with_progress(
        &self,
        path: &Path,
        progress: Option<&(dyn Fn(usize, usize) + Sync)>,
    ) -> Result<(), IndexCacheError> {
        write::save_with_progress(self, path, progress)
    }

    /// Returns immutable cache metadata.
    #[must_use]
    pub const fn metadata(&self) -> &IndexCacheMetadata {
        &self.metadata
    }

    /// Returns the original source root without touching the filesystem.
    #[must_use]
    pub const fn source_root(&self) -> &SourceRoot {
        &self.root
    }

    /// Returns source-file metadata retained for navigation locations.
    #[must_use]
    pub const fn source_files(&self) -> &BTreeMap<SourceFileId, SourceFile> {
        &self.source_files
    }

    /// Returns the cached immutable semantic index.
    #[must_use]
    pub const fn index(&self) -> &WorkspaceIndex {
        &self.index
    }

    /// Returns bounded derived localisation text retained for Hover.
    #[must_use]
    pub const fn localisation_previews(&self) -> &LocalisationPreviewMap {
        &self.localisation_previews
    }

    /// Returns the recorded content fingerprint for one cached source file.
    #[must_use]
    pub fn file_fingerprint(&self, file_id: SourceFileId) -> Option<&str> {
        self.file_fingerprints.get(&file_id).map(String::as_str)
    }

    /// Returns the recorded filesystem metadata fingerprint for one source file, when the
    /// platform exposed a reliable metadata snapshot while the cache was built or refreshed.
    #[must_use]
    pub fn file_metadata_fingerprint(&self, file_id: SourceFileId) -> Option<&str> {
        self.file_metadata_fingerprints
            .get(&file_id)
            .and_then(Option::as_deref)
    }
}
/// Errors raised while building, persisting, loading, or installing an index cache.
#[derive(Debug)]
pub enum IndexCacheError {
    /// The caller cancelled cache loading.
    Cancelled,
    /// Filesystem access failed.
    Io(std::io::Error),
    /// SQLite rejected or could not query the cache.
    Sql(rusqlite::Error),
    /// The file is SQLite but is not a ParadoxCode index cache.
    NotIndexCache,
    /// The cache schema is not understood by this executable.
    UnsupportedSchema(u32),
    /// Required metadata is absent or malformed.
    InvalidMetadata(&'static str),
    /// Valid resource bounds were exceeded.
    LimitExceeded(&'static str, usize),
    /// Structurally invalid or inconsistent cache data was found.
    InvalidData(String),
    /// Cache and selected game profile identities differ.
    GameMismatch { expected: String, actual: String },
    /// The cached root conflicts with a configured source root.
    RootConflict { root: PathBuf, configured: PathBuf },
    /// The cache was built with a different rules hash; a full reindex is required.
    RuleHashMismatch { cached: String, active: String },
}

impl fmt::Display for IndexCacheError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("index cache loading was cancelled"),
            Self::Io(error) => write!(formatter, "index cache I/O error: {error}"),
            Self::Sql(error) => write!(formatter, "index cache SQLite error: {error}"),
            Self::NotIndexCache => formatter.write_str(
                "the selected file is not a ParadoxCode index cache and will not be overwritten",
            ),
            Self::UnsupportedSchema(version) => {
                write!(
                    formatter,
                    "unsupported index cache schema version: {version}"
                )
            }
            Self::InvalidMetadata(key) => {
                write!(formatter, "invalid or missing index cache metadata: {key}")
            }
            Self::LimitExceeded(kind, limit) => {
                write!(formatter, "index cache exceeds the {kind} limit of {limit}")
            }
            Self::InvalidData(detail) => write!(formatter, "invalid index cache data: {detail}"),
            Self::GameMismatch { expected, actual } => write!(
                formatter,
                "index cache game mismatch: expected {expected}, found {actual}"
            ),
            Self::RootConflict { root, configured } => write!(
                formatter,
                "index cache root {} overlaps configured source root {}",
                root.display(),
                configured.display()
            ),
            Self::RuleHashMismatch { cached, active } => write!(
                formatter,
                "index cache rules hash mismatch: cached {cached}, active {active}; a full reindex is required"
            ),
        }
    }
}

impl std::error::Error for IndexCacheError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Sql(error) => Some(error),
            _ => None,
        }
    }
}
impl From<std::io::Error> for IndexCacheError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<rusqlite::Error> for IndexCacheError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sql(error)
    }
}

/// Shared symbol/file budget checks for full builds and refreshes.
fn validate_cache_limits(
    definition_count: usize,
    reference_count: usize,
    macro_definition_count: usize,
    macro_parameter_count: usize,
) -> Result<(), IndexCacheError> {
    for (label, count) in [
        ("definition", definition_count),
        ("reference", reference_count),
        ("macro summary", macro_definition_count),
        ("macro parameter", macro_parameter_count),
    ] {
        if count > MAX_CACHE_SYMBOLS {
            return Err(IndexCacheError::LimitExceeded(label, MAX_CACHE_SYMBOLS));
        }
    }
    Ok(())
}

pub(super) fn put_fingerprint_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(value);
}

/// Stable content digest used by the per-file cache and the aggregate source fingerprint.
pub(super) fn content_fingerprint(source: &str) -> String {
    Sha256::digest(source.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Compute a conservative, platform-specific fingerprint for a regular source file's metadata.
///
/// A missing or unsupported metadata field returns `None`, which deliberately disables the fast
/// path for that file and keeps the existing content-read validation as the safe fallback. The
/// cache key already includes the stable logical path, so the stamp only needs to distinguish a
/// replacement or edit at that path.
pub(super) fn source_metadata_fingerprint(path: &Path) -> Option<String> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    let mut hasher = Sha256::new();
    hasher.update(b"paradoxcode/source-metadata/v1\0");
    hasher.update(metadata.len().to_le_bytes());
    if let Ok(modified) = metadata.modified()
        && let Ok(duration) = modified.duration_since(UNIX_EPOCH)
    {
        hasher.update(duration.as_secs().to_le_bytes());
        hasher.update(duration.subsec_nanos().to_le_bytes());
    } else {
        hasher.update([0_u8; 12]);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        hasher.update(metadata.dev().to_le_bytes());
        hasher.update(metadata.ino().to_le_bytes());
        hasher.update(metadata.ctime().to_le_bytes());
        hasher.update(metadata.ctime_nsec().to_le_bytes());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        hasher.update(metadata.creation_time().to_le_bytes());
        hasher.update(metadata.last_write_time().to_le_bytes());
        hasher.update(metadata.file_attributes().to_le_bytes());
    }
    Some(
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    )
}

/// Canonical on-disk spelling of a cached source root kind.
pub(super) fn root_kind_name(kind: SourceRootKind) -> &'static str {
    match kind {
        SourceRootKind::Vanilla => "vanilla",
        SourceRootKind::Dependency => "dependency",
        SourceRootKind::CurrentMod => "current_mod",
    }
}

/// Parses the canonical on-disk spelling of a cached source root kind.
pub(super) fn parse_root_kind(name: &str) -> Result<SourceRootKind, IndexCacheError> {
    match name {
        "vanilla" => Ok(SourceRootKind::Vanilla),
        "dependency" => Ok(SourceRootKind::Dependency),
        "current_mod" => Ok(SourceRootKind::CurrentMod),
        _ => Err(IndexCacheError::InvalidMetadata("root_kind")),
    }
}
