use lsp_types::{
    CompletionOptions, HoverProviderCapability, InitializeParams, InitializeResult, OneOf,
    RenameOptions, ServerCapabilities, ServerInfo, TextDocumentSyncCapability,
    TextDocumentSyncKind, TextDocumentSyncOptions, WorkDoneProgressOptions,
};
use pdx_engine::{AnalysisHost, WorkspaceChange, WorkspaceScanToken};
use pdx_game::{GameInstallDescriptor, UserPaths};

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
pub(crate) fn prepare_initialize_candidate(
    mut host: AnalysisHost,
    params: InitializeParams,
    scan_workspace: bool,
    auto_vanilla: Option<&AutoVanillaConfiguration>,
    cancellation: &WorkspaceScanToken,
) -> Result<PreparedInitialize, RpcError> {
    if cancellation.is_cancelled() {
        return Err(RpcError::new(REQUEST_CANCELLED, "request was cancelled"));
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
    let mut resolved =
        resolve_source_roots(client_root.as_deref(), initialization_options, cancellation)?;
    let mut warnings = Vec::new();
    let auto_vanilla = apply_user_vanilla_configuration(
        &mut resolved,
        auto_vanilla,
        host.snapshot().rules().game_id(),
        &mut warnings,
    );
    host.apply_change(WorkspaceChange::SetWorkspaceRoot(resolved.workspace_root));
    host.apply_change(WorkspaceChange::SetSourceRoots(resolved.roots.clone()));
    if scan_workspace && !resolved.roots.is_empty() {
        host.refresh_source_roots_cancellable(cancellation)
            .map_err(workspace_scan_error)?;
    }
    let vanilla_cache = match resolved.vanilla_cache.take() {
        None => None,
        Some(path) if !scan_workspace => {
            warnings.push(format!(
                "Vanilla cache {} was not loaded because no validated rules artifact is active",
                path.display()
            ));
            None
        }
        Some(path) if !path.is_file() => {
            warnings.push(format!(
                "Vanilla cache {} does not exist; continuing without Vanilla symbols",
                path.display()
            ));
            None
        }
        Some(path) => Some(path),
    };
    if cancellation.is_cancelled() {
        return Err(RpcError::new(REQUEST_CANCELLED, "request was cancelled"));
    }
    let watcher_registration =
        watched_files_registration(&resolved.roots, watched_files_capability)?;
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
        vanilla_cache,
        watcher_registration,
        client_work_done_progress,
        client_snippet_support,
    })
}
