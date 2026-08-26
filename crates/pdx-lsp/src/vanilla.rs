use std::fs;
use std::path::Path;

use lsp_types::{
    DidChangeWatchedFilesRegistrationOptions, FileSystemWatcher, GlobPattern, OneOf, Registration,
    RegistrationParams, RelativePattern, Uri, WatchKind,
};
use pdx_engine::{
    AnalysisHost, IndexCache, SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange,
};
use pdx_game::{
    DiscoveryOptions, DiscoveryOutcome, UserConfiguration, UserPaths, discover_installations,
    validate_installation_for_source,
};
use pdx_rules::{GameProfile, RuleSet};
use serde_json::{Value, json};

use crate::initialize::AutoVanillaConfiguration;
use crate::protocol::RpcError;
use crate::server::IndexSetupCancellation;
use crate::uri::path_to_uri;
use crate::workspace::ResolvedSourceRoots;
use crate::{
    INTERNAL_ERROR, JSON_RPC_VERSION, WATCHED_FILES_REGISTRATION_ID, WATCHED_FILES_REQUEST_ID,
};

pub(crate) struct IndexCacheLoadRequest<'a> {
    pub(crate) path: &'a Path,
    pub(crate) rules: RuleSet,
    pub(crate) profile: GameProfile,
    pub(crate) current_rule_hash: String,
    pub(crate) auto_vanilla: Option<&'a AutoVanillaConfiguration>,
    pub(crate) log: Option<&'a (dyn Fn(&str) + Sync)>,
    pub(crate) progress: Option<&'a (dyn Fn(usize, usize) + Sync)>,
    pub(crate) cancellation: &'a IndexSetupCancellation,
}

struct VanillaIndexContext<'a> {
    rules: &'a RuleSet,
    profile: &'a GameProfile,
    auto_vanilla: Option<&'a AutoVanillaConfiguration>,
    discovery_options: &'a DiscoveryOptions,
    log: Option<&'a (dyn Fn(&str) + Sync)>,
    progress: Option<&'a (dyn Fn(usize, usize) + Sync)>,
    cancellation: &'a IndexSetupCancellation,
}

pub(crate) fn run_index_cache_load(
    request: IndexCacheLoadRequest<'_>,
) -> Result<(IndexCache, String), String> {
    run_index_cache_load_with_options(request, &DiscoveryOptions::default())
}

pub(crate) fn run_index_cache_load_with_options(
    request: IndexCacheLoadRequest<'_>,
    discovery_options: &DiscoveryOptions,
) -> Result<(IndexCache, String), String> {
    let IndexCacheLoadRequest {
        path,
        rules,
        profile,
        current_rule_hash,
        auto_vanilla,
        log,
        progress,
        cancellation,
    } = request;
    let context = VanillaIndexContext {
        rules: &rules,
        profile: &profile,
        auto_vanilla,
        discovery_options,
        log,
        progress,
        cancellation,
    };
    let started = std::time::Instant::now();
    let result = (|| {
        let load_started = std::time::Instant::now();
        if let Some(log) = log {
            let size = fs::metadata(path)
                .ok()
                .filter(|metadata| metadata.is_file())
                .map_or_else(
                    || "size unknown".to_owned(),
                    |metadata| format!("{} bytes", metadata.len()),
                );
            log(&format!(
                "Vanilla cache phase: opening SQLite index {} ({size}); validating schema, metadata, and indexed rows",
                path.display()
            ));
        }
        let loaded = match IndexCache::load_cancellable_for_install_with_progress(
            path,
            &cancellation.workspace,
            progress,
        ) {
            Ok(loaded) => loaded,
            Err(error) => {
                // A missing, corrupt, or schema-incompatible cache (for example one built
                // by an older test build) falls back to automatic discovery and rebuilds
                // into the same explicit path, instead of silently losing Vanilla symbols.
                if let Some(log) = log {
                    log(&format!(
                        "Vanilla cache {} could not be loaded ({error}); attempting a rebuild",
                        path.display()
                    ));
                }
                if let Some(rebuilt) = rebuild_unavailable_cache(path, &context)? {
                    return Ok((
                        rebuilt,
                        format!(
                            "Vanilla cache {} was unavailable and has been rebuilt from the discovered installation",
                            path.display()
                        ),
                    ));
                }
                return Err(format!(
                    "Vanilla cache {} could not be loaded; continuing without Vanilla symbols: {error}",
                    path.display()
                ));
            }
        };
        if let Some(log) = log {
            log(&format!(
                "Vanilla cache decode: {:.1} ms ({} file(s), {} position(s) loaded)",
                load_started.elapsed().as_secs_f64() * 1000.0,
                loaded.source_files().len(),
                loaded.index().position_ranges().len(),
            ));
        }
        if loaded.metadata().rule_hash == current_rule_hash {
            if let Some(log) = log {
                log(&format!(
                    "Vanilla cache phase: active rules hash matches ({current_rule_hash}); no rebuild required"
                ));
            }
            return Ok((
                loaded,
                format!("Vanilla symbols loaded from {}", path.display()),
            ));
        }
        let stale_hash = loaded.metadata().rule_hash.clone();
        let source = loaded.source_root().path.clone();
        if let Some(log) = log {
            log(&format!(
                "Vanilla cache {} is stale (rules hash {stale_hash} != {current_rule_hash}); regenerating from {}",
                path.display(),
                source.display()
            ));
        }
        let rebuilt =
            build_cache_from_source(&source, path, &context, "Vanilla cache regeneration");
        match rebuilt {
            Ok(cache) => Ok((
                cache,
                format!(
                    "Vanilla cache was regenerated for the active rules hash {current_rule_hash} and loaded from {}",
                    path.display()
                ),
            )),
            Err(error) => Ok((
                loaded,
                format!("{error}; using the existing cache built with rules hash {stale_hash}"),
            )),
        }
    })();
    if let Some(log) = log {
        log(&format!(
            "Vanilla cache worker total: {:.1} ms",
            started.elapsed().as_secs_f64() * 1000.0
        ));
    }
    result
}

/// Rebuilds an explicit cache path when the cache file itself cannot be loaded.
///
/// Returns `Ok(None)` when automatic discovery is unavailable or declines to
/// participate; `Ok(Some(cache))` after a successful rebuild. Discovery failures
/// are reported but never recorded in the user configuration, because an explicit
/// cache path is a caller-owned location and must not mark the user-level
/// automatic setup as attempted.
fn rebuild_unavailable_cache(
    path: &Path,
    context: &VanillaIndexContext<'_>,
) -> Result<Option<IndexCache>, String> {
    let Some(auto_vanilla) = context.auto_vanilla else {
        return Ok(None);
    };
    if context.cancellation.workspace.is_cancelled() {
        return Err("Vanilla cache rebuild was cancelled".to_owned());
    }
    let discovery_started = std::time::Instant::now();
    if let Some(log) = context.log {
        log("Vanilla cache rebuild phase: resolving a usable game installation");
    }
    let configured_source = UserConfiguration::load(&auto_vanilla.user_paths.config_file)
        .ok()
        .and_then(|configuration| {
            configuration
                .games
                .get(auto_vanilla.descriptor.game_id)
                .and_then(|game| game.vanilla_source.clone())
        })
        .filter(|source| source.is_dir());
    let source = match configured_source {
        Some(source) => source,
        None => {
            let report = discover_installations(
                &auto_vanilla.descriptor,
                context.discovery_options,
                &context.cancellation.discovery,
            );
            if report.cancelled {
                return Err("Vanilla cache rebuild discovery was cancelled".to_owned());
            }
            match report.installations.as_slice() {
                [source] => source.clone(),
                [] => return Ok(None),
                candidates => {
                    return Err(format!(
                        "multiple {} installations were found; run `pdx setup vanilla --game {} --source <directory>` to choose one: {}",
                        auto_vanilla.descriptor.display_name,
                        auto_vanilla.descriptor.game_id,
                        candidates
                            .iter()
                            .map(|path| path.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
        }
    };
    if let Some(log) = context.log {
        log(&format!(
            "Vanilla cache rebuild source resolved in {:.1} ms: {}",
            discovery_started.elapsed().as_secs_f64() * 1000.0,
            source.display()
        ));
    }
    // The old file is known to be unusable (missing, corrupt, or from an older
    // schema); remove it so the rebuild can write a fresh cache in its place.
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "Vanilla cache rebuild could not replace {}: {error}",
                path.display()
            ));
        }
    }
    if let Some(log) = context.log {
        log(&format!(
            "Vanilla cache rebuild phase: indexing source {} and replacing {}",
            source.display(),
            path.display()
        ));
    }
    build_cache_from_source(&source, path, context, "Vanilla cache rebuild").map(Some)
}

fn build_cache_from_source(
    source: &Path,
    path: &Path,
    context: &VanillaIndexContext<'_>,
    activity: &str,
) -> Result<IndexCache, String> {
    let mut host = AnalysisHost::with_profile(context.rules.clone(), context.profile.clone());
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(0),
        SourceRootKind::Vanilla,
        source.to_owned(),
    )]));
    if let Some(log) = context.log {
        log(&format!(
            "{activity} phase: scanning and parsing whitelisted files under {}",
            source.display()
        ));
    }
    let scan_started = std::time::Instant::now();
    let scan_report = host
        .refresh_source_roots_cancellable_with_progress(
            &context.cancellation.workspace,
            context.progress,
        )
        .map_err(|error| format!("{activity} failed while indexing {source:?}: {error}"))?;
    if let Some(log) = context.log {
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
    let cache = IndexCache::from_snapshot(&host.snapshot())
        .map_err(|error| format!("{activity} failed: {error}"))?;
    // The cache owns only the compact source metadata and index shards.  Release the temporary
    // parse/HIR-heavy Vanilla host before SQLite serialization so cache materialization does not
    // keep the full source tree live alongside the artifact.
    drop(host);
    if let Some(log) = context.log {
        log(&format!(
            "{activity}: cache built in {:.1} ms",
            build_started.elapsed().as_secs_f64() * 1000.0
        ));
    }
    let save_started = std::time::Instant::now();
    cache
        .save_with_progress(path, context.progress)
        .map_err(|error| {
            format!(
                "{activity} could not be saved to {}: {error}",
                path.display()
            )
        })?;
    if let Some(log) = context.log {
        log(&format!(
            "{activity}: saved to {} in {:.1} ms",
            path.display(),
            save_started.elapsed().as_secs_f64() * 1000.0
        ));
    }
    Ok(cache)
}

pub(crate) fn watched_files_registration(
    roots: &[SourceRoot],
    capability: Option<&lsp_types::DidChangeWatchedFilesClientCapabilities>,
) -> Result<Option<Value>, RpcError> {
    let Some(capability) =
        capability.filter(|capability| capability.dynamic_registration == Some(true))
    else {
        return Ok(None);
    };
    let live_roots = roots
        .iter()
        .filter(|root| {
            matches!(
                root.kind,
                SourceRootKind::CurrentMod | SourceRootKind::Dependency
            )
        })
        .collect::<Vec<_>>();
    if live_roots.is_empty() {
        return Ok(None);
    }

    let kind = WatchKind::Create | WatchKind::Change | WatchKind::Delete;
    let watchers = if capability.relative_pattern_support == Some(true) {
        live_roots
            .into_iter()
            .map(|root| {
                let uri = path_to_uri(&root.path).parse::<Uri>().map_err(|_| {
                    RpcError::new(
                        INTERNAL_ERROR,
                        format!("source root has no valid file URI: {}", root.path.display()),
                    )
                })?;
                Ok(FileSystemWatcher {
                    glob_pattern: GlobPattern::Relative(RelativePattern {
                        base_uri: OneOf::Right(uri),
                        pattern: "**/*".to_owned(),
                    }),
                    kind: Some(kind),
                })
            })
            .collect::<Result<Vec<_>, RpcError>>()?
    } else {
        vec![FileSystemWatcher {
            glob_pattern: GlobPattern::String("**/*".to_owned()),
            kind: Some(kind),
        }]
    };
    let options = serde_json::to_value(DidChangeWatchedFilesRegistrationOptions { watchers })
        .map_err(|error| RpcError {
            code: INTERNAL_ERROR,
            message: format!("failed to serialize watched-file registration: {error}"),
        })?;
    let params = RegistrationParams {
        registrations: vec![Registration {
            id: WATCHED_FILES_REGISTRATION_ID.to_owned(),
            method: "workspace/didChangeWatchedFiles".to_owned(),
            register_options: Some(options),
        }],
    };
    Ok(Some(json!({
        "jsonrpc": JSON_RPC_VERSION,
        "id": WATCHED_FILES_REQUEST_ID,
        "method": "client/registerCapability",
        "params": params,
    })))
}

pub(crate) fn apply_user_vanilla_configuration(
    resolved: &mut ResolvedSourceRoots,
    auto_vanilla: Option<&AutoVanillaConfiguration>,
    active_game_id: &str,
    warnings: &mut Vec<String>,
) -> Option<AutoVanillaConfiguration> {
    let auto_vanilla = auto_vanilla?;
    if resolved.vanilla_explicit || auto_vanilla.descriptor.game_id != active_game_id {
        return None;
    }
    let configuration = match UserConfiguration::load(&auto_vanilla.user_paths.config_file) {
        Ok(configuration) => configuration,
        Err(error) => {
            warnings.push(format!(
                "ParadoxCode user configuration {} could not be loaded; automatic Vanilla discovery is disabled: {error}",
                auto_vanilla.user_paths.config_file.display()
            ));
            return None;
        }
    };
    let Some(game) = configuration.games.get(active_game_id) else {
        return Some(auto_vanilla.clone());
    };
    if let Some(cache) = game.vanilla_cache.as_ref() {
        resolved.index_cache = Some(cache.clone());
        return None;
    }
    if game.auto_discovery_attempted && auto_vanilla.source_override.is_none() {
        warnings.push(format!(
            "Automatic {} discovery was already attempted without a usable cache; run `pdx setup vanilla --game {active_game_id} --deep` to search again",
            auto_vanilla.descriptor.display_name
        ));
        None
    } else {
        Some(auto_vanilla.clone())
    }
}

pub(crate) fn run_auto_vanilla_setup(
    auto_vanilla: &AutoVanillaConfiguration,
    rules: RuleSet,
    profile: GameProfile,
    log: Option<&(dyn Fn(&str) + Sync)>,
    progress: Option<&(dyn Fn(usize, usize) + Sync)>,
    cancellation: &IndexSetupCancellation,
) -> Result<(IndexCache, String), String> {
    run_auto_vanilla_setup_with_options(
        auto_vanilla,
        rules,
        profile,
        log,
        progress,
        cancellation,
        &DiscoveryOptions::default(),
    )
}

pub(crate) fn run_auto_vanilla_setup_with_options(
    auto_vanilla: &AutoVanillaConfiguration,
    rules: RuleSet,
    profile: GameProfile,
    log: Option<&(dyn Fn(&str) + Sync)>,
    progress: Option<&(dyn Fn(usize, usize) + Sync)>,
    cancellation: &IndexSetupCancellation,
    discovery_options: &DiscoveryOptions,
) -> Result<(IndexCache, String), String> {
    let started = std::time::Instant::now();
    let result = (|| {
        let descriptor = auto_vanilla.descriptor;
        if let Some(log) = log {
            log(&format!(
                "Vanilla setup phase: loading user configuration from {}",
                auto_vanilla.user_paths.config_file.display()
            ));
        }
        let mut configuration = UserConfiguration::load(&auto_vanilla.user_paths.config_file)
            .map_err(|error| {
                format!("automatic Vanilla discovery could not load user configuration: {error}")
            })?;
        if configuration
            .games
            .get(descriptor.game_id)
            .is_some_and(|game| game.auto_discovery_attempted)
            && auto_vanilla.source_override.is_none()
        {
            return Err(format!(
                "automatic {} discovery was skipped because it was already attempted",
                descriptor.display_name
            ));
        }
        let source = if let Some(source) = auto_vanilla.source_override.as_ref() {
            if let Some(log) = log {
                log(&format!(
                    "Vanilla setup phase: validating explicitly selected installation {}",
                    source.display()
                ));
            }
            if !validate_installation_for_source(source, &descriptor) {
                return Err(format!(
                    "the selected {} directory is not a valid installation: {}",
                    descriptor.display_name,
                    source.display()
                ));
            }
            source.clone()
        } else {
            if let Some(log) = log {
                log(&format!(
                    "Vanilla setup phase: discovering {} installation locations",
                    descriptor.display_name
                ));
            }
            let discovery_started = std::time::Instant::now();
            let report =
                discover_installations(&descriptor, discovery_options, &cancellation.discovery);
            if let Some(log) = log {
                log(&format!(
                    "Vanilla discovery: {:.1} ms ({} candidate(s) found)",
                    discovery_started.elapsed().as_secs_f64() * 1000.0,
                    report.installations.len()
                ));
            }
            if report.cancelled {
                return Err(format!(
                    "automatic {} discovery was cancelled",
                    descriptor.display_name
                ));
            }
            match report.installations.as_slice() {
                [source] => source.clone(),
                [] => {
                    record_discovery_outcome(
                        &mut configuration,
                        descriptor.game_id,
                        DiscoveryOutcome::NotFound,
                        &auto_vanilla.user_paths,
                    )?;
                    return Err(format!(
                        "{} was not found in common installation locations; run `pdx setup vanilla --game {} --deep` to search local disks",
                        descriptor.display_name, descriptor.game_id
                    ));
                }
                candidates => {
                    record_discovery_outcome(
                        &mut configuration,
                        descriptor.game_id,
                        DiscoveryOutcome::MultipleCandidates,
                        &auto_vanilla.user_paths,
                    )?;
                    return Err(format!(
                        "multiple {} installations were found:\n{}\nrun `pdx setup vanilla --game {} --source <directory>` to choose one",
                        descriptor.display_name,
                        candidates
                            .iter()
                            .map(|path| format!("  {}", path.display()))
                            .collect::<Vec<_>>()
                            .join("\n"),
                        descriptor.game_id
                    ));
                }
            }
        };
        if let Some(log) = log {
            log(&format!(
                "Vanilla setup phase: selected source {}",
                source.display()
            ));
        }

        let mut host = AnalysisHost::with_profile(rules, profile);
        host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
            SourceRootId::new(0),
            SourceRootKind::Vanilla,
            source.clone(),
        )]));
        let setup = (|| {
            if let Some(log) = log {
                log(&format!(
                    "Vanilla setup phase: scanning and parsing whitelisted files under {}",
                    source.display()
                ));
            }
            let scan_started = std::time::Instant::now();
            let scan_report = host
                .refresh_source_roots_cancellable_with_progress(&cancellation.workspace, progress)
                .map_err(|error| format!("Vanilla indexing failed: {error}"))?;
            if let Some(log) = log {
                log(&format!(
                    "Vanilla scan finished in {:.1} ms: discovered={}, indexed={}, legacy-encoded={}, skipped={}, issues={}, active={}",
                    scan_started.elapsed().as_secs_f64() * 1000.0,
                    scan_report.discovered_files,
                    scan_report.indexed_files,
                    scan_report.legacy_encoded_files,
                    scan_report.skipped_entries,
                    scan_report.issues.len() + scan_report.omitted_issues,
                    host.snapshot().source_files().len(),
                ));
            }
            if let Some(log) = log {
                log("Vanilla setup phase: materializing the in-memory index cache");
            }
            let build_started = std::time::Instant::now();
            let cache = IndexCache::from_snapshot(&host.snapshot())
                .map_err(|error| format!("Vanilla cache creation failed: {error}"))?;
            // The returned cache does not retain source text, parsed CSTs, or HIR. Drop the
            // temporary full Vanilla host before saving and handing the cache to the event loop.
            drop(host);
            if let Some(log) = log {
                log(&format!(
                    "Vanilla cache built in {:.1} ms",
                    build_started.elapsed().as_secs_f64() * 1000.0
                ));
            }
            let cache_path = auto_vanilla.user_paths.vanilla_cache(descriptor.game_id);
            if let Some(log) = log {
                log(&format!(
                    "Vanilla setup phase: writing persistent cache to {}",
                    cache_path.display()
                ));
            }
            let save_started = std::time::Instant::now();
            cache
                .save_with_progress(&cache_path, progress)
                .map_err(|error| format!("Vanilla cache could not be saved: {error}"))?;
            if let Some(log) = log {
                log(&format!(
                    "Vanilla cache saved to {} in {:.1} ms",
                    cache_path.display(),
                    save_started.elapsed().as_secs_f64() * 1000.0
                ));
            }
            Ok::<_, String>((cache, cache_path))
        })();
        match setup {
            Ok((cache, cache_path)) => {
                let game = configuration
                    .games
                    .entry(descriptor.game_id.to_owned())
                    .or_default();
                game.auto_discovery_attempted = true;
                game.discovery_outcome = Some(DiscoveryOutcome::Configured);
                game.vanilla_source = Some(source.clone());
                game.vanilla_cache = Some(cache_path.clone());
                configuration
                    .save(&auto_vanilla.user_paths.config_file)
                    .map_err(|error| {
                        format!(
                            "Vanilla cache was built but user configuration could not be saved: {error}"
                        )
                    })?;
                if let Some(log) = log {
                    log(&format!(
                        "Vanilla setup phase: recorded the selected source and cache in {}",
                        auto_vanilla.user_paths.config_file.display()
                    ));
                }
                Ok((
                    cache,
                    format!(
                        "{} Vanilla symbols are now enabled from {}",
                        descriptor.display_name,
                        source.display()
                    ),
                ))
            }
            Err(error) => {
                if cancellation.discovery.is_cancelled() || cancellation.workspace.is_cancelled() {
                    return Err(format!(
                        "automatic {} setup was cancelled",
                        descriptor.display_name
                    ));
                }
                let game = configuration
                    .games
                    .entry(descriptor.game_id.to_owned())
                    .or_default();
                game.auto_discovery_attempted = true;
                game.discovery_outcome = Some(DiscoveryOutcome::Failed);
                game.vanilla_source = Some(source);
                let save_error = configuration
                    .save(&auto_vanilla.user_paths.config_file)
                    .err();
                match save_error {
                    Some(save_error) => Err(format!(
                        "{error}; failed to record the attempt: {save_error}"
                    )),
                    None => Err(error),
                }
            }
        }
    })();
    if let Some(log) = log {
        log(&format!(
            "Vanilla cache worker total: {:.1} ms",
            started.elapsed().as_secs_f64() * 1000.0
        ));
    }
    result
}

fn record_discovery_outcome(
    configuration: &mut UserConfiguration,
    game_id: &str,
    outcome: DiscoveryOutcome,
    paths: &UserPaths,
) -> Result<(), String> {
    let game = configuration.games.entry(game_id.to_owned()).or_default();
    game.auto_discovery_attempted = true;
    game.discovery_outcome = Some(outcome);
    configuration
        .save(&paths.config_file)
        .map_err(|error| format!("automatic discovery result could not be saved: {error}"))
}
