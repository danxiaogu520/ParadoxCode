//! Vanilla cache model, metadata, and public lifecycle facade.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use pdx_text::{PositionRange, TextRange};
use sha2::{Digest, Sha256};

use crate::index::WorkspaceIndex;
use crate::{
    AnalysisSnapshot, LocalisationPreview, SourceFile, SourceFileId, SourceRoot, SourceRootId,
    SourceRootKind, WorkspaceScanToken,
};

mod codec;
mod preview;
mod read;
mod template_codec;
mod write;

/// Current on-disk cache schema.
///
/// Schema 6 records the cached source root identity (`root_id` and `root_kind` metadata), which
/// allows a single cache format to serve any game-data layer. Schema 5 caches remain loadable
/// and are interpreted as Vanilla.
pub const CURRENT_VANILLA_CACHE_SCHEMA_VERSION: u32 = 6;

/// Oldest on-disk cache schema this executable can still load.
pub const MIN_SUPPORTED_CACHE_SCHEMA_VERSION: u32 = 5;

const APPLICATION_ID: i32 = 0x5044_5856;
const MAX_CACHE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_CACHE_FILES: usize = 100_000;
const MAX_CACHE_SYMBOLS: usize = 5_000_000;
const MAX_TEXT_FIELD_BYTES: usize = 1024 * 1024;
const MAX_MACRO_TEMPLATE_BYTES: usize = 16 * 1024 * 1024;
const MAX_MACRO_TEMPLATE_NODES: usize = 1_000_000;
const VANILLA_ROOT_ID: SourceRootId = SourceRootId::new(0);

type VanillaIndexParts = (
    VanillaIndexCacheMetadata,
    SourceRoot,
    BTreeMap<SourceFileId, SourceFile>,
    WorkspaceIndex,
    BTreeMap<(SourceFileId, TextRange), PositionRange>,
    BTreeMap<(SourceFileId, TextRange), LocalisationPreview>,
);

type LoadedIndex = (
    BTreeMap<SourceFileId, SourceFile>,
    WorkspaceIndex,
    BTreeMap<(SourceFileId, TextRange), PositionRange>,
    BTreeMap<(SourceFileId, TextRange), LocalisationPreview>,
);

/// Observable metadata recorded when a Vanilla cache is built manually.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VanillaIndexCacheMetadata {
    /// Cache format version.
    pub schema_version: u32,
    /// Stable game identity carried by the rules artifact.
    pub game_id: String,
    /// Rules hash used to create the cache. A mismatch does not trigger an automatic rebuild.
    pub rule_hash: String,
    /// Human-readable source directory identity.
    pub source_identity: String,
    /// SHA-256 over indexed logical paths and materialized source text at build time. Opaque
    /// assets intentionally contribute no source bytes because they are never read by indexing.
    pub source_fingerprint: String,
    /// Cache creation time as Unix seconds.
    pub created_unix_seconds: u64,
    /// Number of indexed files stored in the cache.
    pub indexed_files: usize,
}

/// A validated local Vanilla cache containing metadata, semantic shards, and bounded derived
/// localisation previews, but no source text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VanillaIndexCache {
    metadata: VanillaIndexCacheMetadata,
    root: SourceRoot,
    source_files: BTreeMap<SourceFileId, SourceFile>,
    index: WorkspaceIndex,
    localisation_previews: BTreeMap<(SourceFileId, TextRange), LocalisationPreview>,
}

impl VanillaIndexCache {
    /// Consumes a validated cache so installation can move its large semantic index.
    pub(crate) fn into_parts(mut self) -> VanillaIndexParts {
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
    pub fn from_snapshot(snapshot: &AnalysisSnapshot) -> Result<Self, VanillaCacheError> {
        let [root] = snapshot.source_roots() else {
            return Err(VanillaCacheError::InvalidData(
                "a cache must be built from exactly one source root".to_owned(),
            ));
        };
        if let Some(file) = snapshot.source_files().values().find(|file| {
            !snapshot
                .game_profile()
                .allows_scan_file(file.logical_path.as_str())
        }) {
            return Err(VanillaCacheError::InvalidData(format!(
                "cache file {} is outside the active profile scan whitelist",
                file.logical_path.as_str()
            )));
        }
        if snapshot.source_files().len() > MAX_CACHE_FILES {
            return Err(VanillaCacheError::LimitExceeded("file", MAX_CACHE_FILES));
        }
        if snapshot.index().definitions_iter().count() > MAX_CACHE_SYMBOLS {
            return Err(VanillaCacheError::LimitExceeded(
                "definition",
                MAX_CACHE_SYMBOLS,
            ));
        }
        if snapshot.index().references_iter().count() > MAX_CACHE_SYMBOLS {
            return Err(VanillaCacheError::LimitExceeded(
                "reference",
                MAX_CACHE_SYMBOLS,
            ));
        }
        let macro_definition_count = snapshot
            .index()
            .shards
            .values()
            .map(|shard| shard.macro_definitions.len())
            .sum::<usize>();
        let macro_parameter_count = snapshot
            .index()
            .shards
            .values()
            .flat_map(|shard| shard.macro_definitions.iter())
            .map(|summary| summary.parameters.len())
            .sum::<usize>();
        if macro_definition_count > MAX_CACHE_SYMBOLS || macro_parameter_count > MAX_CACHE_SYMBOLS {
            return Err(VanillaCacheError::LimitExceeded(
                "macro summary",
                MAX_CACHE_SYMBOLS,
            ));
        }
        let mut hasher = Sha256::new();
        hasher.update(b"paradoxcode/vanilla-source/v1\0");
        for (id, file) in snapshot.source_files() {
            if file.root_id != root.id {
                return Err(VanillaCacheError::InvalidData(format!(
                    "file {} belongs to a different source root",
                    id.get()
                )));
            }
            let state = snapshot.file_state(*id).ok_or_else(|| {
                VanillaCacheError::InvalidData(format!(
                    "Vanilla file {} has no materialized file state",
                    file.logical_path.as_str()
                ))
            })?;
            put_fingerprint_field(&mut hasher, file.logical_path.as_str().as_bytes());
            put_fingerprint_field(&mut hasher, state.source().as_bytes());
        }
        let source_fingerprint = format!("{:x}", hasher.finalize());
        let localisation_previews = preview::collect_localisation_previews(snapshot)?;
        let created_unix_seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| VanillaCacheError::InvalidData(error.to_string()))?
            .as_secs();
        let metadata = VanillaIndexCacheMetadata {
            schema_version: CURRENT_VANILLA_CACHE_SCHEMA_VERSION,
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
        })
    }
}

impl VanillaIndexCache {
    /// Loads a cache without reading or scanning its original Vanilla directory.
    pub fn load(path: &Path) -> Result<Self, VanillaCacheError> {
        Self::load_cancellable(path, &WorkspaceScanToken::new())
    }

    /// Loads a cache while allowing an initialization worker to interrupt SQLite work.
    pub fn load_cancellable(
        path: &Path,
        cancellation: &WorkspaceScanToken,
    ) -> Result<Self, VanillaCacheError> {
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
    ) -> Result<Self, VanillaCacheError> {
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
    ) -> Result<Self, VanillaCacheError> {
        read::load_cancellable_for_install_with_progress(path, cancellation, progress)
    }

    /// Atomically replaces a recognized cache database in one SQLite transaction.
    ///
    /// An existing non-cache SQLite file is never overwritten.
    pub fn save(&self, path: &Path) -> Result<(), VanillaCacheError> {
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
    ) -> Result<(), VanillaCacheError> {
        write::save_with_progress(self, path, progress)
    }

    /// Returns immutable cache metadata.
    #[must_use]
    pub const fn metadata(&self) -> &VanillaIndexCacheMetadata {
        &self.metadata
    }

    /// Returns the original Vanilla source root without touching the filesystem.
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
    pub const fn localisation_previews(
        &self,
    ) -> &BTreeMap<(SourceFileId, TextRange), LocalisationPreview> {
        &self.localisation_previews
    }
}
/// Errors raised while building, persisting, loading, or installing a Vanilla cache.
#[derive(Debug)]
pub enum VanillaCacheError {
    /// The caller cancelled cache loading.
    Cancelled,
    /// Filesystem access failed.
    Io(std::io::Error),
    /// SQLite rejected or could not query the cache.
    Sql(rusqlite::Error),
    /// The file is SQLite but is not a ParadoxCode Vanilla cache.
    NotVanillaCache,
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
    /// The cached Vanilla root conflicts with a configured source root.
    RootConflict {
        vanilla: PathBuf,
        configured: PathBuf,
    },
}

impl fmt::Display for VanillaCacheError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("Vanilla cache loading was cancelled"),
            Self::Io(error) => write!(formatter, "Vanilla cache I/O error: {error}"),
            Self::Sql(error) => write!(formatter, "Vanilla cache SQLite error: {error}"),
            Self::NotVanillaCache => formatter.write_str(
                "the selected file is not a ParadoxCode Vanilla cache and will not be overwritten",
            ),
            Self::UnsupportedSchema(version) => {
                write!(
                    formatter,
                    "unsupported Vanilla cache schema version: {version}"
                )
            }
            Self::InvalidMetadata(key) => {
                write!(
                    formatter,
                    "invalid or missing Vanilla cache metadata: {key}"
                )
            }
            Self::LimitExceeded(kind, limit) => {
                write!(
                    formatter,
                    "Vanilla cache exceeds the {kind} limit of {limit}"
                )
            }
            Self::InvalidData(detail) => write!(formatter, "invalid Vanilla cache data: {detail}"),
            Self::GameMismatch { expected, actual } => write!(
                formatter,
                "Vanilla cache game mismatch: expected {expected}, found {actual}"
            ),
            Self::RootConflict {
                vanilla,
                configured,
            } => write!(
                formatter,
                "Vanilla cache root {} overlaps configured source root {}",
                vanilla.display(),
                configured.display()
            ),
        }
    }
}

impl std::error::Error for VanillaCacheError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Sql(error) => Some(error),
            _ => None,
        }
    }
}
impl From<std::io::Error> for VanillaCacheError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<rusqlite::Error> for VanillaCacheError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sql(error)
    }
}

fn put_fingerprint_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(value);
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
pub(super) fn parse_root_kind(name: &str) -> Result<SourceRootKind, VanillaCacheError> {
    match name {
        "vanilla" => Ok(SourceRootKind::Vanilla),
        "dependency" => Ok(SourceRootKind::Dependency),
        "current_mod" => Ok(SourceRootKind::CurrentMod),
        _ => Err(VanillaCacheError::InvalidMetadata("root_kind")),
    }
}
