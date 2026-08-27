//! Transactional Vanilla cache writes.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use pdx_text::{PositionRange, TextRange};
use rusqlite::{Connection, Transaction, params};

use super::codec::{encode_file_id, encode_path, resolution_name};
use super::{APPLICATION_ID, CURRENT_CACHE_SCHEMA_VERSION, IndexCache, IndexCacheError};
use super::{position_codec, read::validate_database_identity, template_codec};
use crate::SourceFileId;

pub(super) fn save(cache: &IndexCache, path: &Path) -> Result<(), IndexCacheError> {
    save_with_progress(cache, path, None)
}

/// [`save`] with per-source-file `(done, total)` progress reports.
///
/// The total is the cached source-file count, matching the scan progress that precedes the
/// save during a background rebuild; the position and preview tables are written after the
/// final report.
pub(super) fn save_with_progress(
    cache: &IndexCache,
    path: &Path,
    progress: Option<&(dyn Fn(usize, usize) + Sync)>,
) -> Result<(), IndexCacheError> {
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
    } else {
        // Fresh databases are created in incremental auto-vacuum mode so repeated rebuilds
        // reclaim dropped pages with a cheap `incremental_vacuum` instead of growing the file.
        connection.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;
    }
    let transaction = connection.transaction()?;
    write_cache(&transaction, cache, progress)?;
    transaction.commit()?;
    trim_free_pages(&connection)?;
    Ok(())
}

/// Reclaims free pages left behind by the DROP-and-rebuild transaction.
///
/// Dropped pages that the rebuild cannot reuse are scattered through the file, so only a full
/// `VACUUM` (which also converts the database to incremental auto-vacuum for later rebuilds)
/// actually shrinks it; it runs only when the freelist is large enough to matter. This is
/// best-effort after the commit: a failed trim must not report a successful save as failed,
/// the file is still complete and valid.
fn trim_free_pages(connection: &Connection) -> Result<(), IndexCacheError> {
    let freelist: i64 = connection.query_row("PRAGMA freelist_count", [], |row| row.get(0))?;
    if freelist <= 0 {
        return Ok(());
    }
    let page_count: i64 = connection.query_row("PRAGMA page_count", [], |row| row.get(0))?;
    let page_size: i64 = connection.query_row("PRAGMA page_size", [], |row| row.get(0))?;
    let bytes = freelist.saturating_mul(page_size);
    let percent = freelist.saturating_mul(100) / page_count.max(1);
    if bytes >= super::FREELIST_TRIM_THRESHOLD_BYTES && percent >= super::FREELIST_TRIM_MIN_PERCENT
    {
        // `VACUUM` honors the current auto-vacuum setting, so legacy databases are
        // converted to incremental mode while being compacted.
        let _ = connection.pragma_update(None, "auto_vacuum", "INCREMENTAL");
        let _ = connection.execute_batch("VACUUM");
    }
    Ok(())
}

fn write_cache(
    transaction: &Transaction<'_>,
    cache: &IndexCache,
    progress: Option<&(dyn Fn(usize, usize) + Sync)>,
) -> Result<(), IndexCacheError> {
    transaction.execute_batch(
        "DROP TABLE IF EXISTS macro_parameters;
         DROP TABLE IF EXISTS macro_definitions;
         DROP TABLE IF EXISTS symbol_references;
         DROP TABLE IF EXISTS navigation_positions;
         DROP TABLE IF EXISTS definitions;
         DROP TABLE IF EXISTS localisation_previews;
         DROP TABLE IF EXISTS source_files;
         DROP TABLE IF EXISTS metadata;
         -- Text values; platform path bytes may still be stored as BLOBs, which TEXT affinity preserves.
         CREATE TABLE metadata(key TEXT PRIMARY KEY, value TEXT NOT NULL);
         CREATE TABLE source_files(
             file_id BLOB PRIMARY KEY CHECK(length(file_id) = 8),
             logical_path TEXT NOT NULL,
             category_id TEXT,
             resolution TEXT NOT NULL,
             syntax_error_count INTEGER NOT NULL CHECK(syntax_error_count >= 0),
             fingerprint TEXT NOT NULL CHECK(length(fingerprint) = 64),
             metadata_fingerprint TEXT CHECK(metadata_fingerprint IS NULL OR length(metadata_fingerprint) = 64)
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
         CREATE TABLE macro_definitions(
             file_id BLOB NOT NULL REFERENCES source_files(file_id),
             ordinal INTEGER NOT NULL,
             kind TEXT NOT NULL,
             name TEXT NOT NULL,
             definition_range_start INTEGER NOT NULL,
             definition_range_end INTEGER NOT NULL,
             template_payload BLOB,
             PRIMARY KEY(file_id, ordinal)
         );
         CREATE TABLE macro_parameters(
             file_id BLOB NOT NULL,
             macro_ordinal INTEGER NOT NULL,
             ordinal INTEGER NOT NULL,
             name TEXT NOT NULL,
             required INTEGER NOT NULL CHECK(required IN (0, 1)),
             PRIMARY KEY(file_id, macro_ordinal, ordinal),
             FOREIGN KEY(file_id, macro_ordinal)
                 REFERENCES macro_definitions(file_id, ordinal)
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
             file_id BLOB PRIMARY KEY REFERENCES source_files(file_id),
             payload BLOB NOT NULL
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
    transaction.pragma_update(None, "user_version", CURRENT_CACHE_SCHEMA_VERSION)?;
    let (path_encoding, source_root) = encode_path(&cache.root.path)?;
    // Values are written as byte strings; path data (Windows UTF-16) can be non-UTF-8.
    // The column is declared TEXT for intent, and TEXT affinity preserves binary values.
    for (key, value) in [
        (
            "schema_version",
            cache.metadata.schema_version.to_string().into_bytes(),
        ),
        ("game_id", cache.metadata.game_id.clone().into_bytes()),
        ("rule_hash", cache.metadata.rule_hash.clone().into_bytes()),
        (
            "source_identity",
            cache.metadata.source_identity.clone().into_bytes(),
        ),
        (
            "source_fingerprint",
            cache.metadata.source_fingerprint.clone().into_bytes(),
        ),
        (
            "created_unix_seconds",
            cache.metadata.created_unix_seconds.to_string().into_bytes(),
        ),
        (
            "indexed_files",
            cache.metadata.indexed_files.to_string().into_bytes(),
        ),
        ("root_id", cache.root.id.get().to_le_bytes().to_vec()),
        (
            "root_kind",
            super::root_kind_name(cache.root.kind)
                .to_owned()
                .into_bytes(),
        ),
        ("path_encoding", path_encoding.to_owned().into_bytes()),
        ("source_root", source_root),
    ] {
        transaction.execute(
            "INSERT INTO metadata(key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
    }
    let mut insert_source_file = transaction.prepare(
        "INSERT INTO source_files(file_id, logical_path, category_id, resolution, syntax_error_count, fingerprint, metadata_fingerprint)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    let mut insert_definition = transaction.prepare(
        "INSERT INTO definitions(file_id, ordinal, kind, name, range_start, range_end, active)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    let mut insert_reference = transaction.prepare(
        "INSERT INTO symbol_references(file_id, ordinal, kind, name, range_start, range_end)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    let mut insert_macro = transaction.prepare(
        "INSERT INTO macro_definitions(file_id, ordinal, kind, name, definition_range_start, definition_range_end, template_payload)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    let mut insert_macro_parameter = transaction.prepare(
        "INSERT INTO macro_parameters(file_id, macro_ordinal, ordinal, name, required)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    let mut files_written = 0usize;
    for (id, file) in &cache.source_files {
        let shard = cache.index.shard(*id).ok_or_else(|| {
            IndexCacheError::InvalidData(format!(
                "source file {} has no index shard",
                file.logical_path.as_str()
            ))
        })?;
        files_written = files_written.saturating_add(1);
        let metadata_fingerprint = cache.file_metadata_fingerprints.get(id).ok_or_else(|| {
            IndexCacheError::InvalidData(format!(
                "no metadata fingerprint recorded for file {}",
                file.logical_path.as_str()
            ))
        })?;
        insert_source_file.execute(params![
            encode_file_id(*id),
            file.logical_path.as_str(),
            file.category_id,
            resolution_name(file.resolution),
            i64::try_from(shard.syntax_error_count).map_err(|_| {
                IndexCacheError::InvalidData("syntax error count exceeds SQLite range".into())
            })?,
            cache.file_fingerprints.get(id).ok_or_else(|| {
                IndexCacheError::InvalidData(format!(
                    "no content fingerprint recorded for file {}",
                    file.logical_path.as_str()
                ))
            })?,
            metadata_fingerprint.as_deref()
        ])?;
        for (ordinal, definition) in shard.definitions.iter().enumerate() {
            insert_definition.execute(params![
                encode_file_id(*id),
                i64::try_from(ordinal).unwrap_or(i64::MAX),
                definition.kind,
                definition.name,
                i64::from(definition.range.start()),
                i64::from(definition.range.end()),
                i64::from(definition.active)
            ])?;
        }
        for (ordinal, reference) in shard.references.iter().enumerate() {
            insert_reference.execute(params![
                encode_file_id(*id),
                i64::try_from(ordinal).unwrap_or(i64::MAX),
                reference.kind,
                reference.name,
                i64::from(reference.range.start()),
                i64::from(reference.range.end())
            ])?;
        }
        for (macro_ordinal, summary) in shard.macro_definitions.iter().enumerate() {
            if shard.macro_definitions[..macro_ordinal]
                .iter()
                .any(|candidate| {
                    candidate.kind.eq_ignore_ascii_case(&summary.kind)
                        && candidate.name.eq_ignore_ascii_case(&summary.name)
                        && candidate.definition_range == summary.definition_range
                })
            {
                return Err(IndexCacheError::InvalidData(format!(
                    "duplicate macro summary {} `{}`",
                    summary.kind, summary.name
                )));
            }
            if !shard.definitions.iter().any(|definition| {
                definition.kind.eq_ignore_ascii_case(&summary.kind)
                    && definition.name.eq_ignore_ascii_case(&summary.name)
                    && definition.range == summary.definition_range
            }) {
                return Err(IndexCacheError::InvalidData(format!(
                    "macro summary {} `{}` has no matching definition",
                    summary.kind, summary.name
                )));
            }
            let template_payload = summary
                .template
                .as_ref()
                .map(|template| {
                    if !template.kind.eq_ignore_ascii_case(&summary.kind)
                        || !template.name.eq_ignore_ascii_case(&summary.name)
                        || template.definition_range != summary.definition_range
                    {
                        return Err(IndexCacheError::InvalidData(format!(
                            "macro template identity does not match {} `{}`",
                            summary.kind, summary.name
                        )));
                    }
                    template_codec::encode(template)
                })
                .transpose()?;
            insert_macro.execute(params![
                encode_file_id(*id),
                i64::try_from(macro_ordinal).unwrap_or(i64::MAX),
                summary.kind,
                summary.name,
                i64::from(summary.definition_range.start()),
                i64::from(summary.definition_range.end()),
                template_payload,
            ])?;
            for (ordinal, parameter) in summary.parameters.iter().enumerate() {
                if parameter.name.is_empty()
                    || summary.parameters[..ordinal]
                        .iter()
                        .any(|candidate| candidate.name.eq_ignore_ascii_case(&parameter.name))
                {
                    return Err(IndexCacheError::InvalidData(format!(
                        "macro parameter name in {} `{}` is empty or duplicated",
                        summary.kind, summary.name
                    )));
                }
                insert_macro_parameter.execute(params![
                    encode_file_id(*id),
                    i64::try_from(macro_ordinal).unwrap_or(i64::MAX),
                    i64::try_from(ordinal).unwrap_or(i64::MAX),
                    parameter.name,
                    i64::from(parameter.required),
                ])?;
            }
        }
        if let Some(progress) = progress {
            progress(files_written, cache.source_files.len());
        }
    }
    drop(insert_source_file);
    drop(insert_definition);
    drop(insert_reference);
    drop(insert_macro);
    drop(insert_macro_parameter);
    let mut by_file: BTreeMap<SourceFileId, Vec<(TextRange, PositionRange)>> = BTreeMap::new();
    for ((file_id, range), position) in cache.index.position_ranges() {
        if !cache.source_files.contains_key(&file_id) {
            return Err(IndexCacheError::InvalidData(format!(
                "navigation position references unknown file {}",
                file_id.get()
            )));
        }
        by_file.entry(file_id).or_default().push((range, *position));
    }
    let mut insert_position = transaction
        .prepare("INSERT INTO navigation_positions(file_id, payload) VALUES (?1, ?2)")?;
    for (file_id, entries) in by_file {
        insert_position.execute(params![
            encode_file_id(file_id),
            position_codec::encode(&entries)?
        ])?;
    }
    drop(insert_position);
    let mut insert_preview = transaction.prepare(
        "INSERT INTO localisation_previews(file_id, range_start, range_end, language, value)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for ((file_id, range), preview) in &cache.localisation_previews {
        let Some(file) = cache.source_files.get(&file_id) else {
            return Err(IndexCacheError::InvalidData(format!(
                "localisation preview references unknown file {}",
                file_id.get()
            )));
        };
        let Some(shard) = cache.index.shard(file_id) else {
            return Err(IndexCacheError::InvalidData(format!(
                "localisation preview file {} has no index shard",
                file.logical_path.as_str()
            )));
        };
        if !shard.definitions.iter().any(|definition| {
            definition.range == range && definition.kind.eq_ignore_ascii_case("localisation")
        }) {
            return Err(IndexCacheError::InvalidData(format!(
                "localisation preview range {}..{} is not a localisation definition",
                range.start(),
                range.end()
            )));
        }
        insert_preview.execute(params![
            encode_file_id(file_id),
            i64::from(range.start()),
            i64::from(range.end()),
            preview.language,
            preview.value,
        ])?;
    }
    Ok(())
}
