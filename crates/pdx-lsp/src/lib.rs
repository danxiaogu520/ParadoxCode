//! Minimal JSON-RPC/LSP runtime for the generic PDX language server.
//!
//! The crate owns transport framing, protocol state, document versioning, URI and position
//! conversion, and result freshness checks. Parser and language-feature logic remains in the
//! editor-neutral workspace and analysis crates.

pub mod cli;

pub use pdx_game::eu4::{INSTALL_DESCRIPTOR, first_party_rules, profile};

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fmt;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use lsp_types::{
    CancelParams, CompletionItem, CompletionItemKind, CompletionList, CompletionOptions,
    CompletionResponse, CompletionTextEdit, Diagnostic as LspDiagnostic, DiagnosticSeverity,
    DidChangeTextDocumentParams, DidChangeWatchedFilesParams,
    DidChangeWatchedFilesRegistrationOptions, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DidSaveTextDocumentParams, DocumentFormattingParams,
    DocumentSymbol as LspDocumentSymbol, DocumentSymbolParams, Documentation, FileChangeType,
    FileSystemWatcher, GlobPattern, Hover as LspHover, HoverContents, HoverProviderCapability,
    InitializeParams, InitializeResult, InsertTextFormat, Location as LspLocation, MarkupContent,
    MarkupKind, MessageType, NumberOrString, OneOf, Position as LspPosition, PrepareRenameResponse,
    Range as LspRange, ReferenceParams, Registration, RegistrationParams, RelativePattern,
    RenameOptions, RenameParams, ServerCapabilities, ServerInfo, ShowMessageParams,
    SymbolInformation, SymbolKind, TextDocumentPositionParams, TextDocumentSyncCapability,
    TextDocumentSyncKind, TextDocumentSyncOptions, TextEdit, Uri, WatchKind,
    WorkDoneProgressOptions, WorkspaceEdit, WorkspaceSymbolParams,
};
use pdx_analysis::{
    CancellationToken, Cancelled, CompletionKind, Location, RenameError, RenameFailure,
    complete_with_cancellation, definition_with_cancellation, diagnostics_with_cancellation,
    document_symbols_with_cancellation, hover_with_cancellation, prepare_rename_with_cancellation,
    references_with_cancellation, rename_with_cancellation, workspace_symbols_with_cancellation,
};
use pdx_format::format;
use pdx_game::{
    DiscoveryOptions, DiscoveryOutcome, DiscoveryToken, GameInstallDescriptor, UserConfiguration,
    UserPaths, discover_installations,
};
use pdx_rules::{GameProfile, RuleSet, RulesError};
use pdx_text::{LineIndex, Position, TextRange};
use pdx_workspace::{
    AnalysisHost, AnalysisSnapshot, DiskFileChange, DiskFileChangeKind, DocumentError, DocumentId,
    DocumentSource, ParsedSource, PreparedDocument, SourceRoot, SourceRootId, SourceRootKind,
    VanillaCacheError, VanillaIndexCache, WorkspaceChange, WorkspaceError, WorkspaceScanToken,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const JSON_RPC_VERSION: &str = "2.0";
const INTERNAL_ERROR: i64 = -32603;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const SERVER_NOT_INITIALIZED: i64 = -32002;
const REQUEST_CANCELLED: i64 = -32800;
const DIAGNOSTIC_DEBOUNCE: Duration = Duration::from_millis(200);
const PROJECT_CONFIG_MAX_BYTES: u64 = 1024 * 1024;
const MAX_LSP_HEADER_BYTES: usize = 8 * 1024;
const MAX_LSP_MESSAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;
const MAX_COMPLETION_RESULTS: usize = 512;
const MAX_WORKSPACE_SYMBOL_RESULTS: usize = 256;
const MAX_PUBLISHED_DIAGNOSTICS: usize = 1_000;
const WATCHED_FILES_REGISTRATION_ID: &str = "pdx-source-roots";
const WATCHED_FILES_REQUEST_ID: &str = "pdx/register-source-root-watchers";

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

/// Explicit process-level options passed by an editor or CLI.
///
/// Rules are intentionally absent: official composition roots supply their compiled first-party
/// [`RuleSet`] directly and no user-controlled path can replace it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InitializeOptions;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
struct WorkspaceInitializationOptions {
    project_config: Option<PathBuf>,
    mod_directory: Option<PathBuf>,
    dependencies: Option<Vec<DependencyConfiguration>>,
    vanilla_index_cache: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct DependencyConfiguration {
    id: String,
    path: PathBuf,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
struct ProjectConfiguration {
    #[serde(alias = "modDirectory")]
    mod_directory: Option<PathBuf>,
    dependencies: Option<Vec<DependencyConfiguration>>,
    #[serde(alias = "vanillaIndexCache")]
    vanilla_index_cache: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ResolvedSourceRoots {
    workspace_root: Option<PathBuf>,
    roots: Vec<SourceRoot>,
    vanilla_cache: Option<PathBuf>,
    vanilla_explicit: bool,
}

/// Machine-local automatic Vanilla discovery supplied by a game composition root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutoVanillaConfiguration {
    /// Data-only installation facts for the selected profile.
    pub descriptor: GameInstallDescriptor,
    /// Shared user configuration and cache locations.
    pub user_paths: UserPaths,
}

/// Errors raised by the server transport or process lifecycle.
#[derive(Debug)]
pub enum LspError {
    /// The underlying transport failed.
    Io(io::Error),
    /// A JSON message could not be decoded or encoded.
    Json(serde_json::Error),
    /// The message framing or process lifecycle was invalid.
    Protocol(String),
    /// The explicitly requested EU4 rules artifact failed validation.
    Rules(RulesError),
    /// The client exited without first sending `shutdown`.
    ExitWithoutShutdown,
}

impl fmt::Display for LspError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "LSP transport I/O error: {error}"),
            Self::Json(error) => write!(formatter, "invalid LSP JSON message: {error}"),
            Self::Protocol(message) => write!(formatter, "LSP protocol error: {message}"),
            Self::Rules(error) => write!(formatter, "LSP rules error: {error}"),
            Self::ExitWithoutShutdown => formatter.write_str("LSP exit received before shutdown"),
        }
    }
}

impl std::error::Error for LspError {}

impl From<io::Error> for LspError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for LspError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<RulesError> for LspError {
    fn from(error: RulesError) -> Self {
        Self::Rules(error)
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum RequestId {
    Number(i64),
    String(String),
}

impl RequestId {
    fn parse(value: &Value) -> Result<Self, RpcError> {
        if let Some(number) = value.as_i64() {
            return Ok(Self::Number(number));
        }
        if let Some(string) = value.as_str() {
            return Ok(Self::String(string.to_owned()));
        }
        Err(RpcError::new(INVALID_REQUEST, "request id must be a string or integer"))
    }
}

#[derive(Clone, Debug)]
struct RpcError {
    code: i64,
    message: String,
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
struct InFlightRequest {
    cancellation: CancellationToken,
}

#[derive(Debug)]
struct InFlightInitialize {
    request_id: RequestId,
    cancellation: WorkspaceScanToken,
}

#[derive(Debug)]
struct InitializeTaskResult {
    request_id: RequestId,
    id: Value,
    result: Result<PreparedInitialize, RpcError>,
}

#[derive(Debug)]
struct PreparedInitialize {
    host: AnalysisHost,
    result: Value,
    warnings: Vec<String>,
    auto_vanilla: Option<AutoVanillaConfiguration>,
    watcher_registration: Option<Value>,
}

#[derive(Debug)]
struct VanillaSetupResult {
    result: Result<(VanillaIndexCache, String), String>,
}

#[derive(Clone, Debug)]
struct VanillaSetupCancellation {
    discovery: DiscoveryToken,
    workspace: WorkspaceScanToken,
}

impl VanillaSetupCancellation {
    fn new() -> Self {
        Self { discovery: DiscoveryToken::new(), workspace: WorkspaceScanToken::new() }
    }

    fn cancel(&self) {
        self.discovery.cancel();
        self.workspace.cancel();
    }
}

#[derive(Debug)]
struct SnapshotRequestResult {
    request_id: RequestId,
    id: Value,
    result: Result<Value, RpcError>,
}

#[derive(Debug)]
struct InFlightDiskChanges {
    base_revision: u64,
    cancellation: WorkspaceScanToken,
}

#[derive(Debug)]
struct DiskChangesResult {
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
    VanillaSetup(VanillaSetupResult),
    DiskChanges(DiskChangesResult),
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }

    fn response(&self, id: Value) -> Value {
        json!({
            "jsonrpc": JSON_RPC_VERSION,
            "id": id,
            "error": {"code": self.code, "message": self.message},
        })
    }
}

/// An LSP server with a single event-loop-owned workspace host.
#[derive(Debug)]
pub struct LspServer {
    state: ServerState,
    options: InitializeOptions,
    host: AnalysisHost,
    cancelled: HashSet<RequestId>,
    diagnostics: BTreeMap<DocumentId, Value>,
    pending_parses: BTreeMap<DocumentId, PendingParse>,
    pending_diagnostics: BTreeMap<DocumentId, PendingDiagnostics>,
    pending_disk_changes: BTreeMap<PathBuf, DiskFileChangeKind>,
    watcher_registration: Option<Value>,
    auto_vanilla: Option<AutoVanillaConfiguration>,
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

    /// Runs the same framed transport over arbitrary streams.
    ///
    /// This is public so integration tests can drive the actual JSON-RPC framing without a
    /// process or socket. A dedicated reader keeps input available while diagnostics run on
    /// immutable snapshots; the event-loop thread remains the sole workspace-state owner.
    pub fn run_transport<R: Read + Send, W: Write>(
        &mut self,
        input: R,
        mut output: W,
    ) -> Result<(), LspError> {
        std::thread::scope(|scope| {
            let (event_sender, event_receiver) = mpsc::channel::<TransportEvent>();
            let (read_sender, read_receiver) = mpsc::channel::<()>();
            let reader_sender = event_sender.clone();
            scope.spawn(move || {
                let mut input = BufReader::new(input);
                while read_receiver.recv().is_ok() {
                    let result = read_message(&mut input);
                    let terminal = !matches!(result, Ok(Some(_)));
                    if reader_sender.send(TransportEvent::Input(result)).is_err() || terminal {
                        break;
                    }
                }
            });

            let mut reader_active = true;
            read_sender.send(()).map_err(|_| {
                LspError::Protocol("LSP transport reader failed to start".to_owned())
            })?;
            let mut in_flight_parses = BTreeMap::<DocumentId, InFlightParse>::new();
            let mut in_flight = BTreeMap::<DocumentId, InFlightDiagnostics>::new();
            let mut in_flight_requests = HashMap::<RequestId, InFlightRequest>::new();
            let mut in_flight_initialize = None::<InFlightInitialize>;
            let mut in_flight_vanilla = None::<VanillaSetupCancellation>;
            let mut in_flight_disk_changes = None::<InFlightDiskChanges>;
            let mut deferred_messages = VecDeque::<Value>::new();

            loop {
                self.spawn_pending_disk_changes(scope, &event_sender, &mut in_flight_disk_changes);
                self.cancel_stale_parses(&in_flight_parses);
                self.spawn_pending_parses(scope, &event_sender, &mut in_flight_parses);
                self.cancel_stale_diagnostics(&in_flight);
                self.spawn_due_diagnostics(
                    scope,
                    &event_sender,
                    &mut in_flight,
                    self.state == ServerState::ShuttingDown,
                );
                let parse_busy = !self.pending_parses.is_empty() || !in_flight_parses.is_empty();
                let initialize_busy = in_flight_initialize.is_some();
                let disk_changes_busy =
                    !self.pending_disk_changes.is_empty() || in_flight_disk_changes.is_some();
                let deferred_ready = !parse_busy
                    && !initialize_busy
                    && !disk_changes_busy
                    && !deferred_messages.is_empty();
                let (event, from_reader) = if deferred_ready {
                    let message = deferred_messages.pop_front().expect("checked non-empty");
                    (TransportEvent::Input(Ok(Some(message))), false)
                } else {
                    let timeout = self.next_diagnostic_wait(&in_flight);
                    let event = match timeout {
                        Some(timeout) => match event_receiver.recv_timeout(timeout) {
                            Ok(event) => event,
                            Err(RecvTimeoutError::Timeout) => continue,
                            Err(RecvTimeoutError::Disconnected) => {
                                return Err(LspError::Protocol(
                                    "LSP transport workers stopped unexpectedly".to_owned(),
                                ));
                            }
                        },
                        None => event_receiver.recv().map_err(|_| {
                            LspError::Protocol(
                                "LSP transport workers stopped unexpectedly".to_owned(),
                            )
                        })?,
                    };
                    (event, true)
                };

                match event {
                    TransportEvent::Input(result) => {
                        if from_reader {
                            reader_active = false;
                        }
                        let Some(message) = result? else {
                            return if self.state == ServerState::Exited && self.clean_exit {
                                Ok(())
                            } else {
                                Err(LspError::Protocol(
                                    "transport ended before a clean exit".to_owned(),
                                ))
                            };
                        };
                        let parse_busy =
                            !self.pending_parses.is_empty() || !in_flight_parses.is_empty();
                        let initialize_busy = in_flight_initialize.is_some();
                        let disk_changes_busy = !self.pending_disk_changes.is_empty()
                            || in_flight_disk_changes.is_some();
                        if from_reader
                            && (((parse_busy || disk_changes_busy)
                                && is_snapshot_request_message(&message))
                                || (initialize_busy && !is_initialize_control_message(&message)))
                        {
                            deferred_messages.push_back(message);
                        } else {
                            let spawned = self.spawn_initialize_request(
                                scope,
                                &event_sender,
                                &mut in_flight_initialize,
                                &message,
                            ) || self.spawn_snapshot_request(
                                scope,
                                &event_sender,
                                &mut in_flight_requests,
                                &message,
                            );
                            if !spawned {
                                let responses = self.handle_message(message.clone())?;
                                for response in responses {
                                    write_message(&mut output, &response)?;
                                }
                                cancel_request_from_notification(&message, &in_flight_requests);
                                cancel_initialize_from_notification(
                                    &message,
                                    in_flight_initialize.as_ref(),
                                );
                            }
                        }
                        self.cancel_stale_parses(&in_flight_parses);
                        self.cancel_stale_diagnostics(&in_flight);
                        if self.state == ServerState::Exited {
                            for task in in_flight_parses.values() {
                                task.cancelled.store(true, Ordering::Release);
                            }
                            for task in in_flight.values() {
                                task.cancellation.cancel();
                            }
                            for task in in_flight_requests.values() {
                                task.cancellation.cancel();
                            }
                            if let Some(task) = in_flight_initialize.as_ref() {
                                task.cancellation.cancel();
                            }
                            if let Some(task) = in_flight_vanilla.as_ref() {
                                task.cancel();
                            }
                            if let Some(task) = in_flight_disk_changes.as_ref() {
                                task.cancellation.cancel();
                            }
                            return if self.clean_exit {
                                Ok(())
                            } else {
                                Err(LspError::ExitWithoutShutdown)
                            };
                        }
                        if self.state == ServerState::ShuttingDown {
                            self.spawn_due_diagnostics(scope, &event_sender, &mut in_flight, true);
                        }
                    }
                    TransportEvent::Initialize(result) => {
                        let current = in_flight_initialize
                            .as_ref()
                            .is_some_and(|task| task.request_id == result.request_id);
                        if !current {
                            continue;
                        }
                        let task = in_flight_initialize.take().expect("checked initialize task");
                        self.cancelled.remove(&result.request_id);
                        let (response, warnings, auto_vanilla) = match result.result {
                            Ok(prepared) if !task.cancellation.is_cancelled() => {
                                self.host = prepared.host;
                                self.state = ServerState::Initialized;
                                self.watcher_registration = prepared.watcher_registration;
                                (
                                    json!({
                                        "jsonrpc": JSON_RPC_VERSION,
                                        "id": result.id,
                                        "result": prepared.result,
                                    }),
                                    prepared.warnings,
                                    prepared.auto_vanilla,
                                )
                            }
                            Ok(_) => {
                                self.state = ServerState::Uninitialized;
                                (
                                    RpcError::new(REQUEST_CANCELLED, "request was cancelled")
                                        .response(result.id),
                                    Vec::new(),
                                    None,
                                )
                            }
                            Err(error) => {
                                self.state = ServerState::Uninitialized;
                                (error.response(result.id), Vec::new(), None)
                            }
                        };
                        write_message(&mut output, &response)?;
                        for warning in warnings {
                            write_message(&mut output, &show_warning_notification(warning))?;
                        }
                        if let Some(configuration) = auto_vanilla {
                            let cancellation = VanillaSetupCancellation::new();
                            let sender = event_sender.clone();
                            let rules = self.host.snapshot().rules().clone();
                            let profile = self.host.snapshot().game_profile().clone();
                            let worker_cancellation = cancellation.clone();
                            in_flight_vanilla = Some(cancellation);
                            scope.spawn(move || {
                                let result = run_auto_vanilla_setup(
                                    &configuration,
                                    rules,
                                    profile,
                                    &worker_cancellation,
                                );
                                let _ =
                                    sender.send(TransportEvent::VanillaSetup(VanillaSetupResult {
                                        result,
                                    }));
                            });
                        }
                    }
                    TransportEvent::Parse(result) => {
                        if in_flight_parses
                            .get(&result.id)
                            .is_some_and(|task| task.version == result.version)
                        {
                            in_flight_parses.remove(&result.id);
                        }
                        if let Some(prepared) = result.prepared
                            && self.host.commit_prepared_document(prepared)
                        {
                            self.schedule_diagnostics_for_document(
                                result.id,
                                result.version,
                                DIAGNOSTIC_DEBOUNCE,
                            );
                        }
                    }
                    TransportEvent::Diagnostics(result) => {
                        if in_flight
                            .get(&result.id)
                            .is_some_and(|task| task.version == result.version)
                        {
                            in_flight.remove(&result.id);
                        }
                        if let Some(values) = result.values
                            && self.commit_diagnostics(&result.uri, result.version, values.clone())
                        {
                            write_message(
                                &mut output,
                                &diagnostics_notification(&result.uri, values),
                            )?;
                        }
                    }
                    TransportEvent::Request(result) => {
                        in_flight_requests.remove(&result.request_id);
                        self.cancelled.remove(&result.request_id);
                        let response = match result.result {
                            Ok(value) => json!({
                                "jsonrpc": JSON_RPC_VERSION,
                                "id": result.id,
                                "result": value,
                            }),
                            Err(error) => error.response(result.id),
                        };
                        write_message(&mut output, &response)?;
                    }
                    TransportEvent::VanillaSetup(result) => {
                        in_flight_vanilla = None;
                        match result.result {
                            Ok((cache, message)) => match self.host.install_vanilla_cache(cache) {
                                Ok(()) => {
                                    write_message(&mut output, &show_info_notification(message))?;
                                    let open = self
                                        .host
                                        .snapshot()
                                        .documents()
                                        .iter()
                                        .filter_map(|(id, document)| {
                                            document.version().map(|version| (id.clone(), version))
                                        })
                                        .collect::<Vec<_>>();
                                    for (id, version) in open {
                                        self.schedule_diagnostics_for_document(
                                            id,
                                            version,
                                            DIAGNOSTIC_DEBOUNCE,
                                        );
                                    }
                                }
                                Err(error) => write_message(
                                    &mut output,
                                    &show_warning_notification(format!(
                                        "Vanilla cache was built but could not be enabled in this workspace: {error}"
                                    )),
                                )?,
                            },
                            Err(message) => {
                                write_message(&mut output, &show_warning_notification(message))?;
                            }
                        }
                    }
                    TransportEvent::DiskChanges(result) => {
                        let current = in_flight_disk_changes
                            .as_ref()
                            .is_some_and(|task| task.base_revision == result.base_revision);
                        if !current {
                            continue;
                        }
                        let task = in_flight_disk_changes.take().expect("checked disk change task");
                        if task.cancellation.is_cancelled() {
                            continue;
                        }
                        if self.host.snapshot().revision() != result.base_revision {
                            self.requeue_disk_changes(result.changes);
                            continue;
                        }
                        match result.result {
                            Ok(host) => {
                                self.host = host;
                                let open = self
                                    .host
                                    .snapshot()
                                    .documents()
                                    .iter()
                                    .filter_map(|(id, document)| {
                                        document.version().map(|version| (id.clone(), version))
                                    })
                                    .collect::<Vec<_>>();
                                for (id, version) in open {
                                    self.schedule_diagnostics_for_document(
                                        id,
                                        version,
                                        DIAGNOSTIC_DEBOUNCE,
                                    );
                                }
                            }
                            Err(WorkspaceError::Cancelled) => {}
                            Err(error) => {
                                write_message(
                                    &mut output,
                                    &show_warning_notification(format!(
                                        "Workspace file changes could not be indexed: {error}"
                                    )),
                                )?;
                            }
                        }
                    }
                }

                let draining_shutdown = self.state == ServerState::ShuttingDown
                    && (!self.pending_parses.is_empty()
                        || !in_flight_parses.is_empty()
                        || !self.pending_diagnostics.is_empty()
                        || !in_flight.is_empty()
                        || !in_flight_requests.is_empty()
                        || in_flight_initialize.is_some()
                        || in_flight_vanilla.is_some()
                        || !self.pending_disk_changes.is_empty()
                        || in_flight_disk_changes.is_some());
                if !reader_active && !draining_shutdown && deferred_messages.is_empty() {
                    read_sender.send(()).map_err(|_| {
                        LspError::Protocol("LSP transport reader stopped unexpectedly".to_owned())
                    })?;
                    reader_active = true;
                }
            }
        })
    }

    fn spawn_snapshot_request<'scope, 'environment>(
        &self,
        scope: &'scope std::thread::Scope<'scope, 'environment>,
        event_sender: &mpsc::Sender<TransportEvent>,
        in_flight: &mut HashMap<RequestId, InFlightRequest>,
        message: &Value,
    ) -> bool {
        if self.state != ServerState::Initialized {
            return false;
        }
        let Some(object) = message.as_object() else { return false };
        if object.get("jsonrpc").and_then(Value::as_str) != Some(JSON_RPC_VERSION) {
            return false;
        }
        let Some(method) = object.get("method").and_then(Value::as_str) else { return false };
        if !is_snapshot_request(method) {
            return false;
        }
        let Some(id) = object.get("id").filter(|id| !id.is_null()) else { return false };
        let Ok(request_id) = RequestId::parse(id) else { return false };
        if in_flight.contains_key(&request_id) {
            return false;
        }

        let cancellation = CancellationToken::new();
        if self.cancelled.contains(&request_id) {
            cancellation.cancel();
        }
        let context = SnapshotRequestContext::new(self.host.snapshot(), cancellation.clone());
        let method = method.to_owned();
        let params = object.get("params").cloned();
        let id = id.clone();
        let sender = event_sender.clone();
        in_flight
            .insert(request_id.clone(), InFlightRequest { cancellation: cancellation.clone() });
        scope.spawn(move || {
            let result = if cancellation.is_cancelled() {
                Err(RpcError::new(REQUEST_CANCELLED, "request was cancelled"))
            } else {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    context.dispatch(&method, params.as_ref())
                }))
                .unwrap_or_else(|_| {
                    Err(RpcError::new(INTERNAL_ERROR, "request worker failed unexpectedly"))
                });
                if cancellation.is_cancelled() {
                    Err(RpcError::new(REQUEST_CANCELLED, "request was cancelled"))
                } else {
                    result
                }
            };
            let _ = sender.send(TransportEvent::Request(SnapshotRequestResult {
                request_id,
                id,
                result,
            }));
        });
        true
    }

    fn spawn_initialize_request<'scope, 'environment>(
        &mut self,
        scope: &'scope std::thread::Scope<'scope, 'environment>,
        event_sender: &mpsc::Sender<TransportEvent>,
        in_flight: &mut Option<InFlightInitialize>,
        message: &Value,
    ) -> bool {
        if self.state != ServerState::Uninitialized || in_flight.is_some() {
            return false;
        }
        let Some(object) = message.as_object() else { return false };
        if object.get("jsonrpc").and_then(Value::as_str) != Some(JSON_RPC_VERSION)
            || object.get("method").and_then(Value::as_str) != Some("initialize")
        {
            return false;
        }
        let Some(id) = object.get("id").filter(|id| !id.is_null()) else { return false };
        let Ok(request_id) = RequestId::parse(id) else { return false };
        let Ok(params) = typed_params::<InitializeParams>(object.get("params"), "initialize")
        else {
            return false;
        };

        let cancellation = WorkspaceScanToken::new();
        if self.cancelled.contains(&request_id) {
            cancellation.cancel();
        }
        let candidate = self.host.clone();
        let scan_workspace = !self.host.snapshot().rules().game_id().is_empty();
        let auto_vanilla = self.auto_vanilla.clone();
        let sender = event_sender.clone();
        let id = id.clone();
        self.state = ServerState::Initializing;
        *in_flight = Some(InFlightInitialize {
            request_id: request_id.clone(),
            cancellation: cancellation.clone(),
        });
        scope.spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                prepare_initialize_candidate(
                    candidate,
                    params,
                    scan_workspace,
                    auto_vanilla.as_ref(),
                    &cancellation,
                )
            }))
            .unwrap_or_else(|_| {
                Err(RpcError::new(INTERNAL_ERROR, "initialize worker failed unexpectedly"))
            });
            let _ = sender.send(TransportEvent::Initialize(Box::new(InitializeTaskResult {
                request_id,
                id,
                result,
            })));
        });
        true
    }

    fn spawn_pending_disk_changes<'scope, 'environment>(
        &mut self,
        scope: &'scope std::thread::Scope<'scope, 'environment>,
        event_sender: &mpsc::Sender<TransportEvent>,
        in_flight: &mut Option<InFlightDiskChanges>,
    ) {
        if !matches!(self.state, ServerState::Initialized | ServerState::ShuttingDown)
            || in_flight.is_some()
            || self.pending_disk_changes.is_empty()
        {
            return;
        }
        let changes = std::mem::take(&mut self.pending_disk_changes)
            .into_iter()
            .map(|(path, kind)| DiskFileChange::new(path, kind))
            .collect::<Vec<_>>();
        let base_revision = self.host.snapshot().revision();
        let cancellation = WorkspaceScanToken::new();
        let worker_cancellation = cancellation.clone();
        let worker_changes = changes.clone();
        let mut candidate = self.host.clone();
        let sender = event_sender.clone();
        *in_flight = Some(InFlightDiskChanges { base_revision, cancellation });
        scope.spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                candidate
                    .apply_disk_file_changes_cancellable(&worker_changes, &worker_cancellation)
                    .map(|_| candidate)
            }))
            .unwrap_or_else(|_| {
                Err(WorkspaceError::Io(io::Error::other(
                    "workspace file-change worker failed unexpectedly",
                )))
            });
            let _ = sender.send(TransportEvent::DiskChanges(DiskChangesResult {
                base_revision,
                changes: worker_changes,
                result,
            }));
        });
    }

    fn requeue_disk_changes(&mut self, changes: Vec<DiskFileChange>) {
        for change in changes {
            self.pending_disk_changes.entry(change.path).or_insert(change.kind);
        }
    }

    fn schedule_parse(&mut self, uri: &str) {
        let id = DocumentId::new(uri);
        let version = self
            .host
            .snapshot()
            .document(&id)
            .filter(|document| document.source() == DocumentSource::Overlay)
            .and_then(|document| document.version());
        if let Some(version) = version {
            self.pending_parses.insert(id, PendingParse { version });
        }
    }

    fn cancel_stale_parses(&self, in_flight: &BTreeMap<DocumentId, InFlightParse>) {
        let snapshot = self.host.snapshot();
        for (id, task) in in_flight {
            let current_version = snapshot.document(id).and_then(|document| document.version());
            let superseded =
                self.pending_parses.get(id).is_some_and(|pending| pending.version != task.version);
            if current_version != Some(task.version) || superseded {
                task.cancelled.store(true, Ordering::Release);
            }
        }
    }

    fn spawn_pending_parses<'scope, 'environment>(
        &mut self,
        scope: &'scope std::thread::Scope<'scope, 'environment>,
        event_sender: &mpsc::Sender<TransportEvent>,
        in_flight: &mut BTreeMap<DocumentId, InFlightParse>,
    ) {
        let ready = self
            .pending_parses
            .keys()
            .filter(|id| !in_flight.contains_key(*id))
            .cloned()
            .collect::<Vec<_>>();
        for id in ready {
            let Some(pending) = self.pending_parses.remove(&id) else { continue };
            let snapshot = self.host.snapshot();
            let sender = event_sender.clone();
            let cancelled = Arc::new(AtomicBool::new(false));
            in_flight.insert(
                id.clone(),
                InFlightParse { version: pending.version, cancelled: Arc::clone(&cancelled) },
            );
            scope.spawn(move || {
                let prepared = if cancelled.load(Ordering::Acquire) {
                    None
                } else {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        snapshot.prepare_document(&id)
                    }))
                    .ok()
                    .flatten()
                    .filter(|_| !cancelled.load(Ordering::Acquire))
                };
                let _ = sender.send(TransportEvent::Parse(ParseResult {
                    id,
                    version: pending.version,
                    prepared,
                }));
            });
        }
    }

    fn schedule_diagnostics(&mut self, uri: &str, delay: Duration) {
        let id = DocumentId::new(uri);
        let version = self
            .host
            .snapshot()
            .document(&id)
            .filter(|document| document.source() == DocumentSource::Overlay)
            .and_then(|document| document.version());
        if let Some(version) = version {
            self.pending_diagnostics.insert(
                id,
                PendingDiagnostics { uri: uri.to_owned(), version, due: Instant::now() + delay },
            );
        }
    }

    fn schedule_diagnostics_for_document(&mut self, id: DocumentId, version: i64, delay: Duration) {
        self.pending_diagnostics.insert(
            id.clone(),
            PendingDiagnostics {
                uri: id.as_str().to_owned(),
                version,
                due: Instant::now() + delay,
            },
        );
    }

    fn cancel_stale_diagnostics(&self, in_flight: &BTreeMap<DocumentId, InFlightDiagnostics>) {
        let snapshot = self.host.snapshot();
        for (id, task) in in_flight {
            let current_version = snapshot.document(id).and_then(|document| document.version());
            let superseded = self
                .pending_diagnostics
                .get(id)
                .is_some_and(|pending| pending.version != task.version);
            if current_version != Some(task.version) || superseded {
                task.cancellation.cancel();
            }
        }
    }

    fn next_diagnostic_wait(
        &self,
        in_flight: &BTreeMap<DocumentId, InFlightDiagnostics>,
    ) -> Option<Duration> {
        let now = Instant::now();
        self.pending_diagnostics
            .iter()
            .filter(|(id, _)| !in_flight.contains_key(*id))
            .map(|(_, pending)| pending.due.saturating_duration_since(now))
            .min()
    }

    fn spawn_due_diagnostics<'scope, 'environment>(
        &mut self,
        scope: &'scope std::thread::Scope<'scope, 'environment>,
        event_sender: &mpsc::Sender<TransportEvent>,
        in_flight: &mut BTreeMap<DocumentId, InFlightDiagnostics>,
        force: bool,
    ) {
        let now = Instant::now();
        let ready = self
            .pending_diagnostics
            .iter()
            .filter(|(id, pending)| !in_flight.contains_key(*id) && (force || pending.due <= now))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in ready {
            let Some(pending) = self.pending_diagnostics.remove(&id) else { continue };
            let snapshot = self.host.snapshot();
            let sender = event_sender.clone();
            let cancellation = CancellationToken::new();
            in_flight.insert(
                id.clone(),
                InFlightDiagnostics {
                    version: pending.version,
                    cancellation: cancellation.clone(),
                },
            );
            scope.spawn(move || {
                let values = if cancellation.is_cancelled() {
                    None
                } else {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        diagnostic_values(&snapshot, &id, &cancellation)
                    }))
                    .ok()
                    .flatten()
                    .filter(|_| !cancellation.is_cancelled())
                };
                let _ = sender.send(TransportEvent::Diagnostics(DiagnosticsResult {
                    id,
                    uri: pending.uri,
                    version: pending.version,
                    values,
                }));
            });
        }
    }

    fn handle_message(&mut self, message: Value) -> Result<Vec<Value>, LspError> {
        let Some(object) = message.as_object() else {
            return Ok(vec![
                RpcError::new(INVALID_REQUEST, "request must be a JSON object")
                    .response(Value::Null),
            ]);
        };
        if object.get("jsonrpc").and_then(Value::as_str) != Some(JSON_RPC_VERSION) {
            return Ok(vec![
                RpcError::new(INVALID_REQUEST, "jsonrpc must be \"2.0\"")
                    .response(object.get("id").cloned().unwrap_or(Value::Null)),
            ]);
        }
        let Some(method) = object.get("method").and_then(Value::as_str) else {
            // Responses from a client are not part of Phase 2's server input stream.
            if object.contains_key("result") || object.contains_key("error") {
                return Ok(Vec::new());
            }
            return Ok(vec![
                RpcError::new(INVALID_REQUEST, "request method is missing")
                    .response(object.get("id").cloned().unwrap_or(Value::Null)),
            ]);
        };
        let id_value = object.get("id").cloned();
        let request_id = match id_value.as_ref() {
            Some(Value::Null) => None,
            Some(value) => match RequestId::parse(value) {
                Ok(request_id) => Some(request_id),
                Err(error) => return Ok(vec![error.response(Value::Null)]),
            },
            None => None,
        };

        if method == "$/cancelRequest" {
            self.handle_cancel(object.get("params"));
            return Ok(Vec::new());
        }

        let result = self.dispatch_method(method, object.get("params"), request_id.as_ref());
        match (id_value, result) {
            (Some(id), Ok(value)) => {
                Ok(vec![json!({"jsonrpc": JSON_RPC_VERSION, "id": id, "result": value})])
            }
            (Some(id), Err(error)) => Ok(vec![error.response(id)]),
            (None, Ok(value)) if value != Value::Null => Ok(vec![value]),
            (None, _) => Ok(Vec::new()),
        }
    }

    fn dispatch_method(
        &mut self,
        method: &str,
        params: Option<&Value>,
        request_id: Option<&RequestId>,
    ) -> Result<Value, RpcError> {
        if method == "exit" {
            self.clean_exit = self.state == ServerState::ShuttingDown;
            self.state = ServerState::Exited;
            return Ok(Value::Null);
        }
        if method == "initialize" {
            if self.state != ServerState::Uninitialized {
                return Err(RpcError::new(INVALID_REQUEST, "server is already initialized"));
            }
            return self.handle_initialize(params);
        }
        if let Some(request_id) = request_id
            && self.cancelled.remove(request_id)
        {
            return Err(RpcError::new(REQUEST_CANCELLED, "request was cancelled"));
        }
        if matches!(self.state, ServerState::Uninitialized | ServerState::Initializing) {
            return Err(RpcError::new(SERVER_NOT_INITIALIZED, "server is not initialized"));
        }
        if self.state == ServerState::ShuttingDown {
            return Err(RpcError::new(SERVER_NOT_INITIALIZED, "server is shutting down"));
        }

        match method {
            "initialized" => Ok(self.watcher_registration.take().unwrap_or(Value::Null)),
            "shutdown" => {
                self.state = ServerState::ShuttingDown;
                Ok(Value::Null)
            }
            "textDocument/didOpen" => {
                let uri = self.handle_did_open(params)?;
                self.schedule_parse(&uri);
                Ok(Value::Null)
            }
            "textDocument/didChange" => {
                let uri = self.handle_did_change(params)?;
                self.schedule_parse(&uri);
                Ok(Value::Null)
            }
            "textDocument/didClose" => self.handle_did_close(params),
            "textDocument/didSave" => {
                let params = typed_params::<DidSaveTextDocumentParams>(params, "didSave")?;
                let uri = params.text_document.uri.as_str();
                if let Ok(path) = parse_file_uri_str(uri) {
                    self.pending_disk_changes
                        .insert(normalize_workspace_path(path), DiskFileChangeKind::Changed);
                }
                self.schedule_parse(uri);
                self.schedule_diagnostics(uri, Duration::ZERO);
                Ok(Value::Null)
            }
            "workspace/didChangeWatchedFiles" => {
                self.handle_did_change_watched_files(params)?;
                Ok(Value::Null)
            }
            method if is_snapshot_request(method) => {
                SnapshotRequestContext::new(self.host.snapshot(), CancellationToken::new())
                    .dispatch(method, params)
            }
            _ => Err(RpcError::new(METHOD_NOT_FOUND, "method is not implemented")),
        }
    }

    fn handle_cancel(&mut self, params: Option<&Value>) {
        if let Ok(params) = typed_params::<CancelParams>(params, "cancel request") {
            self.cancelled.insert(request_id_from_lsp(params.id));
        }
    }

    fn handle_initialize(&mut self, params: Option<&Value>) -> Result<Value, RpcError> {
        let params = typed_params::<InitializeParams>(params, "initialize")?;
        let prepared = prepare_initialize_candidate(
            self.host.clone(),
            params,
            !self.host.snapshot().rules().game_id().is_empty(),
            None,
            &WorkspaceScanToken::new(),
        )?;
        self.host = prepared.host;
        self.watcher_registration = prepared.watcher_registration;
        self.state = ServerState::Initialized;
        Ok(prepared.result)
    }

    fn handle_did_open(&mut self, params: Option<&Value>) -> Result<String, RpcError> {
        let params = typed_params::<DidOpenTextDocumentParams>(params, "didOpen")?;
        let uri = params.text_document.uri.as_str().to_owned();
        let version = i64::from(params.text_document.version);
        let text = params.text_document.text;
        changed_document_len(0, None, text.len())?;
        let path = uri_to_path(&uri).ok().map(normalize_workspace_path);
        self.host
            .stage_open_document(DocumentId::new(uri.clone()), version, text, path)
            .map_err(document_error)?;
        Ok(uri)
    }

    fn handle_did_change(&mut self, params: Option<&Value>) -> Result<String, RpcError> {
        let params = typed_params::<DidChangeTextDocumentParams>(params, "didChange")?;
        let uri = params.text_document.uri.as_str().to_owned();
        let id = DocumentId::new(uri.clone());
        let version = i64::from(params.text_document.version);
        let snapshot = self.host.snapshot();
        let current = snapshot
            .document(&id)
            .filter(|document| document.source() == DocumentSource::Overlay)
            .ok_or_else(|| RpcError::new(INVALID_PARAMS, "didChange document is not open"))?;
        let mut text = current.text().to_owned();
        let mut line_index = current.line_index().clone();
        for change in params.content_changes {
            let range = change
                .range
                .as_ref()
                .map(|range| lsp_range_to_text_range(range, &line_index, &text))
                .transpose()?;
            changed_document_len(text.len(), range, change.text.len())?;
            apply_text_change(&mut text, range, &change.text)?;
            line_index = LineIndex::new(&text);
        }
        self.host.stage_document_text(&id, version, text).map_err(document_error)?;
        Ok(uri)
    }

    fn handle_did_close(&mut self, params: Option<&Value>) -> Result<Value, RpcError> {
        let params = typed_params::<DidCloseTextDocumentParams>(params, "didClose")?;
        let uri = params.text_document.uri.as_str().to_owned();
        let id = DocumentId::new(uri.clone());
        self.host.close_document(&id).map_err(document_error)?;
        self.pending_parses.remove(&id);
        self.pending_diagnostics.remove(&id);
        self.diagnostics.remove(&id);
        Ok(json!({
            "jsonrpc": JSON_RPC_VERSION,
            "method": "textDocument/publishDiagnostics",
            "params": {"uri": uri, "diagnostics": []}
        }))
    }

    fn handle_did_change_watched_files(&mut self, params: Option<&Value>) -> Result<(), RpcError> {
        let params = typed_params::<DidChangeWatchedFilesParams>(params, "didChangeWatchedFiles")?;
        for event in params.changes {
            let path = normalize_workspace_path(parse_file_uri_str(event.uri.as_str())?);
            let kind = if event.typ == FileChangeType::CREATED {
                DiskFileChangeKind::Created
            } else if event.typ == FileChangeType::CHANGED {
                DiskFileChangeKind::Changed
            } else if event.typ == FileChangeType::DELETED {
                DiskFileChangeKind::Deleted
            } else {
                return Err(RpcError::new(
                    INVALID_PARAMS,
                    "didChangeWatchedFiles contains an unsupported change type",
                ));
            };
            self.pending_disk_changes.insert(path, kind);
        }
        Ok(())
    }
}

fn prepare_initialize_candidate(
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
    let root = root_uri.as_ref().map(|uri| parse_file_uri_str(uri.as_str())).transpose()?;
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
        host.refresh_source_roots_cancellable(cancellation).map_err(workspace_scan_error)?;
    }
    if let Some(path) = resolved.vanilla_cache {
        if !scan_workspace {
            warnings.push(format!(
                "Vanilla cache {} was not loaded because no validated rules artifact is active",
                path.display()
            ));
        } else if !path.is_file() {
            warnings.push(format!(
                "Vanilla cache {} does not exist; continuing without Vanilla symbols",
                path.display()
            ));
        } else {
            match VanillaIndexCache::load_cancellable(&path, cancellation) {
                Err(VanillaCacheError::Cancelled) => {
                    return Err(RpcError::new(REQUEST_CANCELLED, "request was cancelled"));
                }
                Err(error) => warnings.push(format!(
                    "Vanilla cache {} could not be loaded; continuing without Vanilla symbols: {error}",
                    path.display()
                )),
                Ok(cache) => {
                    let cache_rule_hash = cache.metadata().rule_hash.clone();
                    let current_rule_hash = host.snapshot().rules().rule_hash().to_hex();
                    match host.install_vanilla_cache(cache) {
                        Ok(()) => {
                            if cache_rule_hash != current_rule_hash {
                                warnings.push(format!(
                                    "Vanilla cache was built with rules hash {cache_rule_hash}, but the active rules hash is {current_rule_hash}; the cache remains loaded until you refresh it explicitly"
                                ));
                            }
                        }
                        Err(error) => warnings.push(format!(
                            "Vanilla cache {} is incompatible with this workspace; continuing without Vanilla symbols: {error}",
                            path.display()
                        )),
                    }
                }
            }
        }
    }
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
    Ok(PreparedInitialize { host, result, warnings, auto_vanilla, watcher_registration })
}

fn watched_files_registration(
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
        .filter(|root| matches!(root.kind, SourceRootKind::CurrentMod | SourceRootKind::Dependency))
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

fn apply_user_vanilla_configuration(
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

fn run_auto_vanilla_setup(
    auto_vanilla: &AutoVanillaConfiguration,
    rules: RuleSet,
    profile: GameProfile,
    cancellation: &VanillaSetupCancellation,
) -> Result<(VanillaIndexCache, String), String> {
    run_auto_vanilla_setup_with_options(
        auto_vanilla,
        rules,
        profile,
        cancellation,
        &DiscoveryOptions::default(),
    )
}

fn run_auto_vanilla_setup_with_options(
    auto_vanilla: &AutoVanillaConfiguration,
    rules: RuleSet,
    profile: GameProfile,
    cancellation: &VanillaSetupCancellation,
    discovery_options: &DiscoveryOptions,
) -> Result<(VanillaIndexCache, String), String> {
    let descriptor = auto_vanilla.descriptor;
    let mut configuration =
        UserConfiguration::load(&auto_vanilla.user_paths.config_file).map_err(|error| {
            format!("automatic Vanilla discovery could not load user configuration: {error}")
        })?;
    if configuration.games.get(descriptor.game_id).is_some_and(|game| game.auto_discovery_attempted)
    {
        return Err(format!(
            "automatic {} discovery was skipped because it was already attempted",
            descriptor.display_name
        ));
    }
    let report = discover_installations(&descriptor, discovery_options, &cancellation.discovery);
    if report.cancelled {
        return Err(format!("automatic {} discovery was cancelled", descriptor.display_name));
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
        host.refresh_source_roots_cancellable(&cancellation.workspace)
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
            let game = configuration.games.entry(descriptor.game_id.to_owned()).or_default();
            game.auto_discovery_attempted = true;
            game.discovery_outcome = Some(DiscoveryOutcome::Configured);
            game.vanilla_source = Some(source.clone());
            game.vanilla_cache = Some(cache_path.clone());
            configuration.save(&auto_vanilla.user_paths.config_file).map_err(|error| {
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
                return Err(format!("automatic {} setup was cancelled", descriptor.display_name));
            }
            let game = configuration.games.entry(descriptor.game_id.to_owned()).or_default();
            game.auto_discovery_attempted = true;
            game.discovery_outcome = Some(DiscoveryOutcome::Failed);
            game.vanilla_source = Some(source);
            let save_error = configuration.save(&auto_vanilla.user_paths.config_file).err();
            match save_error {
                Some(save_error) => {
                    Err(format!("{error}; failed to record the attempt: {save_error}"))
                }
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

fn resolve_source_roots(
    client_root: Option<&Path>,
    initialization_options: Option<Value>,
    cancellation: &WorkspaceScanToken,
) -> Result<ResolvedSourceRoots, RpcError> {
    let inline = initialization_options.map_or_else(
        || Ok(WorkspaceInitializationOptions::default()),
        |value| {
            serde_json::from_value::<WorkspaceInitializationOptions>(value).map_err(|error| {
                RpcError::new(INVALID_PARAMS, format!("invalid initializationOptions: {error}"))
            })
        },
    )?;
    let base = client_root.map(Path::to_path_buf);
    let mut project = if let Some(path) = inline.project_config.as_deref() {
        let path = resolve_path(path, base.as_deref(), "projectConfig")?;
        if !path.is_file() {
            return Err(RpcError::new(
                INVALID_PARAMS,
                format!("projectConfig is not a file: {}", path.display()),
            ));
        }
        if cancellation.is_cancelled() {
            return Err(RpcError::new(REQUEST_CANCELLED, "request was cancelled"));
        }
        let file = fs::File::open(&path).map_err(|error| {
            RpcError::new(
                INVALID_PARAMS,
                format!("cannot open projectConfig {}: {error}", path.display()),
            )
        })?;
        let mut text = String::new();
        file.take(PROJECT_CONFIG_MAX_BYTES + 1).read_to_string(&mut text).map_err(|error| {
            RpcError::new(
                INVALID_PARAMS,
                format!("cannot read projectConfig {}: {error}", path.display()),
            )
        })?;
        if text.len() as u64 > PROJECT_CONFIG_MAX_BYTES {
            return Err(RpcError::new(INVALID_PARAMS, "projectConfig exceeds 1 MiB"));
        }
        toml::from_str::<ProjectConfiguration>(&text).map_err(|error| {
            RpcError::new(INVALID_PARAMS, format!("invalid projectConfig TOML: {error}"))
        })?
    } else {
        ProjectConfiguration::default()
    };
    if inline.mod_directory.is_some() {
        project.mod_directory = inline.mod_directory;
    }
    if inline.dependencies.is_some() {
        project.dependencies = inline.dependencies;
    }
    if inline.vanilla_index_cache.is_some() {
        project.vanilla_index_cache = inline.vanilla_index_cache;
    }
    let vanilla_index_cache = project
        .vanilla_index_cache
        .as_deref()
        .map(|path| resolve_configured_path(path, base.as_deref(), "vanillaIndexCache"))
        .transpose()?;
    let vanilla_explicit = vanilla_index_cache.is_some();

    let current_mod = match project.mod_directory.as_deref() {
        Some(path) => Some(resolve_directory(path, base.as_deref(), "modDirectory")?),
        None => {
            client_root.filter(|path| path.is_dir()).map(fs::canonicalize).transpose().map_err(
                |error| {
                    RpcError::new(INVALID_PARAMS, format!("cannot resolve workspace root: {error}"))
                },
            )?
        }
    };
    let mut configured = Vec::<(String, PathBuf)>::new();
    let mut root_ids = BTreeMap::<u32, String>::new();
    for dependency in project.dependencies.unwrap_or_default() {
        if dependency.id.trim().is_empty() {
            return Err(RpcError::new(INVALID_PARAMS, "dependency id must not be empty"));
        }
        if dependency.id != dependency.id.trim() {
            return Err(RpcError::new(
                INVALID_PARAMS,
                format!("dependency id must not have surrounding whitespace: {}", dependency.id),
            ));
        }
        if configured.iter().any(|(id, _)| id.eq_ignore_ascii_case(&dependency.id)) {
            return Err(RpcError::new(
                INVALID_PARAMS,
                format!("duplicate dependency id: {}", dependency.id),
            ));
        }
        let path = resolve_directory(&dependency.path, base.as_deref(), "dependency path")?;
        let root_id = stable_dependency_root_id(&dependency.id);
        if let Some(previous) = root_ids.insert(root_id, dependency.id.clone()) {
            return Err(RpcError::new(
                INVALID_PARAMS,
                format!("dependency root id collision between {previous} and {}", dependency.id),
            ));
        }
        configured.push((dependency.id, path));
    }

    let mut paths = configured.iter().map(|(_, path)| path).collect::<Vec<_>>();
    if let Some(current_mod) = current_mod.as_ref() {
        paths.push(current_mod);
    }
    for (index, left) in paths.iter().enumerate() {
        for right in paths.iter().skip(index + 1) {
            if left.starts_with(right) || right.starts_with(left) {
                return Err(RpcError::new(
                    INVALID_PARAMS,
                    format!(
                        "source roots must not overlap: {} and {}",
                        left.display(),
                        right.display()
                    ),
                ));
            }
        }
    }

    let mut roots = Vec::with_capacity(configured.len() + usize::from(current_mod.is_some()));
    for (order, (id, path)) in configured.into_iter().enumerate() {
        let mut root = SourceRoot::new(
            SourceRootId::new(stable_dependency_root_id(&id)),
            SourceRootKind::Dependency,
            path,
        );
        root.order = u32::try_from(order).map_err(|_| {
            RpcError::new(INVALID_PARAMS, "too many dependency roots to assign stable order")
        })?;
        roots.push(root);
    }
    if let Some(path) = current_mod.clone() {
        roots.push(SourceRoot::new(SourceRootId::new(u32::MAX), SourceRootKind::CurrentMod, path));
    }
    Ok(ResolvedSourceRoots {
        workspace_root: current_mod.or(base),
        roots,
        vanilla_cache: vanilla_index_cache,
        vanilla_explicit,
    })
}

fn resolve_configured_path(
    path: &Path,
    base: Option<&Path>,
    field: &'static str,
) -> Result<PathBuf, RpcError> {
    if path.is_absolute() {
        return Ok(path.to_owned());
    }
    let base = base.ok_or_else(|| {
        RpcError::new(INVALID_PARAMS, format!("relative {field} requires a workspace root"))
    })?;
    Ok(base.join(path))
}

fn resolve_directory(
    path: &Path,
    base: Option<&Path>,
    field: &'static str,
) -> Result<PathBuf, RpcError> {
    let path = resolve_path(path, base, field)?;
    if !path.is_dir() {
        return Err(RpcError::new(
            INVALID_PARAMS,
            format!("{field} is not a directory: {}", path.display()),
        ));
    }
    Ok(path)
}

fn resolve_path(
    path: &Path,
    base: Option<&Path>,
    field: &'static str,
) -> Result<PathBuf, RpcError> {
    let candidate = if path.is_absolute() {
        path.to_owned()
    } else {
        let base = base.ok_or_else(|| {
            RpcError::new(INVALID_PARAMS, format!("relative {field} requires a workspace root"))
        })?;
        base.join(path)
    };
    fs::canonicalize(&candidate).map_err(|error| {
        RpcError::new(
            INVALID_PARAMS,
            format!("cannot resolve {field} {}: {error}", candidate.display()),
        )
    })
}

fn stable_dependency_root_id(id: &str) -> u32 {
    let mut value = 0x811c9dc5_u32;
    for byte in id.bytes().map(|byte| byte.to_ascii_lowercase()) {
        value = (value ^ u32::from(byte)).wrapping_mul(0x0100_0193);
    }
    if matches!(value, 0 | u32::MAX) { value ^ 0x8000_0000 } else { value }
}

#[derive(Clone, Debug)]
struct SnapshotRequestContext {
    snapshot: AnalysisSnapshot,
    cancellation: CancellationToken,
}

impl SnapshotRequestContext {
    fn new(snapshot: AnalysisSnapshot, cancellation: CancellationToken) -> Self {
        Self { snapshot, cancellation }
    }

    fn dispatch(&self, method: &str, params: Option<&Value>) -> Result<Value, RpcError> {
        match method {
            "textDocument/completion" => self.completion(params),
            "textDocument/hover" => self.hover(params),
            "textDocument/definition" => self.definition(params),
            "textDocument/references" => self.references(params),
            "textDocument/prepareRename" => self.prepare_rename(params),
            "textDocument/rename" => self.rename(params),
            "textDocument/documentSymbol" => self.document_symbols(params),
            "textDocument/formatting" => self.formatting(params),
            "workspace/symbol" => self.workspace_symbols(params),
            _ => Err(RpcError::new(METHOD_NOT_FOUND, "method is not implemented")),
        }
    }

    fn completion(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let (id, position) = self.document_position(params)?;
        let result = complete_with_cancellation(&self.snapshot, &id, position, &self.cancellation)
            .map_err(cancelled_error)?;
        let document = self
            .snapshot
            .document(&id)
            .ok_or_else(|| RpcError::new(INVALID_PARAMS, "document is not open"))?;
        self.ensure_active()?;
        let (completion_items, is_incomplete) =
            bounded_results(result.items, MAX_COMPLETION_RESULTS);
        let items = completion_items
            .into_iter()
            .map(|item| {
                let insert_text = item.insert_text;
                CompletionItem {
                    label: item.label,
                    kind: Some(completion_kind(item.kind)),
                    detail: Some(item.detail),
                    documentation: item.documentation.map(Documentation::String),
                    deprecated: Some(item.deprecated),
                    sort_text: Some(format!("{:03}", item.sort_score)),
                    insert_text: Some(insert_text.clone()),
                    insert_text_format: Some(if insert_text.contains("$0") {
                        InsertTextFormat::SNIPPET
                    } else {
                        InsertTextFormat::PLAIN_TEXT
                    }),
                    text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                        range: range_to_lsp(
                            document.line_index(),
                            document.text(),
                            item.replacement_range,
                        ),
                        new_text: insert_text,
                    })),
                    ..CompletionItem::default()
                }
            })
            .collect::<Vec<_>>();
        self.ensure_active()?;
        typed_value(
            CompletionResponse::List(CompletionList { is_incomplete, items }),
            "completion response",
        )
    }

    fn hover(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let (id, position) = self.document_position(params)?;
        let Some(value) =
            hover_with_cancellation(&self.snapshot, &id, position, &self.cancellation)
                .map_err(cancelled_error)?
        else {
            return Ok(Value::Null);
        };
        let document = self
            .snapshot
            .document(&id)
            .ok_or_else(|| RpcError::new(INVALID_PARAMS, "document is not open"))?;
        self.ensure_active()?;
        typed_value(
            LspHover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: value.contents,
                }),
                range: value
                    .range
                    .map(|range| range_to_lsp(document.line_index(), document.text(), range)),
            },
            "hover response",
        )
    }

    fn definition(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let (id, position) = self.document_position(params)?;
        let result =
            definition_with_cancellation(&self.snapshot, &id, position, &self.cancellation)
                .map_err(cancelled_error)?
                .into_iter()
                .filter_map(|location| location_to_lsp(&self.snapshot, &location))
                .collect::<Vec<_>>();
        self.ensure_active()?;
        typed_value(result, "definition response")
    }

    fn references(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let params = typed_params::<ReferenceParams>(params, "references")?;
        let (id, position) = self.offset_for(&params.text_document_position)?;
        let include_declaration = params.context.include_declaration;
        let result = references_with_cancellation(
            &self.snapshot,
            &id,
            position,
            include_declaration,
            &self.cancellation,
        )
        .map_err(cancelled_error)?
        .into_iter()
        .filter_map(|location| location_to_lsp(&self.snapshot, &location))
        .collect::<Vec<_>>();
        self.ensure_active()?;
        typed_value(result, "references response")
    }

    fn prepare_rename(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let (id, position) = self.document_position(params)?;
        let result =
            prepare_rename_with_cancellation(&self.snapshot, &id, position, &self.cancellation)
                .map_err(rename_failure)?;
        let document = self
            .snapshot
            .document(&id)
            .ok_or_else(|| RpcError::new(INVALID_PARAMS, "document is not open"))?;
        self.ensure_active()?;
        typed_value(
            PrepareRenameResponse::RangeWithPlaceholder {
                range: range_to_lsp(document.line_index(), document.text(), result.range),
                placeholder: result.placeholder,
            },
            "prepare rename response",
        )
    }

    #[allow(clippy::mutable_key_type)] // lsp_types::Uri contains an internal parse cache.
    fn rename(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let params = typed_params::<RenameParams>(params, "rename")?;
        let (id, position) = self.offset_for(&params.text_document_position)?;
        let plan = rename_with_cancellation(
            &self.snapshot,
            &id,
            position,
            &params.new_name,
            &self.cancellation,
        )
        .map_err(rename_failure)?;
        let mut changes = HashMap::<Uri, Vec<TextEdit>>::new();
        for edit in plan.edits {
            self.ensure_active()?;
            let location = location_to_lsp(&self.snapshot, &edit.location).ok_or_else(|| {
                RpcError::new(INVALID_PARAMS, "rename target has no client-visible URI")
            })?;
            changes
                .entry(location.uri)
                .or_default()
                .push(TextEdit { range: location.range, new_text: edit.new_text });
        }
        typed_value(WorkspaceEdit::new(changes), "rename response")
    }

    #[allow(deprecated)]
    fn document_symbols(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let params = typed_params::<DocumentSymbolParams>(params, "document symbols")?;
        let id = DocumentId::new(params.text_document.uri.as_str());
        let result = document_symbols_with_cancellation(&self.snapshot, &id, &self.cancellation)
            .map_err(cancelled_error)?
            .into_iter()
            .map(|symbol| LspDocumentSymbol {
                name: symbol.name,
                detail: None,
                kind: symbol_kind(&symbol.kind),
                tags: None,
                deprecated: None,
                range: location_range_to_lsp(&self.snapshot, &symbol.location),
                selection_range: range_to_lsp_for_location(
                    &self.snapshot,
                    &symbol.location,
                    symbol.selection_range,
                ),
                children: None,
            })
            .collect::<Vec<_>>();
        self.ensure_active()?;
        typed_value(result, "document symbols response")
    }

    fn formatting(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let params = typed_params::<DocumentFormattingParams>(params, "document formatting")?;
        let id = DocumentId::new(params.text_document.uri.as_str());
        let document = self
            .snapshot
            .document(&id)
            .ok_or_else(|| RpcError::new(INVALID_PARAMS, "document is not open"))?;
        self.ensure_active()?;
        let Some(ParsedSource::Text(parsed)) = document.parsed() else {
            return typed_value(Vec::<TextEdit>::new(), "formatting response");
        };
        let _client_options = params.options;
        let result = format(parsed);
        self.ensure_active()?;
        let edits = result
            .edits
            .into_iter()
            .map(|edit| TextEdit {
                range: range_to_lsp(document.line_index(), document.text(), edit.range),
                new_text: edit.replacement,
            })
            .collect::<Vec<_>>();
        typed_value(edits, "formatting response")
    }

    #[allow(deprecated)]
    fn workspace_symbols(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let params = typed_params::<WorkspaceSymbolParams>(params, "workspace symbols")?;
        let result =
            workspace_symbols_with_cancellation(&self.snapshot, &params.query, &self.cancellation)
                .map_err(cancelled_error)?
                .into_iter()
                .filter_map(|symbol| {
                    Some(SymbolInformation {
                        name: symbol.name,
                        kind: symbol_kind(&symbol.kind),
                        tags: None,
                        deprecated: None,
                        location: location_to_lsp(&self.snapshot, &symbol.location)?,
                        container_name: None,
                    })
                })
                .take(MAX_WORKSPACE_SYMBOL_RESULTS)
                .collect::<Vec<_>>();
        self.ensure_active()?;
        typed_value(result, "workspace symbols response")
    }

    fn ensure_active(&self) -> Result<(), RpcError> {
        if self.cancellation.is_cancelled() { Err(cancelled_error(Cancelled)) } else { Ok(()) }
    }

    fn document_position(&self, params: Option<&Value>) -> Result<(DocumentId, u32), RpcError> {
        let params = typed_params::<TextDocumentPositionParams>(params, "document position")?;
        self.offset_for(&params)
    }

    fn offset_for(
        &self,
        params: &TextDocumentPositionParams,
    ) -> Result<(DocumentId, u32), RpcError> {
        let id = DocumentId::new(params.text_document.uri.as_str());
        let position = Position::new(params.position.line, params.position.character);
        let document = self
            .snapshot
            .document(&id)
            .ok_or_else(|| RpcError::new(INVALID_PARAMS, "document is not open"))?;
        document
            .line_index()
            .offset(document.text(), position)
            .ok_or_else(|| RpcError::new(INVALID_PARAMS, "position is not valid UTF-16"))
            .map(|offset| (id, offset))
    }
}

fn bounded_results<T>(mut values: Vec<T>, maximum: usize) -> (Vec<T>, bool) {
    let incomplete = values.len() > maximum;
    values.truncate(maximum);
    (values, incomplete)
}

fn diagnostic_result_counts(total: usize, maximum: usize) -> (usize, usize) {
    if total <= maximum {
        (total, 0)
    } else {
        let retained = maximum.saturating_sub(1);
        (retained, total - retained)
    }
}

fn is_snapshot_request(method: &str) -> bool {
    matches!(
        method,
        "textDocument/completion"
            | "textDocument/hover"
            | "textDocument/definition"
            | "textDocument/references"
            | "textDocument/prepareRename"
            | "textDocument/rename"
            | "textDocument/documentSymbol"
            | "textDocument/formatting"
            | "workspace/symbol"
    )
}

fn is_snapshot_request_message(message: &Value) -> bool {
    message
        .as_object()
        .and_then(|object| object.get("method"))
        .and_then(Value::as_str)
        .is_some_and(is_snapshot_request)
}

fn is_initialize_control_message(message: &Value) -> bool {
    message
        .as_object()
        .and_then(|object| object.get("method"))
        .and_then(Value::as_str)
        .is_some_and(|method| matches!(method, "$/cancelRequest" | "exit"))
}

fn cancel_request_from_notification(
    message: &Value,
    in_flight: &HashMap<RequestId, InFlightRequest>,
) {
    let Some(object) = message.as_object() else { return };
    if object.get("method").and_then(Value::as_str) != Some("$/cancelRequest") {
        return;
    }
    let Ok(params) = typed_params::<CancelParams>(object.get("params"), "cancel request") else {
        return;
    };
    let request_id = request_id_from_lsp(params.id);
    if let Some(request) = in_flight.get(&request_id) {
        request.cancellation.cancel();
    }
}

fn cancel_initialize_from_notification(message: &Value, in_flight: Option<&InFlightInitialize>) {
    let Some(in_flight) = in_flight else { return };
    let Some(object) = message.as_object() else { return };
    if object.get("method").and_then(Value::as_str) != Some("$/cancelRequest") {
        return;
    }
    let Ok(params) = typed_params::<CancelParams>(object.get("params"), "cancel request") else {
        return;
    };
    if request_id_from_lsp(params.id) == in_flight.request_id {
        in_flight.cancellation.cancel();
    }
}

fn diagnostic_values(
    snapshot: &AnalysisSnapshot,
    id: &DocumentId,
    cancellation: &CancellationToken,
) -> Option<Value> {
    let values = snapshot.document(id).map_or_else(Vec::new, |document| {
        let diagnostics =
            diagnostics_with_cancellation(snapshot, id, cancellation).ok().unwrap_or_default();
        let (retained, omitted) =
            diagnostic_result_counts(diagnostics.len(), MAX_PUBLISHED_DIAGNOSTICS);
        let mut values = diagnostics
            .into_iter()
            .take(retained)
            .map(|diagnostic| {
                LspDiagnostic::new(
                    range_to_lsp(document.line_index(), document.text(), diagnostic.range),
                    match diagnostic.severity {
                        1 => Some(DiagnosticSeverity::ERROR),
                        2 => Some(DiagnosticSeverity::WARNING),
                        3 => Some(DiagnosticSeverity::INFORMATION),
                        4 => Some(DiagnosticSeverity::HINT),
                        _ => None,
                    },
                    Some(NumberOrString::String(diagnostic.code.as_str().to_owned())),
                    Some("pdx-analysis".to_owned()),
                    diagnostic.message,
                    None,
                    None,
                )
            })
            .collect::<Vec<_>>();
        if omitted > 0 {
            values.push(LspDiagnostic::new(
                LspRange::default(),
                Some(DiagnosticSeverity::INFORMATION),
                Some(NumberOrString::String("pdx-diagnostics-truncated".to_owned())),
                Some("pdx-lsp".to_owned()),
                format!("{omitted} additional diagnostics were omitted"),
                None,
                None,
            ));
        }
        values
    });
    serde_json::to_value(values).ok()
}

fn diagnostics_notification(uri: &str, values: Value) -> Value {
    json!({
        "jsonrpc": JSON_RPC_VERSION,
        "method": "textDocument/publishDiagnostics",
        "params": {"uri": uri, "diagnostics": values},
    })
}

fn show_warning_notification(message: String) -> Value {
    let params = ShowMessageParams { typ: MessageType::WARNING, message };
    json!({
        "jsonrpc": JSON_RPC_VERSION,
        "method": "window/showMessage",
        "params": params,
    })
}

fn show_info_notification(message: String) -> Value {
    let params = ShowMessageParams { typ: MessageType::INFO, message };
    json!({
        "jsonrpc": JSON_RPC_VERSION,
        "method": "window/showMessage",
        "params": params,
    })
}

fn completion_kind(kind: CompletionKind) -> CompletionItemKind {
    match kind {
        CompletionKind::Key => CompletionItemKind::PROPERTY,
        CompletionKind::Value => CompletionItemKind::VALUE,
        CompletionKind::Symbol => CompletionItemKind::FUNCTION,
        CompletionKind::Localisation => CompletionItemKind::KEYWORD,
    }
}

fn symbol_kind(kind: &str) -> SymbolKind {
    match kind {
        "localisation" => SymbolKind::STRING,
        "event" => SymbolKind::FUNCTION,
        "scripted_effect" | "scripted_trigger" => SymbolKind::NAMESPACE,
        _ => SymbolKind::VARIABLE,
    }
}

fn range_to_lsp(index: &LineIndex, text: &str, range: TextRange) -> LspRange {
    let start = index.position(text, range.start()).unwrap_or_default();
    let end = index.position(text, range.end()).unwrap_or(start);
    LspRange::new(
        LspPosition::new(start.line, start.character),
        LspPosition::new(end.line, end.character),
    )
}

fn location_range_to_lsp(snapshot: &AnalysisSnapshot, location: &Location) -> LspRange {
    if let Some(document) = location.document.as_ref()
        && let Some(document) = snapshot.document(document)
    {
        return range_to_lsp(document.line_index(), document.text(), location.range);
    }
    if let Some(file) = location.file.and_then(|file| snapshot.source_text(file)) {
        let index = LineIndex::new(file);
        return range_to_lsp(&index, file, location.range);
    }
    LspRange::default()
}

fn range_to_lsp_for_location(
    snapshot: &AnalysisSnapshot,
    location: &Location,
    range: TextRange,
) -> LspRange {
    if let Some(document) = location.document.as_ref()
        && let Some(document) = snapshot.document(document)
    {
        return range_to_lsp(document.line_index(), document.text(), range);
    }
    if let Some(file) = location.file.and_then(|file| snapshot.source_text(file)) {
        let index = LineIndex::new(file);
        return range_to_lsp(&index, file, range);
    }
    LspRange::default()
}

fn location_to_lsp(snapshot: &AnalysisSnapshot, location: &Location) -> Option<LspLocation> {
    let uri = if let Some(document) = location.document.as_ref() {
        document.as_str().parse::<Uri>().ok()?
    } else if let Some(file) = location.file.and_then(|file| snapshot.source_files().get(&file)) {
        path_to_uri(&file.physical_path).parse::<Uri>().ok()?
    } else if let (Some(root), Some(path)) = (snapshot.workspace_root(), location.path.as_ref()) {
        path_to_uri(&root.join(path.as_str())).parse::<Uri>().ok()?
    } else {
        return None;
    };
    Some(LspLocation::new(uri, location_range_to_lsp(snapshot, location)))
}

fn document_error(error: DocumentError) -> RpcError {
    RpcError { code: INVALID_PARAMS, message: error.to_string() }
}

fn workspace_scan_error(error: WorkspaceError) -> RpcError {
    let code =
        if matches!(error, WorkspaceError::Cancelled) { REQUEST_CANCELLED } else { INVALID_PARAMS };
    RpcError { code, message: error.to_string() }
}

fn rename_error(error: RenameError) -> RpcError {
    RpcError { code: INVALID_PARAMS, message: format!("rename unavailable: {error}") }
}

fn cancelled_error(_: Cancelled) -> RpcError {
    RpcError::new(REQUEST_CANCELLED, "request was cancelled")
}

fn rename_failure(error: RenameFailure) -> RpcError {
    match error {
        RenameFailure::Cancelled => cancelled_error(Cancelled),
        RenameFailure::Rejected(error) => rename_error(error),
    }
}

fn typed_params<T: DeserializeOwned>(
    params: Option<&Value>,
    context: &'static str,
) -> Result<T, RpcError> {
    serde_json::from_value(params.cloned().unwrap_or(Value::Null)).map_err(|error| RpcError {
        code: INVALID_PARAMS,
        message: format!("invalid {context} params: {error}"),
    })
}

fn typed_value<T: Serialize>(value: T, context: &'static str) -> Result<Value, RpcError> {
    serde_json::to_value(value).map_err(|error| RpcError {
        code: INTERNAL_ERROR,
        message: format!("failed to serialize {context}: {error}"),
    })
}

fn request_id_from_lsp(id: NumberOrString) -> RequestId {
    match id {
        NumberOrString::Number(value) => RequestId::Number(i64::from(value)),
        NumberOrString::String(value) => RequestId::String(value),
    }
}

fn parse_file_uri_str(uri: &str) -> Result<PathBuf, RpcError> {
    uri_to_path(uri).map_err(|_| RpcError::new(INVALID_PARAMS, "only file:// URIs are supported"))
}

fn normalize_workspace_path(path: PathBuf) -> PathBuf {
    if let Ok(canonical) = fs::canonicalize(&path) {
        return canonical;
    }

    let mut ancestor = path.as_path();
    let mut missing = Vec::new();
    while let Some(name) = ancestor.file_name() {
        missing.push(name.to_owned());
        let Some(parent) = ancestor.parent() else { break };
        ancestor = parent;
        if let Ok(mut canonical) = fs::canonicalize(ancestor) {
            for component in missing.iter().rev() {
                canonical.push(component);
            }
            return canonical;
        }
    }
    path
}

fn lsp_range_to_text_range(
    range: &LspRange,
    index: &LineIndex,
    text: &str,
) -> Result<TextRange, RpcError> {
    let start = Position::new(range.start.line, range.start.character);
    let end = Position::new(range.end.line, range.end.character);
    let start = index.offset(text, start).ok_or_else(|| {
        RpcError::new(INVALID_PARAMS, "range start is not a valid UTF-16 position")
    })?;
    let end = index
        .offset(text, end)
        .ok_or_else(|| RpcError::new(INVALID_PARAMS, "range end is not a valid UTF-16 position"))?;
    TextRange::new(start, end)
        .ok_or_else(|| RpcError::new(INVALID_PARAMS, "range end precedes start"))
}

fn apply_text_change(
    text: &mut String,
    range: Option<TextRange>,
    replacement: &str,
) -> Result<(), RpcError> {
    if let Some(range) = range {
        let start = usize::try_from(range.start())
            .map_err(|_| RpcError::new(INVALID_PARAMS, "range is too large"))?;
        let end = usize::try_from(range.end())
            .map_err(|_| RpcError::new(INVALID_PARAMS, "range is too large"))?;
        if text.get(start..end).is_none() {
            return Err(RpcError::new(INVALID_PARAMS, "range is outside the document"));
        }
        text.replace_range(start..end, replacement);
    } else {
        text.clear();
        text.push_str(replacement);
    }
    Ok(())
}

fn changed_document_len(
    current_len: usize,
    range: Option<TextRange>,
    replacement_len: usize,
) -> Result<usize, RpcError> {
    let removed =
        range.map_or(current_len, |range| usize::try_from(range.len()).unwrap_or(usize::MAX));
    let next = current_len
        .checked_sub(removed)
        .and_then(|length| length.checked_add(replacement_len))
        .ok_or_else(|| RpcError::new(INVALID_PARAMS, "document change has an invalid size"))?;
    if next > MAX_DOCUMENT_BYTES {
        return Err(RpcError::new(
            INVALID_PARAMS,
            format!("document exceeds the {MAX_DOCUMENT_BYTES}-byte safety limit"),
        ));
    }
    Ok(next)
}

/// Converts a `file://` URI to a filesystem path.
pub fn uri_to_path(uri: &str) -> Result<PathBuf, UriError> {
    let rest = uri
        .strip_prefix("file://")
        .or_else(|| uri.strip_prefix("FILE://"))
        .ok_or(UriError::UnsupportedScheme)?;
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let (authority, encoded_path) = if rest.starts_with('/') {
        (None, rest.to_owned())
    } else if let Some((authority, path)) = rest.split_once('/') {
        (Some(authority), format!("/{path}"))
    } else {
        (Some(rest), "/".to_owned())
    };
    if authority.is_some_and(|value| !value.is_empty() && !value.eq_ignore_ascii_case("localhost"))
    {
        return Err(UriError::UnsupportedAuthority);
    }
    let decoded = percent_decode(&encoded_path)?;
    #[cfg(windows)]
    let decoded = decoded.strip_prefix('/').unwrap_or(&decoded).to_owned();
    Ok(PathBuf::from(decoded))
}

/// Converts an absolute filesystem path to a percent-encoded `file://` URI.
#[must_use]
pub fn path_to_uri(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let mut uri = String::from("file://");
    if !raw.starts_with('/') {
        uri.push('/');
    }
    for byte in raw.as_bytes() {
        if *byte == b'/' || *byte == b':' || is_uri_unreserved(*byte) {
            uri.push(char::from(*byte));
        } else {
            uri.push('%');
            uri.push(hex_digit(byte >> 4));
            uri.push(hex_digit(byte & 0x0f));
        }
    }
    uri
}

/// URI conversion failure.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum UriError {
    /// The URI is not a supported `file://` URI.
    UnsupportedScheme,
    /// A non-local authority was supplied.
    UnsupportedAuthority,
    /// A percent escape or UTF-8 sequence is invalid.
    InvalidEncoding,
}

impl fmt::Display for UriError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedScheme => "unsupported URI scheme",
            Self::UnsupportedAuthority => "unsupported URI authority",
            Self::InvalidEncoding => "invalid URI percent encoding",
        })
    }
}

impl std::error::Error for UriError {}

fn percent_decode(value: &str) -> Result<String, UriError> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(UriError::InvalidEncoding);
            }
            let high = hex_value(bytes[index + 1]).ok_or(UriError::InvalidEncoding)?;
            let low = hex_value(bytes[index + 2]).ok_or(UriError::InvalidEncoding)?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| UriError::InvalidEncoding)
}

fn is_uri_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn hex_digit(value: u8) -> char {
    match value {
        0..=9 => char::from(b'0' + value),
        _ => char::from(b'A' + value - 10),
    }
}

fn read_message<R: BufRead>(reader: &mut R) -> Result<Option<Value>, LspError> {
    let mut content_length = None;
    let mut saw_header = false;
    let mut header_bytes = 0_usize;
    loop {
        let remaining = MAX_LSP_HEADER_BYTES.saturating_sub(header_bytes);
        if remaining == 0 {
            return Err(LspError::Protocol("LSP headers exceed the safety limit".to_owned()));
        }
        let mut line = String::new();
        let bytes = (&mut *reader)
            .take(u64::try_from(remaining).unwrap_or(u64::MAX).saturating_add(1))
            .read_line(&mut line)?;
        header_bytes = header_bytes.saturating_add(bytes);
        if header_bytes > MAX_LSP_HEADER_BYTES {
            return Err(LspError::Protocol("LSP headers exceed the safety limit".to_owned()));
        }
        if bytes == 0 {
            if saw_header {
                return Err(LspError::Protocol("unexpected EOF in LSP headers".to_owned()));
            }
            return Ok(None);
        }
        saw_header = true;
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("Content-Length")
        {
            let parsed = value
                .trim()
                .parse::<usize>()
                .map_err(|_| LspError::Protocol("invalid Content-Length".to_owned()))?;
            if content_length.replace(parsed).is_some() {
                return Err(LspError::Protocol("duplicate Content-Length".to_owned()));
            }
        }
    }
    let content_length =
        content_length.ok_or_else(|| LspError::Protocol("missing Content-Length".to_owned()))?;
    if content_length > MAX_LSP_MESSAGE_BYTES {
        return Err(LspError::Protocol(format!(
            "LSP message exceeds the {MAX_LSP_MESSAGE_BYTES}-byte safety limit"
        )));
    }
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body).map_err(|error| {
        if error.is_data() || error.is_syntax() {
            LspError::Json(error)
        } else {
            LspError::Protocol(format!("invalid JSON-RPC body: {error}"))
        }
    })
}

fn write_message<W: Write>(writer: &mut W, message: &Value) -> Result<(), LspError> {
    let body = serde_json::to_vec(message)?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::fs;
    use std::io::{Cursor, Read};

    use super::{
        AutoVanillaConfiguration, CancellationToken, DocumentId, INVALID_PARAMS,
        InFlightInitialize, InFlightRequest, InitializeOptions, LspError, LspServer,
        MAX_DOCUMENT_BYTES, MAX_LSP_HEADER_BYTES, MAX_LSP_MESSAGE_BYTES, RequestId,
        ResolvedSourceRoots, ServerState, VanillaSetupCancellation,
        apply_user_vanilla_configuration, bounded_results, cancel_initialize_from_notification,
        cancel_request_from_notification, changed_document_len, diagnostic_result_counts,
        path_to_uri, read_message, run_auto_vanilla_setup_with_options, uri_to_path,
    };
    use lsp_types::{
        CompletionResponse, Diagnostic, DocumentSymbol, Hover, Location, PrepareRenameResponse,
        SymbolInformation, SymbolKind, WorkspaceEdit,
    };
    use pdx_game::{DiscoveryOptions, DiscoveryOutcome, UserConfiguration, UserPaths};
    use pdx_rules::{RuleSet, RulesError, RulesModel};
    use pdx_text::{LineIndex, Position, TextRange};
    use pdx_workspace::{
        AnalysisHost, SourceRoot, SourceRootId, SourceRootKind, TextChange, VanillaIndexCache,
        WorkspaceChange,
    };
    use serde::de::DeserializeOwned;
    use serde_json::{Value, json};

    /// Creates a temporary directory for use as a cross-platform workspace root.
    /// Returns the canonical path, its file:// URI, and a DocumentId URI rooted under it.
    fn temp_workspace_dir() -> (std::path::PathBuf, String) {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("pdx-lsp-test-{nonce}"));
        fs::create_dir_all(&dir).expect("create temp workspace");
        let canonical = fs::canonicalize(&dir).expect("canonicalize temp workspace");
        (canonical.clone(), path_to_uri(&canonical))
    }

    /// Canonicalizes a path and returns its file:// URI, matching the format used by
    /// workspace scanning so that URI-keyed maps can be compared directly.
    fn canonical_uri(path: &std::path::Path) -> String {
        let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        path_to_uri(&canonical)
    }

    #[test]
    fn workspace_root_is_scanned_as_current_mod_without_project_config() {
        let (root, root_uri) = temp_workspace_dir();
        fs::create_dir_all(root.join("common/country_tags")).expect("country tags directory");
        fs::create_dir_all(root.join("missions")).expect("missions directory");
        fs::write(root.join("common/country_tags/00_tags.txt"), "KTP = \"countries/KTP.txt\"\n")
            .expect("country tag source");
        let mission = root.join("missions/test_missions.txt");
        fs::write(&mission, "country_event = { id = test.1 }\n").expect("mission source");
        let mission_uri = canonical_uri(&mission);
        let input = frames([
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":root_uri,"capabilities":{}}}),
            json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
            json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":mission_uri,"languageId":"eu4","version":1,"text":"country_event = { id = test.1 }\n"}}}),
            json!({"jsonrpc":"2.0","id":2,"method":"shutdown","params":{}}),
            json!({"jsonrpc":"2.0","method":"exit"}),
        ]);
        let mut output = Vec::new();
        let mut server = eu4_server(InitializeOptions).expect("embedded rules");
        server.run_transport(Cursor::new(input), &mut output).expect("transport");
        assert!(server.snapshot().index().active_definition("country_tag", "KTP").is_some());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn noncanonical_document_uri_preserves_rule_path_context() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let container = std::env::temp_dir().join(format!("pdx-lsp-path-context-{nonce}"));
        let root = container.join("workspace");
        let decrees = root.join("common/decrees");
        fs::create_dir_all(&decrees).expect("decrees directory");
        fs::create_dir_all(container.join("detour")).expect("detour directory");
        let file = decrees.join("test.txt");
        fs::write(&file, "my_decree = { cost = 50 }\n").expect("decree source");

        let aliased_root = container.join("detour/../workspace");
        let aliased_file = aliased_root.join("common/decrees/test.txt");
        let root_uri = path_to_uri(&aliased_root);
        let uri = path_to_uri(&aliased_file);
        let input = frames([
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":root_uri,"capabilities":{}}}),
            json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
            json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":uri,"languageId":"eu4","version":1,"text":"my_decree = { cost = 50 }\n"}}}),
            json!({"jsonrpc":"2.0","id":2,"method":"textDocument/hover","params":{"textDocument":{"uri":uri},"position":{"line":0,"character":16}}}),
            json!({"jsonrpc":"2.0","id":3,"method":"shutdown","params":{}}),
            json!({"jsonrpc":"2.0","method":"exit"}),
        ]);
        let mut output = Vec::new();
        let mut server = eu4_server(InitializeOptions).expect("embedded rules");
        server.run_transport(Cursor::new(input), &mut output).expect("transport");
        let responses = decode_frames(&output);
        let hover = responses.iter().find(|value| value["id"] == 2).expect("hover response");
        assert!(
            hover["result"]["contents"]["value"]
                .as_str()
                .is_some_and(|contents| contents.contains("Cost in meritocracy of enacting")),
            "hover response={hover}"
        );
        fs::remove_dir_all(container).expect("cleanup");
    }

    fn eu4_server(options: InitializeOptions) -> Result<LspServer, LspError> {
        LspServer::try_new_with_rules(
            options,
            pdx_game::eu4::first_party_rules()?,
            pdx_game::eu4::profile(),
        )
    }

    fn frame(value: Value) -> Vec<u8> {
        let body = serde_json::to_vec(&value).expect("test JSON should serialize");
        let mut framed = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        framed.extend(body);
        framed
    }

    fn frames(values: impl IntoIterator<Item = Value>) -> Vec<u8> {
        values.into_iter().flat_map(frame).collect()
    }

    #[test]
    fn transport_framing_rejects_oversized_and_ambiguous_headers() {
        let oversized =
            format!("Content-Length: {}\r\n\r\n", MAX_LSP_MESSAGE_BYTES.saturating_add(1));
        assert!(matches!(
            read_message(&mut Cursor::new(oversized)),
            Err(LspError::Protocol(message)) if message.contains("safety limit")
        ));

        let duplicate = b"Content-Length: 2\r\nContent-Length: 2\r\n\r\n{}";
        assert!(matches!(
            read_message(&mut Cursor::new(duplicate)),
            Err(LspError::Protocol(message)) if message.contains("duplicate")
        ));

        let oversized_header = format!("X-Test: {}\r\n\r\n", "x".repeat(MAX_LSP_HEADER_BYTES));
        assert!(matches!(
            read_message(&mut Cursor::new(oversized_header)),
            Err(LspError::Protocol(message)) if message.contains("headers")
        ));
    }

    #[test]
    fn document_changes_are_bounded_before_allocation() {
        assert_eq!(
            changed_document_len(0, None, MAX_DOCUMENT_BYTES).expect("boundary document"),
            MAX_DOCUMENT_BYTES
        );
        assert!(changed_document_len(0, None, MAX_DOCUMENT_BYTES + 1).is_err());
        assert!(
            changed_document_len(
                MAX_DOCUMENT_BYTES,
                Some(TextRange::new(0, 1).expect("range")),
                2,
            )
            .is_err()
        );
    }

    #[test]
    fn ranked_result_limits_report_completion_truncation() {
        let (values, incomplete) = bounded_results(vec![0, 1, 2, 3], 3);
        assert_eq!(values, [0, 1, 2]);
        assert!(incomplete);
        let (values, incomplete) = bounded_results(vec![0, 1, 2], 3);
        assert_eq!(values, [0, 1, 2]);
        assert!(!incomplete);
        assert_eq!(diagnostic_result_counts(3, 3), (3, 0));
        assert_eq!(diagnostic_result_counts(4, 3), (2, 2));
    }

    type ReadAction = Option<Box<dyn FnOnce() + Send>>;

    struct ScriptedReader {
        steps: VecDeque<(Vec<u8>, ReadAction)>,
        current: Cursor<Vec<u8>>,
    }

    impl ScriptedReader {
        fn new(steps: impl IntoIterator<Item = (Value, ReadAction)>) -> Self {
            Self {
                steps: steps.into_iter().map(|(value, action)| (frame(value), action)).collect(),
                current: Cursor::new(Vec::new()),
            }
        }
    }

    impl Read for ScriptedReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if usize::try_from(self.current.position()).unwrap_or(usize::MAX)
                >= self.current.get_ref().len()
            {
                let Some((bytes, action)) = self.steps.pop_front() else {
                    return Ok(0);
                };
                if let Some(action) = action {
                    action();
                }
                self.current = Cursor::new(bytes);
            }
            self.current.read(buffer)
        }
    }

    fn decode_frames(bytes: &[u8]) -> Vec<Value> {
        let mut cursor = Cursor::new(bytes);
        let mut decoded = Vec::new();
        while let Some(value) = super::read_message(&mut cursor).expect("test frame is valid") {
            decoded.push(value);
        }
        decoded
    }

    #[test]
    fn watched_file_registration_and_notification_update_the_disk_index() {
        let (root, root_uri) = temp_workspace_dir();
        let events = root.join("common/events");
        fs::create_dir_all(&events).expect("events directory");
        let definition = events.join("watched.txt");
        fs::write(&definition, "country_event = { id = old.1 }\n").expect("initial definition");
        let definition_uri = canonical_uri(&definition);
        let changed_definition = definition.clone();
        let input = ScriptedReader::new([
            (
                json!({
                    "jsonrpc":"2.0",
                    "id":1,
                    "method":"initialize",
                    "params":{
                        "rootUri":root_uri,
                        "capabilities":{
                            "workspace":{
                                "didChangeWatchedFiles":{
                                    "dynamicRegistration":true,
                                    "relativePatternSupport":true
                                }
                            }
                        }
                    }
                }),
                None,
            ),
            (json!({"jsonrpc":"2.0","method":"initialized","params":{}}), None),
            (
                json!({
                    "jsonrpc":"2.0",
                    "method":"workspace/didChangeWatchedFiles",
                    "params":{"changes":[{"uri":definition_uri,"type":2}]}
                }),
                Some(Box::new(move || {
                    fs::write(changed_definition, "country_event = { id = watched-new.1 }\n")
                        .expect("write watched definition");
                }) as Box<dyn FnOnce() + Send>),
            ),
            (
                json!({
                    "jsonrpc":"2.0",
                    "id":2,
                    "method":"workspace/symbol",
                    "params":{"query":"watched-new"}
                }),
                None,
            ),
            (json!({"jsonrpc":"2.0","id":3,"method":"shutdown","params":{}}), None),
            (json!({"jsonrpc":"2.0","method":"exit"}), None),
        ]);
        let mut output = Vec::new();
        let mut server = eu4_server(InitializeOptions).expect("embedded rules");
        server.run_transport(input, &mut output).expect("transport");
        let responses = decode_frames(&output);

        let registration = responses
            .iter()
            .find(|value| value["method"] == "client/registerCapability")
            .expect("watched-file dynamic registration");
        let watcher = &registration["params"]["registrations"][0]["registerOptions"]["watchers"][0];
        assert_eq!(watcher["globPattern"]["baseUri"], root_uri);
        assert_eq!(watcher["globPattern"]["pattern"], "**/*");
        assert_eq!(watcher["kind"], 7);

        let symbols = typed_result::<Vec<SymbolInformation>>(&responses, 2);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "watched-new.1");
        assert!(server.snapshot().index().active_definition("event", "old.1").is_none());
        assert!(server.snapshot().index().active_definition("event", "watched-new.1").is_some());
        fs::remove_dir_all(root).expect("cleanup");
    }

    fn typed_result<T: DeserializeOwned>(responses: &[Value], id: i64) -> T {
        let value = responses
            .iter()
            .find(|value| value["id"] == id)
            .unwrap_or_else(|| panic!("missing response {id}"));
        serde_json::from_value(value["result"].clone())
            .unwrap_or_else(|error| panic!("response {id} is not valid LSP: {error}"))
    }

    #[test]
    fn uri_round_trip_preserves_unicode_and_spaces() {
        let path = std::env::temp_dir().join("Paradox Code").join("汉.txt");
        let uri = path_to_uri(&path);
        assert!(uri.contains("%20"));
        assert_eq!(uri_to_path(&uri).expect("URI should decode"), path);
    }

    #[test]
    fn selected_game_rejects_a_mismatched_rules_artifact() {
        let rules = RuleSet::from_model(RulesModel {
            game_id: "another-game".to_owned(),
            ..RulesModel::default()
        });

        let error =
            LspServer::try_new_with_rules(InitializeOptions, rules, pdx_game::eu4::profile())
                .expect_err("mismatched game must be rejected");
        assert!(matches!(
            error,
            LspError::Rules(RulesError::GameMismatch { expected, actual })
                if expected == "eu4" && actual == "another-game"
        ));
    }

    #[test]
    fn memory_transport_runs_real_json_rpc_lifecycle_and_sync() {
        let path = std::env::temp_dir().join(format!("pdx-lsp-{}.txt", std::process::id()));
        fs::write(&path, "disk").expect("write disk fixture");
        let uri = path_to_uri(&path);
        let input = frames([
            json!({"jsonrpc":"2.0","id":1,"method":"shutdown","params":{}}),
            json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"rootUri":uri,"capabilities":{}}}),
            json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
            json!({
                "jsonrpc":"2.0",
                "method":"textDocument/didOpen",
                "params":{"textDocument":{"uri":uri,"languageId":"eu4","version":1,"text":"a\r\n汉😀e\u{301}\r\n"}}
            }),
            json!({
                "jsonrpc":"2.0",
                "method":"textDocument/didChange",
                "params":{"textDocument":{"uri":uri,"version":2},"contentChanges":[{"range":{"start":{"line":1,"character":1},"end":{"line":1,"character":3}},"text":"猫"}]}
            }),
            json!({
                "jsonrpc":"2.0",
                "method":"textDocument/didChange",
                "params":{"textDocument":{"uri":uri,"version":1},"contentChanges":[{"text":"stale"}]}
            }),
            json!({"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":99}}),
            json!({"jsonrpc":"2.0","id":99,"method":"textDocument/hover","params":{}}),
            json!({
                "jsonrpc":"2.0",
                "method":"textDocument/didChange",
                "params":{"textDocument":{"uri":uri,"version":3},"contentChanges":[{"text":"current"}]}
            }),
            json!({"jsonrpc":"2.0","method":"textDocument/didClose","params":{"textDocument":{"uri":uri}}}),
            json!({"jsonrpc":"2.0","id":4,"method":"shutdown","params":{}}),
            json!({"jsonrpc":"2.0","method":"exit"}),
        ]);
        let mut output = Vec::new();
        let mut server =
            eu4_server(InitializeOptions).expect("syntax-only server should initialize");
        server.run_transport(Cursor::new(input), &mut output).expect("transport should finish");

        let responses = decode_frames(&output);
        let before_initialize =
            responses.iter().find(|value| value["id"] == 1).expect("pre-init response");
        assert_eq!(before_initialize["error"]["code"], -32002);
        let initialize =
            responses.iter().find(|value| value["id"] == 2).expect("initialize response");
        assert_eq!(initialize["result"]["capabilities"]["textDocumentSync"]["change"], 2);
        assert_eq!(initialize["result"]["capabilities"]["renameProvider"]["prepareProvider"], true);
        assert_eq!(initialize["result"]["capabilities"]["documentFormattingProvider"], true);
        let cancelled =
            responses.iter().find(|value| value["id"] == 99).expect("cancelled response");
        assert_eq!(cancelled["error"]["code"], -32800);
        let shutdown = responses.iter().find(|value| value["id"] == 4).expect("shutdown response");
        assert_eq!(shutdown["result"], Value::Null);
        assert!(responses.iter().any(|value| value["method"] == "textDocument/publishDiagnostics"));
        let snapshot = server.snapshot();
        let document = snapshot
            .document(&pdx_workspace::DocumentId::new(uri.clone()))
            .expect("close restores disk candidate");
        assert_eq!(document.text(), "disk");
        assert_eq!(document.version(), None);
        assert_eq!(server.state(), ServerState::Exited);
        fs::remove_file(path).expect("remove disk fixture");
    }

    #[test]
    fn typed_protocol_rejects_malformed_params_without_corrupting_lifecycle() {
        let input = frames([
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":"file:///tmp"}}),
            json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"rootUri":"file:///tmp","capabilities":{}}}),
            json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
            json!({"jsonrpc":"2.0","id":3,"method":"textDocument/hover","params":{}}),
            json!({"jsonrpc":"2.0","id":4,"method":"shutdown","params":{}}),
            json!({"jsonrpc":"2.0","method":"exit"}),
        ]);
        let mut output = Vec::new();
        let mut server = eu4_server(InitializeOptions).expect("server");

        server.run_transport(Cursor::new(input), &mut output).expect("transport");

        let responses = decode_frames(&output);
        let malformed_initialize =
            responses.iter().find(|value| value["id"] == 1).expect("invalid initialize");
        assert_eq!(malformed_initialize["error"]["code"], INVALID_PARAMS);
        assert!(
            malformed_initialize["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("invalid initialize params"))
        );
        assert!(
            responses
                .iter()
                .find(|value| value["id"] == 2)
                .is_some_and(|value| value["result"]["capabilities"].is_object())
        );
        let malformed_hover =
            responses.iter().find(|value| value["id"] == 3).expect("invalid hover");
        assert_eq!(malformed_hover["error"]["code"], INVALID_PARAMS);
        assert_eq!(server.state(), ServerState::Exited);
    }

    #[test]
    fn memory_transport_delegates_phase5_requests_to_analysis() {
        let (root_dir, root_uri) = temp_workspace_dir();
        let events_dir = root_dir.join("events");
        fs::create_dir_all(&events_dir).expect("create events dir");
        let file_path = events_dir.join("phase5.txt");
        fs::write(&file_path, "").expect("create placeholder file");
        let uri = canonical_uri(&file_path);
        let text = "country_event = { id = test.1 }\nevent = test.1\nscope = nowhere\n";
        let input = frames([
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":root_uri,"capabilities":{}}}),
            json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
            json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":uri,"languageId":"eu4","version":1,"text":text}}}),
            json!({"jsonrpc":"2.0","id":2,"method":"textDocument/completion","params":{"textDocument":{"uri":uri},"position":{"line":2,"character":8}}}),
            json!({"jsonrpc":"2.0","id":3,"method":"textDocument/hover","params":{"textDocument":{"uri":uri},"position":{"line":1,"character":8}}}),
            json!({"jsonrpc":"2.0","id":4,"method":"textDocument/definition","params":{"textDocument":{"uri":uri},"position":{"line":1,"character":8}}}),
            json!({"jsonrpc":"2.0","id":5,"method":"textDocument/references","params":{"textDocument":{"uri":uri},"position":{"line":1,"character":8},"context":{"includeDeclaration":true}}}),
            json!({"jsonrpc":"2.0","id":6,"method":"textDocument/documentSymbol","params":{"textDocument":{"uri":uri}}}),
            json!({"jsonrpc":"2.0","id":7,"method":"workspace/symbol","params":{"query":"test"}}),
            json!({"jsonrpc":"2.0","id":9,"method":"textDocument/prepareRename","params":{"textDocument":{"uri":uri},"position":{"line":1,"character":8}}}),
            json!({"jsonrpc":"2.0","id":10,"method":"textDocument/rename","params":{"textDocument":{"uri":uri},"position":{"line":1,"character":8},"newName":"renamed.1"}}),
            json!({"jsonrpc":"2.0","id":8,"method":"shutdown","params":{}}),
            json!({"jsonrpc":"2.0","method":"exit"}),
        ]);
        let mut output = Vec::new();
        let mut server =
            eu4_server(InitializeOptions).expect("syntax-only server should initialize");
        server.run_transport(Cursor::new(input), &mut output).expect("transport should finish");
        let responses = decode_frames(&output);
        let completion =
            responses.iter().find(|value| value["id"] == 2).expect("completion response");
        assert!(completion["result"]["items"].as_array().is_some_and(|items| !items.is_empty()));
        assert!(
            responses
                .iter()
                .find(|value| value["id"] == 3)
                .is_some_and(|value| value["result"]["contents"].is_object())
        );
        assert_eq!(
            responses.iter().find(|value| value["id"] == 4).expect("definition")["result"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(
            responses.iter().find(|value| value["id"] == 5).expect("references")["result"]
                .as_array()
                .map(Vec::len),
            Some(2)
        );
        // With embedded EU4 rules, top-level keys (country_event, event, scope) all
        // produce document symbols — richer than the identity-only baseline.
        assert!(
            responses.iter().find(|value| value["id"] == 6).expect("document symbols")["result"]
                .as_array()
                .is_some_and(|symbols| !symbols.is_empty())
        );
        assert_eq!(
            responses.iter().find(|value| value["id"] == 7).expect("workspace symbols")["result"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
        let prepare = responses.iter().find(|value| value["id"] == 9).expect("prepare rename");
        assert_eq!(prepare["result"]["placeholder"], "test.1");
        let rename = responses.iter().find(|value| value["id"] == 10).expect("rename");
        assert_eq!(rename["result"]["changes"][uri.clone()].as_array().map(Vec::len), Some(2));
        assert!(
            rename["result"]["changes"][uri]
                .as_array()
                .is_some_and(|edits| { edits.iter().all(|edit| edit["newText"] == "renamed.1") })
        );
        let diagnostics = responses
            .iter()
            .find(|value| value["method"] == "textDocument/publishDiagnostics")
            .expect("diagnostic notification");
        assert!(
            diagnostics["params"]["diagnostics"].as_array().is_some_and(|items| {
                items.iter().any(|item| item["code"] == "pdx-unknown-scope")
            })
        );

        let _: CompletionResponse = typed_result(&responses, 2);
        let _: Hover = typed_result(&responses, 3);
        let _: Vec<Location> = typed_result(&responses, 4);
        let _: Vec<Location> = typed_result(&responses, 5);
        let _: Vec<DocumentSymbol> = typed_result(&responses, 6);
        let _: Vec<SymbolInformation> = typed_result(&responses, 7);
        let _: PrepareRenameResponse = typed_result(&responses, 9);
        let _: WorkspaceEdit = typed_result(&responses, 10);
        let _: Vec<Diagnostic> =
            serde_json::from_value(diagnostics["params"]["diagnostics"].clone())
                .expect("diagnostic notification should use the standard LSP shape");
        fs::remove_dir_all(root_dir).expect("cleanup");
    }

    #[test]
    fn memory_transport_preserves_hir_disambiguated_mixed_context_completion() {
        let (root_dir, root_uri) = temp_workspace_dir();
        let events_dir = root_dir.join("events");
        fs::create_dir_all(&events_dir).expect("create events dir");
        let file_path = events_dir.join("mixed-completion.txt");
        fs::write(&file_path, "").expect("create placeholder file");
        let uri = canonical_uri(&file_path);
        let text = concat!(
            "country_event = {\n",
            "  mean_time_to_happen = {\n",
            "    modifier = {\n",
            "      factor = 0.5\n",
            "      \n",
            "      always = maybe\n",
            "    }\n",
            "  }\n",
            "}\n",
        );
        let input = frames([
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":root_uri,"capabilities":{}}}),
            json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
            json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":uri,"languageId":"eu4","version":1,"text":text}}}),
            json!({"jsonrpc":"2.0","id":2,"method":"textDocument/completion","params":{"textDocument":{"uri":uri},"position":{"line":4,"character":6}}}),
            json!({"jsonrpc":"2.0","id":3,"method":"shutdown","params":{}}),
            json!({"jsonrpc":"2.0","method":"exit"}),
        ]);
        let mut output = Vec::new();
        let mut server = eu4_server(InitializeOptions).expect("embedded rules");
        server.run_transport(Cursor::new(input), &mut output).expect("transport");
        let responses = decode_frames(&output);
        let completion =
            responses.iter().find(|value| value["id"] == 2).expect("completion response");
        let labels = completion["result"]["items"]
            .as_array()
            .expect("completion items")
            .iter()
            .filter_map(|item| item["label"].as_str())
            .collect::<Vec<_>>();
        assert!(labels.contains(&"factor"), "missing structural completion: {labels:?}");
        assert!(labels.contains(&"always"), "missing trigger completion: {labels:?}");

        let diagnostics = responses
            .iter()
            .find(|value| value["method"] == "textDocument/publishDiagnostics")
            .expect("diagnostic notification");
        let diagnostics = diagnostics["params"]["diagnostics"].as_array().expect("diagnostics");
        assert!(
            diagnostics.iter().all(|item| item["code"] != "pdx-unknown-key"),
            "known mixed-context keys were rejected: {diagnostics:?}"
        );
        assert!(
            diagnostics.iter().any(|item| item["code"] == "pdx-invalid-value"),
            "invalid trigger value was not diagnosed: {diagnostics:?}"
        );
        fs::remove_dir_all(root_dir).expect("cleanup");
    }

    #[test]
    fn memory_transport_exposes_parameters_as_document_local_symbols() {
        let (root_dir, root_uri) = temp_workspace_dir();
        let effects_dir = root_dir.join("common/scripted_effects");
        fs::create_dir_all(&effects_dir).expect("create scripted effects directory");
        let file_path = effects_dir.join("parameters.txt");
        fs::write(&file_path, "").expect("create placeholder file");
        let uri = canonical_uri(&file_path);
        let text = "apply = { value = $Amount$ again = $amount$ [[optional] enabled = yes ] }\n";
        let input = frames([
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":root_uri,"capabilities":{}}}),
            json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
            json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":uri,"languageId":"eu4","version":1,"text":text}}}),
            json!({"jsonrpc":"2.0","id":2,"method":"textDocument/documentSymbol","params":{"textDocument":{"uri":uri}}}),
            json!({"jsonrpc":"2.0","id":3,"method":"workspace/symbol","params":{"query":"amount"}}),
            json!({"jsonrpc":"2.0","id":4,"method":"shutdown","params":{}}),
            json!({"jsonrpc":"2.0","method":"exit"}),
        ]);
        let mut output = Vec::new();
        let mut server = eu4_server(InitializeOptions).expect("embedded rules");
        server.run_transport(Cursor::new(input), &mut output).expect("transport");
        let responses = decode_frames(&output);

        let symbols: Vec<DocumentSymbol> = typed_result(&responses, 2);
        let amount = symbols
            .iter()
            .find(|symbol| symbol.name == "Amount")
            .expect("inferred parameter document symbol");
        assert_eq!(amount.kind, SymbolKind::VARIABLE);
        assert_eq!(
            amount.selection_range.end.character - amount.selection_range.start.character,
            u32::try_from("Amount".len()).expect("name length")
        );
        assert!(amount.range.start.character < amount.selection_range.start.character);
        assert!(amount.selection_range.end.character < amount.range.end.character);

        let workspace: Vec<SymbolInformation> = typed_result(&responses, 3);
        assert!(workspace.iter().all(|symbol| !symbol.name.eq_ignore_ascii_case("amount")));
        fs::remove_dir_all(root_dir).expect("cleanup");
    }

    #[test]
    fn memory_transport_formats_safe_text_and_refuses_recovered_syntax() {
        let valid_uri = "file:///tmp/format-valid.txt";
        let unsafe_uri = "file:///tmp/format-unsafe.txt";
        let input = frames([
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":"file:///tmp","capabilities":{}}}),
            json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
            json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":valid_uri,"languageId":"eu4","version":1,"text":"root={name=\"汉😀\" other=yes}"}}}),
            json!({"jsonrpc":"2.0","id":2,"method":"textDocument/formatting","params":{"textDocument":{"uri":valid_uri},"options":{"tabSize":2,"insertSpaces":true}}}),
            json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":unsafe_uri,"languageId":"eu4","version":1,"text":"country_event = {"}}}),
            json!({"jsonrpc":"2.0","id":3,"method":"textDocument/formatting","params":{"textDocument":{"uri":unsafe_uri},"options":{"tabSize":4,"insertSpaces":true}}}),
            json!({"jsonrpc":"2.0","id":4,"method":"shutdown","params":{}}),
            json!({"jsonrpc":"2.0","method":"exit"}),
        ]);
        let mut output = Vec::new();
        let mut server = eu4_server(InitializeOptions).expect("server");
        server.run_transport(Cursor::new(input), &mut output).expect("transport");
        let responses = decode_frames(&output);

        let edits = typed_result::<Vec<lsp_types::TextEdit>>(&responses, 2);
        let source = "root={name=\"汉😀\" other=yes}";
        let line_index = LineIndex::new(source);
        let mut formatted = source.to_owned();
        for edit in edits.iter().rev() {
            let start = line_index
                .offset(source, Position::new(edit.range.start.line, edit.range.start.character))
                .expect("format edit start");
            let end = line_index
                .offset(source, Position::new(edit.range.end.line, edit.range.end.character))
                .expect("format edit end");
            formatted.replace_range(start as usize..end as usize, &edit.new_text);
        }
        assert_eq!(formatted, "root = {\n\tname = \"汉😀\"\n\tother = yes\n}\n");
        let unsafe_edits = typed_result::<Vec<lsp_types::TextEdit>>(&responses, 3);
        assert!(unsafe_edits.is_empty());
    }

    #[test]
    fn memory_transport_rename_covers_current_mod_disk_references() {
        let nonce = std::process::id();
        let root = std::env::temp_dir().join(format!("pdx-lsp-rename-{nonce}"));
        let target_path = root.join("common/events/target.txt");
        let references_path = root.join("common/events/references.txt");
        fs::create_dir_all(target_path.parent().expect("target parent")).expect("directories");
        fs::write(&target_path, "country_event = { id = cross.1 }\n").expect("target");
        fs::write(&references_path, "event = cross.1\n").expect("reference");
        let target_uri = canonical_uri(&target_path);
        let references_uri = canonical_uri(&references_path);
        let root_uri = canonical_uri(&fs::canonicalize(&root).expect("canonical root"));
        let input = frames([
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":root_uri,"capabilities":{}}}),
            json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
            json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":target_uri,"languageId":"eu4","version":1,"text":"country_event = { id = cross.1 }\n"}}}),
            json!({"jsonrpc":"2.0","id":2,"method":"textDocument/rename","params":{"textDocument":{"uri":target_uri},"position":{"line":0,"character":25},"newName":"renamed.1"}}),
            json!({"jsonrpc":"2.0","id":3,"method":"shutdown","params":{}}),
            json!({"jsonrpc":"2.0","method":"exit"}),
        ]);
        let mut output = Vec::new();
        let mut server = eu4_server(InitializeOptions).expect("bundled rules should load");
        server.run_transport(Cursor::new(input), &mut output).expect("transport should finish");
        let responses = decode_frames(&output);
        let rename = responses.iter().find(|value| value["id"] == 2).expect("rename response");
        assert!(rename["error"].is_null(), "rename response={rename}");
        let changes = rename["result"]["changes"].as_object().expect("workspace changes");
        assert_eq!(changes.get(&target_uri).and_then(Value::as_array).map(Vec::len), Some(1));
        assert_eq!(changes.get(&references_uri).and_then(Value::as_array).map(Vec::len), Some(1));
        assert!(changes.values().all(|edits| {
            edits
                .as_array()
                .is_some_and(|edits| edits.iter().all(|edit| edit["newText"] == "renamed.1"))
        }));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn project_config_loads_ordered_dependencies_and_keeps_them_read_only() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-lsp-project-config-{nonce}"));
        let config_dir = root.join(".pdx");
        let current = root.join("mod");
        let low = root.join("dependencies/low");
        let high = root.join("dependencies/high");
        let vanilla = root.join("vanilla");
        fs::create_dir_all(&config_dir).expect("config directory");
        for directory in [&current, &low, &high, &vanilla] {
            fs::create_dir_all(directory.join("common/events")).expect("fixture directory");
        }
        let canonical_root = fs::canonicalize(&root).expect("canonical root");
        let current = fs::canonicalize(&current).expect("canonical current");
        let low = fs::canonicalize(&low).expect("canonical low");
        let high = fs::canonicalize(&high).expect("canonical high");
        let vanilla = fs::canonicalize(&vanilla).expect("canonical vanilla");
        let inline = super::resolve_source_roots(
            Some(&canonical_root),
            Some(json!({
                "modDirectory": "mod",
                "dependencies": [
                    {"id": "low", "path": "dependencies/low"},
                    {"id": "high", "path": "dependencies/high"}
                ]
            })),
            &pdx_workspace::WorkspaceScanToken::new(),
        )
        .expect("inline initializationOptions");
        assert_eq!(inline.roots.len(), 3);
        let overlap = super::resolve_source_roots(
            Some(&canonical_root),
            Some(json!({
                "modDirectory": "mod",
                "dependencies": [{"id": "nested", "path": "mod/common"}]
            })),
            &pdx_workspace::WorkspaceScanToken::new(),
        )
        .expect_err("nested source roots must be rejected");
        assert_eq!(overlap.code, INVALID_PARAMS);
        assert!(overlap.message.contains("must not overlap"));
        fs::write(
            vanilla.join("common/events/definitions.txt"),
            "country_event = { id = vanilla.1 }\n",
        )
        .expect("Vanilla definition");
        let mut vanilla_host = AnalysisHost::with_profile(
            pdx_game::eu4::first_party_rules().expect("rules for Vanilla cache"),
            pdx_game::eu4::profile(),
        );
        vanilla_host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
            SourceRootId::new(0),
            SourceRootKind::Vanilla,
            fs::canonicalize(&vanilla).expect("canonical Vanilla root"),
        )]));
        vanilla_host.refresh_source_roots().expect("scan Vanilla once");
        let vanilla_cache = VanillaIndexCache::from_snapshot(&vanilla_host.snapshot())
            .expect("build Vanilla cache");
        let vanilla_cache_path = config_dir.join("vanilla.pdxindex");
        vanilla_cache.save(&vanilla_cache_path).expect("save Vanilla cache");
        fs::rename(&vanilla, root.join("vanilla-moved"))
            .expect("make Vanilla source unavailable after caching");
        fs::write(
            config_dir.join("project.toml"),
            r#"mod_directory = "mod"
vanilla_index_cache = ".pdx/vanilla.pdxindex"

[[dependencies]]
id = "low"
path = "dependencies/low"

[[dependencies]]
id = "high"
path = "dependencies/high"
"#,
        )
        .expect("project config");
        fs::write(
            low.join("common/events/definitions.txt"),
            concat!(
                "country_event = { id = shared.1 }\n",
                "country_event = { id = dependency-shared.1 }\n",
                "country_event = { id = dependency.1 }\n"
            ),
        )
        .expect("low dependency");
        fs::write(
            high.join("common/events/definitions.txt"),
            "country_event = { id = shared.1 }\ncountry_event = { id = dependency-shared.1 }\n",
        )
        .expect("high dependency");
        fs::write(
            current.join("common/events/definitions.txt"),
            "country_event = { id = shared.1 }\n",
        )
        .expect("current mod");
        let reference_path = current.join("common/events/reference.txt");
        fs::write(&reference_path, "event = dependency.1\n").expect("current reference");

        let reference_uri = canonical_uri(&reference_path);
        let root_uri = canonical_uri(&canonical_root);
        let input = frames([
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":root_uri,"capabilities":{},"initializationOptions":{"projectConfig":".pdx/project.toml"}}}),
            json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
            json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":reference_uri,"languageId":"eu4","version":1,"text":"event = dependency.1\n"}}}),
            json!({"jsonrpc":"2.0","id":2,"method":"textDocument/rename","params":{"textDocument":{"uri":reference_uri},"position":{"line":0,"character":10},"newName":"renamed.1"}}),
            json!({"jsonrpc":"2.0","id":3,"method":"shutdown","params":{}}),
            json!({"jsonrpc":"2.0","method":"exit"}),
        ]);
        let mut output = Vec::new();
        let mut server = eu4_server(InitializeOptions).expect("embedded rules");
        server.run_transport(Cursor::new(input), &mut output).expect("transport");
        let responses = decode_frames(&output);

        let snapshot = server.snapshot();
        let roots = snapshot.source_roots();
        assert_eq!(roots.len(), 4);
        assert_eq!(roots[0].kind, pdx_workspace::SourceRootKind::Vanilla);
        assert_eq!(roots[1].kind, pdx_workspace::SourceRootKind::Dependency);
        assert_eq!(roots[1].order, 0);
        assert_eq!(roots[2].kind, pdx_workspace::SourceRootKind::Dependency);
        assert_eq!(roots[2].order, 1);
        assert_eq!(roots[3].kind, pdx_workspace::SourceRootKind::CurrentMod);
        assert!(roots[3].writable);
        let active = snapshot
            .index()
            .active_definition("event", "shared.1")
            .expect("current definition should win");
        assert!(
            snapshot
                .source_files()
                .get(&active.file_id)
                .is_some_and(|file| file.physical_path.starts_with(&current))
        );
        let active_dependency = snapshot
            .index()
            .active_definition("event", "dependency-shared.1")
            .expect("higher ordered dependency should win");
        assert!(
            snapshot
                .source_files()
                .get(&active_dependency.file_id)
                .is_some_and(|file| file.physical_path.starts_with(&high))
        );
        let vanilla_definition = snapshot
            .index()
            .active_definition("event", "vanilla.1")
            .expect("cached Vanilla definition");
        assert_eq!(
            snapshot
                .source_files()
                .get(&vanilla_definition.file_id)
                .expect("cached source metadata")
                .root_id,
            SourceRootId::new(0)
        );
        let rename = responses.iter().find(|value| value["id"] == 2).expect("rename response");
        assert_eq!(rename["error"]["code"], INVALID_PARAMS);
        assert!(
            rename["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("read-only"))
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn missing_vanilla_cache_degrades_with_an_lsp_warning() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-lsp-missing-vanilla-cache-{nonce}"));
        fs::create_dir_all(root.join("common/events")).expect("workspace fixture");
        let input = frames([
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":path_to_uri(&root),"capabilities":{},"initializationOptions":{"vanillaIndexCache":".pdx/missing.pdxindex"}}}),
            json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
            json!({"jsonrpc":"2.0","id":2,"method":"shutdown","params":{}}),
            json!({"jsonrpc":"2.0","method":"exit"}),
        ]);
        let mut output = Vec::new();
        let mut server = eu4_server(InitializeOptions).expect("embedded rules");
        server.run_transport(Cursor::new(input), &mut output).expect("transport");
        let responses = decode_frames(&output);

        assert!(responses.iter().any(|value| value["id"] == 1 && value.get("result").is_some()));
        let warning = responses
            .iter()
            .find(|value| value["method"] == "window/showMessage")
            .expect("missing cache warning");
        assert_eq!(warning["params"]["type"], 2);
        assert!(
            warning["params"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("continuing without Vanilla symbols"))
        );
        assert_eq!(server.snapshot().source_roots().len(), 1);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn stale_diagnostics_do_not_replace_newer_results() {
        let mut server =
            eu4_server(InitializeOptions).expect("syntax-only server should initialize");
        let uri = "file:///tmp/diagnostics.txt";
        let id = pdx_workspace::DocumentId::new(uri);
        server
            .host
            .open_document(id.clone(), 1, "key = value".to_owned(), None)
            .expect("open should succeed");
        assert!(server.commit_diagnostics(uri, 1, json!([{"message":"old"}])));
        server
            .host
            .apply_document_changes(
                &id,
                2,
                &[TextChange::ranged(TextRange::new(0, 3).expect("range"), "new")],
            )
            .expect("change should succeed");
        assert!(!server.commit_diagnostics(uri, 1, json!([{"message":"stale"}])));
        assert_eq!(server.diagnostics(uri).expect("old result remains")[0]["message"], "old");
        assert!(server.commit_diagnostics(uri, 2, json!([{"message":"new"}])));
        assert_eq!(server.diagnostics(uri).expect("new result accepted")[0]["message"], "new");
    }

    #[test]
    fn rapid_changes_debounce_and_publish_only_the_latest_diagnostics() {
        let uri = "file:///tmp/debounced-diagnostics.txt";
        let input = frames([
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":"file:///tmp","capabilities":{}}}),
            json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
            json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":uri,"languageId":"eu4","version":1,"text":"scope = nowhere\n"}}}),
            json!({"jsonrpc":"2.0","method":"textDocument/didChange","params":{"textDocument":{"uri":uri,"version":2},"contentChanges":[{"text":"scope = country\n"}]}}),
            json!({"jsonrpc":"2.0","id":2,"method":"shutdown","params":{}}),
            json!({"jsonrpc":"2.0","method":"exit"}),
        ]);
        let mut output = Vec::new();
        let mut server = eu4_server(InitializeOptions).expect("server");

        server.run_transport(Cursor::new(input), &mut output).expect("transport");

        let responses = decode_frames(&output);
        let published = responses
            .iter()
            .filter(|value| {
                value["method"] == "textDocument/publishDiagnostics"
                    && value["params"]["uri"] == uri
            })
            .collect::<Vec<_>>();
        assert_eq!(published.len(), 1);
        assert!(
            published[0]["params"]["diagnostics"]
                .as_array()
                .is_some_and(|items| items.iter().all(|item| item["code"] != "pdx-unknown-scope"))
        );
        let snapshot = server.snapshot();
        let document = snapshot.document(&DocumentId::new(uri)).expect("latest overlay");
        assert_eq!(document.version(), Some(2));
        assert_eq!(document.text(), "scope = country\n");
        assert!(document.parsed().is_some());
        assert!(document.hir().is_some());
    }

    #[test]
    fn cancel_notification_marks_the_matching_in_flight_request() {
        let request_id = RequestId::String("active-query".to_owned());
        let cancellation = CancellationToken::new();
        let in_flight = HashMap::from([(
            request_id.clone(),
            InFlightRequest { cancellation: cancellation.clone() },
        )]);

        cancel_request_from_notification(
            &json!({
                "jsonrpc": "2.0",
                "method": "$/cancelRequest",
                "params": {"id": "active-query"},
            }),
            &in_flight,
        );

        assert!(cancellation.is_cancelled());
        assert!(in_flight.contains_key(&request_id));
    }

    #[test]
    fn initialize_cancellation_is_forwarded_and_a_retry_can_succeed() {
        let request_id = RequestId::Number(1);
        let scan_cancellation = pdx_workspace::WorkspaceScanToken::new();
        let in_flight = InFlightInitialize {
            request_id: request_id.clone(),
            cancellation: scan_cancellation.clone(),
        };
        cancel_initialize_from_notification(
            &json!({
                "jsonrpc": "2.0",
                "method": "$/cancelRequest",
                "params": {"id": 1},
            }),
            Some(&in_flight),
        );
        assert!(scan_cancellation.is_cancelled());

        let input = frames([
            json!({"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":1}}),
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":"file:///tmp/cancelled","capabilities":{}}}),
            json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"rootUri":"file:///tmp/retry","capabilities":{}}}),
            json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
            json!({"jsonrpc":"2.0","id":3,"method":"shutdown","params":{}}),
            json!({"jsonrpc":"2.0","method":"exit"}),
        ]);
        let mut output = Vec::new();
        let mut server = eu4_server(InitializeOptions).expect("server");
        server.run_transport(Cursor::new(input), &mut output).expect("transport");
        let responses = decode_frames(&output);

        assert_eq!(
            responses.iter().find(|value| value["id"] == 1).expect("cancelled initialize")["error"]
                ["code"],
            super::REQUEST_CANCELLED
        );
        assert!(
            responses
                .iter()
                .find(|value| value["id"] == 2)
                .is_some_and(|value| value["result"]["capabilities"].is_object())
        );
        assert_eq!(server.state(), ServerState::Exited);
    }

    #[test]
    fn automatic_vanilla_setup_builds_cache_and_records_single_attempt() {
        let (root, _) = temp_workspace_dir();
        let source = root.join("library/Europa Universalis IV");
        for directory in pdx_game::eu4::INSTALL_DESCRIPTOR.validation_directories {
            fs::create_dir_all(source.join(directory)).expect("validation directory");
        }
        #[cfg(target_os = "windows")]
        let executable = source.join("eu4.exe");
        #[cfg(target_os = "linux")]
        let executable = source.join("eu4");
        #[cfg(target_os = "macos")]
        let executable = source.join("Europa Universalis IV.app/Contents/MacOS/eu4");
        fs::create_dir_all(executable.parent().expect("executable parent"))
            .expect("executable parent directory");
        fs::write(executable, b"fixture executable").expect("executable marker");
        fs::create_dir_all(source.join("common/events")).expect("indexed directory");
        fs::write(
            source.join("common/events/definitions.txt"),
            "country_event = { id = vanilla.1 }\n",
        )
        .expect("fixture source");
        let automatic = AutoVanillaConfiguration {
            descriptor: pdx_game::eu4::INSTALL_DESCRIPTOR,
            user_paths: UserPaths {
                config_file: root.join("user/config.toml"),
                cache_root: root.join("user/cache"),
            },
        };
        let options = DiscoveryOptions {
            roots: vec![root.join("library")],
            include_platform_locations: false,
            ..DiscoveryOptions::default()
        };
        let (cache, message) = run_auto_vanilla_setup_with_options(
            &automatic,
            pdx_game::eu4::first_party_rules().expect("rules"),
            pdx_game::eu4::profile(),
            &VanillaSetupCancellation::new(),
            &options,
        )
        .expect("automatic setup");
        assert!(message.contains("Vanilla symbols are now enabled"));
        assert_eq!(cache.metadata().game_id, "eu4");
        assert!(automatic.user_paths.vanilla_cache("eu4").is_file());
        let configuration =
            UserConfiguration::load(&automatic.user_paths.config_file).expect("configuration");
        let game = configuration.games.get("eu4").expect("EU4 configuration");
        assert!(game.auto_discovery_attempted);
        assert_eq!(game.discovery_outcome, Some(DiscoveryOutcome::Configured));

        let repeated = run_auto_vanilla_setup_with_options(
            &automatic,
            pdx_game::eu4::first_party_rules().expect("rules"),
            pdx_game::eu4::profile(),
            &VanillaSetupCancellation::new(),
            &options,
        )
        .expect_err("automatic setup only runs once");
        assert!(repeated.contains("already attempted"));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn explicit_project_cache_precedes_user_discovery_configuration() {
        let (root, _) = temp_workspace_dir();
        let automatic = AutoVanillaConfiguration {
            descriptor: pdx_game::eu4::INSTALL_DESCRIPTOR,
            user_paths: UserPaths {
                config_file: root.join("user/config.toml"),
                cache_root: root.join("user/cache"),
            },
        };
        let mut configuration = UserConfiguration::default();
        let game = configuration.games.entry("eu4".to_owned()).or_default();
        game.auto_discovery_attempted = true;
        game.discovery_outcome = Some(DiscoveryOutcome::Configured);
        game.vanilla_cache = Some(root.join("user/cache/eu4/vanilla.pdxindex"));
        configuration.save(&automatic.user_paths.config_file).expect("save user configuration");

        let project_cache = root.join("project/vanilla.pdxindex");
        let mut resolved = ResolvedSourceRoots {
            workspace_root: None,
            roots: Vec::new(),
            vanilla_cache: Some(project_cache.clone()),
            vanilla_explicit: true,
        };
        let mut warnings = Vec::new();
        let setup =
            apply_user_vanilla_configuration(&mut resolved, Some(&automatic), "eu4", &mut warnings);
        assert!(setup.is_none());
        assert_eq!(resolved.vanilla_cache, Some(project_cache));
        assert!(warnings.is_empty());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn unsuccessful_automatic_discovery_is_recorded_and_not_repeated() {
        let (root, _) = temp_workspace_dir();
        let automatic = AutoVanillaConfiguration {
            descriptor: pdx_game::eu4::INSTALL_DESCRIPTOR,
            user_paths: UserPaths {
                config_file: root.join("user/config.toml"),
                cache_root: root.join("user/cache"),
            },
        };
        let options = DiscoveryOptions {
            roots: Vec::new(),
            include_platform_locations: false,
            ..DiscoveryOptions::default()
        };
        let first = run_auto_vanilla_setup_with_options(
            &automatic,
            pdx_game::eu4::first_party_rules().expect("rules"),
            pdx_game::eu4::profile(),
            &VanillaSetupCancellation::new(),
            &options,
        )
        .expect_err("empty search has no candidate");
        assert!(first.contains("was not found"));
        let configuration =
            UserConfiguration::load(&automatic.user_paths.config_file).expect("configuration");
        let game = configuration.games.get("eu4").expect("EU4 configuration");
        assert!(game.auto_discovery_attempted);
        assert_eq!(game.discovery_outcome, Some(DiscoveryOutcome::NotFound));

        let second = run_auto_vanilla_setup_with_options(
            &automatic,
            pdx_game::eu4::first_party_rules().expect("rules"),
            pdx_game::eu4::profile(),
            &VanillaSetupCancellation::new(),
            &options,
        )
        .expect_err("failed automatic search is not repeated");
        assert!(second.contains("already attempted"));
        fs::remove_dir_all(root).expect("cleanup");
    }
}
