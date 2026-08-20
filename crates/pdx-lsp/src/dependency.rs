//! Background load and rebuild of dependency index caches.

use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pdx_engine::{AnalysisHost, IndexCache, WorkspaceChange, WorkspaceScanToken};
use pdx_rules::{GameProfile, RuleSet};

use crate::workspace::DependencyIndexCache;

const MAX_DEPENDENCY_WORKERS: usize = 4;

/// One dependency cache and the result of loading or rebuilding it.
pub(crate) type DependencySetupOutcome =
    (DependencyIndexCache, Result<(IndexCache, String), String>);

/// Loads dependency caches with bounded parallelism while preserving configuration order.
///
/// Loading and refreshing each dependency is independent. The event loop still installs the
/// returned caches in the original order, so source priority remains deterministic even when
/// disk work completes out of order.
pub(crate) fn run_dependency_cache_loads(
    configs: Vec<DependencyIndexCache>,
    rules: RuleSet,
    profile: GameProfile,
    current_rule_hash: String,
    log: Option<&(dyn Fn(&str) + Sync)>,
    progress: Option<&(dyn Fn(usize, usize) + Sync)>,
    cancellation: &WorkspaceScanToken,
) -> Vec<DependencySetupOutcome> {
    if configs.is_empty() {
        return Vec::new();
    }

    let configs = Arc::new(configs);
    let next = Arc::new(AtomicUsize::new(0));
    let results = Arc::new(Mutex::new(
        (0..configs.len())
            .map(|_| None)
            .collect::<Vec<Option<DependencySetupOutcome>>>(),
    ));
    let worker_count = configs.len().min(MAX_DEPENDENCY_WORKERS);

    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            let configs = Arc::clone(&configs);
            let next = Arc::clone(&next);
            let results = Arc::clone(&results);
            let worker_rules = rules.clone();
            let worker_profile = profile.clone();
            let worker_rule_hash = current_rule_hash.clone();
            scope.spawn(move || {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(config) = configs.get(index) else {
                        break;
                    };
                    let result = run_dependency_cache_load(
                        config,
                        worker_rules.clone(),
                        worker_profile.clone(),
                        worker_rule_hash.clone(),
                        log,
                        progress,
                        cancellation,
                    );
                    results
                        .lock()
                        .unwrap_or_else(|_| panic!("dependency result mutex poisoned"))[index] =
                        Some((config.clone(), result));
                }
            });
        }
    });

    let results = Arc::try_unwrap(results)
        .unwrap_or_else(|_| panic!("dependency result workers still exist"))
        .into_inner()
        .unwrap_or_else(|_| panic!("dependency result mutex poisoned"));
    results
        .into_iter()
        .map(|result| result.expect("every dependency cache must produce a result"))
        .collect()
}

/// Loads (or rebuilds) the persistent index cache for one configured dependency.
///
/// A usable cache is loaded for installation and refreshed against the dependency directory so
/// symbol changes are picked up without a full reindex. A missing, corrupt, or
/// schema-incompatible cache is rebuilt from the configured dependency directory in place. A
/// rules-hash mismatch triggers a regeneration attempt; if that fails the stale cache is still
/// returned so the dependency keeps its symbols, mirroring the Vanilla cache policy.
pub(crate) fn run_dependency_cache_load(
    config: &DependencyIndexCache,
    rules: RuleSet,
    profile: GameProfile,
    current_rule_hash: String,
    log: Option<&(dyn Fn(&str) + Sync)>,
    progress: Option<&(dyn Fn(usize, usize) + Sync)>,
    cancellation: &WorkspaceScanToken,
) -> Result<(IndexCache, String), String> {
    let started = std::time::Instant::now();
    let result = (|| {
        let load_started = std::time::Instant::now();
        let loaded = match IndexCache::load_cancellable_for_install_with_progress(
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
                    log,
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
        if let Some(log) = log {
            log(&format!(
                "Dependency cache decode: {:.1} ms for {} ({} file(s), {} position(s) loaded)",
                load_started.elapsed().as_secs_f64() * 1000.0,
                config.root.path.display(),
                loaded.source_files().len(),
                loaded.index().position_ranges().len(),
            ));
        }
        if loaded.metadata().rule_hash == current_rule_hash {
            // The rules still match, so only the source files may have moved on: refresh the cache
            // against the dependency directory (a fingerprint diff, not a reparse). A failed
            // refresh — moved or unavailable source, cancellation — degrades to the cached
            // symbols, and a save failure keeps the refreshed cache in memory with a warning.
            let refresh_started = std::time::Instant::now();
            return match loaded.refresh_cancellable(&rules, &profile, cancellation, progress) {
                Ok(refreshed) => {
                    if let Some(log) = log {
                        log(&format!(
                            "Dependency refresh: {:.1} ms against {}",
                            refresh_started.elapsed().as_secs_f64() * 1000.0,
                            config.root.path.display()
                        ));
                    }
                    let save = refreshed.save_with_progress(&config.index_path, progress);
                    let suffix = match save {
                        Ok(()) => String::new(),
                        Err(error) => format!(
                            "; refreshed content could not be saved to {}: {error}",
                            config.index_path.display()
                        ),
                    };
                    Ok((
                        refreshed,
                        format!(
                            "Dependency {} symbols refreshed against {} and loaded from {}{suffix}",
                            config.root.path.display(),
                            config.root.path.display(),
                            config.index_path.display()
                        ),
                    ))
                }
                Err(error) => Ok((
                    loaded,
                    format!(
                        "Dependency {} symbols loaded from {}; refresh skipped: {error}",
                        config.root.path.display(),
                        config.index_path.display()
                    ),
                )),
            };
        }
        let stale_hash = loaded.metadata().rule_hash.clone();
        if let Some(log) = log {
            log(&format!(
                "Dependency cache {} is stale (rules hash {stale_hash} != {current_rule_hash}); regenerating from {}",
                config.index_path.display(),
                config.root.path.display()
            ));
        }
        let rebuilt = build_dependency_cache(
            config,
            &rules,
            &profile,
            log,
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
    })();
    if let Some(log) = log {
        log(&format!(
            "Dependency cache worker total: {:.1} ms",
            started.elapsed().as_secs_f64() * 1000.0
        ));
    }
    result
}

/// Scans one dependency directory and writes its cache file atomically.
fn build_dependency_cache(
    config: &DependencyIndexCache,
    rules: &RuleSet,
    profile: &GameProfile,
    log: Option<&(dyn Fn(&str) + Sync)>,
    progress: Option<&(dyn Fn(usize, usize) + Sync)>,
    cancellation: &WorkspaceScanToken,
    activity: &str,
) -> Result<IndexCache, String> {
    if cancellation.is_cancelled() {
        return Err("dependency index build was cancelled".to_owned());
    }
    let mut host = AnalysisHost::with_profile(rules.clone(), profile.clone());
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![config.root.clone()]));
    let scan_started = std::time::Instant::now();
    host.refresh_source_roots_cancellable_with_progress(cancellation, progress)
        .map_err(|error| {
            format!(
                "{activity} failed while indexing {}: {error}",
                config.root.path.display()
            )
        })?;
    if let Some(log) = log {
        log(&format!(
            "{activity}: scanned {} file(s) in {:.1} ms",
            host.snapshot().source_files().len(),
            scan_started.elapsed().as_secs_f64() * 1000.0
        ));
    }
    let build_started = std::time::Instant::now();
    let cache = IndexCache::from_snapshot(&host.snapshot())
        .map_err(|error| format!("{activity} failed: {error}"))?;
    if let Some(log) = log {
        log(&format!(
            "{activity}: cache built in {:.1} ms",
            build_started.elapsed().as_secs_f64() * 1000.0
        ));
    }
    let save_started = std::time::Instant::now();
    cache
        .save_with_progress(&config.index_path, progress)
        .map_err(|error| {
            format!(
                "{activity} could not be saved to {}: {error}",
                config.index_path.display()
            )
        })?;
    if let Some(log) = log {
        log(&format!(
            "{activity}: saved to {} in {:.1} ms",
            config.index_path.display(),
            save_started.elapsed().as_secs_f64() * 1000.0
        ));
    }
    Ok(cache)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdx_engine::{SourceRoot, SourceRootId, SourceRootKind};
    use pdx_game::eu4::{first_party_rules, profile};
    use tempfile::tempdir;

    #[test]
    fn parallel_loader_preserves_configuration_order() {
        let container = tempdir().expect("temporary dependency container");
        let mut configs = Vec::new();
        for (index, name) in [(1_u32, "first"), (2, "second")] {
            let root = container.path().join(name);
            fs::create_dir_all(root.join("common/events")).expect("dependency directory");
            fs::write(
                root.join("common/events/events.txt"),
                format!("country_event = {{ id = {name}.1 }}\n"),
            )
            .expect("dependency source");
            configs.push(DependencyIndexCache {
                root: SourceRoot::new(
                    SourceRootId::new(index),
                    SourceRootKind::Dependency,
                    fs::canonicalize(root).expect("canonical dependency root"),
                ),
                index_path: container.path().join(format!("{name}.pdxindex")),
            });
        }

        let rules = first_party_rules().expect("embedded rules");
        let results = run_dependency_cache_loads(
            configs,
            rules.clone(),
            profile(),
            rules.rule_hash().to_hex(),
            None,
            None,
            &WorkspaceScanToken::new(),
        );

        assert_eq!(results.len(), 2);
        assert_eq!(
            results
                .iter()
                .map(|(config, _)| config.root.id)
                .collect::<Vec<_>>(),
            vec![SourceRootId::new(1), SourceRootId::new(2)]
        );
        for (config, result) in results {
            assert!(
                result.is_ok(),
                "dependency {} failed: {result:?}",
                config.root.path.display()
            );
            assert!(
                config.index_path.is_file(),
                "cache was saved for {}",
                config.root.path.display()
            );
        }
    }
}
