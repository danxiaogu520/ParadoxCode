//! Read, validate, and decode Vanilla cache databases.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;

use pdx_text::{LogicalPath, PositionRange, TextRange};
use rusqlite::{Connection, OpenFlags, OptionalExtension};

use crate::index::{
    Definition, FileIndexShard, LocalisationPreviewMap, MacroDefinitionSummary,
    MacroParameterSignature, PositionMap, Reference, WorkspaceIndex,
};
use crate::model::LocalisationPreview;
use crate::scan::stable_file_id;
use crate::{SourceFile, SourceFileId, SourceRoot, SourceRootId, WorkspaceScanToken};

use super::codec::{
    decode_file_id, decode_path, decode_range, join_logical_path, parse_resolution,
};
use super::position_codec;
use super::template_codec;
use super::{
    APPLICATION_ID, CURRENT_CACHE_SCHEMA_VERSION, IndexCache, IndexCacheError, IndexCacheMetadata,
    LoadedIndex, MAX_CACHE_BYTES, MAX_CACHE_FILES, MAX_CACHE_SYMBOLS, MAX_POSITION_PAYLOAD_BYTES,
    MAX_TEXT_FIELD_BYTES, MIN_SUPPORTED_CACHE_SCHEMA_VERSION, parse_root_kind,
};

/// Row-count and text-length limits per table, in validation order.
const TABLE_LIMITS: [(&str, usize, &str); 7] = [
    (
        "source_files",
        MAX_CACHE_FILES,
        "logical_path, category_id, resolution, fingerprint",
    ),
    ("definitions", MAX_CACHE_SYMBOLS, "kind, name"),
    ("symbol_references", MAX_CACHE_SYMBOLS, "kind, name"),
    ("macro_definitions", MAX_CACHE_SYMBOLS, "kind, name"),
    ("macro_parameters", MAX_CACHE_SYMBOLS, "name"),
    // Position payloads have their own byte budget below; the row count is per file.
    ("navigation_positions", MAX_CACHE_FILES, ""),
    (
        "localisation_previews",
        MAX_CACHE_SYMBOLS,
        "range_start, range_end, language, value",
    ),
];

pub(super) fn load_cancellable(
    path: &Path,
    cancellation: &WorkspaceScanToken,
) -> Result<IndexCache, IndexCacheError> {
    load_cancellable_with(path, cancellation, true, None)
}

/// Loads a cache while skipping the derivation of symbol lookup maps.
///
/// The returned cache is only suitable for immediate installation: `install_index_cache`
/// merges the shards with the workspace and rebuilds the maps once, so the maps derived here
/// would be discarded. Validation is identical to [`load_cancellable`].
pub(super) fn load_cancellable_for_install(
    path: &Path,
    cancellation: &WorkspaceScanToken,
) -> Result<IndexCache, IndexCacheError> {
    load_cancellable_with(path, cancellation, false, None)
}

/// [`load_cancellable_for_install`] with `(done, total)` row-level progress reports.
///
/// The totals are derived from the table-limit validation pass, so the first report fires
/// before any row is materialized and the final report lands after cross-table validation.
pub(super) fn load_cancellable_for_install_with_progress(
    path: &Path,
    cancellation: &WorkspaceScanToken,
    progress: Option<&(dyn Fn(usize, usize) + Sync)>,
) -> Result<IndexCache, IndexCacheError> {
    load_cancellable_with(path, cancellation, false, progress)
}

fn load_cancellable_with(
    path: &Path,
    cancellation: &WorkspaceScanToken,
    build_lookup_maps: bool,
    progress: Option<&(dyn Fn(usize, usize) + Sync)>,
) -> Result<IndexCache, IndexCacheError> {
    if cancellation.is_cancelled() {
        return Err(IndexCacheError::Cancelled);
    }
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() {
        return Err(IndexCacheError::InvalidData(format!(
            "cache is not a regular file: {}",
            path.display()
        )));
    }
    if metadata.len() > MAX_CACHE_BYTES {
        return Err(IndexCacheError::LimitExceeded(
            "cache byte",
            usize::try_from(MAX_CACHE_BYTES).unwrap_or(usize::MAX),
        ));
    }
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let cancellation = cancellation.clone();
    let _ = connection.progress_handler(1_000, Some(move || cancellation.is_cancelled()));
    load_connection(&connection, build_lookup_maps, progress).map_err(map_interrupted)
}

fn load_connection(
    connection: &Connection,
    build_lookup_maps: bool,
    progress: Option<&(dyn Fn(usize, usize) + Sync)>,
) -> Result<IndexCache, IndexCacheError> {
    validate_database_identity(connection)?;
    let schema_version = metadata_text(connection, "schema_version")?
        .parse::<u32>()
        .map_err(|_| IndexCacheError::InvalidMetadata("schema_version"))?;
    if !(MIN_SUPPORTED_CACHE_SCHEMA_VERSION..=CURRENT_CACHE_SCHEMA_VERSION)
        .contains(&schema_version)
    {
        return Err(IndexCacheError::UnsupportedSchema(schema_version));
    }
    let table_counts = validate_table_limits(connection)?;
    // Every loaded row plus the derived cross-table validation work: the known-range set
    // holds one entry per definition and reference, and every navigation position and
    // localisation preview is checked once against it.
    let [
        files,
        definitions,
        references,
        macros,
        macro_parameters,
        positions,
        previews,
    ] = table_counts;
    let total = files
        + definitions
        + references
        + macros
        + macro_parameters
        + positions
        + previews
        + definitions
        + references
        + positions
        + previews;
    let mut progress = LoadProgress {
        callback: progress,
        total,
        done: 0,
    };
    progress.report(0);
    let source_root = decode_path(
        &metadata_blob(connection, "source_root")?,
        &metadata_text(connection, "path_encoding")?,
    )?;
    // Schema 7 records the cached source root identity; older caches are rebuilt once.
    let root_id = u32::from_le_bytes(
        metadata_blob(connection, "root_id")?
            .try_into()
            .map_err(|_| IndexCacheError::InvalidMetadata("root_id"))?,
    );
    let root = SourceRoot::new(
        SourceRootId::new(root_id),
        parse_root_kind(&metadata_text(connection, "root_kind")?)?,
        source_root,
    );
    let game_id = metadata_text(connection, "game_id")?;
    let rule_hash = metadata_text(connection, "rule_hash")?;
    let source_identity = metadata_text(connection, "source_identity")?;
    let source_fingerprint = metadata_text(connection, "source_fingerprint")?;
    let created_unix_seconds = metadata_text(connection, "created_unix_seconds")?
        .parse::<u64>()
        .map_err(|_| IndexCacheError::InvalidMetadata("created_unix_seconds"))?;
    let indexed_files = metadata_text(connection, "indexed_files")?
        .parse::<usize>()
        .map_err(|_| IndexCacheError::InvalidMetadata("indexed_files"))?;
    let (source_files, index, positions, localisation_previews, file_fingerprints) =
        load_index(connection, &root, build_lookup_maps, &mut progress)?;
    if indexed_files != source_files.len() {
        return Err(IndexCacheError::InvalidData(format!(
            "metadata records {indexed_files} files but cache contains {}",
            source_files.len()
        )));
    }
    let mut index = index;
    index.replace_all_position_ranges(positions);
    Ok(IndexCache {
        metadata: IndexCacheMetadata {
            schema_version,
            game_id,
            rule_hash,
            source_identity,
            source_fingerprint,
            created_unix_seconds,
            indexed_files,
        },
        root,
        source_files,
        index,
        localisation_previews,
        file_fingerprints,
    })
}

fn map_interrupted(error: IndexCacheError) -> IndexCacheError {
    match error {
        IndexCacheError::Sql(ref sqlite)
            if sqlite.sqlite_error_code() == Some(rusqlite::ErrorCode::OperationInterrupted) =>
        {
            IndexCacheError::Cancelled
        }
        error => error,
    }
}

pub(super) fn validate_database_identity(connection: &Connection) -> Result<(), IndexCacheError> {
    let application_id =
        connection.pragma_query_value(None, "application_id", |row| row.get::<_, i32>(0))?;
    if application_id != APPLICATION_ID {
        return Err(IndexCacheError::NotIndexCache);
    }
    Ok(())
}

fn metadata_blob(connection: &Connection, key: &'static str) -> Result<Vec<u8>, IndexCacheError> {
    let length = connection
        .query_row(
            "SELECT length(value) FROM metadata WHERE key = ?1",
            [key],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .ok_or(IndexCacheError::InvalidMetadata(key))?;
    if length < 0 || usize::try_from(length).map_or(true, |length| length > MAX_TEXT_FIELD_BYTES) {
        return Err(IndexCacheError::LimitExceeded(
            "metadata field byte",
            MAX_TEXT_FIELD_BYTES,
        ));
    }
    connection
        .query_row("SELECT value FROM metadata WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .optional()?
        .ok_or(IndexCacheError::InvalidMetadata(key))
}

fn metadata_text(connection: &Connection, key: &'static str) -> Result<String, IndexCacheError> {
    let bytes = metadata_blob(connection, key)?;
    String::from_utf8(bytes).map_err(|_| IndexCacheError::InvalidMetadata(key))
}

/// Shared row-progress accounting for one cache load.
struct LoadProgress<'a> {
    callback: Option<&'a (dyn Fn(usize, usize) + Sync)>,
    total: usize,
    done: usize,
}

impl LoadProgress<'_> {
    /// Adds completed work units and forwards a `(done, total)` report.
    fn report(&mut self, added: usize) {
        self.done = self.done.saturating_add(added);
        if let Some(callback) = self.callback {
            callback(self.done, self.total);
        }
    }
}

fn validate_table_limits(connection: &Connection) -> Result<[usize; 7], IndexCacheError> {
    // One scan per table returns both the row count and the longest text field, so the
    // bounds checks never rescan a table. The order matches TABLE_LIMITS so failures can
    // name the offending table statically. Navigation payloads have their own budget.
    let mut counts = [0usize; 7];
    for (index, (table, limit, fields)) in TABLE_LIMITS.iter().enumerate() {
        let (count, max) = if fields.is_empty() {
            let count =
                connection.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get::<_, i64>(0)
                })?;
            (count, 0i64)
        } else {
            let fields = fields
                .split(", ")
                .map(|field| format!("COALESCE(length({field}), 0)"))
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT count(*), COALESCE(MAX(max_length), 0) FROM (SELECT max({fields}) AS max_length FROM {table})"
            );
            connection.query_row(&query, [], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })?
        };
        let limit = *limit;
        if count < 0 || usize::try_from(count).map_or(true, |count| count > limit) {
            return Err(IndexCacheError::LimitExceeded(TABLE_LIMITS[index].0, limit));
        }
        counts[index] = usize::try_from(count).map_err(|_| {
            IndexCacheError::InvalidData("table row count exceeds platform usize".to_owned())
        })?;
        if max < 0 || usize::try_from(max).map_or(true, |max| max > MAX_TEXT_FIELD_BYTES) {
            return Err(IndexCacheError::LimitExceeded(
                "text field byte",
                MAX_TEXT_FIELD_BYTES,
            ));
        }
    }
    let max_template = connection.query_row(
        "SELECT COALESCE(MAX(length(template_payload)), 0) FROM macro_definitions",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if max_template < 0
        || usize::try_from(max_template).map_or(true, |max| max > super::MAX_MACRO_TEMPLATE_BYTES)
    {
        return Err(IndexCacheError::LimitExceeded(
            "macro template byte",
            super::MAX_MACRO_TEMPLATE_BYTES,
        ));
    }
    let max_position_payload = connection.query_row(
        "SELECT COALESCE(MAX(length(payload)), 0) FROM navigation_positions",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if max_position_payload < 0
        || u64::try_from(max_position_payload)
            .map_or(true, |bytes| bytes > MAX_POSITION_PAYLOAD_BYTES)
    {
        return Err(IndexCacheError::LimitExceeded(
            "navigation position payload byte",
            usize::try_from(MAX_POSITION_PAYLOAD_BYTES).unwrap_or(usize::MAX),
        ));
    }
    Ok(counts)
}

fn load_index(
    connection: &Connection,
    root: &SourceRoot,
    build_lookup_maps: bool,
    progress: &mut LoadProgress<'_>,
) -> Result<LoadedIndex, IndexCacheError> {
    let mut source_files = BTreeMap::new();
    let mut shards = BTreeMap::new();
    let mut file_fingerprints = BTreeMap::new();
    let mut statement = connection.prepare(
        "SELECT file_id, logical_path, category_id, resolution, syntax_error_count, fingerprint
         FROM source_files ORDER BY file_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, String>(5)?,
        ))
    })?;
    for row in rows {
        let (id, logical_path, category_id, resolution, syntax_error_count, fingerprint) = row?;
        let id = decode_file_id(&id)?;
        if fingerprint.len() != 64 {
            return Err(IndexCacheError::InvalidData(format!(
                "file {} has a malformed fingerprint",
                id.get()
            )));
        }
        let logical_path = LogicalPath::parse(&logical_path)
            .map_err(|error| IndexCacheError::InvalidData(error.to_string()))?;
        if stable_file_id(root.id, &logical_path) != id.get() {
            return Err(IndexCacheError::InvalidData(format!(
                "file id does not match logical path {}",
                logical_path.as_str()
            )));
        }
        let syntax_error_count = usize::try_from(syntax_error_count)
            .map_err(|_| IndexCacheError::InvalidData("negative syntax error count".to_owned()))?;
        let file = SourceFile {
            id,
            root_id: root.id,
            physical_path: join_logical_path(&root.path, &logical_path),
            logical_path,
            category_id,
            resolution: parse_resolution(&resolution)?,
        };
        if source_files.insert(id, file).is_some() {
            return Err(IndexCacheError::InvalidData(format!(
                "duplicate source file id {}",
                id.get()
            )));
        }
        if file_fingerprints.insert(id, fingerprint).is_some() {
            return Err(IndexCacheError::InvalidData(format!(
                "duplicate source file fingerprint for {}",
                id.get()
            )));
        }
        shards.insert(
            id,
            FileIndexShard {
                file_id: id,
                definitions: Vec::new(),
                references: Vec::new(),
                macro_definitions: Vec::new(),
                syntax_error_count,
            },
        );
    }
    progress.report(source_files.len());
    let definition_count = load_definitions(connection, &mut shards)?;
    progress.report(definition_count);
    let macro_count = load_macro_definitions(connection, &mut shards)?;
    progress.report(macro_count);
    let reference_count = load_references(connection, &mut shards)?;
    progress.report(reference_count);
    // Membership sets replace per-position linear scans of a shard's symbol vectors, which
    // are quadratic for files with tens of thousands of symbols (the EU4 localisation
    // files). Validation semantics are identical: a position range must belong to a
    // definition or reference of the same file, and a preview range must belong to a
    // localisation definition of the same file.
    let mut known_ranges = HashSet::with_capacity(definition_count.saturating_add(reference_count));
    let mut localisation_ranges = HashSet::with_capacity(definition_count);
    for shard in shards.values() {
        for definition in &shard.definitions {
            known_ranges.insert((definition.file_id, definition.range));
            if definition.kind.eq_ignore_ascii_case("localisation") {
                localisation_ranges.insert((definition.file_id, definition.range));
            }
        }
        for reference in &shard.references {
            known_ranges.insert((reference.file_id, reference.range));
        }
    }
    progress.report(definition_count.saturating_add(reference_count));
    let localisation_previews =
        load_localisation_previews(connection, &shards, &localisation_ranges)?;
    drop(localisation_ranges);
    progress.report(localisation_previews.len());
    let positions = load_navigation_positions(connection)?;
    progress.report(positions.len());
    for ((file_id, range), position) in &positions {
        if !known_ranges.contains(&(file_id, range)) {
            return Err(IndexCacheError::InvalidData(format!(
                "navigation position references unknown range {}..{}",
                range.start(),
                range.end()
            )));
        }
        if position.start > position.end {
            return Err(IndexCacheError::InvalidData(
                "navigation position end precedes start".to_owned(),
            ));
        }
    }
    drop(known_ranges);
    progress.report(positions.len());
    Ok((
        source_files,
        if build_lookup_maps {
            WorkspaceIndex::from_shards(shards.into_values())
        } else {
            // Installation merges these shards and rebuilds the maps once; skip the throwaway
            // derivation here and keep only the shards and the position table.
            let mut index = WorkspaceIndex::empty();
            index.shards = shards;
            index
        },
        positions,
        localisation_previews,
        file_fingerprints,
    ))
}

fn load_macro_definitions(
    connection: &Connection,
    shards: &mut BTreeMap<SourceFileId, FileIndexShard>,
) -> Result<usize, IndexCacheError> {
    let mut rows_loaded = 0usize;
    let mut statement = connection.prepare(
        "SELECT file_id, ordinal, kind, name, definition_range_start, definition_range_end, template_payload
         FROM macro_definitions ORDER BY file_id, ordinal",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, Option<Vec<u8>>>(6)?,
        ))
    })?;
    for row in rows {
        let (file_id, ordinal, kind, name, start, end, template_payload) = row?;
        let file_id = decode_file_id(&file_id)?;
        let ordinal = usize::try_from(ordinal)
            .map_err(|_| IndexCacheError::InvalidData("negative macro ordinal".to_owned()))?;
        let definition_range = decode_range(start, end)?;
        let shard = shards.get_mut(&file_id).ok_or_else(|| {
            IndexCacheError::InvalidData(format!(
                "macro definition references unknown file {}",
                file_id.get()
            ))
        })?;
        if ordinal != shard.macro_definitions.len() {
            return Err(IndexCacheError::InvalidData(
                "macro definition ordinals are not contiguous".to_owned(),
            ));
        }
        if shard.macro_definitions.iter().any(|summary| {
            summary.kind.eq_ignore_ascii_case(&kind)
                && summary.name.eq_ignore_ascii_case(&name)
                && summary.definition_range == definition_range
        }) {
            return Err(IndexCacheError::InvalidData(format!(
                "duplicate macro summary {kind} `{name}`"
            )));
        }
        if !shard.definitions.iter().any(|definition| {
            definition.kind.eq_ignore_ascii_case(&kind)
                && definition.name.eq_ignore_ascii_case(&name)
                && definition.range == definition_range
        }) {
            return Err(IndexCacheError::InvalidData(format!(
                "macro summary {kind} `{name}` has no matching definition"
            )));
        }
        let template = template_payload
            .as_deref()
            .map(|payload| template_codec::decode(payload, &kind, &name, definition_range))
            .transpose()?;
        shard.macro_definitions.push(MacroDefinitionSummary {
            kind,
            name,
            definition_range,
            parameters: Vec::new(),
            template,
        });
        rows_loaded = rows_loaded.saturating_add(1);
    }
    drop(statement);

    let mut statement = connection.prepare(
        "SELECT file_id, macro_ordinal, ordinal, name, required
         FROM macro_parameters ORDER BY file_id, macro_ordinal, ordinal",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
        ))
    })?;
    for row in rows {
        let (file_id, macro_ordinal, ordinal, name, required) = row?;
        let file_id = decode_file_id(&file_id)?;
        let macro_ordinal = usize::try_from(macro_ordinal)
            .map_err(|_| IndexCacheError::InvalidData("negative macro ordinal".to_owned()))?;
        let ordinal = usize::try_from(ordinal).map_err(|_| {
            IndexCacheError::InvalidData("negative macro parameter ordinal".to_owned())
        })?;
        let required = match required {
            0 => false,
            1 => true,
            _ => {
                return Err(IndexCacheError::InvalidData(
                    "macro parameter required flag is not boolean".to_owned(),
                ));
            }
        };
        let summary = shards
            .get_mut(&file_id)
            .and_then(|shard| shard.macro_definitions.get_mut(macro_ordinal))
            .ok_or_else(|| {
                IndexCacheError::InvalidData("macro parameter has no owner".to_owned())
            })?;
        if ordinal != summary.parameters.len() {
            return Err(IndexCacheError::InvalidData(
                "macro parameter ordinals are not contiguous".to_owned(),
            ));
        }
        if name.is_empty()
            || summary
                .parameters
                .iter()
                .any(|parameter| parameter.name.eq_ignore_ascii_case(&name))
        {
            return Err(IndexCacheError::InvalidData(
                "macro parameter name is empty or duplicated".to_owned(),
            ));
        }
        summary
            .parameters
            .push(MacroParameterSignature { name, required });
        rows_loaded = rows_loaded.saturating_add(1);
    }
    Ok(rows_loaded)
}

fn load_localisation_previews(
    connection: &Connection,
    shards: &BTreeMap<SourceFileId, FileIndexShard>,
    localisation_ranges: &HashSet<(SourceFileId, TextRange)>,
) -> Result<LocalisationPreviewMap, IndexCacheError> {
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
    let mut grouped = BTreeMap::<SourceFileId, Vec<(TextRange, LocalisationPreview)>>::new();
    let mut rows_loaded = 0usize;
    for row in rows {
        let (file_id, start, end, language, value) = row?;
        let file_id = decode_file_id(&file_id)?;
        let range = decode_range(start, end)?;
        if shards.get(&file_id).is_none() {
            return Err(IndexCacheError::InvalidData(format!(
                "localisation preview references unknown file {}",
                file_id.get()
            )));
        }
        if !localisation_ranges.contains(&(file_id, range)) {
            return Err(IndexCacheError::InvalidData(format!(
                "localisation preview range {}..{} is not a localisation definition",
                range.start(),
                range.end()
            )));
        }
        if value.is_empty() {
            return Err(IndexCacheError::InvalidData(
                "localisation preview value is empty".to_owned(),
            ));
        }
        grouped
            .entry(file_id)
            .or_default()
            .push((range, LocalisationPreview { language, value }));
        rows_loaded = rows_loaded.saturating_add(1);
    }
    let previews = LocalisationPreviewMap::from_grouped(grouped);
    if previews.len() != rows_loaded {
        return Err(IndexCacheError::InvalidData(
            "duplicate localisation preview".to_owned(),
        ));
    }
    Ok(previews)
}

fn load_definitions(
    connection: &Connection,
    shards: &mut BTreeMap<SourceFileId, FileIndexShard>,
) -> Result<usize, IndexCacheError> {
    let mut rows_loaded = 0usize;
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
                return Err(IndexCacheError::InvalidData(
                    "definition active flag is not boolean".to_owned(),
                ));
            }
        };
        shards
            .get_mut(&file_id)
            .ok_or_else(|| {
                IndexCacheError::InvalidData(format!(
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
        rows_loaded = rows_loaded.saturating_add(1);
    }
    Ok(rows_loaded)
}

fn load_references(
    connection: &Connection,
    shards: &mut BTreeMap<SourceFileId, FileIndexShard>,
) -> Result<usize, IndexCacheError> {
    let mut rows_loaded = 0usize;
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
                IndexCacheError::InvalidData(format!(
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
        rows_loaded = rows_loaded.saturating_add(1);
    }
    Ok(rows_loaded)
}

fn load_navigation_positions(connection: &Connection) -> Result<PositionMap, IndexCacheError> {
    let mut statement =
        connection.prepare("SELECT file_id, payload FROM navigation_positions ORDER BY file_id")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
    })?;
    let mut grouped = BTreeMap::<SourceFileId, Vec<(TextRange, PositionRange)>>::new();
    for row in rows {
        let (file_id, payload) = row?;
        let file_id = decode_file_id(&file_id)?;
        grouped
            .entry(file_id)
            .or_default()
            .extend(position_codec::decode(&payload)?);
    }
    let expected = grouped.values().map(Vec::len).sum::<usize>();
    let positions = PositionMap::from_grouped(grouped);
    // `PositionMap` keeps the last value when a duplicate range is supplied. A duplicate is
    // malformed cache data, so compare the compacted count with the decoded row count.
    if positions.len() != expected {
        return Err(IndexCacheError::InvalidData(
            "duplicate navigation position".to_owned(),
        ));
    }
    Ok(positions)
}
