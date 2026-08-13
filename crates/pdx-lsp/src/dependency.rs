//! Background load and rebuild of dependency index caches.

use std::fs;

use pdx_engine::{AnalysisHost, VanillaIndexCache, WorkspaceChange, WorkspaceScanToken};
use pdx_rules::{GameProfile, RuleSet};

use crate::workspace::DependencyIndexCache;

/// Loads (or rebuilds) the persistent index cache for one configured dependency.
///
/// A usable cache is loaded for installation. A missing, corrupt, or schema-incompatible cache
/// is rebuilt from the configured dependency directory in place. A rules-hash mismatch triggers
/// a regeneration attempt; if that fails the stale cache is still returned so the dependency
/// keeps its symbols, mirroring the Vanilla cache policy.
pub(crate) fn run_dependency_cache_load(
    config: &DependencyIndexCache,
    rules: RuleSet,
    profile: GameProfile,
    current_rule_hash: String,
    progress: Option<&(dyn Fn(usize, usize) + Sync)>,
    cancellation: &WorkspaceScanToken,
) -> Result<(VanillaIndexCache, String), String> {
    let loaded = match VanillaIndexCache::load_cancellable_for_install_with_progress(
        &config.index_path,
        cancellation,
        progress,
    ) {
        Ok(loaded) => loaded,
        Err(_) => {
            // The old file is unusable (missing, corrupt, or from an older schema); remove it so
            // the rebuild can write a fresh cache in its place.
            match fs::remove_file(&config.index_path) {
                Ok(()) => {}
                Err(remove_error) if remove_error.kind() == std::io::ErrorKind::NotFound => {}
                Err(remove_error) => {
                    return Err(format!(
                        "dependency cache for {} could not be replaced at {}: {remove_error}",
                        config.root.path.display(),
                        config.index_path.display()
                    ));
                }
            }
            return build_dependency_cache(
                config,
                &rules,
                &profile,
                progress,
                cancellation,
                "Dependency index build",
            )
            .map(|cache| {
                (
                    cache,
                    format!(
                        "Dependency {} index was built and loaded from {}",
                        config.root.path.display(),
                        config.index_path.display()
                    ),
                )
            });
        }
    };
    if loaded.metadata().rule_hash == current_rule_hash {
        return Ok((
            loaded,
            format!(
                "Dependency {} symbols loaded from {}",
                config.root.path.display(),
                config.index_path.display()
            ),
        ));
    }
    let stale_hash = loaded.metadata().rule_hash.clone();
    let rebuilt = build_dependency_cache(
        config,
        &rules,
        &profile,
        progress,
        cancellation,
        "Dependency index regeneration",
    );
    match rebuilt {
        Ok(cache) => Ok((
            cache,
            format!(
                "Dependency {} index was regenerated for the active rules hash {current_rule_hash} and loaded from {}",
                config.root.path.display(),
                config.index_path.display()
            ),
        )),
        Err(error) => Ok((
            loaded,
            format!(
                "{error}; using the existing dependency cache built with rules hash {stale_hash}"
            ),
        )),
    }
}

/// Scans one dependency directory and writes its cache file atomically.
fn build_dependency_cache(
    config: &DependencyIndexCache,
    rules: &RuleSet,
    profile: &GameProfile,
    progress: Option<&(dyn Fn(usize, usize) + Sync)>,
    cancellation: &WorkspaceScanToken,
    activity: &str,
) -> Result<VanillaIndexCache, String> {
    if cancellation.is_cancelled() {
        return Err("dependency index build was cancelled".to_owned());
    }
    let mut host = AnalysisHost::with_profile(rules.clone(), profile.clone());
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![config.root.clone()]));
    host.refresh_source_roots_cancellable_with_progress(cancellation, progress)
        .map_err(|error| {
            format!(
                "{activity} failed while indexing {}: {error}",
                config.root.path.display()
            )
        })?;
    let cache = VanillaIndexCache::from_snapshot(&host.snapshot())
        .map_err(|error| format!("{activity} failed: {error}"))?;
    cache
        .save_with_progress(&config.index_path, progress)
        .map_err(|error| {
            format!(
                "{activity} could not be saved to {}: {error}",
                config.index_path.display()
            )
        })?;
    Ok(cache)
}
