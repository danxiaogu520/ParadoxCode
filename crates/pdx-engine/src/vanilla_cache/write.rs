//! Transactional Vanilla cache writes.

use std::fs;
use std::path::Path;

use rusqlite::{Connection, Transaction, params};

use super::codec::{encode_file_id, encode_path, resolution_name};
use super::read::validate_database_identity;
use super::template_codec;
use super::{
    APPLICATION_ID, CURRENT_VANILLA_CACHE_SCHEMA_VERSION, VanillaCacheError, VanillaIndexCache,
};

pub(super) fn save(cache: &VanillaIndexCache, path: &Path) -> Result<(), VanillaCacheError> {
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
    write_cache(&transaction, cache)?;
    transaction.commit()?;
    Ok(())
}

fn write_cache(
    transaction: &Transaction<'_>,
    cache: &VanillaIndexCache,
) -> Result<(), VanillaCacheError> {
    transaction.execute_batch(
        "DROP TABLE IF EXISTS macro_parameters;
         DROP TABLE IF EXISTS macro_definitions;
         DROP TABLE IF EXISTS symbol_references;
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
    let mut insert_source_file = transaction.prepare(
        "INSERT INTO source_files(file_id, logical_path, category_id, resolution, syntax_error_count)
         VALUES (?1, ?2, ?3, ?4, ?5)",
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
    for (id, file) in &cache.source_files {
        let shard = cache.index.shard(*id).ok_or_else(|| {
            VanillaCacheError::InvalidData(format!(
                "source file {} has no index shard",
                file.logical_path.as_str()
            ))
        })?;
        insert_source_file.execute(params![
            encode_file_id(*id),
            file.logical_path.as_str(),
            file.category_id,
            resolution_name(file.resolution),
            i64::try_from(shard.syntax_error_count).map_err(|_| {
                VanillaCacheError::InvalidData("syntax error count exceeds SQLite range".into())
            })?
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
                return Err(VanillaCacheError::InvalidData(format!(
                    "duplicate macro summary {} `{}`",
                    summary.kind, summary.name
                )));
            }
            if !shard.definitions.iter().any(|definition| {
                definition.kind.eq_ignore_ascii_case(&summary.kind)
                    && definition.name.eq_ignore_ascii_case(&summary.name)
                    && definition.range == summary.definition_range
            }) {
                return Err(VanillaCacheError::InvalidData(format!(
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
                        return Err(VanillaCacheError::InvalidData(format!(
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
                    return Err(VanillaCacheError::InvalidData(format!(
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
    }
    drop(insert_source_file);
    drop(insert_definition);
    drop(insert_reference);
    drop(insert_macro);
    drop(insert_macro_parameter);
    let mut insert_position = transaction.prepare(
        "INSERT INTO navigation_positions(file_id, range_start, range_end, start_line, start_character, end_line, end_character)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for ((file_id, range), position) in cache.index.position_ranges() {
        if !cache.source_files.contains_key(file_id) {
            return Err(VanillaCacheError::InvalidData(format!(
                "navigation position references unknown file {}",
                file_id.get()
            )));
        }
        insert_position.execute(params![
            encode_file_id(*file_id),
            i64::from(range.start()),
            i64::from(range.end()),
            i64::from(position.start.line),
            i64::from(position.start.character),
            i64::from(position.end.line),
            i64::from(position.end.character),
        ])?;
    }
    drop(insert_position);
    let mut insert_preview = transaction.prepare(
        "INSERT INTO localisation_previews(file_id, range_start, range_end, language, value)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
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
        insert_preview.execute(params![
            encode_file_id(*file_id),
            i64::from(range.start()),
            i64::from(range.end()),
            preview.language,
            preview.value,
        ])?;
    }
    Ok(())
}
