//! Background load and rebuild of dependency index caches.

use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use engine::{AnalysisHost, IndexCache, WorkspaceChange, WorkspaceScanLimits, WorkspaceScanToken};
use rules::ir::RulesIr;
use rules::{GameProfile, RuleSet};

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
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_dependency_cache_loads(
    configs: Vec<DependencyIndexCache>,
    rules: RuleSet,
    ir: Arc<RulesIr>,
    profile: GameProfile,
    current_rule_hash: String,
    current_ir_hash: String,
    scan_limits: WorkspaceScanLimits,
    preferred_localisation_languages: &[String],
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
    if let Some(log) = log {
        log(&format!(
            "Dependency index phase: loading {} cache(s) with {} worker(s)",
            configs.len(),
            worker_count
        ));
    }

    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            let configs = Arc::clone(&configs);
            let next = Arc::clone(&next);
            let results = Arc::clone(&results);
            let worker_rules = rules.clone();
            let worker_ir = Arc::clone(&ir);
            let worker_profile = profile.clone();
            let worker_rule_hash = current_rule_hash.clone();
            let worker_ir_hash = current_ir_hash.clone();
            let worker_scan_limits = scan_limits;
            scope.spawn(move || {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(config) = configs.get(index) else {
                        break;
                    };
                    let result = run_dependency_cache_load(
                        config,
                        worker_rules.clone(),
                        Arc::clone(&worker_ir),
                        worker_profile.clone(),
                        worker_rule_hash.clone(),
                        worker_ir_hash.clone(),
                        worker_scan_limits,
                        preferred_localisation_languages,
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
/// rules or rules-v2 IR mismatch triggers regeneration. If the IR changed and regeneration fails,
/// the stale cache is rejected because its semantic shards were produced by a different analyzer.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_dependency_cache_load(
    config: &DependencyIndexCache,
    rules: RuleSet,
    ir: Arc<RulesIr>,
    profile: GameProfile,
    current_rule_hash: String,
    current_ir_hash: String,
    scan_limits: WorkspaceScanLimits,
    preferred_localisation_languages: &[String],
    log: Option<&(dyn Fn(&str) + Sync)>,
    progress: Option<&(dyn Fn(usize, usize) + Sync)>,
    cancellation: &WorkspaceScanToken,
) -> Result<(IndexCache, String), String> {
    let started = std::time::Instant::now();
    let result = (|| {
        let load_started = std::time::Instant::now();
        if let Some(log) = log {
            let size = fs::metadata(&config.index_path)
                .ok()
                .filter(|metadata| metadata.is_file())
                .map_or_else(
                    || "size unknown".to_owned(),
                    |metadata| format!("{} bytes", metadata.len()),
                );
            log(&format!(
                "Dependency cache phase: opening {} ({size}) for source {}",
                config.index_path.display(),
                config.root.path.display()
            ));
        }
        let loaded = match IndexCache::load_cancellable_for_install_with_progress(
            &config.index_path,
            cancellation,
            progress,
            Some(&rules),
            preferred_localisation_languages,
        ) {
            Ok(loaded) => loaded,
            Err(error) => {
                if let Some(log) = log {
                    log(&format!(
                        "Dependency cache phase: {} could not be loaded ({error}); rebuilding",
                        config.index_path.display()
                    ));
                }
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
                    &ir,
                    &profile,
                    scan_limits,
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
        if loaded.metadata().rule_hash == current_rule_hash
            && loaded.metadata().ir_hash == current_ir_hash
        {
            if let Some(log) = log {
                log(&format!(
                    "Dependency cache phase: active rules hash matches for {}; refreshing fingerprints",
                    config.root.path.display()
                ));
            }
            // Both rule identities still match, so only the source files may have moved on: refresh the cache
            // against the dependency directory (a fingerprint diff, not a reparse). A failed
            // refresh — moved or unavailable source, cancellation — degrades to the cached
            // symbols, and a save failure keeps the refreshed cache in memory with a warning.
            let refresh_started = std::time::Instant::now();
            if let Some(log) = log {
                log(&format!(
                    "Dependency refresh phase: checking source files under {}",
                    config.root.path.display()
                ));
            }
            return match loaded.refresh_with_ir_cancellable(
                &rules,
                &profile,
                &ir,
                cancellation,
                progress,
            ) {
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
        let stale_ir_hash = loaded.metadata().ir_hash.clone();
        let ir_mismatch = stale_ir_hash != current_ir_hash;
        let incompatible_legacy_hash = !ir.files.is_empty() && stale_hash != current_rule_hash;
        if let Some(log) = log {
            log(&format!(
                "Dependency cache {} is stale (rules hash {stale_hash} != {current_rule_hash}, IR fingerprint {stale_ir_hash} != {current_ir_hash}); regenerating from {}",
                config.index_path.display(),
                config.root.path.display()
            ));
        }
        let rebuilt = build_dependency_cache(
            config,
            &rules,
            &ir,
            &profile,
            scan_limits,
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
            Err(error) if ir_mismatch || incompatible_legacy_hash => Err(format!(
                "{error}; refusing to install the dependency cache because its rule identity is stale (cached legacy hash {stale_hash}, active {current_rule_hash}; cached rules-v2 IR fingerprint {stale_ir_hash}, active {current_ir_hash})"
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
#[allow(clippy::too_many_arguments)]
fn build_dependency_cache(
    config: &DependencyIndexCache,
    rules: &RuleSet,
    ir: &Arc<RulesIr>,
    profile: &GameProfile,
    scan_limits: WorkspaceScanLimits,
    log: Option<&(dyn Fn(&str) + Sync)>,
    progress: Option<&(dyn Fn(usize, usize) + Sync)>,
    cancellation: &WorkspaceScanToken,
    activity: &str,
) -> Result<IndexCache, String> {
    if cancellation.is_cancelled() {
        return Err("dependency index build was cancelled".to_owned());
    }
    if let Some(log) = log {
        log(&format!(
            "{activity} phase: scanning and parsing whitelisted files under {}",
            config.root.path.display()
        ));
    }
    let mut host = AnalysisHost::with_ir(rules.clone(), profile.clone(), Arc::clone(ir));
    host.set_scan_limits(scan_limits);
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![config.root.clone()]));
    let scan_started = std::time::Instant::now();
    let scan_report = host
        .refresh_source_roots_cancellable_with_progress(cancellation, progress)
        .map_err(|error| {
            format!(
                "{activity} failed while indexing {}: {error}",
                config.root.path.display()
            )
        })?;
    if let Some(log) = log {
        log(&format!(
            "{activity}: scanned in {:.1} ms (discovered={}, indexed={}, legacy-encoded={}, skipped={}, issues={}, active={})",
            scan_started.elapsed().as_secs_f64() * 1000.0,
            scan_report.discovered_files,
            scan_report.indexed_files,
            scan_report.legacy_encoded_files,
            scan_report.skipped_entries,
            scan_report.issues.len() + scan_report.omitted_issues,
            host.snapshot().source_files().len(),
        ));
    }
    let build_started = std::time::Instant::now();
    if let Some(log) = log {
        log(&format!(
            "{activity} phase: materializing the persistent index representation"
        ));
    }
    let cache = IndexCache::from_snapshot(&host.snapshot())
        .map_err(|error| format!("{activity} failed: {error}"))?;
    if let Some(log) = log {
        log(&format!(
            "{activity}: cache built in {:.1} ms",
            build_started.elapsed().as_secs_f64() * 1000.0
        ));
    }
    let save_started = std::time::Instant::now();
    if let Some(log) = log {
        log(&format!(
            "{activity} phase: writing cache file {}",
            config.index_path.display()
        ));
    }
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
    use engine::{SourceRoot, SourceRootId, SourceRootKind};
    use game::eu4::{first_party_rules, profile};
    use tempfile::tempdir;
    use text::AbsPath;

    #[test]
    fn parallel_loader_preserves_configuration_order() {
        let container = tempdir().expect("temporary dependency container");
        let mut configs = Vec::new();
        for (index, name) in [(1_u32, "first"), (2, "second")] {
            let root = container.path().join(name);
            fs::create_dir_all(root.join("events")).expect("dependency directory");
            fs::write(
                root.join("events/events.txt"),
                format!("country_event = {{ id = {name}.1 }}\n"),
            )
            .expect("dependency source");
            configs.push(DependencyIndexCache {
                root: SourceRoot::new(
                    SourceRootId::new(index),
                    SourceRootKind::Dependency,
                    AbsPath::normalize(&fs::canonicalize(root).expect("canonical dependency root")),
                ),
                index_path: container.path().join(format!("{name}.pdcindex")),
            });
        }

        let rules = first_party_rules().expect("embedded rules");
        let results = run_dependency_cache_loads(
            configs,
            rules.clone(),
            Arc::new(RulesIr::empty()),
            profile(),
            rules.rule_hash().to_hex(),
            RulesIr::empty().fingerprint(),
            engine::WorkspaceScanLimits::default(),
            &[],
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

    #[test]
    fn changed_ir_rejects_stale_dependency_cache_when_rebuild_fails() {
        let container = tempdir().expect("temporary dependency container");
        let source = container.path().join("dependency");
        fs::create_dir_all(source.join("events")).expect("dependency directory");
        fs::write(
            source.join("events/events.txt"),
            "country_event = { id = stale.1 }\n",
        )
        .expect("dependency source");
        let source = dunce::canonicalize(source).expect("canonical dependency root");
        let root = SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::Dependency,
            AbsPath::normalize(&source),
        );
        let index_path = container.path().join("dependency.pdcindex");
        let rules = first_party_rules().expect("embedded rules");
        let profile = profile();
        let empty_ir = Arc::new(RulesIr::empty());
        let mut stale_host = AnalysisHost::with_ir(rules.clone(), profile.clone(), empty_ir);
        stale_host.apply_change(WorkspaceChange::SetSourceRoots(vec![root.clone()]));
        stale_host
            .refresh_source_roots()
            .expect("scan stale dependency");
        IndexCache::from_snapshot(&stale_host.snapshot())
            .expect("build stale cache")
            .save(&index_path)
            .expect("save stale cache");
        drop(stale_host);
        fs::remove_dir_all(&source).expect("remove dependency so rebuild fails");

        let ir = game::eu4::first_party_ir().expect("embedded rules-v2 IR");
        let result = run_dependency_cache_load(
            &DependencyIndexCache { root, index_path },
            rules.clone(),
            Arc::clone(&ir),
            profile,
            rules.rule_hash().to_hex(),
            ir.fingerprint(),
            WorkspaceScanLimits::default(),
            &[],
            None,
            None,
            &WorkspaceScanToken::new(),
        );
        let error = result.expect_err("stale IR cache must not be returned");
        assert!(
            error.contains("refusing to install the dependency cache"),
            "IR mismatch is explicit: {error}"
        );
    }
}
