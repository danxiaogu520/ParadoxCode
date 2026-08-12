//! Read, validate, and decode Vanilla cache databases.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use pdx_text::{LogicalPath, Position, PositionRange, TextRange};
use rusqlite::{Connection, OpenFlags, OptionalExtension};

use crate::index::{
    Definition, FileIndexShard, MacroDefinitionSummary, MacroParameterSignature, Reference,
    WorkspaceIndex,
};
use crate::model::LocalisationPreview;
use crate::scan::stable_file_id;
use crate::{SourceFile, SourceFileId, SourceRoot, SourceRootKind, WorkspaceScanToken};

use super::codec::{
    decode_file_id, decode_path, decode_position_component, decode_range, join_logical_path,
    parse_resolution,
};
use super::template_codec;
use super::{
    APPLICATION_ID, CURRENT_VANILLA_CACHE_SCHEMA_VERSION, LoadedIndex, MAX_CACHE_BYTES,
    MAX_CACHE_FILES, MAX_CACHE_SYMBOLS, MAX_TEXT_FIELD_BYTES, VANILLA_ROOT_ID, VanillaCacheError,
    VanillaIndexCache, VanillaIndexCacheMetadata,
};

/// Row-count and text-length limits per table, in validation order.
const TABLE_LIMITS: [(&str, usize, &str); 7] = [
    (
        "source_files",
        MAX_CACHE_FILES,
        "logical_path, category_id, resolution",
    ),
    ("definitions", MAX_CACHE_SYMBOLS, "kind, name"),
    ("symbol_references", MAX_CACHE_SYMBOLS, "kind, name"),
    ("macro_definitions", MAX_CACHE_SYMBOLS, "kind, name"),
    ("macro_parameters", MAX_CACHE_SYMBOLS, "name"),
    (
        "navigation_positions",
        MAX_CACHE_SYMBOLS,
        "range_start, range_end, start_line, start_character, end_line, end_character",
    ),
    (
        "localisation_previews",
        MAX_CACHE_SYMBOLS,
        "range_start, range_end, language, value",
    ),
];

pub(super) fn load_cancellable(
    path: &Path,
    cancellation: &WorkspaceScanToken,
) -> Result<VanillaIndexCache, VanillaCacheError> {
    load_cancellable_with(path, cancellation, true)
}

/// Loads a cache while skipping the derivation of symbol lookup maps.
///
/// The returned cache is only suitable for immediate installation: `install_vanilla_cache`
/// merges the shards with the workspace and rebuilds the maps once, so the maps derived here
/// would be discarded. Validation is identical to [`load_cancellable`].
pub(super) fn load_cancellable_for_install(
    path: &Path,
    cancellation: &WorkspaceScanToken,
) -> Result<VanillaIndexCache, VanillaCacheError> {
    load_cancellable_with(path, cancellation, false)
}

fn load_cancellable_with(
    path: &Path,
    cancellation: &WorkspaceScanToken,
    build_lookup_maps: bool,
) -> Result<VanillaIndexCache, VanillaCacheError> {
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
    load_connection(&connection, build_lookup_maps).map_err(map_interrupted)
}

fn load_connection(
    connection: &Connection,
    build_lookup_maps: bool,
) -> Result<VanillaIndexCache, VanillaCacheError> {
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
        load_index(connection, &source_root, build_lookup_maps)?;
    if indexed_files != source_files.len() {
        return Err(VanillaCacheError::InvalidData(format!(
            "metadata records {indexed_files} files but cache contains {}",
            source_files.len()
        )));
    }
    let mut index = index;
    index.replace_all_position_ranges(positions);
    Ok(VanillaIndexCache {
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

pub(super) fn validate_database_identity(connection: &Connection) -> Result<(), VanillaCacheError> {
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
    // One scan per table returns both the row count and the longest text field, so the
    // bounds checks never rescan a table. The order matches TABLE_LIMITS so failures can
    // name the offending table statically.
    for (index, (table, limit, fields)) in TABLE_LIMITS.iter().enumerate() {
        let fields = fields
            .split(", ")
            .map(|field| format!("COALESCE(length({field}), 0)"))
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "SELECT count(*), COALESCE(MAX(max_length), 0) FROM (SELECT max({fields}) AS max_length FROM {table})"
        );
        let (count, max) = connection.query_row(&query, [], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })?;
        let limit = *limit;
        if count < 0 || usize::try_from(count).map_or(true, |count| count > limit) {
            return Err(VanillaCacheError::LimitExceeded(
                TABLE_LIMITS[index].0,
                limit,
            ));
        }
        if max < 0 || usize::try_from(max).map_or(true, |max| max > MAX_TEXT_FIELD_BYTES) {
            return Err(VanillaCacheError::LimitExceeded(
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
        return Err(VanillaCacheError::LimitExceeded(
            "macro template byte",
            super::MAX_MACRO_TEMPLATE_BYTES,
        ));
    }
    Ok(())
}

fn load_index(
    connection: &Connection,
    source_root: &Path,
    build_lookup_maps: bool,
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
                macro_definitions: Vec::new(),
                syntax_error_count,
            },
        );
    }
    load_definitions(connection, &mut shards)?;
    load_macro_definitions(connection, &mut shards)?;
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
    ))
}

fn load_macro_definitions(
    connection: &Connection,
    shards: &mut BTreeMap<SourceFileId, FileIndexShard>,
) -> Result<(), VanillaCacheError> {
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
            .map_err(|_| VanillaCacheError::InvalidData("negative macro ordinal".to_owned()))?;
        let definition_range = decode_range(start, end)?;
        let shard = shards.get_mut(&file_id).ok_or_else(|| {
            VanillaCacheError::InvalidData(format!(
                "macro definition references unknown file {}",
                file_id.get()
            ))
        })?;
        if ordinal != shard.macro_definitions.len() {
            return Err(VanillaCacheError::InvalidData(
                "macro definition ordinals are not contiguous".to_owned(),
            ));
        }
        if shard.macro_definitions.iter().any(|summary| {
            summary.kind.eq_ignore_ascii_case(&kind)
                && summary.name.eq_ignore_ascii_case(&name)
                && summary.definition_range == definition_range
        }) {
            return Err(VanillaCacheError::InvalidData(format!(
                "duplicate macro summary {kind} `{name}`"
            )));
        }
        if !shard.definitions.iter().any(|definition| {
            definition.kind.eq_ignore_ascii_case(&kind)
                && definition.name.eq_ignore_ascii_case(&name)
                && definition.range == definition_range
        }) {
            return Err(VanillaCacheError::InvalidData(format!(
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
            .map_err(|_| VanillaCacheError::InvalidData("negative macro ordinal".to_owned()))?;
        let ordinal = usize::try_from(ordinal).map_err(|_| {
            VanillaCacheError::InvalidData("negative macro parameter ordinal".to_owned())
        })?;
        let required = match required {
            0 => false,
            1 => true,
            _ => {
                return Err(VanillaCacheError::InvalidData(
                    "macro parameter required flag is not boolean".to_owned(),
                ));
            }
        };
        let summary = shards
            .get_mut(&file_id)
            .and_then(|shard| shard.macro_definitions.get_mut(macro_ordinal))
            .ok_or_else(|| {
                VanillaCacheError::InvalidData("macro parameter has no owner".to_owned())
            })?;
        if ordinal != summary.parameters.len() {
            return Err(VanillaCacheError::InvalidData(
                "macro parameter ordinals are not contiguous".to_owned(),
            ));
        }
        if name.is_empty()
            || summary
                .parameters
                .iter()
                .any(|parameter| parameter.name.eq_ignore_ascii_case(&name))
        {
            return Err(VanillaCacheError::InvalidData(
                "macro parameter name is empty or duplicated".to_owned(),
            ));
        }
        summary
            .parameters
            .push(MacroParameterSignature { name, required });
    }
    Ok(())
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
