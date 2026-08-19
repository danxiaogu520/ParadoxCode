use lsp_types::{
    CompletionOptions, HoverProviderCapability, InitializeParams, InitializeResult, OneOf,
    RenameOptions, SemanticTokenModifier as LspSemanticTokenModifier,
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
use crate::{INTERNAL_ERROR, REQUEST_CANCELLED};

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
    #[allow(deprecated)]
    let root_uri = params.root_uri;
    let root = root_uri
        .as_ref()
        .map(|uri| parse_file_uri_str(uri.as_str()))
        .transpose()?;
    let workspace_root = params
        .workspace_folders
        .as_ref()
        .and_then(|folders| folders.first())
        .map(|folder| parse_file_uri_str(folder.uri.as_str()))
        .transpose()?;
    let client_root = root.or(workspace_root);
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
    let mut resolved =
        resolve_source_roots(client_root.as_deref(), initialization_options, cancellation)?;
    if let Some(log) = callbacks.log {
        log(&format!(
            "Source roots resolved: workspace {}, {} source root(s), {} dependency cache(s)",
            resolved
                .workspace_root
                .as_deref()
                .map_or_else(|| "<none>".to_owned(), |path| path.display().to_string()),
            resolved.roots.len(),
            resolved.dependency_caches.len(),
        ));
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
                "Vanilla index: configured cache {}",
                path.display()
            )),
            None => log("Vanilla index: automatic discovery"),
        }
    }
    let auto_vanilla = apply_user_vanilla_configuration(
        &mut resolved,
        auto_vanilla,
        host.snapshot().rules().game_id(),
        &mut warnings,
    );
    host.apply_change(WorkspaceChange::SetWorkspaceRoot(resolved.workspace_root));
    host.apply_change(WorkspaceChange::SetSourceRoots(resolved.roots.clone()));
    if scan_workspace && !resolved.roots.is_empty() {
        if let Some(stage) = callbacks.stage {
            stage("Scanning workspace…");
        }
        host.refresh_source_roots_cancellable_with_progress(cancellation, callbacks.progress)
            .map_err(workspace_scan_error)?;
        if let Some(log) = callbacks.log {
            log(&format!(
                "Workspace scan finished: {} source file(s)",
                host.snapshot().source_files().len()
            ));
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
    // Mission-preview textures: an explicitly configured game directory wins;
    // otherwise a one-time quick discovery via the profile descriptor. This is
    // independent of the Vanilla cache configuration — a configured
    // `vanilla_index_cache` must not disable textures. Texture failures are
    // silent — the preview simply renders without textures.
    let textures =
        resolve_texture_assets(resolved.game_directory.take(), texture_descriptor.as_ref());
    if let Some(log) = callbacks.log {
        match textures.as_ref() {
            Some(textures) => log(&format!(
                "Mission textures ready: {} sprite(s) from the game installation",
                textures.sprite_count()
            )),
            None => log("Mission textures: none (no game installation found)"),
        }
    }
    if cancellation.is_cancelled() {
        return Err(RpcError::new(REQUEST_CANCELLED, "request was cancelled"));
    }
    let watcher_registration =
        watched_files_registration(&resolved.roots, watched_files_capability)?;
    if let Some(log) = callbacks.log {
        log(&format!(
            "Initialization finished in {:.1} ms ({} source file(s) indexed)",
            started.elapsed().as_secs_f64() * 1000.0,
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
