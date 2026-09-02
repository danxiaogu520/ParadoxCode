use lsp_types::{
    CodeActionProviderCapability, CompletionOptions, ExecuteCommandOptions,
    HoverProviderCapability, InitializeParams, InitializeResult, OneOf, RenameOptions,
    SemanticTokenModifier as LspSemanticTokenModifier, SemanticTokenType as LspSemanticTokenType,
    SemanticTokensFullOptions, SemanticTokensLegend, SemanticTokensOptions,
    SemanticTokensServerCapabilities, ServerCapabilities, ServerInfo, TextDocumentSyncCapability,
    TextDocumentSyncKind, TextDocumentSyncOptions, WorkDoneProgressOptions,
};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use pdx_analysis::SemanticTokenType;
use pdx_engine::{AnalysisHost, WorkspaceChange, WorkspaceScanToken};
use pdx_game::eu4::mission::TextureAssets;
use pdx_game::{DiscoveryOptions, DiscoveryToken, GameInstallDescriptor, UserPaths};

use crate::protocol::{RpcError, parse_file_uri_str};
use crate::server::PreparedInitialize;
use crate::vanilla::{apply_user_vanilla_configuration, watched_files_registration};
use crate::workspace::{VanillaMode, resolve_source_roots};
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
    /// Machine-local user configuration and cache locations.
    pub user_paths: UserPaths,
    /// Optional user-selected installation root from an editor-guided setup. This takes
    /// precedence over one-time platform discovery.
    pub source_override: Option<PathBuf>,
}
/// Progress-reporting callbacks for the initialize worker. Each is optional so
/// in-memory transport paths (tests) can pass none; the stdio worker supplies
/// both. `stage` feeds the work-done-progress bar and `log` the
/// `window/logMessage` trail. The workspace-scan file counter lives on the
/// background scan worker, which owns the scan after the initialize response.
pub(crate) struct InitializeCallbacks<'a> {
    pub(crate) stage: Option<&'a (dyn Fn(&str) + Sync)>,
    pub(crate) log: Option<&'a (dyn Fn(&str) + Sync)>,
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
        log("Initialization phase: resolving editor configuration and source roots");
    }
    let roots_started = std::time::Instant::now();
    let mut resolved = resolve_source_roots(
        Some(client_root.as_path()),
        initialization_options,
        cancellation,
    )?;
    // Apply query/scan preferences to the candidate host before the first refresh. Keeping these
    // values on the engine host means every immutable snapshot and background clone observes the
    // same user configuration without leaking editor settings into analysis code.
    host.set_scan_limits(resolved.scan_limits);
    host.set_preferred_localisation_languages(resolved.preferred_localisation_languages.clone());
    host.set_completion_source_layers(resolved.completion_source_layers.clone());
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
                "Vanilla index candidate from editor configuration: {}",
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
    let mut auto_vanilla = apply_user_vanilla_configuration(
        &mut resolved,
        auto_vanilla_with_source.as_ref(),
        host.snapshot().rules().game_id(),
        &mut warnings,
    );
    match resolved.vanilla_mode {
        VanillaMode::Auto => {}
        VanillaMode::CacheOnly => {
            if auto_vanilla.is_some() {
                warnings.push(
                    "vanilla.mode=cacheOnly: automatic Vanilla discovery/build is disabled"
                        .to_owned(),
                );
            }
            auto_vanilla = None;
        }
        VanillaMode::Disabled => {
            resolved.index_cache = None;
            auto_vanilla = None;
            warnings.push(
                "vanilla.mode=disabled: Vanilla symbols and automatic cache setup are disabled"
                    .to_owned(),
            );
        }
    }
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
    host.set_scan_filters(resolved.scan_filters.clone());
    host.apply_change(WorkspaceChange::SetSourceRoots(resolved.roots.clone()));
    // The workspace scan runs as a background worker after the initialize
    // response is sent. Scanning thousands of mod files synchronously held the
    // response for tens of seconds while saturating every core; the LSP
    // handshake only needs the configuration and capabilities resolved here.
    // `pdx/ready` is emitted once the scan and any cache installs finish.
    let scan_pending = scan_workspace && !resolved.roots.is_empty();
    if let Some(log) = callbacks.log {
        if scan_pending {
            log(&format!(
                "Initialization phase: scheduling background scan of {} live source root(s)",
                resolved.roots.len()
            ));
        } else {
            let reason = if !scan_workspace {
                "the active rules profile has no game-specific scan"
            } else {
                "no live source roots were configured"
            };
            log(&format!("Workspace scan skipped: {reason}"));
        }
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
    // Mission-preview textures are resolved lazily on the first preview
    // request: discovery scans the game installation's interface definitions
    // and most sessions never open a preview, so startup only captures the
    // (cheap, `Copy`) inputs. An explicitly configured game directory wins;
    // otherwise a one-time quick discovery via the profile descriptor is
    // deferred to the same lazy path. Texture failures stay silent — the
    // preview simply renders without textures.
    let textures = Arc::new(TextureStore::new(
        resolved.game_directory.take(),
        texture_descriptor,
    ));
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
                commands: vec![
                    "pdx/reindexWorkspace".to_owned(),
                    "validateWorkspace".to_owned(),
                ],
                ..ExecuteCommandOptions::default()
            }),
            hover_provider: Some(HoverProviderCapability::Simple(true)),
            definition_provider: Some(OneOf::Left(true)),
            references_provider: Some(OneOf::Left(true)),
            code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
            rename_provider: Some(OneOf::Right(RenameOptions {
                prepare_provider: Some(true),
                work_done_progress_options: WorkDoneProgressOptions::default(),
            })),
            document_symbol_provider: Some(OneOf::Left(true)),
            document_formatting_provider: Some(OneOf::Left(true)),
            workspace_symbol_provider: Some(OneOf::Left(true)),
            inlay_hint_provider: Some(OneOf::Left(true)),
            semantic_tokens_provider: Some(
                SemanticTokensServerCapabilities::SemanticTokensOptions(SemanticTokensOptions {
                    legend: SemanticTokensLegend {
                        token_types: SemanticTokenType::ALL
                            .iter()
                            .map(|token_type| LspSemanticTokenType::new(token_type.as_str()))
                            .collect(),
                        token_modifiers: vec![LspSemanticTokenModifier::DEFINITION],
                    },
                    range: Some(true),
                    full: Some(SemanticTokensFullOptions::Delta { delta: Some(true) }),
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
        scan_pending,
        watcher_registration,
        client_work_done_progress,
        client_snippet_support,
        background_reindex_interval_minutes: resolved.background_reindex_interval_minutes,
        background_reindex_idle_seconds: resolved.background_reindex_idle_seconds,
        ignored_diagnostic_codes: resolved.ignored_diagnostic_codes,
        diagnostic_severity_overrides: resolved.diagnostic_severity_overrides,
        workspace_wide_diagnostics: resolved.workspace_wide_diagnostics,
    })
}

/// Lazily resolved mission-preview texture assets.
///
/// Texture discovery scans the game installation's interface definitions,
/// which costs real time at startup and is only needed when a mission preview
/// is actually opened. The initialize path captures the inputs (both cheap to
/// hold: an optional path and a `Copy` descriptor) and the first preview
/// request materializes the store exactly once.
#[derive(Debug)]
pub(crate) struct TextureStore {
    game_directory: Option<PathBuf>,
    descriptor: Option<GameInstallDescriptor>,
    resolved: OnceLock<Option<Arc<TextureAssets>>>,
}

impl TextureStore {
    /// Captures discovery inputs without touching the game installation.
    pub(crate) fn new(
        game_directory: Option<PathBuf>,
        descriptor: Option<GameInstallDescriptor>,
    ) -> Self {
        Self {
            game_directory,
            descriptor,
            resolved: OnceLock::new(),
        }
    }

    /// Returns the texture assets, discovering them on first use.
    ///
    /// Always `None` for stores created without any input. Repeated calls
    /// return clones of the first resolution.
    pub(crate) fn get(&self) -> Option<Arc<TextureAssets>> {
        self.resolved
            .get_or_init(|| {
                resolve_texture_assets(self.game_directory.clone(), self.descriptor.as_ref())
            })
            .clone()
    }
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
