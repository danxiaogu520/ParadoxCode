//! Incremental reindex of a persistent cache against its recorded source directory.
//!
//! `refresh` reuses the cache's per-file content fingerprints to parse only files whose bytes
//! changed. The tree fingerprint, shards, positions, and previews of unchanged files are
//! carried over, so a refresh is bounded by disk I/O plus the changed subset instead of a full
//! parse of the source root.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use pdx_rules::{GameProfile, RuleSet};
use sha2::{Digest, Sha256};

use crate::index::WorkspaceIndex;
use crate::model::{
    SourceFile, SourceFileId, WorkspaceError, WorkspaceScanLimits, WorkspaceScanReport,
    WorkspaceScanToken,
};
use crate::pipeline::{build_file_state, position_ranges_for_state};
use crate::scan::{collect_whitelisted_files, read_source_file_cancellable, stable_file_id};

use super::{
    CURRENT_CACHE_SCHEMA_VERSION, IndexCache, IndexCacheError, IndexCacheMetadata, MAX_CACHE_FILES,
    put_fingerprint_field, validate_cache_limits,
};

pub(super) fn refresh_cancellable(
    cache: &IndexCache,
    rules: &RuleSet,
    profile: &GameProfile,
    cancellation: &WorkspaceScanToken,
    progress: Option<&(dyn Fn(usize, usize) + Sync)>,
) -> Result<IndexCache, IndexCacheError> {
    if cache.metadata.game_id != rules.game_id() || cache.metadata.game_id != profile.game_id {
        return Err(IndexCacheError::GameMismatch {
            expected: profile.game_id.clone(),
            actual: cache.metadata.game_id.clone(),
        });
    }
    if cache.metadata.rule_hash != rules.rule_hash().to_hex() {
        return Err(IndexCacheError::RuleHashMismatch {
            cached: cache.metadata.rule_hash.clone(),
            active: rules.rule_hash().to_hex(),
        });
    }
    let mut report = WorkspaceScanReport::default();
    let limits = WorkspaceScanLimits::default();
    let mut walked = Vec::new();
    collect_whitelisted_files(
        &cache.root.path,
        profile,
        limits,
        &mut report,
        &mut walked,
        cancellation,
    )
    .map_err(map_workspace_error)?;
    // Reindex in stable file-id order so the tree fingerprint stays byte-identical to a full
    // build, which hashes path + source over the same ascending-id sequence.
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
    let mut hasher = Sha256::new();
    hasher.update(b"paradoxcode/vanilla-source/v1\0");
    let mut retained = BTreeMap::new();
    let total = items.len();
    for (index, (id, logical, physical)) in items.iter().enumerate() {
        cancellation.checkpoint().map_err(map_workspace_error)?;
        let Some(category) = rules.classify(logical) else {
            continue;
        };
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
        put_fingerprint_field(&mut hasher, source.as_bytes());
        let digest = format!("{:x}", Sha256::digest(source.as_bytes()));
        retained.insert(*id, ());
        let unchanged = files
            .get(id)
            .is_some_and(|file| file.logical_path == *logical)
            && file_fingerprints
                .get(id)
                .is_some_and(|cached| cached == &digest);
        if unchanged {
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
        // Revision is irrelevant for cached states; only the shard, positions, and previews
        // are retained after `cache_only`.
        let state = build_file_state(&source_file, source, 0, rules, profile);
        let positions_for_file = position_ranges_for_state(&state);
        let state = state.cache_only(positions_for_file);
        let shard = (*state.shard).clone();
        shards.insert(*id, shard);
        positions.retain(|(file_id, _), _| *file_id != *id);
        if let Some(cached) = state.cached_positions.as_deref() {
            positions.extend(
                cached
                    .iter()
                    .map(|(range, position)| ((*id, *range), *position)),
            );
        }
        previews.retain(|(file_id, _), _| *file_id != *id);
        if let Some(cached) = state.cached_localisation_previews() {
            previews.extend(
                cached
                    .iter()
                    .map(|(range, preview)| ((*id, *range), preview.clone())),
            );
        }
        files.insert(*id, source_file);
        file_fingerprints.insert(*id, digest);
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
        positions.retain(|(file_id, _), _| *file_id != id);
        previews.retain(|(file_id, _), _| *file_id != id);
        file_fingerprints.remove(&id);
    }
    if files.len() > MAX_CACHE_FILES {
        return Err(IndexCacheError::LimitExceeded("file", MAX_CACHE_FILES));
    }
    validate_cache_limits(
        shards.values().map(|shard| shard.definitions.len()).sum(),
        shards.values().map(|shard| shard.references.len()).sum(),
        shards
            .values()
            .map(|shard| shard.macro_definitions.len())
            .sum(),
        shards
            .values()
            .flat_map(|shard| shard.macro_definitions.iter())
            .map(|summary| summary.parameters.len())
            .sum(),
    )?;
    let indexed_files = files.len();
    let source_fingerprint = format!("{:x}", hasher.finalize());
    let created_unix_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| IndexCacheError::InvalidData(error.to_string()))?
        .as_secs();
    let metadata = IndexCacheMetadata {
        schema_version: CURRENT_CACHE_SCHEMA_VERSION,
        game_id: cache.metadata.game_id.clone(),
        rule_hash: cache.metadata.rule_hash.clone(),
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
        file_fingerprints,
    })
}

fn map_workspace_error(error: WorkspaceError) -> IndexCacheError {
    match error {
        WorkspaceError::Cancelled => IndexCacheError::Cancelled,
        WorkspaceError::Io(error) => IndexCacheError::Io(error),
        other => IndexCacheError::InvalidData(format!("source refresh failed: {other}")),
    }
}
