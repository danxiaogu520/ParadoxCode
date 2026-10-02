//! Incremental reindex of a persistent cache against its recorded source directory.
//!
//! `refresh` first compares per-file filesystem metadata fingerprints and falls back to content
//! reads when a stamp is unavailable or changed. The tree fingerprint, shards, positions, and
//! previews of unchanged files are carried over, so a refresh can avoid both I/O and parsing for
//! the common unchanged-source case.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use index::WorkspaceIndex;
use index::{
    IndexSymbolFacts, build_file_state, build_file_state_with_ir,
    build_file_state_with_ir_and_facts, empty_file_state, position_ranges_for_state,
};
use rules::ir::RulesIr;
use rules::{GameProfile, RuleSet};
use sha2::{Digest, Sha256};
use vfs::scan::{collect_whitelisted_files, read_source_file_cancellable, stable_file_id};
use vfs::{
    SourceFile, SourceFileId, WorkspaceError, WorkspaceScanFilters, WorkspaceScanLimits,
    WorkspaceScanReport, WorkspaceScanToken,
};

use super::{
    CURRENT_CACHE_SCHEMA_VERSION, IndexCache, IndexCacheError, IndexCacheMetadata, MAX_CACHE_FILES,
    content_fingerprint, put_fingerprint_field, source_metadata_fingerprint, validate_cache_limits,
};

pub(super) fn refresh_cancellable(
    cache: &IndexCache,
    rules: &RuleSet,
    profile: &GameProfile,
    ir: &RulesIr,
    cancellation: &WorkspaceScanToken,
    progress: Option<&(dyn Fn(usize, usize) + Sync)>,
) -> Result<IndexCache, IndexCacheError> {
    if cache.metadata.game_id != rules.game_id() || cache.metadata.game_id != profile.game_id {
        return Err(IndexCacheError::GameMismatch {
            expected: profile.game_id.clone(),
            actual: cache.metadata.game_id.clone(),
        });
    }
    if cache.metadata.build_id != crate::ANALYZER_BUILD_ID {
        return Err(IndexCacheError::BuildMismatch {
            cached: cache.metadata.build_id.clone(),
            active: crate::ANALYZER_BUILD_ID.to_owned(),
        });
    }
    if !ir.files.is_empty() {
        return refresh_with_ir_full(cache, rules, profile, ir, cancellation, progress);
    }
    let mut report = WorkspaceScanReport::default();
    let limits = WorkspaceScanLimits::default();
    let filters = WorkspaceScanFilters::default();
    let mut walked = Vec::new();
    collect_whitelisted_files(
        &cache.root.path,
        profile,
        &filters,
        limits,
        &mut report,
        &mut walked,
        cancellation,
    )
    .map_err(map_workspace_error)?;
    // Reindex in stable file-id order so the source fingerprint stays byte-identical to a full
    // build, which hashes path + per-file content digest over the same ascending-id sequence.
    let mut items = walked
        .into_iter()
        .map(|(logical, physical)| {
            (
                SourceFileId::new(stable_file_id(cache.root.id, &logical)),
                logical,
                physical,
            )
        })
        .collect::<Vec<_>>();
    items.sort_by_key(|(id, _, _)| *id);
    for pair in items.windows(2) {
        if pair[0].0 == pair[1].0 {
            return Err(IndexCacheError::InvalidData(format!(
                "two walked files produced the same stable file id {}",
                pair[0].0.get()
            )));
        }
    }

    let mut files = cache.source_files.clone();
    let mut shards = cache.index.shards.clone();
    let mut positions = cache.index.position_ranges().clone();
    let mut previews = cache.localisation_previews.clone();
    let mut file_fingerprints = cache.file_fingerprints.clone();
    let mut file_metadata_fingerprints = cache.file_metadata_fingerprints.clone();
    let mut hasher = Sha256::new();
    hasher.update(b"paradoxcode/vanilla-source/v2\0");
    let mut retained = BTreeMap::new();
    let total = items.len();
    for (index, (id, logical, physical)) in items.iter().enumerate() {
        cancellation.checkpoint().map_err(map_workspace_error)?;
        let Some(category) = rules.classify(logical) else {
            continue;
        };
        let metadata_fingerprint = source_metadata_fingerprint(physical);
        let unchanged_by_metadata = metadata_fingerprint.as_ref().is_some_and(|metadata| {
            files
                .get(id)
                .is_some_and(|file| file.logical_path == *logical)
                && file_fingerprints.contains_key(id)
                && cache
                    .file_metadata_fingerprints
                    .get(id)
                    .and_then(Option::as_ref)
                    == Some(metadata)
        });
        if unchanged_by_metadata {
            let Some(content_fingerprint) = file_fingerprints.get(id) else {
                continue;
            };
            put_fingerprint_field(&mut hasher, logical.as_str().as_bytes());
            put_fingerprint_field(&mut hasher, content_fingerprint.as_bytes());
            retained.insert(*id, ());
            if let Some(progress) = progress {
                progress(index.saturating_add(1), total);
            }
            continue;
        }
        let Some(source) = read_source_file_cancellable(
            physical,
            limits,
            &mut report,
            cancellation,
            profile.source_encoding,
        )
        .map_err(map_workspace_error)?
        else {
            // Skipped (binary, oversized, unreadable): any cached entry for this file is
            // dropped after the walk.
            continue;
        };
        put_fingerprint_field(&mut hasher, logical.as_str().as_bytes());
        let digest = content_fingerprint(&source);
        put_fingerprint_field(&mut hasher, digest.as_bytes());
        let recorded_metadata_fingerprint =
            source_metadata_fingerprint(physical).or(metadata_fingerprint);
        retained.insert(*id, ());
        let unchanged = files
            .get(id)
            .is_some_and(|file| file.logical_path == *logical)
            && file_fingerprints
                .get(id)
                .is_some_and(|cached| cached == &digest);
        if unchanged {
            file_metadata_fingerprints.insert(*id, recorded_metadata_fingerprint);
            if let Some(progress) = progress {
                progress(index.saturating_add(1), total);
            }
            continue;
        }
        let source_file = SourceFile {
            id: *id,
            root_id: cache.root.id,
            physical_path: physical.clone(),
            logical_path: logical.clone(),
            category_id: Some(category.id.clone()),
            resolution: category.resolution,
        };
        // Revision is irrelevant for cached states; only the shard and previews
        // are retained after `cache_only`; positions recompute from the source.
        let state = build_file_state(&source_file, source, 0, rules, profile);
        let positions_for_file = position_ranges_for_state(&state);
        let state = state.cache_only();
        let shard = state.shard_handle();
        shards.insert(*id, shard);
        positions.remove_file(*id);
        positions.extend(
            positions_for_file
                .into_iter()
                .map(|(range, position)| ((*id, range), position)),
        );
        previews.remove_file(*id);
        if let Some(cached) = state.cached_localisation_previews() {
            previews.replace_file(
                *id,
                cached
                    .iter()
                    .map(|(range, preview)| (*range, preview.clone())),
            );
        }
        files.insert(*id, source_file);
        file_fingerprints.insert(*id, digest);
        file_metadata_fingerprints.insert(*id, recorded_metadata_fingerprint);
        if let Some(progress) = progress {
            progress(index.saturating_add(1), total);
        }
    }
    // Drop cached entries whose files were deleted, skipped, or left the profile whitelist.
    let removed = files
        .keys()
        .copied()
        .filter(|id| !retained.contains_key(id))
        .collect::<Vec<_>>();
    for id in removed {
        files.remove(&id);
        shards.remove(&id);
        positions.remove_file(id);
        previews.remove_file(id);
        file_fingerprints.remove(&id);
        file_metadata_fingerprints.remove(&id);
    }
    if files.len() > MAX_CACHE_FILES {
        return Err(IndexCacheError::LimitExceeded("file", MAX_CACHE_FILES));
    }
    validate_cache_limits(
        shards.values().map(|shard| shard.definitions.len()).sum(),
        shards.values().map(|shard| shard.references.len()).sum(),
        shards
            .values()
            .map(|shard| shard.dynamic_definitions.len())
            .sum(),
        shards
            .values()
            .flat_map(|shard| shard.dynamic_definitions.iter())
            .map(|summary| summary.parameters.len())
            .sum(),
    )?;
    let indexed_files = files.len();
    let source_fingerprint: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let created_unix_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| IndexCacheError::InvalidData(error.to_string()))?
        .as_secs();
    let metadata = IndexCacheMetadata {
        schema_version: CURRENT_CACHE_SCHEMA_VERSION,
        game_id: cache.metadata.game_id.clone(),
        build_id: cache.metadata.build_id.clone(),
        rule_hash: cache.metadata.rule_hash.clone(),
        ir_hash: cache.metadata.ir_hash.clone(),
        source_identity: cache.metadata.source_identity.clone(),
        source_fingerprint,
        created_unix_seconds,
        indexed_files,
    };
    let mut index = WorkspaceIndex::empty();
    index.shards = shards;
    index.replace_all_position_ranges(positions);
    Ok(IndexCache {
        metadata,
        root: cache.root.clone(),
        source_files: files,
        index,
        localisation_previews: previews,
        reference_source: None,
        file_fingerprints,
        file_metadata_fingerprints,
    })
}

/// Rebuild until cross-file symbol facts stabilize, including symbols introduced by Callable
/// payloads. Unchanged files must not retain references from an earlier candidate index.
fn refresh_with_ir_full(
    cache: &IndexCache,
    rules: &RuleSet,
    profile: &GameProfile,
    ir: &RulesIr,
    cancellation: &WorkspaceScanToken,
    progress: Option<&(dyn Fn(usize, usize) + Sync)>,
) -> Result<IndexCache, IndexCacheError> {
    let limits = WorkspaceScanLimits::default();
    let filters = WorkspaceScanFilters::default();
    let mut report = WorkspaceScanReport::default();
    let mut walked = Vec::new();
    collect_whitelisted_files(
        &cache.root.path,
        profile,
        &filters,
        limits,
        &mut report,
        &mut walked,
        cancellation,
    )
    .map_err(map_workspace_error)?;
    let mut items = walked
        .into_iter()
        .map(|(logical, physical)| {
            (
                SourceFileId::new(stable_file_id(cache.root.id, &logical)),
                logical,
                physical,
            )
        })
        .collect::<Vec<_>>();
    items.sort_by_key(|(id, _, _)| *id);
    let mut files = BTreeMap::new();
    let mut sources = BTreeMap::<SourceFileId, String>::new();
    let mut initial = BTreeMap::new();
    let mut fingerprints = BTreeMap::new();
    let mut metadata_fingerprints = BTreeMap::new();
    let total = items.len();
    let mut hasher = Sha256::new();
    hasher.update(b"paradoxcode/vanilla-source/v2\0");
    for (ordinal, (id, logical, physical)) in items.iter().enumerate() {
        cancellation.checkpoint().map_err(map_workspace_error)?;
        let Some((_, rule)) = ir.file_rule(logical) else {
            continue;
        };
        let category_id = ir.strings.resolve(rule.name).to_owned();
        let resolution = match rule.resolution {
            rules::ir::FileResolution::ReplaceByPath => {
                rules::FileResolutionPolicy::ReplaceByRelativePath
            }
            rules::ir::FileResolution::Merge => rules::FileResolutionPolicy::Merge,
        };
        let source_file = SourceFile {
            id: *id,
            root_id: cache.root.id,
            physical_path: physical.clone(),
            logical_path: logical.clone(),
            category_id: Some(category_id),
            resolution,
        };
        if matches!(rule.parser, rules::ir::DocumentParser::Asset) {
            let digest = content_fingerprint("");
            put_fingerprint_field(&mut hasher, logical.as_str().as_bytes());
            put_fingerprint_field(&mut hasher, digest.as_bytes());
            files.insert(*id, source_file.clone());
            sources.insert(*id, String::new());
            fingerprints.insert(*id, digest);
            metadata_fingerprints.insert(*id, source_metadata_fingerprint(physical));
            initial.insert(*id, empty_file_state(&source_file, 0));
            if let Some(progress) = progress {
                progress(ordinal.saturating_add(1), total);
            }
            continue;
        }
        let Some(source) = read_source_file_cancellable(
            physical,
            limits,
            &mut report,
            cancellation,
            profile.source_encoding,
        )
        .map_err(map_workspace_error)?
        else {
            continue;
        };
        let digest = content_fingerprint(&source);
        put_fingerprint_field(&mut hasher, logical.as_str().as_bytes());
        put_fingerprint_field(&mut hasher, digest.as_bytes());
        let metadata = source_metadata_fingerprint(physical);
        let state =
            build_file_state_with_ir(&source_file, source.clone(), 0, rules, profile, ir, None);
        files.insert(*id, source_file);
        sources.insert(*id, source);
        fingerprints.insert(*id, digest);
        metadata_fingerprints.insert(*id, metadata);
        initial.insert(*id, state.cache_only());
        if let Some(progress) = progress {
            progress(ordinal.saturating_add(1), total);
        }
    }
    if files.len() > MAX_CACHE_FILES {
        return Err(IndexCacheError::LimitExceeded("file", MAX_CACHE_FILES));
    }
    let mut final_states = initial;
    let mut passes = 0;
    loop {
        passes += 1;
        let candidate =
            WorkspaceIndex::from_shards(final_states.values().map(|state| state.shard_handle()));
        let facts = IndexSymbolFacts::new(ir, &candidate, &[]);
        let mut changed = false;
        let previous = std::mem::take(&mut final_states);
        for (id, old) in previous {
            cancellation.checkpoint().map_err(map_workspace_error)?;
            let Some(source_file) = files.get(&id) else {
                continue;
            };
            let source = sources.get(&id).cloned().unwrap_or_default();
            let state = build_file_state_with_ir_and_facts(
                source_file,
                source,
                0,
                rules,
                profile,
                ir,
                &facts,
                None,
            );
            changed |= !state.shard().same_symbol_facts(old.shard());
            final_states.insert(id, state.cache_only());
        }
        if !changed {
            break;
        }
        if passes >= 32 {
            return Err(IndexCacheError::InvalidData(
                "IR symbol facts did not stabilize after 32 passes".into(),
            ));
        }
    }
    let shards = final_states
        .values()
        .map(|state| state.shard_handle())
        .collect::<Vec<_>>();
    validate_cache_limits(
        shards.iter().map(|shard| shard.definitions.len()).sum(),
        shards.iter().map(|shard| shard.references.len()).sum(),
        shards
            .iter()
            .map(|shard| shard.dynamic_definitions.len())
            .sum(),
        shards
            .iter()
            .flat_map(|shard| shard.dynamic_definitions.iter())
            .map(|summary| summary.parameters.len())
            .sum(),
    )?;
    let mut index = WorkspaceIndex::from_shards(shards);
    let mut positions = index::PositionMap::new();
    let mut previews = index::LocalisationPreviewMap::new();
    for (id, state) in &final_states {
        positions.extend(
            position_ranges_for_state(state)
                .into_iter()
                .map(|(range, position)| ((*id, range), position)),
        );
        if let Some(entries) = state.cached_localisation_previews() {
            previews.replace_file(
                *id,
                entries
                    .iter()
                    .map(|(range, preview)| (*range, preview.clone())),
            );
        }
    }
    index.replace_all_position_ranges(positions);
    let source_fingerprint = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let created_unix_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| IndexCacheError::InvalidData(error.to_string()))?
        .as_secs();
    let metadata = IndexCacheMetadata {
        schema_version: CURRENT_CACHE_SCHEMA_VERSION,
        game_id: cache.metadata.game_id.clone(),
        build_id: cache.metadata.build_id.clone(),
        rule_hash: cache.metadata.rule_hash.clone(),
        ir_hash: cache.metadata.ir_hash.clone(),
        source_identity: cache.metadata.source_identity.clone(),
        source_fingerprint,
        created_unix_seconds,
        indexed_files: files.len(),
    };
    Ok(IndexCache {
        metadata,
        root: cache.root.clone(),
        source_files: files,
        index,
        localisation_previews: previews,
        reference_source: None,
        file_fingerprints: fingerprints,
        file_metadata_fingerprints: metadata_fingerprints,
    })
}

fn map_workspace_error(error: WorkspaceError) -> IndexCacheError {
    match error {
        WorkspaceError::Cancelled => IndexCacheError::Cancelled,
        WorkspaceError::Io(error) => IndexCacheError::Io(error),
        other => IndexCacheError::InvalidData(format!("source refresh failed: {other}")),
    }
}
