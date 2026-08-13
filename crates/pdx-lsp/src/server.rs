use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io::{self, BufReader, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use lsp_types::{
    CancelParams, DidChangeTextDocumentParams, DidChangeWatchedFilesParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, DidSaveTextDocumentParams,
    FileChangeType, InitializeParams,
};
use pdx_analysis::CancellationToken;
use pdx_engine::{
    AnalysisHost, AnalysisSnapshot, DiskFileChange, DiskFileChangeKind, DocumentId, DocumentSource,
    IndexCache, PreparedDocument, WorkspaceError, WorkspaceScanToken,
};
use pdx_game::DiscoveryToken;
use pdx_rules::{GameProfile, RuleSet};
use pdx_text::LineIndex;
use serde_json::{Value, json};

use crate::initialize::{
    AutoVanillaConfiguration, InitializeOptions, prepare_initialize_candidate,
};
use crate::protocol::{
    LspError, RequestId, RpcError, cancel_initialize_from_notification,
    cancel_request_from_notification, diagnostic_values, diagnostics_notification, document_error,
    is_initialize_control_message, is_snapshot_request, is_snapshot_request_message,
    parse_file_uri_str, request_id_from_lsp, show_info_notification, show_warning_notification,
    typed_params,
};
use crate::requests::SnapshotRequestContext;
use crate::text::{
    apply_text_change, changed_document_len, lsp_range_to_text_range, normalize_workspace_path,
};
use crate::transport::{read_message, write_message};
use crate::uri::uri_to_path;
use crate::vanilla::{run_auto_vanilla_setup, run_index_cache_load};
use crate::workspace::DependencyIndexCache;
use crate::{
    DIAGNOSTIC_DEBOUNCE, INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, JSON_RPC_VERSION,
    METHOD_NOT_FOUND, REQUEST_CANCELLED, SERVER_NOT_INITIALIZED,
};

mod document_events;
mod event_loop;
mod workers;

/// Lifecycle state of the server process.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ServerState {
    /// The process accepts only `initialize`, `exit`, and cancellation notifications.
    Uninitialized,
    /// An initialize worker is materializing the first workspace snapshot.
    Initializing,
    /// The server has completed `initialize` and accepts document events.
    Initialized,
    /// `shutdown` completed; only `exit` is accepted.
    ShuttingDown,
    /// The `exit` notification was received.
    Exited,
}

#[derive(Debug)]
struct PendingDiagnostics {
    uri: String,
    version: i64,
    due: Instant,
}

#[derive(Debug)]
struct PendingParse {
    version: i64,
}

#[derive(Debug)]
struct InFlightParse {
    version: i64,
    cancelled: Arc<AtomicBool>,
}

#[derive(Debug)]
struct ParseResult {
    id: DocumentId,
    version: i64,
    prepared: Option<PreparedDocument>,
}

#[derive(Debug)]
struct InFlightDiagnostics {
    version: i64,
    cancellation: CancellationToken,
}

#[derive(Debug)]
struct DiagnosticsResult {
    id: DocumentId,
    uri: String,
    version: i64,
    values: Option<Value>,
}

#[derive(Debug)]
pub(crate) struct InFlightRequest {
    pub(crate) cancellation: CancellationToken,
}

#[derive(Debug)]
pub(crate) struct InFlightInitialize {
    pub(crate) request_id: RequestId,
    pub(crate) cancellation: WorkspaceScanToken,
}

#[derive(Debug)]
pub(crate) struct InitializeTaskResult {
    request_id: RequestId,
    id: Value,
    result: Result<PreparedInitialize, RpcError>,
}

#[derive(Debug)]
pub(crate) struct PreparedInitialize {
    pub(crate) host: AnalysisHost,
    pub(crate) result: Value,
    pub(crate) warnings: Vec<String>,
    pub(crate) auto_vanilla: Option<AutoVanillaConfiguration>,
    pub(crate) index_cache: Option<PathBuf>,
    /// Dependencies configured with persistent index caches, loaded in the background after
    /// the initialize response is sent.
    pub(crate) dependency_caches: Vec<DependencyIndexCache>,
    pub(crate) watcher_registration: Option<Value>,
    pub(crate) client_work_done_progress: bool,
    pub(crate) client_snippet_support: bool,
}

#[derive(Debug)]
pub(crate) struct IndexSetupResult {
    result: Result<(IndexCache, String), String>,
}

/// One dependency cache background result: the configured cache and its load/rebuild outcome.
pub(crate) type DependencySetupOutcome =
    (DependencyIndexCache, Result<(IndexCache, String), String>);

#[derive(Debug)]
pub(crate) struct DependencySetupResult {
    pub(crate) results: Vec<DependencySetupOutcome>,
}

/// One `$/progress` workDoneProgress payload emitted by a background worker.
pub(crate) struct Progress {
    params: Value,
}

#[derive(Clone, Debug)]
pub(crate) struct IndexSetupCancellation {
    pub(crate) discovery: DiscoveryToken,
    pub(crate) workspace: WorkspaceScanToken,
}

impl IndexSetupCancellation {
    pub(crate) fn new() -> Self {
        Self {
            discovery: DiscoveryToken::new(),
            workspace: WorkspaceScanToken::new(),
        }
    }

    pub(crate) fn cancel(&self) {
        self.discovery.cancel();
        self.workspace.cancel();
    }
}

#[derive(Debug)]
pub(crate) struct SnapshotRequestResult {
    request_id: RequestId,
    id: Value,
    result: Result<Value, RpcError>,
}

#[derive(Debug)]
pub(crate) struct InFlightDiskChanges {
    base_revision: u64,
    cancellation: WorkspaceScanToken,
}

#[derive(Debug)]
pub(crate) struct DiskChangesResult {
    base_revision: u64,
    changes: Vec<DiskFileChange>,
    result: Result<AnalysisHost, WorkspaceError>,
}

enum TransportEvent {
    Input(Result<Option<Value>, LspError>),
    Initialize(Box<InitializeTaskResult>),
    Parse(ParseResult),
    Diagnostics(DiagnosticsResult),
    Request(SnapshotRequestResult),
    VanillaSetup(IndexSetupResult),
    DependencySetup(DependencySetupResult),
    Progress(Progress),
    DiskChanges(DiskChangesResult),
}

/// An LSP server with a single event-loop-owned workspace host.
#[derive(Debug)]
pub struct LspServer {
    state: ServerState,
    options: InitializeOptions,
    pub(crate) host: AnalysisHost,
    cancelled: HashSet<RequestId>,
    diagnostics: BTreeMap<DocumentId, Value>,
    pending_parses: BTreeMap<DocumentId, PendingParse>,
    pending_diagnostics: BTreeMap<DocumentId, PendingDiagnostics>,
    pending_disk_changes: BTreeMap<PathBuf, DiskFileChangeKind>,
    watcher_registration: Option<Value>,
    auto_vanilla: Option<AutoVanillaConfiguration>,
    /// Whether the client advertises `window.workDoneProgress`, so server-initiated background
    /// work can be surfaced as a progress bar instead of only start/end messages.
    client_work_done_progress: bool,
    /// Whether the client advertises snippet support for completion items. When absent, snippet
    /// placeholders are stripped so the inserted text stays valid plain text.
    client_snippet_support: bool,
    clean_exit: bool,
}

impl LspServer {
    /// Creates an identity-only server for protocol tests and generic syntax operation.
    pub fn try_new(options: InitializeOptions) -> Result<Self, LspError> {
        Self::try_new_with_rules(options, RuleSet::empty(), GameProfile::default())
    }

    /// Creates a server from a composition-root-owned immutable rule set.
    pub fn try_new_with_rules(
        options: InitializeOptions,
        rules: RuleSet,
        profile: GameProfile,
    ) -> Result<Self, LspError> {
        if !profile.game_id.is_empty() {
            rules.ensure_game(&profile.game_id)?;
        }
        Ok(Self {
            state: ServerState::Uninitialized,
            options,
            host: AnalysisHost::with_profile(rules, profile),
            cancelled: HashSet::new(),
            diagnostics: BTreeMap::new(),
            pending_parses: BTreeMap::new(),
            pending_diagnostics: BTreeMap::new(),
            pending_disk_changes: BTreeMap::new(),
            watcher_registration: None,
            auto_vanilla: None,
            client_work_done_progress: false,
            client_snippet_support: false,
            clean_exit: false,
        })
    }

    /// Enables one-time user-level Vanilla discovery for a game composition root.
    #[must_use]
    pub fn with_auto_vanilla(mut self, configuration: AutoVanillaConfiguration) -> Self {
        self.auto_vanilla = Some(configuration);
        self
    }

    /// Returns the current lifecycle state.
    #[must_use]
    pub const fn state(&self) -> ServerState {
        self.state
    }

    /// Returns the options captured at construction time.
    #[must_use]
    pub const fn options(&self) -> &InitializeOptions {
        &self.options
    }

    /// Captures the immutable workspace view used by editor-neutral queries.
    #[must_use]
    pub fn snapshot(&self) -> AnalysisSnapshot {
        self.host.snapshot()
    }

    /// Commits diagnostics only if they still match the current open-document version.
    ///
    /// Phase 2 has no syntax analyzer yet, but this freshness gate is the boundary used by later
    /// background diagnostics workers. A stale result is discarded without changing the store.
    pub fn commit_diagnostics(&mut self, uri: &str, version: i64, diagnostics: Value) -> bool {
        let id = DocumentId::new(uri);
        let snapshot = self.host.snapshot();
        let current = snapshot.document(&id);
        if current.is_some_and(|document| {
            document.source() == DocumentSource::Overlay && document.version() == Some(version)
        }) {
            self.diagnostics.insert(id, diagnostics);
            true
        } else {
            false
        }
    }

    /// Returns the last accepted diagnostics batch for a document.
    #[must_use]
    pub fn diagnostics(&self, uri: &str) -> Option<&Value> {
        self.diagnostics.get(&DocumentId::new(uri))
    }

    /// Runs the framed stdio transport used by `pdx-ls`.
    pub fn run_stdio(options: InitializeOptions) -> Result<(), LspError> {
        let stdin = io::stdin();
        let stdout = io::stdout();
        let mut server = Self::try_new(options)?;
        server.run_transport(stdin, stdout.lock())
    }

    /// Runs stdio with explicit game-profile interpretation and identity validation.
    pub fn run_stdio_with_profile(
        options: InitializeOptions,
        rules: RuleSet,
        profile: GameProfile,
    ) -> Result<(), LspError> {
        let stdin = io::stdin();
        let stdout = io::stdout();
        let mut server = Self::try_new_with_rules(options, rules, profile)?;
        server.run_transport(stdin, stdout.lock())
    }

    /// Runs stdio with a selected game profile and one-time user-level Vanilla discovery.
    pub fn run_stdio_with_profile_and_auto_vanilla(
        options: InitializeOptions,
        rules: RuleSet,
        profile: GameProfile,
        auto_vanilla: AutoVanillaConfiguration,
    ) -> Result<(), LspError> {
        let stdin = io::stdin();
        let stdout = io::stdout();
        let mut server =
            Self::try_new_with_rules(options, rules, profile)?.with_auto_vanilla(auto_vanilla);
        server.run_transport(stdin, stdout.lock())
    }
}
