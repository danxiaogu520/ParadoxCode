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
    ExecuteCommandParams, FileChangeType, InitializeParams, MessageType,
};
use pdx_analysis::{CancellationToken, Severity, source_file_diagnostics_with_cancellation};
use pdx_engine::{
    AnalysisHost, AnalysisSnapshot, DiskFileChange, DiskFileChangeKind, DocumentId, DocumentSource,
    IndexCache, PreparedDocument, SourceRootKind, WorkspaceError, WorkspaceScanFilters,
    WorkspaceScanToken,
};
use pdx_game::DiscoveryToken;
use pdx_rules::{GameProfile, RuleSet};
use pdx_text::LineIndex;
use serde_json::{Value, json};

use crate::dependency::DependencySetupOutcome;
use crate::initialize::{
    AutoVanillaConfiguration, InitializeCallbacks, InitializeOptions, prepare_initialize_candidate,
};
use crate::protocol::{
    LspError, RequestId, RpcError, cancel_initialize_from_notification,
    cancel_request_from_notification, diagnostic_values_with_ignored, diagnostics_notification,
    document_error, is_execute_command_message, is_initialize_control_message, is_snapshot_request,
    is_snapshot_request_message, log_message_notification, parse_file_uri_str, request_id_from_lsp,
    show_info_notification, show_warning_notification, typed_params,
};
use crate::requests::SnapshotRequestContext;
use crate::text::{
    apply_text_change, changed_document_len, lsp_range_to_text_range, normalize_workspace_path,
};
use crate::transport::{read_message, write_message};
use crate::uri::uri_to_path;
use crate::vanilla::{IndexCacheLoadRequest, run_auto_vanilla_setup, run_index_cache_load};
use crate::workspace::DependencyIndexCache;
use crate::{
    DIAGNOSTIC_DEBOUNCE, INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, JSON_RPC_VERSION,
    METHOD_NOT_FOUND, REQUEST_CANCELLED, SERVER_NOT_INITIALIZED,
};

/// Trailing window used to coalesce editor watcher floods into one index update.
///
/// File watching backends commonly report a save as several create/modify events. Waiting for a
/// short quiet period keeps those events from starting one parse/index pass each while still
/// making ordinary changes visible promptly.
pub(crate) const WATCHED_FILE_DEBOUNCE: Duration = Duration::from_millis(500);
/// Number of distinct watched paths after which a targeted update is less useful than one bounded
/// full scan. This mirrors the bulk guard in the reference server and prevents a generated-file
/// storm from filling the event loop with hundreds of workers.
pub(crate) const WATCHED_BULK_CAP: usize = 200;

mod document_events;
mod event_loop;
mod workers;

/// Monotonic nonce for work-done-progress tokens and their create-request ids.
fn progress_nonce() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos())
}

/// Server-initiated work-done-progress create request; the client's response is ignored.
fn work_done_progress_create(token: &str) -> Value {
    json!({
        "jsonrpc": JSON_RPC_VERSION,
        "id": format!("pdx-progress-{token}"),
        "method": "window/workDoneProgress/create",
        "params": {"token": token},
    })
}

fn work_done_progress_begin(token: &str, message: &str) -> Value {
    json!({
        "jsonrpc": JSON_RPC_VERSION,
        "method": "$/progress",
        "params": {
            "token": token,
            "value": {
                "kind": "begin",
                "title": "ParadoxCode",
                "cancellable": false,
                "message": message,
                "percentage": 0,
            },
        },
    })
}

fn work_done_progress_end(token: &str, message: &str) -> Value {
    json!({
        "jsonrpc": JSON_RPC_VERSION,
        "method": "$/progress",
        "params": {
            "token": token,
            "value": {"kind": "end", "message": message},
        },
    })
}

/// Announces that the initial workspace/index setup has completed.
///
/// `LanguageClient` reaching `Running` only means that the LSP handshake finished.  Vanilla and
/// dependency indexes may still be loading in the background, so editors need a separate,
/// protocol-level signal before presenting the server as fully ready.
fn ready_notification(revision: u64, source_files: usize) -> Value {
    json!({
        "jsonrpc": JSON_RPC_VERSION,
        "method": "pdx/ready",
        "params": {
            "state": "ready",
            "revision": revision,
            "sourceFiles": source_files,
        },
    })
}

/// Builds the worker progress callback that forwards engine progress as `$/progress` reports.
///
/// `discovering` is shown while the work unit total is still unknown; `indexing` carries the
/// running `(done/total)` counter once the engine knows the full scope.
fn progress_sender(
    sender: mpsc::Sender<TransportEvent>,
    token: String,
    discovering: &'static str,
    indexing: &'static str,
) -> impl Fn(usize, usize) {
    move |done, total| {
        let message = if total == 0 {
            format!("{discovering}…")
        } else {
            format!("{indexing} ({done}/{total})…")
        };
        let mut value = json!({"kind": "report", "message": message});
        if let Some(percent) = done
            .checked_mul(100)
            .and_then(|percent| percent.checked_div(total))
        {
            value["percentage"] = json!(u32::try_from(percent).unwrap_or(100));
        }
        let _ = sender.send(TransportEvent::Progress(Progress {
            params: json!({"token": token, "value": value}),
        }));
    }
}

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
    /// Mission-preview texture store (game sprites), when a game installation
    /// was configured or discovered.
    pub(crate) textures: Option<Arc<pdx_game::eu4::mission::TextureAssets>>,
    /// Dependencies configured with persistent index caches, loaded in the background after
    /// the initialize response is sent.
    pub(crate) dependency_caches: Vec<DependencyIndexCache>,
    pub(crate) watcher_registration: Option<Value>,
    pub(crate) client_work_done_progress: bool,
    pub(crate) client_snippet_support: bool,
    /// Optional quiet workspace re-scan cadence selected from initialization/project config.
    pub(crate) background_reindex_interval_minutes: u64,
    /// User-idle window required before a quiet workspace re-scan.
    pub(crate) background_reindex_idle_seconds: u64,
    /// Canonical diagnostic categories omitted from LSP output.
    pub(crate) ignored_diagnostic_codes: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct IndexSetupResult {
    result: Result<(IndexCache, String), String>,
}

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
pub(crate) struct InFlightBackgroundReindex {
    pub(crate) base_revision: u64,
    pub(crate) cancellation: WorkspaceScanToken,
}

#[derive(Debug)]
pub(crate) struct InFlightReindexCommand {
    pub(crate) request_id: RequestId,
    pub(crate) base_revision: u64,
    pub(crate) command: WorkspaceCommand,
    pub(crate) cancellation: WorkspaceScanToken,
}

/// Explicit workspace command executed by the single serialized scan worker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WorkspaceCommand {
    Reindex,
    Validate,
}

/// Aggregate diagnostics returned by `validateWorkspace`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct WorkspaceValidationSummary {
    pub(crate) total_files: usize,
    pub(crate) validated_files: usize,
    pub(crate) files_with_errors: usize,
    pub(crate) total_errors: usize,
    pub(crate) total_warnings: usize,
    pub(crate) total_infos: usize,
    pub(crate) total_hints: usize,
}

#[derive(Debug)]
pub(crate) struct ReindexCommandResult {
    pub(crate) request_id: RequestId,
    pub(crate) id: Value,
    pub(crate) base_revision: u64,
    pub(crate) command: WorkspaceCommand,
    pub(crate) result: Result<(AnalysisHost, Option<WorkspaceValidationSummary>), WorkspaceError>,
}

#[derive(Debug)]
pub(crate) struct DiskChangesResult {
    base_revision: u64,
    changes: Vec<DiskFileChange>,
    result: Result<AnalysisHost, WorkspaceError>,
}

#[derive(Debug)]
pub(crate) struct BackgroundReindexResult {
    pub(crate) base_revision: u64,
    pub(crate) result: Result<AnalysisHost, WorkspaceError>,
}

enum TransportEvent {
    Input(Result<Option<Value>, LspError>),
    Initialize(Box<InitializeTaskResult>),
    Parse(ParseResult),
    Diagnostics(DiagnosticsResult),
    Request(SnapshotRequestResult),
    VanillaSetup(IndexSetupResult),
    DependencySetup(DependencySetupResult),
    BackgroundReindex(BackgroundReindexResult),
    ReindexCommand(ReindexCommandResult),
    /// A server-side `window/logMessage` notification produced by a worker.
    Log(Value),
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
    pending_disk_changes_due: Option<Instant>,
    pending_disk_changes_rescan: bool,
    watcher_registration: Option<Value>,
    auto_vanilla: Option<AutoVanillaConfiguration>,
    /// Mission-preview texture store shared with snapshot requests.
    textures: Option<Arc<pdx_game::eu4::mission::TextureAssets>>,
    /// Whether the client advertises `window.workDoneProgress`, so server-initiated background
    /// work can be surfaced as a progress bar instead of only start/end messages.
    client_work_done_progress: bool,
    /// Whether the client advertises snippet support for completion items. When absent, snippet
    /// placeholders are stripped so the inserted text stays valid plain text.
    client_snippet_support: bool,
    /// Opt-in quiet source-root re-scan cadence. Zero disables the loop.
    pub(crate) background_reindex_interval_minutes: u64,
    /// Idle window required before a quiet source-root re-scan may start.
    pub(crate) background_reindex_idle_seconds: u64,
    /// Monotonic timestamp of the most recent editor activity handled by the event loop.
    pub(crate) last_activity: Instant,
    /// Next eligible background re-scan deadline. It is armed once the initial workspace/index
    /// setup is ready and reset after each pass or live cadence change.
    pub(crate) background_reindex_due: Option<Instant>,
    /// Diagnostic categories hidden from published diagnostics and diagnostic query responses.
    pub(crate) ignored_diagnostic_codes: Arc<HashSet<String>>,
    /// Process-start messages collected before an LSP client can receive
    /// `window/logMessage`. They are replayed at the beginning of the first
    /// initialize worker so the editor's log has no unexplained pre-initialize gap.
    startup_log: Vec<String>,
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
            pending_disk_changes_due: None,
            pending_disk_changes_rescan: false,
            watcher_registration: None,
            auto_vanilla: None,
            textures: None,
            client_work_done_progress: false,
            client_snippet_support: false,
            background_reindex_interval_minutes: 0,
            background_reindex_idle_seconds: 15,
            last_activity: Instant::now(),
            background_reindex_due: None,
            ignored_diagnostic_codes: Arc::new(HashSet::new()),
            startup_log: Vec::new(),
            clean_exit: false,
        })
    }

    /// Adds process-start diagnostics that will be replayed through the LSP log channel when the
    /// first client initialize request arrives. Stdio startup diagnostics are still emitted by
    /// the composition root, because no LSP client is available before `initialize`.
    #[must_use]
    pub fn with_startup_log(mut self, messages: Vec<String>) -> Self {
        self.startup_log = messages;
        self
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
        Self::run_stdio_with_profile_and_startup_log(options, rules, profile, Vec::new())
    }

    /// Runs stdio with explicit game-profile interpretation and process-start diagnostics.
    pub fn run_stdio_with_profile_and_startup_log(
        options: InitializeOptions,
        rules: RuleSet,
        profile: GameProfile,
        startup_log: Vec<String>,
    ) -> Result<(), LspError> {
        let stdin = io::stdin();
        let stdout = io::stdout();
        let mut server =
            Self::try_new_with_rules(options, rules, profile)?.with_startup_log(startup_log);
        server.run_transport(stdin, stdout.lock())
    }

    /// Runs stdio with a selected game profile and one-time user-level Vanilla discovery.
    pub fn run_stdio_with_profile_and_auto_vanilla(
        options: InitializeOptions,
        rules: RuleSet,
        profile: GameProfile,
        auto_vanilla: AutoVanillaConfiguration,
    ) -> Result<(), LspError> {
        Self::run_stdio_with_profile_and_auto_vanilla_with_startup_log(
            options,
            rules,
            profile,
            auto_vanilla,
            Vec::new(),
        )
    }

    /// Runs stdio with a selected game profile, automatic Vanilla discovery, and process-start
    /// diagnostics that are replayed through `window/logMessage` after initialize.
    pub fn run_stdio_with_profile_and_auto_vanilla_with_startup_log(
        options: InitializeOptions,
        rules: RuleSet,
        profile: GameProfile,
        auto_vanilla: AutoVanillaConfiguration,
        startup_log: Vec<String>,
    ) -> Result<(), LspError> {
        let stdin = io::stdin();
        let stdout = io::stdout();
        let mut server =
            Self::try_new_with_rules(options, rules, profile)?.with_auto_vanilla(auto_vanilla);
        server.startup_log = startup_log;
        server.run_transport(stdin, stdout.lock())
    }
}

impl LspServer {
    /// Returns whether a watcher update or its bulk-rescan marker is waiting to be processed.
    pub(crate) fn has_pending_disk_changes(&self) -> bool {
        self.pending_disk_changes_rescan || !self.pending_disk_changes.is_empty()
    }

    /// Queues one watcher event and arms/resets the trailing coalescing window.
    pub(crate) fn queue_watched_disk_change(&mut self, path: PathBuf, kind: DiskFileChangeKind) {
        if !self.pending_disk_changes_rescan {
            self.pending_disk_changes.insert(path, kind);
            if self.pending_disk_changes.len() > WATCHED_BULK_CAP {
                self.pending_disk_changes.clear();
                self.pending_disk_changes_rescan = true;
            }
        }
        self.pending_disk_changes_due = Instant::now().checked_add(WATCHED_FILE_DEBOUNCE);
    }

    /// Returns the event-loop wait needed before a coalesced watcher batch may start.
    pub(crate) fn pending_disk_change_wait(
        &self,
        in_flight: Option<&InFlightDiskChanges>,
    ) -> Option<Duration> {
        if in_flight.is_some() || !self.has_pending_disk_changes() {
            return None;
        }
        Some(self.pending_disk_changes_due.map_or(Duration::ZERO, |due| {
            due.saturating_duration_since(Instant::now())
        }))
    }

    /// Records editor activity used by the idle gate for quiet background re-scans.
    pub(crate) fn mark_activity(&mut self) {
        self.last_activity = Instant::now();
    }

    /// Arms the next quiet re-scan deadline after initial setup or a completed pass.
    pub(crate) fn arm_background_reindex(&mut self) {
        self.background_reindex_due = if self.background_reindex_interval_minutes == 0 {
            None
        } else {
            let seconds = self.background_reindex_interval_minutes.saturating_mul(60);
            Instant::now().checked_add(Duration::from_secs(seconds))
        };
    }

    /// Returns the configured cadence as a duration. Configuration validation bounds the value,
    /// but saturating arithmetic keeps the worker safe if a caller constructs a server directly.
    pub(crate) fn background_reindex_interval(&self) -> Option<Duration> {
        (self.background_reindex_interval_minutes != 0).then(|| {
            Duration::from_secs(self.background_reindex_interval_minutes.saturating_mul(60))
        })
    }
}
