//! Persistent, local-only Vanilla index artifacts.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use pdx_parser::{CstKind, FileFormat};
use pdx_rules::FileResolutionPolicy;
use pdx_text::{LogicalPath, Position, PositionRange, TextRange};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};

use super::{
    AnalysisSnapshot, Definition, FileIndexShard, LocalisationPreview, ParsedSource, Reference,
    SourceFile, SourceFileId, SourceRoot, SourceRootId, SourceRootKind, WorkspaceIndex,
    WorkspaceScanToken, stable_file_id,
};

/// Current on-disk Vanilla cache schema.
pub const CURRENT_VANILLA_CACHE_SCHEMA_VERSION: u32 = 3;

const APPLICATION_ID: i32 = 0x5044_5856;
const MAX_CACHE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_CACHE_FILES: usize = 100_000;
const MAX_CACHE_SYMBOLS: usize = 5_000_000;
const MAX_TEXT_FIELD_BYTES: usize = 1024 * 1024;
const MAX_LOCALISATION_PREVIEW_CHARS: usize = 240;
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
    /// SHA-256 over indexed logical paths and source bytes at build time.
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
    pub(crate) fn into_parts(self) -> VanillaIndexParts {
        let positions = self.index.position_ranges().clone();
        (
            self.metadata,
            self.root,
            self.source_files,
            self.index,
            positions,
            self.localisation_previews,
        )
    }

    /// Builds a cache from a dedicated Vanilla-only workspace snapshot.
    pub fn from_snapshot(snapshot: &AnalysisSnapshot) -> Result<Self, VanillaCacheError> {
        let [root] = snapshot.source_roots() else {
            return Err(VanillaCacheError::InvalidData(
                "a Vanilla cache must be built from exactly one source root".to_owned(),
            ));
        };
        if root.kind != SourceRootKind::Vanilla {
            return Err(VanillaCacheError::InvalidData(
                "the cache source root is not marked as Vanilla".to_owned(),
            ));
        }
        if root.id != VANILLA_ROOT_ID {
            return Err(VanillaCacheError::InvalidData(format!(
                "the Vanilla source root must use reserved id {}",
                VANILLA_ROOT_ID.get()
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
        let localisation_previews = collect_localisation_previews(snapshot)?;
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

    /// Loads and validates a cache without reading or scanning its original Vanilla directory.
    pub fn load(path: &Path) -> Result<Self, VanillaCacheError> {
        Self::load_cancellable(path, &WorkspaceScanToken::new())
    }

    /// Loads a cache while allowing an initialization worker to interrupt SQLite work.
    pub fn load_cancellable(
        path: &Path,
        cancellation: &WorkspaceScanToken,
    ) -> Result<Self, VanillaCacheError> {
        if cancellation.is_cancelled() {
            return Err(VanillaCacheError::Cancelled);
        }
        let metadata = fs::metadata(path)?;
        if !metadata.is_file() {
            return Err(VanillaCacheError::InvalidData(format!(
                "cache is not a regular file: {}",
                path.display()
            )));
        }
        if metadata.len() > MAX_CACHE_BYTES {
            return Err(VanillaCacheError::LimitExceeded(
                "cache byte",
                usize::try_from(MAX_CACHE_BYTES).unwrap_or(usize::MAX),
            ));
        }
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let cancellation = cancellation.clone();
        connection.progress_handler(1_000, Some(move || cancellation.is_cancelled()));
        Self::load_connection(&connection).map_err(map_interrupted)
    }

    fn load_connection(connection: &Connection) -> Result<Self, VanillaCacheError> {
        validate_database_identity(connection)?;
        let schema_version = metadata_text(connection, "schema_version")?
            .parse::<u32>()
            .map_err(|_| VanillaCacheError::InvalidMetadata("schema_version"))?;
        if schema_version != CURRENT_VANILLA_CACHE_SCHEMA_VERSION {
            return Err(VanillaCacheError::UnsupportedSchema(schema_version));
        }
        validate_table_limits(connection)?;
        let source_root = decode_path(
            &metadata_blob(connection, "source_root")?,
            &metadata_text(connection, "path_encoding")?,
        )?;
        let game_id = metadata_text(connection, "game_id")?;
        let rule_hash = metadata_text(connection, "rule_hash")?;
        let source_identity = metadata_text(connection, "source_identity")?;
        let source_fingerprint = metadata_text(connection, "source_fingerprint")?;
        let created_unix_seconds = metadata_text(connection, "created_unix_seconds")?
            .parse::<u64>()
            .map_err(|_| VanillaCacheError::InvalidMetadata("created_unix_seconds"))?;
        let indexed_files = metadata_text(connection, "indexed_files")?
            .parse::<usize>()
            .map_err(|_| VanillaCacheError::InvalidMetadata("indexed_files"))?;
        let (source_files, index, positions, localisation_previews) =
            load_index(connection, &source_root)?;
        if indexed_files != source_files.len() {
            return Err(VanillaCacheError::InvalidData(format!(
                "metadata records {indexed_files} files but cache contains {}",
                source_files.len()
            )));
        }
        let mut index = index;
        index.replace_all_position_ranges(positions);
        Ok(Self {
            metadata: VanillaIndexCacheMetadata {
                schema_version,
                game_id,
                rule_hash,
                source_identity,
                source_fingerprint,
                created_unix_seconds,
                indexed_files,
            },
            root: SourceRoot::new(VANILLA_ROOT_ID, SourceRootKind::Vanilla, source_root),
            source_files,
            index,
            localisation_previews,
        })
    }

    /// Atomically replaces a recognized cache database in one SQLite transaction.
    ///
    /// An existing non-cache SQLite file is never overwritten.
    pub fn save(&self, path: &Path) -> Result<(), VanillaCacheError> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        let existed = path.exists();
        let existing_len = if existed {
            fs::metadata(path)?.len()
        } else {
            0
        };
        let mut connection = Connection::open(path)?;
        if existed && existing_len > 0 {
            validate_database_identity(&connection)?;
        }
        let transaction = connection.transaction()?;
        write_cache(&transaction, self)?;
        transaction.commit()?;
        Ok(())
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

fn map_interrupted(error: VanillaCacheError) -> VanillaCacheError {
    match error {
        VanillaCacheError::Sql(ref sqlite)
            if sqlite.sqlite_error_code() == Some(rusqlite::ErrorCode::OperationInterrupted) =>
        {
            VanillaCacheError::Cancelled
        }
        error => error,
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

fn collect_localisation_previews(
    snapshot: &AnalysisSnapshot,
) -> Result<BTreeMap<(SourceFileId, TextRange), LocalisationPreview>, VanillaCacheError> {
    let mut previews = BTreeMap::new();
    for (file_id, file) in snapshot.source_files() {
        let state = snapshot.file_state(*file_id).ok_or_else(|| {
            VanillaCacheError::InvalidData(format!(
                "Vanilla file {} has no materialized file state",
                file.logical_path.as_str()
            ))
        })?;
        let Some(ParsedSource::Text(parsed)) = state.parsed() else {
            continue;
        };
        if parsed.format() != FileFormat::Localisation {
            continue;
        }
        let mut language = None;
        for node in parsed.root().children() {
            match node.kind() {
                CstKind::LanguageHeader => {
                    language = node
                        .children()
                        .iter()
                        .find(|child| child.kind() == CstKind::LocalisationKey)
                        .and_then(|child| parsed.text(child.range()))
                        .map(|value| value.trim().to_owned())
                        .filter(|value| !value.is_empty());
                }
                CstKind::LocalisationEntry => {
                    let Some(value_node) = node.children().iter().find(|child| {
                        matches!(
                            child.kind(),
                            CstKind::LocalisationString | CstKind::UnquotedValue
                        )
                    }) else {
                        continue;
                    };
                    let Some(raw) = parsed.text(value_node.range()).map(str::trim) else {
                        continue;
                    };
                    let value = raw
                        .strip_prefix('"')
                        .and_then(|value| value.strip_suffix('"'))
                        .unwrap_or(raw);
                    let value = truncate_localisation_preview(value);
                    if value.is_empty() {
                        continue;
                    }
                    if previews
                        .insert(
                            (*file_id, node.range()),
                            LocalisationPreview {
                                language: language.clone(),
                                value,
                            },
                        )
                        .is_some()
                    {
                        return Err(VanillaCacheError::InvalidData(format!(
                            "duplicate localisation preview in {}",
                            file.logical_path.as_str()
                        )));
                    }
                }
                _ => {}
            }
        }
    }
    Ok(previews)
}

fn truncate_localisation_preview(value: &str) -> String {
    let mut truncated = value
        .chars()
        .take(MAX_LOCALISATION_PREVIEW_CHARS)
        .collect::<String>();
    if value.chars().count() > MAX_LOCALISATION_PREVIEW_CHARS {
        truncated.push('…');
    }
    truncated
}

fn validate_database_identity(connection: &Connection) -> Result<(), VanillaCacheError> {
    let application_id =
        connection.pragma_query_value(None, "application_id", |row| row.get::<_, i32>(0))?;
    if application_id != APPLICATION_ID {
        return Err(VanillaCacheError::NotVanillaCache);
    }
    Ok(())
}

fn metadata_blob(connection: &Connection, key: &'static str) -> Result<Vec<u8>, VanillaCacheError> {
    let length = connection
        .query_row(
            "SELECT length(value) FROM metadata WHERE key = ?1",
            [key],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .ok_or(VanillaCacheError::InvalidMetadata(key))?;
    if length < 0 || usize::try_from(length).map_or(true, |length| length > MAX_TEXT_FIELD_BYTES) {
        return Err(VanillaCacheError::LimitExceeded(
            "metadata field byte",
            MAX_TEXT_FIELD_BYTES,
        ));
    }
    connection
        .query_row("SELECT value FROM metadata WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .optional()?
        .ok_or(VanillaCacheError::InvalidMetadata(key))
}

fn metadata_text(connection: &Connection, key: &'static str) -> Result<String, VanillaCacheError> {
    let bytes = metadata_blob(connection, key)?;
    String::from_utf8(bytes).map_err(|_| VanillaCacheError::InvalidMetadata(key))
}

fn validate_table_limits(connection: &Connection) -> Result<(), VanillaCacheError> {
    validate_count(connection, "source_files", MAX_CACHE_FILES)?;
    validate_count(connection, "definitions", MAX_CACHE_SYMBOLS)?;
    validate_count(connection, "symbol_references", MAX_CACHE_SYMBOLS)?;
    validate_count(connection, "navigation_positions", MAX_CACHE_SYMBOLS)?;
    validate_count(connection, "localisation_previews", MAX_CACHE_SYMBOLS)?;
    for (table, fields) in [
        ("source_files", "logical_path, category_id, resolution"),
        ("definitions", "kind, name"),
        ("symbol_references", "kind, name"),
        (
            "navigation_positions",
            "range_start, range_end, start_line, start_character, end_line, end_character",
        ),
        (
            "localisation_previews",
            "range_start, range_end, language, value",
        ),
    ] {
        let query = format!(
            "SELECT COALESCE(MAX(max_length), 0) FROM (SELECT max({}) AS max_length FROM {table})",
            fields
                .split(", ")
                .map(|field| format!("COALESCE(length({field}), 0)"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let max = connection.query_row(&query, [], |row| row.get::<_, i64>(0))?;
        if max < 0 || usize::try_from(max).map_or(true, |max| max > MAX_TEXT_FIELD_BYTES) {
            return Err(VanillaCacheError::LimitExceeded(
                "text field byte",
                MAX_TEXT_FIELD_BYTES,
            ));
        }
    }
    Ok(())
}

fn validate_count(
    connection: &Connection,
    table: &'static str,
    limit: usize,
) -> Result<(), VanillaCacheError> {
    let count = connection.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
        row.get::<_, i64>(0)
    })?;
    if count < 0 || usize::try_from(count).map_or(true, |count| count > limit) {
        return Err(VanillaCacheError::LimitExceeded(table, limit));
    }
    Ok(())
}

fn write_cache(
    transaction: &Transaction<'_>,
    cache: &VanillaIndexCache,
) -> Result<(), VanillaCacheError> {
    transaction.execute_batch(
        "DROP TABLE IF EXISTS symbol_references;
         DROP TABLE IF EXISTS navigation_positions;
         DROP TABLE IF EXISTS definitions;
         DROP TABLE IF EXISTS localisation_previews;
         DROP TABLE IF EXISTS source_files;
         DROP TABLE IF EXISTS metadata;
         CREATE TABLE metadata(key TEXT PRIMARY KEY, value BLOB NOT NULL);
         CREATE TABLE source_files(
             file_id BLOB PRIMARY KEY CHECK(length(file_id) = 8),
             logical_path TEXT NOT NULL,
             category_id TEXT,
             resolution TEXT NOT NULL,
             syntax_error_count INTEGER NOT NULL CHECK(syntax_error_count >= 0)
         );
         CREATE TABLE definitions(
             file_id BLOB NOT NULL REFERENCES source_files(file_id),
             ordinal INTEGER NOT NULL,
             kind TEXT NOT NULL,
             name TEXT NOT NULL,
             range_start INTEGER NOT NULL,
             range_end INTEGER NOT NULL,
             active INTEGER NOT NULL CHECK(active IN (0, 1)),
             PRIMARY KEY(file_id, ordinal)
         );
         CREATE TABLE symbol_references(
             file_id BLOB NOT NULL REFERENCES source_files(file_id),
             ordinal INTEGER NOT NULL,
             kind TEXT NOT NULL,
             name TEXT NOT NULL,
             range_start INTEGER NOT NULL,
             range_end INTEGER NOT NULL,
             PRIMARY KEY(file_id, ordinal)
         );
         CREATE TABLE navigation_positions(
             file_id BLOB NOT NULL REFERENCES source_files(file_id),
             range_start INTEGER NOT NULL,
             range_end INTEGER NOT NULL,
             start_line INTEGER NOT NULL CHECK(start_line >= 0),
             start_character INTEGER NOT NULL CHECK(start_character >= 0),
             end_line INTEGER NOT NULL CHECK(end_line >= 0),
             end_character INTEGER NOT NULL CHECK(end_character >= 0),
             PRIMARY KEY(file_id, range_start, range_end)
         );
         CREATE TABLE localisation_previews(
             file_id BLOB NOT NULL REFERENCES source_files(file_id),
             range_start INTEGER NOT NULL,
             range_end INTEGER NOT NULL,
             language TEXT,
             value TEXT NOT NULL,
             PRIMARY KEY(file_id, range_start, range_end)
         );",
    )?;
    transaction.pragma_update(None, "application_id", APPLICATION_ID)?;
    transaction.pragma_update(None, "user_version", CURRENT_VANILLA_CACHE_SCHEMA_VERSION)?;
    let (path_encoding, source_root) = encode_path(&cache.root.path)?;
    for (key, value) in [
        (
            "schema_version",
            cache.metadata.schema_version.to_string().into_bytes(),
        ),
        ("game_id", cache.metadata.game_id.as_bytes().to_vec()),
        ("rule_hash", cache.metadata.rule_hash.as_bytes().to_vec()),
        (
            "source_identity",
            cache.metadata.source_identity.as_bytes().to_vec(),
        ),
        (
            "source_fingerprint",
            cache.metadata.source_fingerprint.as_bytes().to_vec(),
        ),
        (
            "created_unix_seconds",
            cache.metadata.created_unix_seconds.to_string().into_bytes(),
        ),
        (
            "indexed_files",
            cache.metadata.indexed_files.to_string().into_bytes(),
        ),
        ("path_encoding", path_encoding.as_bytes().to_vec()),
        ("source_root", source_root),
    ] {
        transaction.execute(
            "INSERT INTO metadata(key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
    }
    for (id, file) in &cache.source_files {
        let shard = cache.index.shard(*id).ok_or_else(|| {
            VanillaCacheError::InvalidData(format!(
                "source file {} has no index shard",
                file.logical_path.as_str()
            ))
        })?;
        transaction.execute(
            "INSERT INTO source_files(file_id, logical_path, category_id, resolution, syntax_error_count)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                encode_file_id(*id),
                file.logical_path.as_str(),
                file.category_id,
                resolution_name(file.resolution),
                i64::try_from(shard.syntax_error_count).map_err(|_| {
                    VanillaCacheError::InvalidData("syntax error count exceeds SQLite range".into())
                })?
            ],
        )?;
        for (ordinal, definition) in shard.definitions.iter().enumerate() {
            transaction.execute(
                "INSERT INTO definitions(file_id, ordinal, kind, name, range_start, range_end, active)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    encode_file_id(*id),
                    i64::try_from(ordinal).unwrap_or(i64::MAX),
                    definition.kind,
                    definition.name,
                    i64::from(definition.range.start()),
                    i64::from(definition.range.end()),
                    i64::from(definition.active)
                ],
            )?;
        }
        for (ordinal, reference) in shard.references.iter().enumerate() {
            transaction.execute(
                "INSERT INTO symbol_references(file_id, ordinal, kind, name, range_start, range_end)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    encode_file_id(*id),
                    i64::try_from(ordinal).unwrap_or(i64::MAX),
                    reference.kind,
                    reference.name,
                    i64::from(reference.range.start()),
                    i64::from(reference.range.end())
                ],
            )?;
        }
    }
    for ((file_id, range), position) in cache.index.position_ranges() {
        if !cache.source_files.contains_key(file_id) {
            return Err(VanillaCacheError::InvalidData(format!(
                "navigation position references unknown file {}",
                file_id.get()
            )));
        }
        transaction.execute(
            "INSERT INTO navigation_positions(file_id, range_start, range_end, start_line, start_character, end_line, end_character)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                encode_file_id(*file_id),
                i64::from(range.start()),
                i64::from(range.end()),
                i64::from(position.start.line),
                i64::from(position.start.character),
                i64::from(position.end.line),
                i64::from(position.end.character),
            ],
        )?;
    }
    for ((file_id, range), preview) in &cache.localisation_previews {
        let Some(file) = cache.source_files.get(file_id) else {
            return Err(VanillaCacheError::InvalidData(format!(
                "localisation preview references unknown file {}",
                file_id.get()
            )));
        };
        let Some(shard) = cache.index.shard(*file_id) else {
            return Err(VanillaCacheError::InvalidData(format!(
                "localisation preview file {} has no index shard",
                file.logical_path.as_str()
            )));
        };
        if !shard.definitions.iter().any(|definition| {
            definition.range == *range && definition.kind.eq_ignore_ascii_case("localisation")
        }) {
            return Err(VanillaCacheError::InvalidData(format!(
                "localisation preview range {}..{} is not a localisation definition",
                range.start(),
                range.end()
            )));
        }
        transaction.execute(
            "INSERT INTO localisation_previews(file_id, range_start, range_end, language, value)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                encode_file_id(*file_id),
                i64::from(range.start()),
                i64::from(range.end()),
                preview.language,
                preview.value,
            ],
        )?;
    }
    Ok(())
}

fn load_index(
    connection: &Connection,
    source_root: &Path,
) -> Result<LoadedIndex, VanillaCacheError> {
    let mut source_files = BTreeMap::new();
    let mut shards = BTreeMap::new();
    let mut statement = connection.prepare(
        "SELECT file_id, logical_path, category_id, resolution, syntax_error_count
         FROM source_files ORDER BY file_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
        ))
    })?;
    for row in rows {
        let (id, logical_path, category_id, resolution, syntax_error_count) = row?;
        let id = decode_file_id(&id)?;
        let logical_path = LogicalPath::parse(&logical_path)
            .map_err(|error| VanillaCacheError::InvalidData(error.to_string()))?;
        if stable_file_id(VANILLA_ROOT_ID, &logical_path) != id.get() {
            return Err(VanillaCacheError::InvalidData(format!(
                "file id does not match logical path {}",
                logical_path.as_str()
            )));
        }
        let syntax_error_count = usize::try_from(syntax_error_count).map_err(|_| {
            VanillaCacheError::InvalidData("negative syntax error count".to_owned())
        })?;
        let file = SourceFile {
            id,
            root_id: VANILLA_ROOT_ID,
            physical_path: join_logical_path(source_root, &logical_path),
            logical_path,
            category_id,
            resolution: parse_resolution(&resolution)?,
        };
        if source_files.insert(id, file).is_some() {
            return Err(VanillaCacheError::InvalidData(format!(
                "duplicate source file id {}",
                id.get()
            )));
        }
        shards.insert(
            id,
            FileIndexShard {
                file_id: id,
                definitions: Vec::new(),
                references: Vec::new(),
                syntax_error_count,
            },
        );
    }
    load_definitions(connection, &mut shards)?;
    load_references(connection, &mut shards)?;
    let localisation_previews = load_localisation_previews(connection, &shards)?;
    let positions = load_navigation_positions(connection)?;
    for ((file_id, range), position) in &positions {
        let Some(shard) = shards.get(file_id) else {
            return Err(VanillaCacheError::InvalidData(format!(
                "navigation position references unknown file {}",
                file_id.get()
            )));
        };
        let known_range = shard
            .definitions
            .iter()
            .any(|definition| definition.range == *range)
            || shard
                .references
                .iter()
                .any(|reference| reference.range == *range);
        if !known_range {
            return Err(VanillaCacheError::InvalidData(format!(
                "navigation position references unknown range {}..{}",
                range.start(),
                range.end()
            )));
        }
        if position.start > position.end {
            return Err(VanillaCacheError::InvalidData(
                "navigation position end precedes start".to_owned(),
            ));
        }
    }
    Ok((
        source_files,
        WorkspaceIndex::from_shards(shards.into_values()),
        positions,
        localisation_previews,
    ))
}

fn load_localisation_previews(
    connection: &Connection,
    shards: &BTreeMap<SourceFileId, FileIndexShard>,
) -> Result<BTreeMap<(SourceFileId, TextRange), LocalisationPreview>, VanillaCacheError> {
    let mut statement = connection.prepare(
        "SELECT file_id, range_start, range_end, language, value
         FROM localisation_previews ORDER BY file_id, range_start, range_end",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    let mut previews = BTreeMap::new();
    for row in rows {
        let (file_id, start, end, language, value) = row?;
        let file_id = decode_file_id(&file_id)?;
        let range = decode_range(start, end)?;
        let Some(shard) = shards.get(&file_id) else {
            return Err(VanillaCacheError::InvalidData(format!(
                "localisation preview references unknown file {}",
                file_id.get()
            )));
        };
        if !shard.definitions.iter().any(|definition| {
            definition.range == range && definition.kind.eq_ignore_ascii_case("localisation")
        }) {
            return Err(VanillaCacheError::InvalidData(format!(
                "localisation preview range {}..{} is not a localisation definition",
                range.start(),
                range.end()
            )));
        }
        if value.is_empty() {
            return Err(VanillaCacheError::InvalidData(
                "localisation preview value is empty".to_owned(),
            ));
        }
        if previews
            .insert((file_id, range), LocalisationPreview { language, value })
            .is_some()
        {
            return Err(VanillaCacheError::InvalidData(
                "duplicate localisation preview".to_owned(),
            ));
        }
    }
    Ok(previews)
}

fn load_definitions(
    connection: &Connection,
    shards: &mut BTreeMap<SourceFileId, FileIndexShard>,
) -> Result<(), VanillaCacheError> {
    let mut statement = connection.prepare(
        "SELECT file_id, kind, name, range_start, range_end, active
         FROM definitions ORDER BY file_id, ordinal",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
        ))
    })?;
    for row in rows {
        let (file_id, kind, name, start, end, active) = row?;
        let file_id = decode_file_id(&file_id)?;
        let range = decode_range(start, end)?;
        let active = match active {
            0 => false,
            1 => true,
            _ => {
                return Err(VanillaCacheError::InvalidData(
                    "definition active flag is not boolean".to_owned(),
                ));
            }
        };
        shards
            .get_mut(&file_id)
            .ok_or_else(|| {
                VanillaCacheError::InvalidData(format!(
                    "definition references unknown file {}",
                    file_id.get()
                ))
            })?
            .definitions
            .push(Definition {
                kind,
                name,
                file_id,
                range,
                active,
            });
    }
    Ok(())
}

fn load_references(
    connection: &Connection,
    shards: &mut BTreeMap<SourceFileId, FileIndexShard>,
) -> Result<(), VanillaCacheError> {
    let mut statement = connection.prepare(
        "SELECT file_id, kind, name, range_start, range_end
         FROM symbol_references ORDER BY file_id, ordinal",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
        ))
    })?;
    for row in rows {
        let (file_id, kind, name, start, end) = row?;
        let file_id = decode_file_id(&file_id)?;
        let range = decode_range(start, end)?;
        shards
            .get_mut(&file_id)
            .ok_or_else(|| {
                VanillaCacheError::InvalidData(format!(
                    "reference targets unknown file {}",
                    file_id.get()
                ))
            })?
            .references
            .push(Reference {
                kind,
                name,
                file_id,
                range,
            });
    }
    Ok(())
}

fn load_navigation_positions(
    connection: &Connection,
) -> Result<BTreeMap<(SourceFileId, TextRange), PositionRange>, VanillaCacheError> {
    let mut statement = connection.prepare(
        "SELECT file_id, range_start, range_end, start_line, start_character, end_line, end_character
         FROM navigation_positions ORDER BY file_id, range_start, range_end",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, i64>(6)?,
        ))
    })?;
    let mut positions = BTreeMap::new();
    for row in rows {
        let (file_id, start, end, start_line, start_character, end_line, end_character) = row?;
        let file_id = decode_file_id(&file_id)?;
        let range = decode_range(start, end)?;
        let start_line = decode_position_component(start_line, "start line")?;
        let start_character = decode_position_component(start_character, "start character")?;
        let end_line = decode_position_component(end_line, "end line")?;
        let end_character = decode_position_component(end_character, "end character")?;
        let position = PositionRange::new(
            Position::new(start_line, start_character),
            Position::new(end_line, end_character),
        );
        if positions.insert((file_id, range), position).is_some() {
            return Err(VanillaCacheError::InvalidData(
                "duplicate navigation position".to_owned(),
            ));
        }
    }
    Ok(positions)
}

fn decode_range(start: i64, end: i64) -> Result<TextRange, VanillaCacheError> {
    let start = u32::try_from(start)
        .map_err(|_| VanillaCacheError::InvalidData("range start exceeds u32".to_owned()))?;
    let end = u32::try_from(end)
        .map_err(|_| VanillaCacheError::InvalidData("range end exceeds u32".to_owned()))?;
    TextRange::new(start, end)
        .ok_or_else(|| VanillaCacheError::InvalidData("range end precedes start".to_owned()))
}

fn decode_position_component(value: i64, label: &str) -> Result<u32, VanillaCacheError> {
    u32::try_from(value).map_err(|_| VanillaCacheError::InvalidData(format!("{label} exceeds u32")))
}

fn encode_file_id(id: SourceFileId) -> Vec<u8> {
    id.get().to_be_bytes().to_vec()
}

fn decode_file_id(bytes: &[u8]) -> Result<SourceFileId, VanillaCacheError> {
    let bytes: [u8; 8] = bytes
        .try_into()
        .map_err(|_| VanillaCacheError::InvalidData("file id is not eight bytes".to_owned()))?;
    Ok(SourceFileId::new(u64::from_be_bytes(bytes)))
}

fn resolution_name(resolution: FileResolutionPolicy) -> &'static str {
    match resolution {
        FileResolutionPolicy::ReplaceByRelativePath => "replace-by-relative-path",
        FileResolutionPolicy::Merge => "merge",
        FileResolutionPolicy::ReplaceDirectory => "replace-directory",
    }
}

fn parse_resolution(value: &str) -> Result<FileResolutionPolicy, VanillaCacheError> {
    match value {
        "replace-by-relative-path" => Ok(FileResolutionPolicy::ReplaceByRelativePath),
        "merge" => Ok(FileResolutionPolicy::Merge),
        "replace-directory" => Ok(FileResolutionPolicy::ReplaceDirectory),
        value => Err(VanillaCacheError::InvalidData(format!(
            "unknown file resolution policy: {value}"
        ))),
    }
}

fn join_logical_path(root: &Path, logical: &LogicalPath) -> PathBuf {
    logical
        .as_str()
        .split('/')
        .fold(root.to_owned(), |path, component| path.join(component))
}

#[cfg(unix)]
fn encode_path(path: &Path) -> Result<(&'static str, Vec<u8>), VanillaCacheError> {
    use std::os::unix::ffi::OsStrExt;
    Ok(("unix-bytes-v1", path.as_os_str().as_bytes().to_vec()))
}

#[cfg(unix)]
fn decode_path(bytes: &[u8], encoding: &str) -> Result<PathBuf, VanillaCacheError> {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    if encoding != "unix-bytes-v1" {
        return Err(VanillaCacheError::InvalidData(format!(
            "cache path encoding {encoding} is not usable on this platform"
        )));
    }
    Ok(PathBuf::from(OsStr::from_bytes(bytes)))
}

#[cfg(windows)]
fn encode_path(path: &Path) -> Result<(&'static str, Vec<u8>), VanillaCacheError> {
    use std::os::windows::ffi::OsStrExt;
    let bytes = path
        .as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    Ok(("windows-utf16le-v1", bytes))
}

#[cfg(windows)]
fn decode_path(bytes: &[u8], encoding: &str) -> Result<PathBuf, VanillaCacheError> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    if encoding != "windows-utf16le-v1" || !bytes.len().is_multiple_of(2) {
        return Err(VanillaCacheError::InvalidData(format!(
            "cache path encoding {encoding} is not usable on this platform"
        )));
    }
    let units = bytes
        .chunks_exact(2)
        .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
        .collect::<Vec<_>>();
    Ok(PathBuf::from(OsString::from_wide(&units)))
}

#[cfg(not(any(unix, windows)))]
fn encode_path(path: &Path) -> Result<(&'static str, Vec<u8>), VanillaCacheError> {
    let value = path.to_str().ok_or_else(|| {
        VanillaCacheError::InvalidData("source root is not valid UTF-8".to_owned())
    })?;
    Ok(("utf8-v1", value.as_bytes().to_vec()))
}

#[cfg(not(any(unix, windows)))]
fn decode_path(bytes: &[u8], encoding: &str) -> Result<PathBuf, VanillaCacheError> {
    if encoding != "utf8-v1" {
        return Err(VanillaCacheError::InvalidData(format!(
            "cache path encoding {encoding} is not usable on this platform"
        )));
    }
    let value = std::str::from_utf8(bytes)
        .map_err(|_| VanillaCacheError::InvalidData("source root is not UTF-8".to_owned()))?;
    Ok(PathBuf::from(value))
}
