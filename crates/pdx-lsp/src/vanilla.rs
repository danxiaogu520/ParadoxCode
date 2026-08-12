use std::fs;
use std::path::Path;

use lsp_types::{
    DidChangeWatchedFilesRegistrationOptions, FileSystemWatcher, GlobPattern, OneOf, Registration,
    RegistrationParams, RelativePattern, Uri, WatchKind,
};
use pdx_engine::{
    AnalysisHost, SourceRoot, SourceRootId, SourceRootKind, VanillaIndexCache, WorkspaceChange,
};
use pdx_game::{
    DiscoveryOptions, DiscoveryOutcome, UserConfiguration, UserPaths, discover_installations,
};
use pdx_rules::{GameProfile, RuleSet};
use serde_json::{Value, json};

use crate::initialize::AutoVanillaConfiguration;
use crate::protocol::RpcError;
use crate::server::VanillaSetupCancellation;
use crate::uri::path_to_uri;
use crate::workspace::ResolvedSourceRoots;
use crate::{
    INTERNAL_ERROR, JSON_RPC_VERSION, WATCHED_FILES_REGISTRATION_ID, WATCHED_FILES_REQUEST_ID,
};

pub(crate) fn run_vanilla_cache_load(
    path: &Path,
    rules: RuleSet,
    profile: GameProfile,
    current_rule_hash: String,
    auto_vanilla: Option<&AutoVanillaConfiguration>,
    progress: Option<&(dyn Fn(usize, usize) + Sync)>,
    cancellation: &VanillaSetupCancellation,
) -> Result<(VanillaIndexCache, String), String> {
    let loaded = match VanillaIndexCache::load_cancellable_for_install(
        path,
        &cancellation.workspace,
    ) {
        Ok(loaded) => loaded,
        Err(error) => {
            // A missing, corrupt, or schema-incompatible cache (for example one built
            // by an older test build) falls back to automatic discovery and rebuilds
            // into the same explicit path, instead of silently losing Vanilla symbols.
            if let Some(rebuilt) = rebuild_unavailable_cache(
                path,
                &rules,
                &profile,
                auto_vanilla,
                progress,
                cancellation,
            )? {
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
    if loaded.metadata().rule_hash == current_rule_hash {
        return Ok((
            loaded,
            format!("Vanilla symbols loaded from {}", path.display()),
        ));
    }
    let stale_hash = loaded.metadata().rule_hash.clone();
    let source = loaded.source_root().path.clone();
    let rebuilt = build_cache_from_source(
        &source,
        path,
        &rules,
        &profile,
        progress,
        cancellation,
        "Vanilla cache regeneration",
    );
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
    rules: &RuleSet,
    profile: &GameProfile,
    auto_vanilla: Option<&AutoVanillaConfiguration>,
    progress: Option<&(dyn Fn(usize, usize) + Sync)>,
    cancellation: &VanillaSetupCancellation,
) -> Result<Option<VanillaIndexCache>, String> {
    let Some(auto_vanilla) = auto_vanilla else {
        return Ok(None);
    };
    if cancellation.workspace.is_cancelled() {
        return Err("Vanilla cache rebuild was cancelled".to_owned());
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
                &DiscoveryOptions::default(),
                &cancellation.discovery,
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
    build_cache_from_source(
        &source,
        path,
        rules,
        profile,
        progress,
        cancellation,
        "Vanilla cache rebuild",
    )
    .map(Some)
}

fn build_cache_from_source(
    source: &Path,
    path: &Path,
    rules: &RuleSet,
    profile: &GameProfile,
    progress: Option<&(dyn Fn(usize, usize) + Sync)>,
    cancellation: &VanillaSetupCancellation,
    activity: &str,
) -> Result<VanillaIndexCache, String> {
    let mut host = AnalysisHost::with_profile(rules.clone(), profile.clone());
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(0),
        SourceRootKind::Vanilla,
        source.to_owned(),
    )]));
    host.refresh_source_roots_cancellable_with_progress(&cancellation.workspace, progress)
        .map_err(|error| format!("{activity} failed while indexing {source:?}: {error}"))?;
    let cache = VanillaIndexCache::from_snapshot(&host.snapshot())
        .map_err(|error| format!("{activity} failed: {error}"))?;
    cache.save(path).map_err(|error| {
        format!(
            "{activity} could not be saved to {}: {error}",
            path.display()
        )
    })?;
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
        resolved.vanilla_cache = Some(cache.clone());
        return None;
    }
    if game.auto_discovery_attempted {
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
    progress: Option<&(dyn Fn(usize, usize) + Sync)>,
    cancellation: &VanillaSetupCancellation,
) -> Result<(VanillaIndexCache, String), String> {
    run_auto_vanilla_setup_with_options(
        auto_vanilla,
        rules,
        profile,
        progress,
        cancellation,
        &DiscoveryOptions::default(),
    )
}

pub(crate) fn run_auto_vanilla_setup_with_options(
    auto_vanilla: &AutoVanillaConfiguration,
    rules: RuleSet,
    profile: GameProfile,
    progress: Option<&(dyn Fn(usize, usize) + Sync)>,
    cancellation: &VanillaSetupCancellation,
    discovery_options: &DiscoveryOptions,
) -> Result<(VanillaIndexCache, String), String> {
    let descriptor = auto_vanilla.descriptor;
    let mut configuration =
        UserConfiguration::load(&auto_vanilla.user_paths.config_file).map_err(|error| {
            format!("automatic Vanilla discovery could not load user configuration: {error}")
        })?;
    if configuration
        .games
        .get(descriptor.game_id)
        .is_some_and(|game| game.auto_discovery_attempted)
    {
        return Err(format!(
            "automatic {} discovery was skipped because it was already attempted",
            descriptor.display_name
        ));
    }
    let report = discover_installations(&descriptor, discovery_options, &cancellation.discovery);
    if report.cancelled {
        return Err(format!(
            "automatic {} discovery was cancelled",
            descriptor.display_name
        ));
    }
    let source = match report.installations.as_slice() {
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
    };

    let mut host = AnalysisHost::with_profile(rules, profile);
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(0),
        SourceRootKind::Vanilla,
        source.clone(),
    )]));
    let setup = (|| {
        host.refresh_source_roots_cancellable_with_progress(&cancellation.workspace, progress)
            .map_err(|error| format!("Vanilla indexing failed: {error}"))?;
        let cache = VanillaIndexCache::from_snapshot(&host.snapshot())
            .map_err(|error| format!("Vanilla cache creation failed: {error}"))?;
        let cache_path = auto_vanilla.user_paths.vanilla_cache(descriptor.game_id);
        cache
            .save(&cache_path)
            .map_err(|error| format!("Vanilla cache could not be saved: {error}"))?;
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
