use lsp_types::{
    CompletionOptions, ExecuteCommandOptions, HoverProviderCapability, InitializeParams,
    InitializeResult, OneOf, RenameOptions, SemanticTokenModifier as LspSemanticTokenModifier,
    SemanticTokenType as LspSemanticTokenType, SemanticTokensFullOptions, SemanticTokensLegend,
    SemanticTokensOptions, SemanticTokensServerCapabilities, ServerCapabilities, ServerInfo,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextDocumentSyncOptions,
    WorkDoneProgressOptions,
};
use std::path::PathBuf;
use std::sync::Arc;

use pdx_analysis::SemanticTokenType;
use pdx_engine::{AnalysisHost, WorkspaceChange, WorkspaceScanToken};
use pdx_game::eu4::mission::TextureAssets;
use pdx_game::{DiscoveryOptions, DiscoveryToken, GameInstallDescriptor, UserPaths};

use crate::protocol::{RpcError, parse_file_uri_str, workspace_scan_error};
use crate::server::PreparedInitialize;
use crate::vanilla::{apply_user_vanilla_configuration, watched_files_registration};
use crate::workspace::resolve_source_roots;
use crate::{INTERNAL_ERROR, INVALID_PARAMS, REQUEST_CANCELLED};

/// Explicit process-level options passed by an editor or CLI.
///
/// Rules are intentionally absent: official composition roots supply their compiled first-party
/// [`pdx_rules::RuleSet`] directly and no user-controlled path can replace it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InitializeOptions;

/// Machine-local automatic Vanilla discovery supplied by a game composition root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutoVanillaConfiguration {
    /// Data-only installation facts for the selected profile.
    pub descriptor: GameInstallDescriptor,
    /// Shared user configuration and cache locations.
    pub user_paths: UserPaths,
    /// Optional user-selected installation root from an editor-guided setup. This takes
    /// precedence over one-time platform discovery.
    pub source_override: Option<PathBuf>,
}
/// Progress-reporting callbacks for the initialize worker. Each is optional so
/// in-memory transport paths (tests) can pass none; the stdio worker supplies
/// all three. `stage` feeds the work-done-progress bar, `log` the
/// `window/logMessage` trail, and `progress` the workspace-scan file counter.
pub(crate) struct InitializeCallbacks<'a> {
    pub(crate) stage: Option<&'a (dyn Fn(&str) + Sync)>,
    pub(crate) log: Option<&'a (dyn Fn(&str) + Sync)>,
    pub(crate) progress: Option<&'a (dyn Fn(usize, usize) + Sync)>,
}

pub(crate) fn prepare_initialize_candidate(
    mut host: AnalysisHost,
    params: InitializeParams,
    scan_workspace: bool,
    auto_vanilla: Option<&AutoVanillaConfiguration>,
    cancellation: &WorkspaceScanToken,
    callbacks: &InitializeCallbacks<'_>,
) -> Result<PreparedInitialize, RpcError> {
    if cancellation.is_cancelled() {
        return Err(RpcError::new(REQUEST_CANCELLED, "request was cancelled"));
    }
    let started = std::time::Instant::now();
    let (rule_hash, game_id) = {
        let snapshot = host.snapshot();
        let rules = snapshot.rules();
        (rules.rule_hash().to_hex(), rules.game_id().to_owned())
    };
    if let Some(log) = callbacks.log {
        log(&format!(
            "pdx-ls initializing (game profile '{game_id}', rules hash {rule_hash})"
        ));
    }
    let initialization_options = params.initialization_options.clone();
    let workspace_root = params
        .workspace_folders
        .as_ref()
        .and_then(|folders| folders.first())
        .map(|folder| parse_file_uri_str(folder.uri.as_str()))
        .transpose()?
        .ok_or_else(|| {
            RpcError::new(
                INVALID_PARAMS,
                "initialize requires at least one workspace folder; rootUri-only clients are not supported",
            )
    })?;
    let client_root = workspace_root;
    if let Some(configuration) = auto_vanilla {
        let parse_cache = configuration
            .user_paths
            .cache_root
            .join(game_id.as_str())
            .join("parse-cache");
        host.set_parse_cache_dir(Some(parse_cache.clone()));
        if let Some(log) = callbacks.log {
            log(&format!(
                "Persistent syntax-tree cache enabled at {}",
                parse_cache.display()
            ));
        }
    }
    let watched_files_capability = params
        .capabilities
        .workspace
        .as_ref()
        .and_then(|workspace| workspace.did_change_watched_files.as_ref());
    let client_work_done_progress = params
        .capabilities
        .window
        .as_ref()
        .and_then(|window| window.work_done_progress)
        .unwrap_or(false);
    let client_snippet_support = params
        .capabilities
        .text_document
        .as_ref()
        .and_then(|text_document| text_document.completion.as_ref())
        .and_then(|completion| completion.completion_item.as_ref())
        .and_then(|item| item.snippet_support)
        .unwrap_or(false);
    if let Some(stage) = callbacks.stage {
        stage("Loading workspace configuration…");
    }
    if let Some(log) = callbacks.log {
        log("Initialization phase: resolving project configuration and source roots");
    }
    let roots_started = std::time::Instant::now();
    let mut resolved = resolve_source_roots(
        Some(client_root.as_path()),
        initialization_options,
        cancellation,
    )?;
    if let Some(log) = callbacks.log {
        log(&format!(
            "Source roots resolved in {:.1} ms: workspace {}, {} live source root(s), {} dependency cache(s)",
            roots_started.elapsed().as_secs_f64() * 1000.0,
            resolved
                .workspace_root
                .as_deref()
                .map_or_else(|| "<none>".to_owned(), |path| path.display().to_string()),
            resolved.roots.len(),
            resolved.dependency_caches.len(),
        ));
        for root in &resolved.roots {
            log(&format!(
                "  live root: id={}, kind={:?}, order={}, path={}",
                root.id.get(),
                root.kind,
                root.order,
                root.path.display()
            ));
        }
        for cache in &resolved.dependency_caches {
            log(&format!(
                "  cached dependency: id={}, order={}, source={}, index={}",
                cache.root.id.get(),
                cache.root.order,
                cache.root.path.display(),
                cache.index_path.display()
            ));
        }
    }
    let mut warnings = Vec::new();
    // The texture loader needs the profile descriptor for discovery. Capture it
    // BEFORE the vanilla configuration pass: that pass legitimately returns
    // `None` when the user configured an explicit `vanilla_index_cache`, but
    // mission-preview textures must not depend on it.
    let texture_descriptor = auto_vanilla.map(|config| config.descriptor);
    if let Some(log) = callbacks.log {
        match resolved.index_cache.as_ref() {
            Some(path) => log(&format!(
                "Vanilla index candidate from project configuration: {}",
                path.display()
            )),
            None => log("Vanilla index candidate: automatic discovery or user configuration"),
        }
    }
    let selected_game_directory = resolved.game_directory.clone();
    let auto_vanilla_with_source = auto_vanilla.map(|configuration| {
        let mut configuration = configuration.clone();
        if let Some(source) = selected_game_directory {
            configuration.source_override = Some(source);
        }
        configuration
    });
    if let Some(log) = callbacks.log {
        log("Initialization phase: applying user-level Vanilla configuration");
    }
    let user_vanilla_started = std::time::Instant::now();
    let auto_vanilla = apply_user_vanilla_configuration(
        &mut resolved,
        auto_vanilla_with_source.as_ref(),
        host.snapshot().rules().game_id(),
        &mut warnings,
    );
    if let Some(log) = callbacks.log {
        let selection = match (resolved.index_cache.as_ref(), auto_vanilla.is_some()) {
            (Some(path), _) => format!("cache selected at {}", path.display()),
            (None, true) => "automatic discovery/build selected".to_owned(),
            (None, false) => "no Vanilla cache worker selected".to_owned(),
        };
        log(&format!(
            "User-level Vanilla configuration applied in {:.1} ms: {selection}",
            user_vanilla_started.elapsed().as_secs_f64() * 1000.0
        ));
    }
    host.apply_change(WorkspaceChange::SetWorkspaceRoot(resolved.workspace_root));
    host.apply_change(WorkspaceChange::SetSourceRoots(resolved.roots.clone()));
    if scan_workspace && !resolved.roots.is_empty() {
        if let Some(stage) = callbacks.stage {
            stage("Discovering and indexing workspace files…");
        }
        if let Some(log) = callbacks.log {
            log(&format!(
                "Initialization phase: scanning {} live source root(s)",
                resolved.roots.len()
            ));
        }
        let scan_started = std::time::Instant::now();
        let scan_report = host
            .refresh_source_roots_cancellable_with_progress(cancellation, callbacks.progress)
            .map_err(workspace_scan_error)?;
        if let Some(log) = callbacks.log {
            log(&format!(
                "Workspace scan finished in {:.1} ms: discovered={}, indexed={}, legacy-encoded={}, skipped={}, issues={}, source file(s) active={}",
                scan_started.elapsed().as_secs_f64() * 1000.0,
                scan_report.discovered_files,
                scan_report.indexed_files,
                scan_report.legacy_encoded_files,
                scan_report.skipped_entries,
                scan_report.issues.len() + scan_report.omitted_issues,
                host.snapshot().source_files().len(),
            ));
        }
    } else if let Some(log) = callbacks.log {
        let reason = if !scan_workspace {
            "the active rules profile has no game-specific scan"
        } else {
            "no live source roots were configured"
        };
        log(&format!("Workspace scan skipped: {reason}"));
    }
    let index_cache = match resolved.index_cache.take() {
        None => None,
        Some(path) if !scan_workspace => {
            warnings.push(format!(
                "Vanilla cache {} was not loaded because no validated rules artifact is active",
                path.display()
            ));
            None
        }
        // A missing file is handled by the background load worker, which falls back
        // to automatic discovery and rebuilds the cache in place.
        Some(path) => Some(path),
    };
    // Mission-preview textures: an explicitly configured game directory wins;
    // otherwise a one-time quick discovery via the profile descriptor. This is
    // independent of the Vanilla cache configuration — a configured
    // `vanilla_index_cache` must not disable textures. Texture failures are
    // silent — the preview simply renders without textures.
    if let Some(stage) = callbacks.stage {
        stage("Preparing mission preview textures…");
    }
    if let Some(log) = callbacks.log {
        log("Initialization phase: discovering and indexing mission-preview textures");
    }
    let texture_started = std::time::Instant::now();
    let textures =
        resolve_texture_assets(resolved.game_directory.take(), texture_descriptor.as_ref());
    if let Some(log) = callbacks.log {
        match textures.as_ref() {
            Some(textures) => log(&format!(
                "Mission textures ready in {:.1} ms: {} sprite(s) from the game installation",
                texture_started.elapsed().as_secs_f64() * 1000.0,
                textures.sprite_count(),
            )),
            None => log(&format!(
                "Mission textures unavailable after {:.1} ms (no usable game installation found)",
                texture_started.elapsed().as_secs_f64() * 1000.0
            )),
        }
    }
    if cancellation.is_cancelled() {
        return Err(RpcError::new(REQUEST_CANCELLED, "request was cancelled"));
    }
    if let Some(stage) = callbacks.stage {
        stage("Registering source-root file watchers…");
    }
    let watcher_started = std::time::Instant::now();
    let watcher_registration =
        watched_files_registration(&resolved.roots, watched_files_capability)?;
    if let Some(log) = callbacks.log {
        log(&format!(
            "Source-root watcher registration prepared in {:.1} ms: {}",
            watcher_started.elapsed().as_secs_f64() * 1000.0,
            if watcher_registration.is_some() {
                "client registration request will be sent"
            } else {
                "client capability not available or no live roots"
            }
        ));
        log(&format!(
            "Initialization finished in {:.1} ms (revision {}, {} source file(s) indexed)",
            started.elapsed().as_secs_f64() * 1000.0,
            host.snapshot().revision(),
            host.snapshot().source_files().len()
        ));
    }
    let result = serde_json::to_value(InitializeResult {
        capabilities: ServerCapabilities {
            text_document_sync: Some(TextDocumentSyncCapability::Options(
                TextDocumentSyncOptions {
                    open_close: Some(true),
                    change: Some(TextDocumentSyncKind::INCREMENTAL),
                    ..TextDocumentSyncOptions::default()
                },
            )),
            completion_provider: Some(CompletionOptions {
                trigger_characters: Some(vec!["=".to_owned(), " ".to_owned(), ":".to_owned()]),
                resolve_provider: Some(true),
                ..CompletionOptions::default()
            }),
            execute_command_provider: Some(ExecuteCommandOptions {
                commands: vec!["pdx/reindexWorkspace".to_owned()],
                ..ExecuteCommandOptions::default()
            }),
            hover_provider: Some(HoverProviderCapability::Simple(true)),
            definition_provider: Some(OneOf::Left(true)),
            references_provider: Some(OneOf::Left(true)),
            rename_provider: Some(OneOf::Right(RenameOptions {
                prepare_provider: Some(true),
                work_done_progress_options: WorkDoneProgressOptions::default(),
            })),
            document_symbol_provider: Some(OneOf::Left(true)),
            document_formatting_provider: Some(OneOf::Left(true)),
            workspace_symbol_provider: Some(OneOf::Left(true)),
            semantic_tokens_provider: Some(
                SemanticTokensServerCapabilities::SemanticTokensOptions(SemanticTokensOptions {
                    legend: SemanticTokensLegend {
                        token_types: SemanticTokenType::ALL
                            .iter()
                            .map(|token_type| LspSemanticTokenType::new(token_type.as_str()))
                            .collect(),
                        token_modifiers: vec![LspSemanticTokenModifier::DEFINITION],
                    },
                    range: Some(false),
                    full: Some(SemanticTokensFullOptions::Bool(true)),
                    work_done_progress_options: WorkDoneProgressOptions::default(),
                }),
            ),
            ..ServerCapabilities::default()
        },
        server_info: Some(ServerInfo {
            name: "pdx-ls".to_owned(),
            version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        }),
    })
    .map_err(|error| RpcError {
        code: INTERNAL_ERROR,
        message: format!("failed to serialize initialize result: {error}"),
    })?;
    Ok(PreparedInitialize {
        host,
        result,
        warnings,
        auto_vanilla,
        index_cache,
        textures,
        dependency_caches: resolved.dependency_caches,
        watcher_registration,
        client_work_done_progress,
        client_snippet_support,
        background_reindex_interval_minutes: resolved.background_reindex_interval_minutes,
        background_reindex_idle_seconds: resolved.background_reindex_idle_seconds,
    })
}

/// Builds the mission-preview texture store for the active game installation.
/// `configured` (explicit `gameDirectory`) wins; otherwise a one-time quick
/// discovery using the profile descriptor is attempted.
fn resolve_texture_assets(
    configured: Option<PathBuf>,
    descriptor: Option<&GameInstallDescriptor>,
) -> Option<Arc<TextureAssets>> {
    let root = if let Some(root) = configured {
        Some(root)
    } else if let Some(descriptor) = descriptor {
        let report = pdx_game::discover_installations(
            descriptor,
            &DiscoveryOptions::default(),
            &DiscoveryToken::new(),
        );
        report.installations.into_iter().next()
    } else {
        None
    };
    root.as_deref().and_then(TextureAssets::load).map(Arc::new)
}
