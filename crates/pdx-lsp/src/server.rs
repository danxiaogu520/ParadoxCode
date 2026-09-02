use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::io::{self, BufReader, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use lsp_types::{
    CancelParams, DidChangeTextDocumentParams, DidChangeWatchedFilesParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, DidSaveTextDocumentParams,
    ExecuteCommandParams, FileChangeType, InitializeParams, MessageType,
};
use pdx_analysis::{
    CancellationToken, Severity, diagnostics_with_cancellation,
    source_file_diagnostics_with_cancellation,
};
use pdx_engine::{
    AnalysisHost, AnalysisSnapshot, DiskFileChange, DiskFileChangeKind, DocumentId, DocumentSource,
    IndexCache, PreparedDocument, SourceRootKind, WorkspaceError, WorkspaceScanFilters,
    WorkspaceScanReport, WorkspaceScanToken,
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
    cancel_request_from_notification, diagnostic_values_for_text_with_ignored_and_overrides,
    diagnostic_values_with_ignored_and_overrides, diagnostics_notification, document_error,
    filter_diagnostics_with_ignored_and_overrides, is_execute_command_message,
    is_exit_notification, is_initialize_control_message, is_snapshot_request,
    is_snapshot_request_message, log_message_notification, parse_file_uri_str, request_id_from_lsp,
    show_info_notification, show_warning_notification, typed_params,
};
use crate::requests::SnapshotRequestContext;
use crate::text::{
    apply_text_change, changed_document_len, lsp_range_to_text_range, normalize_workspace_path,
};
use crate::transport::{read_message, write_message};
use crate::uri::uri_to_path;
use crate::vanilla::{IndexCacheLoadRequest, run_index_cache_load};
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
/// `LanguageClient` reaching `Running` only means that the LSP handshake finished — the
/// initialize response returns before the workspace scan, and Vanilla/dependency indexes may
/// still be loading in the background, so editors need a separate, protocol-level signal before
/// presenting the server as fully ready.
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

/// One immutable semantic-token result retained for LSP `full/delta` requests.
#[derive(Clone, Debug)]
pub(crate) struct SemanticTokensCacheEntry {
    pub(crate) revision: u64,
    pub(crate) result_id: String,
    pub(crate) data: Vec<lsp_types::SemanticToken>,
}

/// Bounded protocol cache shared by snapshot workers and owned by the event-loop server.
///
/// Entries are keyed by URI but tagged with the immutable workspace revision that produced them.
/// A worker finishing for an older snapshot can therefore never satisfy a delta request against a
/// newer host revision. The cache is intentionally small and is cleared wholesale at its bound;
/// semantic tokens are cheap to recompute and stale cache state must not grow with a workspace.
#[derive(Debug)]
pub(crate) struct SemanticTokensCache {
    entries: std::sync::Mutex<BTreeMap<String, SemanticTokensCacheEntry>>,
    next_result_id: AtomicU64,
}

impl SemanticTokensCache {
    pub(crate) const MAX_ENTRIES: usize = 1_024;

    pub(crate) fn new() -> Self {
        Self {
            entries: std::sync::Mutex::new(BTreeMap::new()),
            next_result_id: AtomicU64::new(0),
        }
    }

    pub(crate) fn next_result_id(&self) -> String {
        self.next_result_id
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1)
            .to_string()
    }

    pub(crate) fn get(&self, uri: &str, revision: u64) -> Option<SemanticTokensCacheEntry> {
        self.entries
            .lock()
            .expect("semantic token cache lock poisoned")
            .get(uri)
            .filter(|entry| entry.revision == revision)
            .cloned()
    }

    pub(crate) fn insert(
        &self,
        uri: String,
        revision: u64,
        result_id: String,
        data: Vec<lsp_types::SemanticToken>,
    ) {
        let mut entries = self
            .entries
            .lock()
            .expect("semantic token cache lock poisoned");
        if entries
            .get(&uri)
            .is_some_and(|entry| entry.revision > revision)
        {
            return;
        }
        if entries.len() >= Self::MAX_ENTRIES && !entries.contains_key(&uri) {
            entries.clear();
        }
        entries.insert(
            uri,
            SemanticTokensCacheEntry {
                revision,
                result_id,
                data,
            },
        );
    }

    pub(crate) fn remove(&self, uri: &str) {
        self.entries
            .lock()
            .expect("semantic token cache lock poisoned")
            .remove(uri);
    }

    pub(crate) fn clear(&self) {
        self.entries
            .lock()
            .expect("semantic token cache lock poisoned")
            .clear();
    }
}

impl Default for SemanticTokensCache {
    fn default() -> Self {
        Self::new()
    }
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
    /// Mission-preview textures, resolved lazily from the captured discovery
    /// inputs on the first preview request.
    pub(crate) textures: Arc<crate::initialize::TextureStore>,
    /// Dependencies configured with persistent index caches, loaded in the background after
    /// the initialize response is sent.
    pub(crate) dependency_caches: Vec<DependencyIndexCache>,
    /// Whether the initial workspace scan still needs to run as a background
    /// worker after the initialize response is sent.
    pub(crate) scan_pending: bool,
    pub(crate) watcher_registration: Option<Value>,
    pub(crate) client_work_done_progress: bool,
    pub(crate) client_snippet_support: bool,
    /// Optional quiet workspace re-scan cadence selected from editor initialization options.
    pub(crate) background_reindex_interval_minutes: u64,
    /// User-idle window required before a quiet workspace re-scan.
    pub(crate) background_reindex_idle_seconds: u64,
    /// Canonical diagnostic categories omitted from LSP output.
    pub(crate) ignored_diagnostic_codes: Vec<String>,
    /// Per-category severity remapping applied before publication and workspace aggregation.
    pub(crate) diagnostic_severity_overrides: BTreeMap<String, Option<Severity>>,
    /// Whether automatic and explicit workspace refreshes publish diagnostics for closed Current
    /// Mod files.
    pub(crate) workspace_wide_diagnostics: bool,
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
pub(crate) struct InFlightWorkspaceDiagnostics {
    pub(crate) base_revision: u64,
    pub(crate) cancellation: WorkspaceScanToken,
}

#[derive(Debug)]
/// In-flight Vanilla cache worker slot. `is_load` distinguishes the bounded
/// cache-load flavor (snapshot requests wait for complete answers) from an
/// unbounded background rebuild (requests are served from partial state).
pub(crate) struct InFlightIndexSlot {
    pub(crate) cancellation: IndexSetupCancellation,
    pub(crate) is_load: bool,
}

/// In-flight dependency cache worker slot. Dependency installs always load or
/// build bounded caches before installing in place.
pub(crate) struct InFlightDependencySlot {
    pub(crate) cancellation: WorkspaceScanToken,
}

/// Cache workers started by one `spawn_background_cache_workers` call.
pub(crate) struct CacheWorkersSpawned {
    pub(crate) index: Option<InFlightIndexSlot>,
    pub(crate) index_progress_token: Option<String>,
    pub(crate) dependency: Option<InFlightDependencySlot>,
    pub(crate) dependency_progress_token: Option<String>,
}

/// Inputs for background cache workers, stashed while the initial workspace
/// scan runs so cache installs never race the scan's host commit.
#[derive(Debug, Default)]
pub(crate) struct PendingCacheSetup {
    pub(crate) index_cache: Option<PathBuf>,
    pub(crate) dependency_caches: Vec<DependencyIndexCache>,
    /// User-level automatic Vanilla configuration captured by the initialize
    /// handshake, replayed when the deferred workers finally start.
    pub(crate) auto_vanilla: Option<AutoVanillaConfiguration>,
}

/// In-flight initial background workspace scan.
pub(crate) struct InFlightScan {
    pub(crate) base_revision: u64,
    pub(crate) cancellation: WorkspaceScanToken,
    /// Progress token created for this scan, when the client supports
    /// `window.workDoneProgress`; the event loop ends it on completion.
    pub(crate) progress_token: Option<String>,
}

/// Test-only seam invoked by a background scan worker once its scan has fully
/// completed but before the completion is reported to the event loop. In-crate
/// shutdown-race regression tests use it to pin exact interleavings — events
/// processed while the scan is verifiably still in flight — deterministically
/// instead of racing wall-clock time. Production servers never configure a
/// gate, and a configured gate must be guaranteed to release.
#[derive(Clone)]
pub(crate) struct ScanGate(Arc<dyn Fn() + Send + Sync>);

impl ScanGate {
    /// Parks the calling scan worker until the gate releases it.
    pub(crate) fn hold(&self) {
        (self.0)();
    }
}

impl std::fmt::Debug for ScanGate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ScanGate")
    }
}

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
pub(crate) struct WorkspaceDiagnosticPublication {
    pub(crate) uri: String,
    pub(crate) values: Value,
}

#[derive(Debug)]
pub(crate) struct WorkspaceValidationResult {
    pub(crate) summary: WorkspaceValidationSummary,
    /// Notifications for the bounded prefix of closed Current Mod files. Files beyond the
    /// publication budget remain represented by `current_uris` so previously published entries
    /// can be retained rather than flooding the client with clears.
    pub(crate) publications: Vec<WorkspaceDiagnosticPublication>,
    pub(crate) current_uris: Vec<String>,
    /// Whether this validation walked the whole Current Mod. A full walk
    /// answers the queued post-ready pass; an incremental watched-file batch
    /// covers only the files it touched and must leave that pass queued.
    pub(crate) full_workspace: bool,
}

#[derive(Debug)]
pub(crate) struct ReindexCommandResult {
    pub(crate) request_id: RequestId,
    pub(crate) id: Value,
    pub(crate) base_revision: u64,
    pub(crate) command: WorkspaceCommand,
    pub(crate) result: Result<(AnalysisHost, Option<WorkspaceValidationResult>), WorkspaceError>,
}

#[derive(Debug)]
pub(crate) struct DiskChangesResult {
    base_revision: u64,
    changes: Vec<DiskFileChange>,
    result: Result<(AnalysisHost, Option<WorkspaceValidationResult>), WorkspaceError>,
}

#[derive(Debug)]
pub(crate) struct BackgroundReindexResult {
    pub(crate) base_revision: u64,
    pub(crate) result: Result<(AnalysisHost, Option<WorkspaceValidationResult>), WorkspaceError>,
}

#[derive(Debug)]
pub(crate) struct WorkspaceDiagnosticsResult {
    pub(crate) base_revision: u64,
    pub(crate) result: Result<WorkspaceValidationResult, WorkspaceError>,
}

/// Background initial workspace scan completion. The worker refreshes a host
/// clone; the event loop commits it only when the live revision still matches.
#[derive(Debug)]
pub(crate) struct ScanSetupResult {
    pub(crate) base_revision: u64,
    pub(crate) result: Result<(AnalysisHost, WorkspaceScanReport), WorkspaceError>,
}

enum TransportEvent {
    Input(Result<Option<Value>, LspError>),
    Initialize(Box<InitializeTaskResult>),
    Parse(ParseResult),
    Diagnostics(DiagnosticsResult),
    Request(SnapshotRequestResult),
    VanillaSetup(IndexSetupResult),
    DependencySetup(DependencySetupResult),
    ScanSetup(ScanSetupResult),
    BackgroundReindex(BackgroundReindexResult),
    ReindexCommand(ReindexCommandResult),
    /// A server-side `window/logMessage` notification produced by a worker.
    Log(Value),
    Progress(Progress),
    DiskChanges(DiskChangesResult),
    WorkspaceDiagnostics(WorkspaceDiagnosticsResult),
    /// Loop heartbeat synthesized when `recv_timeout` expires. Carries no
    /// payload: its only job is to route control flow through the end-of-loop
    /// shutdown-drain/reader-arm block, which `continue` on timeout would
    /// otherwise starve when no worker or timer will ever fire again.
    Tick,
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
    /// Mission-preview textures, resolved lazily on first preview use.
    textures: Arc<crate::initialize::TextureStore>,
    /// Whether the client advertises `window.workDoneProgress`, so server-initiated background
    /// work can be surfaced as a progress bar instead of only start/end messages.
    client_work_done_progress: bool,
    /// Whether the client advertises snippet support for completion items. When absent, snippet
    /// placeholders are stripped so the inserted text stays valid plain text.
    client_snippet_support: bool,
    /// Bounded semantic-token results used to answer `full/delta` requests without retaining
    /// unbounded per-document protocol state.
    pub(crate) semantic_tokens_cache: Arc<SemanticTokensCache>,
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
    /// Per-category severity remapping applied to all analysis diagnostics at the protocol edge.
    pub(crate) diagnostic_severity_overrides: Arc<BTreeMap<String, Option<Severity>>>,
    /// Whether automatic and explicit workspace refreshes also publish diagnostics for closed
    /// Current Mod files. The bounded publication path is enabled by default and can be disabled
    /// by clients that only want diagnostics for open documents.
    pub(crate) workspace_wide_diagnostics: bool,
    /// Whether an automatic closed-file validation pass is waiting for a quiet worker slot.
    pub(crate) workspace_diagnostics_pending: bool,
    /// Whether the initial background workspace scan still needs to start. Set
    /// during the initialize handshake when live roots exist; the scan worker
    /// commits while the live revision is unchanged and retries otherwise.
    pub(crate) scan_pending: bool,
    /// Cache worker inputs deferred until the initial background scan commits.
    pub(crate) pending_cache_setup: PendingCacheSetup,
    /// Consecutive background-scan attempts that lost the revision race. After
    /// a bounded number of retries the scan is abandoned with an explicit
    /// warning instead of looping forever under continuous edits.
    pub(crate) scan_retries: u8,
    /// URIs for which the last workspace pass published closed-file diagnostics. This lets a
    /// subsequent pass clear deleted or ignored files without retaining diagnostic payloads.
    pub(crate) workspace_diagnostic_uris: BTreeSet<String>,
    /// Notifications queued by a live setting change; drained by the transport loop so the
    /// configuration handler remains a pure state transition.
    pub(crate) workspace_diagnostic_clear_queue: Vec<String>,
    /// Test-only scan-completion gate; see [`ScanGate`]. `None` in production.
    scan_gate: Option<ScanGate>,
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
            textures: Arc::new(crate::initialize::TextureStore::new(None, None)),
            client_work_done_progress: false,
            client_snippet_support: false,
            semantic_tokens_cache: Arc::new(SemanticTokensCache::new()),
            background_reindex_interval_minutes: 0,
            background_reindex_idle_seconds: 15,
            last_activity: Instant::now(),
            background_reindex_due: None,
            ignored_diagnostic_codes: Arc::new(HashSet::new()),
            diagnostic_severity_overrides: Arc::new(BTreeMap::new()),
            workspace_wide_diagnostics: true,
            workspace_diagnostics_pending: false,
            scan_pending: false,
            pending_cache_setup: PendingCacheSetup::default(),
            scan_retries: 0,
            workspace_diagnostic_uris: BTreeSet::new(),
            workspace_diagnostic_clear_queue: Vec::new(),
            scan_gate: None,
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

    /// Attaches a gate each background scan worker invokes just before it
    /// reports completion. In-crate test seam only: production servers never
    /// set it, and the gate blocks the worker until it returns, so the
    /// closure must be guaranteed to release.
    #[cfg(test)]
    pub(crate) fn with_scan_gate(mut self, gate: Arc<dyn Fn() + Send + Sync>) -> Self {
        self.scan_gate = Some(ScanGate(gate));
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
            // The overlay re-run after a scan commit legitimately recomputes the
            // same version against the completed index; republishing a byte-identical
            // batch would only churn the client, so only changed results go out.
            if self
                .diagnostics
                .get(&id)
                .is_some_and(|published| *published == diagnostics)
            {
                return false;
            }
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

    /// Publishes the bounded closed-file diagnostic batch returned by a workspace worker and
    /// clears entries that disappeared from the refreshed Current Mod. Files beyond the worker's
    /// publication budget remain in `workspace_diagnostic_uris`, so a large workspace does not
    /// generate a second storm of empty notifications just because it was truncated.
    pub(crate) fn publish_workspace_diagnostics<W: Write>(
        &mut self,
        output: &mut W,
        result: &WorkspaceValidationResult,
    ) -> Result<(), LspError> {
        if result.full_workspace {
            // A whole-workspace validation answers the queued post-ready pass. An
            // incremental watched-file batch covers only the files it touched;
            // clearing the flag there would drop the initial closed-file
            // publication whenever a watched change lands between `ready` and
            // the pass spawn.
            self.workspace_diagnostics_pending = false;
        }
        let current = result.current_uris.iter().cloned().collect::<BTreeSet<_>>();
        let stale = self
            .workspace_diagnostic_uris
            .difference(&current)
            .cloned()
            .collect::<Vec<_>>();
        for uri in stale
            .into_iter()
            .take(crate::MAX_WORKSPACE_DIAGNOSTIC_CLEARS)
        {
            write_message(output, &diagnostics_notification(&uri, json!([])))?;
            self.workspace_diagnostic_uris.remove(&uri);
        }
        for publication in &result.publications {
            write_message(
                output,
                &diagnostics_notification(&publication.uri, publication.values.clone()),
            )?;
            self.workspace_diagnostic_uris
                .insert(publication.uri.clone());
        }
        Ok(())
    }

    /// Queues an automatic closed-file validation pass. The event loop starts it once no
    /// foreground scan is active; explicit `validateWorkspace` remains synchronous with its own
    /// response and does not use this flag. The pass may also be queued during shutdown: the
    /// shutdown drain deliberately waits for an in-flight scan to publish, so suppressing it
    /// after `shutdown` would drop that publication entirely.
    pub(crate) fn request_workspace_diagnostics(&mut self) {
        if self.workspace_wide_diagnostics
            && matches!(
                self.state,
                ServerState::Initialized | ServerState::ShuttingDown
            )
        {
            self.workspace_diagnostics_pending = true;
        }
    }

    /// Removes one closed-file publication when that path becomes an open overlay. The regular
    /// document diagnostic worker will publish the overlay result independently.
    pub(crate) fn clear_workspace_diagnostic_uri(&mut self, uri: &str) {
        if self.workspace_diagnostic_uris.remove(uri) {
            self.workspace_diagnostic_clear_queue.push(uri.to_owned());
        }
    }

    /// Drops the semantic-token result for one document after an overlay lifecycle change.
    pub(crate) fn invalidate_semantic_tokens(&self, uri: &str) {
        self.semantic_tokens_cache.remove(uri);
    }

    /// Drops all semantic-token results after a workspace/index revision change. Revision tags
    /// already protect against stale workers, while this eager clear bounds retained payloads
    /// during large refreshes.
    pub(crate) fn invalidate_all_semantic_tokens(&self) {
        self.semantic_tokens_cache.clear();
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

    /// Requests a bounded full source-root refresh for a live configuration change. Ignore
    /// filters alter the discovered file set, so replaying only the next watcher event would
    /// leave removed entries active in the index.
    pub(crate) fn request_full_disk_rescan(&mut self) {
        self.pending_disk_changes.clear();
        self.pending_disk_changes_rescan = true;
        self.pending_disk_changes_due = Some(Instant::now());
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

#[cfg(test)]
mod semantic_tokens_cache_tests {
    use super::SemanticTokensCache;

    #[test]
    fn cache_rejects_results_from_older_revisions() {
        let cache = SemanticTokensCache::new();
        cache.insert(
            "file:///test.txt".to_owned(),
            2,
            "new".to_owned(),
            Vec::new(),
        );
        cache.insert(
            "file:///test.txt".to_owned(),
            1,
            "old".to_owned(),
            Vec::new(),
        );
        assert_eq!(
            cache
                .get("file:///test.txt", 2)
                .expect("current entry")
                .result_id,
            "new"
        );
    }

    #[test]
    fn cache_clears_when_the_bounded_entry_count_is_reached() {
        let cache = SemanticTokensCache::new();
        cache.insert("first".to_owned(), 1, "first".to_owned(), Vec::new());
        for index in 0..SemanticTokensCache::MAX_ENTRIES {
            let uri = format!("file:///entry-{index}.txt");
            cache.insert(uri, 1, index.to_string(), Vec::new());
        }
        assert!(cache.get("first", 1).is_none());
        let last_uri = format!(
            "file:///entry-{}.txt",
            SemanticTokensCache::MAX_ENTRIES.saturating_sub(1)
        );
        assert!(cache.get(&last_uri, 1).is_some());
    }
}
