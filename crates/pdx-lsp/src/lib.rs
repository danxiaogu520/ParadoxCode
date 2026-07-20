//! Minimal JSON-RPC/LSP runtime for the generic PDX language server.
//!
//! The crate owns transport framing, protocol state, document versioning, URI and position
//! conversion, and result freshness checks. Parser and language-feature logic remains in the
//! editor-neutral workspace and analysis crates.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fmt;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use lsp_types::{
    CancelParams, CompletionOptions, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DidSaveTextDocumentParams, DocumentSymbolParams,
    HoverProviderCapability, InitializeParams, InitializeResult, NumberOrString, OneOf,
    Range as LspRange, ReferenceParams, RenameOptions, RenameParams, ServerCapabilities,
    ServerInfo, TextDocumentPositionParams, TextDocumentSyncCapability, TextDocumentSyncKind,
    TextDocumentSyncOptions, WorkDoneProgressOptions, WorkspaceSymbolParams,
};
use pdx_analysis::{
    CompletionKind, Hover, Location, RenameError, complete, definition, diagnostics,
    document_symbols, hover, prepare_rename, references, rename, workspace_symbols,
};
use pdx_rules::{GameProfile, RuleSet, RulesError};
use pdx_text::{LineIndex, Position, TextRange};
use pdx_workspace::{
    AnalysisHost, AnalysisSnapshot, DocumentError, DocumentId, DocumentSource, PreparedDocument,
    SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

const JSON_RPC_VERSION: &str = "2.0";
const INTERNAL_ERROR: i64 = -32603;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const SERVER_NOT_INITIALIZED: i64 = -32002;
const REQUEST_CANCELLED: i64 = -32800;
const DIAGNOSTIC_DEBOUNCE: Duration = Duration::from_millis(200);

/// Lifecycle state of the server process.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ServerState {
    /// The process accepts only `initialize`, `exit`, and cancellation notifications.
    Uninitialized,
    /// The server has completed `initialize` and accepts document events.
    Initialized,
    /// `shutdown` completed; only `exit` is accepted.
    ShuttingDown,
    /// The `exit` notification was received.
    Exited,
}

/// Explicit options passed by an editor or CLI.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InitializeOptions {
    /// Optional packaged EU4 rules path. The server validates and loads it read-only before serving.
    pub rules_path: Option<PathBuf>,
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
    cancelled: Arc<AtomicBool>,
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
    cancelled: Arc<AtomicBool>,
}

#[derive(Debug)]
struct SnapshotRequestResult {
    request_id: RequestId,
    id: Value,
    result: Result<Value, RpcError>,
}

enum TransportEvent {
    Input(Result<Option<Value>, LspError>),
    Parse(ParseResult),
    Diagnostics(DiagnosticsResult),
    Request(SnapshotRequestResult),
}

impl RpcError {
    fn new(code: i64, message: &'static str) -> Self {
        Self { code, message: message.to_owned() }
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
    clean_exit: bool,
}

impl LspServer {
    /// Creates a server and fails when an explicitly supplied rules artifact is invalid.
    pub fn try_new(options: InitializeOptions) -> Result<Self, LspError> {
        Self::try_new_with_expected_game(options, None, GameProfile::default())
    }

    /// Creates an identity-only server and rejects a rules artifact for a different game.
    ///
    /// Call [`Self::try_new_with_profile`] when game-specific interpretation is required.
    pub fn try_new_for_game(
        options: InitializeOptions,
        expected_game_id: &str,
    ) -> Result<Self, LspError> {
        Self::try_new_with_expected_game(
            options,
            Some(expected_game_id),
            GameProfile::empty(expected_game_id),
        )
    }

    /// Creates a server with explicit data-only game semantics.
    pub fn try_new_with_profile(
        options: InitializeOptions,
        profile: GameProfile,
    ) -> Result<Self, LspError> {
        let expected_game_id = (!profile.game_id.is_empty()).then(|| profile.game_id.clone());
        Self::try_new_with_expected_game(options, expected_game_id.as_deref(), profile)
    }

    fn try_new_with_expected_game(
        options: InitializeOptions,
        expected_game_id: Option<&str>,
        profile: GameProfile,
    ) -> Result<Self, LspError> {
        let rules = match options.rules_path.as_deref() {
            Some(path) => {
                let rules = RuleSet::load(path)?;
                if let Some(expected) = expected_game_id {
                    rules.ensure_game(expected)?;
                }
                rules
            }
            None => RuleSet::empty(),
        };
        Ok(Self {
            state: ServerState::Uninitialized,
            options,
            host: AnalysisHost::with_profile(rules, profile),
            cancelled: HashSet::new(),
            diagnostics: BTreeMap::new(),
            pending_parses: BTreeMap::new(),
            pending_diagnostics: BTreeMap::new(),
            clean_exit: false,
        })
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

    /// Runs identity-only stdio while enforcing the selected game identity.
    ///
    /// Call [`Self::run_stdio_with_profile`] for a game-aware language server.
    pub fn run_stdio_for_game(
        options: InitializeOptions,
        expected_game_id: &str,
    ) -> Result<(), LspError> {
        let stdin = io::stdin();
        let stdout = io::stdout();
        let mut server = Self::try_new_for_game(options, expected_game_id)?;
        server.run_transport(stdin, stdout.lock())
    }

    /// Runs stdio with explicit game-profile interpretation and identity validation.
    pub fn run_stdio_with_profile(
        options: InitializeOptions,
        profile: GameProfile,
    ) -> Result<(), LspError> {
        let stdin = io::stdin();
        let stdout = io::stdout();
        let mut server = Self::try_new_with_profile(options, profile)?;
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
            let mut deferred_messages = VecDeque::<Value>::new();

            loop {
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
                let deferred_ready = !parse_busy && !deferred_messages.is_empty();
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
                        if from_reader && parse_busy && is_snapshot_request_message(&message) {
                            deferred_messages.push_back(message);
                        } else {
                            let spawned = self.spawn_snapshot_request(
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
                            }
                        }
                        self.cancel_stale_parses(&in_flight_parses);
                        self.cancel_stale_diagnostics(&in_flight);
                        if self.state == ServerState::Exited {
                            for task in in_flight_parses.values() {
                                task.cancelled.store(true, Ordering::Release);
                            }
                            for task in in_flight.values() {
                                task.cancelled.store(true, Ordering::Release);
                            }
                            for task in in_flight_requests.values() {
                                task.cancelled.store(true, Ordering::Release);
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
                }

                let draining_shutdown = self.state == ServerState::ShuttingDown
                    && (!self.pending_parses.is_empty()
                        || !in_flight_parses.is_empty()
                        || !self.pending_diagnostics.is_empty()
                        || !in_flight.is_empty()
                        || !in_flight_requests.is_empty());
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

        let context = SnapshotRequestContext::new(self.host.snapshot());
        let method = method.to_owned();
        let params = object.get("params").cloned();
        let id = id.clone();
        let sender = event_sender.clone();
        let cancelled = Arc::new(AtomicBool::new(self.cancelled.contains(&request_id)));
        in_flight.insert(request_id.clone(), InFlightRequest { cancelled: Arc::clone(&cancelled) });
        scope.spawn(move || {
            let result = if cancelled.load(Ordering::Acquire) {
                Err(RpcError::new(REQUEST_CANCELLED, "request was cancelled"))
            } else {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    context.dispatch(&method, params.as_ref())
                }))
                .unwrap_or_else(|_| {
                    Err(RpcError::new(INTERNAL_ERROR, "request worker failed unexpectedly"))
                });
                if cancelled.load(Ordering::Acquire) {
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
                task.cancelled.store(true, Ordering::Release);
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
            let cancelled = Arc::new(AtomicBool::new(false));
            in_flight.insert(
                id.clone(),
                InFlightDiagnostics { version: pending.version, cancelled: Arc::clone(&cancelled) },
            );
            scope.spawn(move || {
                let values = if cancelled.load(Ordering::Acquire) {
                    None
                } else {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        diagnostic_values(&snapshot, &id)
                    }))
                    .ok()
                    .filter(|_| !cancelled.load(Ordering::Acquire))
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
        if let Some(request_id) = request_id {
            if self.cancelled.remove(request_id) {
                return Err(RpcError::new(REQUEST_CANCELLED, "request was cancelled"));
            }
        }
        if self.state == ServerState::Uninitialized {
            return Err(RpcError::new(SERVER_NOT_INITIALIZED, "server is not initialized"));
        }
        if self.state == ServerState::ShuttingDown {
            return Err(RpcError::new(SERVER_NOT_INITIALIZED, "server is shutting down"));
        }

        match method {
            "initialized" => Ok(Value::Null),
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
                self.schedule_parse(uri);
                self.schedule_diagnostics(uri, Duration::ZERO);
                Ok(Value::Null)
            }
            method if is_snapshot_request(method) => {
                SnapshotRequestContext::new(self.host.snapshot()).dispatch(method, params)
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
        #[allow(deprecated)]
        let root_uri = params.root_uri;
        let root = root_uri.as_ref().map(|uri| parse_file_uri_str(uri.as_str())).transpose()?;
        let workspace_root = params
            .workspace_folders
            .as_ref()
            .and_then(|folders| folders.first())
            .map(|folder| parse_file_uri_str(folder.uri.as_str()))
            .transpose()?;
        let root = root.or(workspace_root);
        self.host.apply_change(WorkspaceChange::SetWorkspaceRoot(root.clone()));
        if let Some(root) = root.filter(|path| path.is_dir()) {
            self.host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
                SourceRootId::new(1),
                SourceRootKind::CurrentMod,
                root,
            )]));
            if self.options.rules_path.is_some() {
                self.host.refresh_source_roots().map_err(|error| RpcError {
                    code: INVALID_PARAMS,
                    message: error.to_string(),
                })?;
            }
        }
        self.state = ServerState::Initialized;
        serde_json::to_value(InitializeResult {
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
        })
    }

    fn handle_did_open(&mut self, params: Option<&Value>) -> Result<String, RpcError> {
        let params = typed_params::<DidOpenTextDocumentParams>(params, "didOpen")?;
        let uri = params.text_document.uri.as_str().to_owned();
        let version = i64::from(params.text_document.version);
        let text = params.text_document.text;
        let path = uri_to_path(&uri).ok();
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
}

#[derive(Clone, Debug)]
struct SnapshotRequestContext {
    snapshot: AnalysisSnapshot,
}

impl SnapshotRequestContext {
    fn new(snapshot: AnalysisSnapshot) -> Self {
        Self { snapshot }
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
            "workspace/symbol" => self.workspace_symbols(params),
            _ => Err(RpcError::new(METHOD_NOT_FOUND, "method is not implemented")),
        }
    }

    fn completion(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let (id, position) = self.document_position(params)?;
        let result = complete(&self.snapshot, &id, position);
        let document = self
            .snapshot
            .document(&id)
            .ok_or_else(|| RpcError::new(INVALID_PARAMS, "document is not open"))?;
        let items = result
            .items
            .into_iter()
            .map(|item| {
                let insert_format = u8::from(item.insert_text.contains("$0"));
                json!({
                    "label": item.label,
                    "kind": completion_kind(item.kind),
                    "detail": item.detail,
                    "documentation": item.documentation,
                    "deprecated": item.deprecated,
                    "sortText": format!("{:03}", item.sort_score),
                    "insertText": item.insert_text,
                    "insertTextFormat": if insert_format == 1 { 2 } else { 1 },
                    "textEdit": {
                        "range": range_to_json(document.line_index(), document.text(), item.replacement_range),
                        "newText": item.insert_text,
                    },
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({"isIncomplete": false, "items": items}))
    }

    fn hover(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let (id, position) = self.document_position(params)?;
        let Some(value) = hover(&self.snapshot, &id, position) else { return Ok(Value::Null) };
        let document = self
            .snapshot
            .document(&id)
            .ok_or_else(|| RpcError::new(INVALID_PARAMS, "document is not open"))?;
        Ok(hover_to_json(&value, document.line_index(), document.text()))
    }

    fn definition(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let (id, position) = self.document_position(params)?;
        let result = definition(&self.snapshot, &id, position)
            .into_iter()
            .filter_map(|location| location_to_json(&self.snapshot, &location))
            .collect::<Vec<_>>();
        Ok(Value::Array(result))
    }

    fn references(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let params = typed_params::<ReferenceParams>(params, "references")?;
        let (id, position) = self.offset_for(&params.text_document_position)?;
        let include_declaration = params.context.include_declaration;
        let result = references(&self.snapshot, &id, position, include_declaration)
            .into_iter()
            .filter_map(|location| location_to_json(&self.snapshot, &location))
            .collect::<Vec<_>>();
        Ok(Value::Array(result))
    }

    fn prepare_rename(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let (id, position) = self.document_position(params)?;
        let result = prepare_rename(&self.snapshot, &id, position).map_err(rename_error)?;
        let document = self
            .snapshot
            .document(&id)
            .ok_or_else(|| RpcError::new(INVALID_PARAMS, "document is not open"))?;
        Ok(json!({
            "range": range_to_json(document.line_index(), document.text(), result.range),
            "placeholder": result.placeholder,
        }))
    }

    fn rename(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let params = typed_params::<RenameParams>(params, "rename")?;
        let (id, position) = self.offset_for(&params.text_document_position)?;
        let plan = rename(&self.snapshot, &id, position, &params.new_name).map_err(rename_error)?;
        let mut changes = BTreeMap::<String, Vec<Value>>::new();
        for edit in plan.edits {
            let location = location_to_json(&self.snapshot, &edit.location).ok_or_else(|| {
                RpcError::new(INVALID_PARAMS, "rename target has no client-visible URI")
            })?;
            let uri = location
                .get("uri")
                .and_then(Value::as_str)
                .ok_or_else(|| RpcError::new(INVALID_PARAMS, "rename target has no URI"))?
                .to_owned();
            let range = location
                .get("range")
                .cloned()
                .ok_or_else(|| RpcError::new(INVALID_PARAMS, "rename target has no range"))?;
            changes.entry(uri).or_default().push(json!({
                "range": range,
                "newText": edit.new_text,
            }));
        }
        Ok(json!({"changes": changes}))
    }

    fn document_symbols(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let params = typed_params::<DocumentSymbolParams>(params, "document symbols")?;
        let id = DocumentId::new(params.text_document.uri.as_str());
        let result = document_symbols(&self.snapshot, &id)
            .into_iter()
            .map(|symbol| {
                json!({
                    "name": symbol.name,
                    "kind": symbol_kind(&symbol.kind),
                    "range": location_range_to_json(&self.snapshot, &symbol.location),
                    "selectionRange": range_to_json_for_location(&self.snapshot, &symbol.location, symbol.selection_range),
                })
            })
            .collect::<Vec<_>>();
        Ok(Value::Array(result))
    }

    fn workspace_symbols(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let params = typed_params::<WorkspaceSymbolParams>(params, "workspace symbols")?;
        let result = workspace_symbols(&self.snapshot, &params.query)
            .into_iter()
            .filter_map(|symbol| {
                let location = location_to_json(&self.snapshot, &symbol.location)?;
                Some(json!({
                    "name": symbol.name,
                    "kind": symbol_kind(&symbol.kind),
                    "location": location,
                }))
            })
            .collect::<Vec<_>>();
        Ok(Value::Array(result))
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
        request.cancelled.store(true, Ordering::Release);
    }
}

fn diagnostic_values(snapshot: &AnalysisSnapshot, id: &DocumentId) -> Value {
    let values = snapshot.document(id).map_or_else(Vec::new, |document| {
        diagnostics(snapshot, id)
            .into_iter()
            .map(|diagnostic| {
                json!({
                    "range": range_to_json(
                        document.line_index(),
                        document.text(),
                        diagnostic.range,
                    ),
                    "severity": diagnostic.severity,
                    "code": diagnostic.code.as_str(),
                    "source": "pdx-analysis",
                    "message": diagnostic.message,
                })
            })
            .collect::<Vec<_>>()
    });
    Value::Array(values)
}

fn diagnostics_notification(uri: &str, values: Value) -> Value {
    json!({
        "jsonrpc": JSON_RPC_VERSION,
        "method": "textDocument/publishDiagnostics",
        "params": {"uri": uri, "diagnostics": values},
    })
}

fn completion_kind(kind: CompletionKind) -> u8 {
    match kind {
        CompletionKind::Key => 10,
        CompletionKind::Value => 12,
        CompletionKind::Symbol => 3,
        CompletionKind::Localisation => 14,
    }
}

fn symbol_kind(kind: &str) -> u8 {
    match kind {
        "localisation" => 15,
        "event" => 12,
        "scripted_effect" | "scripted_trigger" => 3,
        _ => 13,
    }
}

fn hover_to_json(value: &Hover, index: &LineIndex, text: &str) -> Value {
    let mut result = json!({
        "contents": {"kind": "markdown", "value": value.contents},
    });
    if let Some(range) = value.range {
        result["range"] = range_to_json(index, text, range);
    }
    result
}

fn range_to_json(index: &LineIndex, text: &str, range: TextRange) -> Value {
    let start = index.position(text, range.start()).unwrap_or_default();
    let end = index.position(text, range.end()).unwrap_or(start);
    json!({
        "start": {"line": start.line, "character": start.character},
        "end": {"line": end.line, "character": end.character},
    })
}

fn location_range_to_json(snapshot: &AnalysisSnapshot, location: &Location) -> Value {
    if let Some(document) = location.document.as_ref()
        && let Some(document) = snapshot.document(document)
    {
        return range_to_json(document.line_index(), document.text(), location.range);
    }
    if let Some(file) = location.file.and_then(|file| snapshot.source_text(file)) {
        let index = LineIndex::new(file);
        return range_to_json(&index, file, location.range);
    }
    json!({"start":{"line":0,"character":0},"end":{"line":0,"character":0}})
}

fn range_to_json_for_location(
    snapshot: &AnalysisSnapshot,
    location: &Location,
    range: TextRange,
) -> Value {
    if let Some(document) = location.document.as_ref()
        && let Some(document) = snapshot.document(document)
    {
        return range_to_json(document.line_index(), document.text(), range);
    }
    if let Some(file) = location.file.and_then(|file| snapshot.source_text(file)) {
        let index = LineIndex::new(file);
        return range_to_json(&index, file, range);
    }
    json!({"start":{"line":0,"character":0},"end":{"line":0,"character":0}})
}

fn location_to_json(snapshot: &AnalysisSnapshot, location: &Location) -> Option<Value> {
    let uri = if let Some(document) = location.document.as_ref() {
        document.as_str().to_owned()
    } else if let Some(file) = location.file.and_then(|file| snapshot.source_files().get(&file)) {
        path_to_uri(&file.physical_path)
    } else if let (Some(root), Some(path)) = (snapshot.workspace_root(), location.path.as_ref()) {
        path_to_uri(&root.join(path.as_str()))
    } else {
        return None;
    };
    Some(json!({"uri": uri, "range": location_range_to_json(snapshot, location)}))
}

fn document_error(error: DocumentError) -> RpcError {
    RpcError { code: INVALID_PARAMS, message: error.to_string() }
}

fn rename_error(error: RenameError) -> RpcError {
    RpcError { code: INVALID_PARAMS, message: format!("rename unavailable: {error}") }
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

fn request_id_from_lsp(id: NumberOrString) -> RequestId {
    match id {
        NumberOrString::Number(value) => RequestId::Number(i64::from(value)),
        NumberOrString::String(value) => RequestId::String(value),
    }
}

fn parse_file_uri_str(uri: &str) -> Result<PathBuf, RpcError> {
    uri_to_path(uri).map_err(|_| RpcError::new(INVALID_PARAMS, "only file:// URIs are supported"))
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
    loop {
        let mut line = String::new();
        let bytes = reader.read_line(&mut line)?;
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
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| LspError::Protocol("invalid Content-Length".to_owned()))?,
            );
        }
    }
    let content_length =
        content_length.ok_or_else(|| LspError::Protocol("missing Content-Length".to_owned()))?;
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
    use std::collections::HashMap;
    use std::fs;
    use std::io::Cursor;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::{
        DocumentId, INVALID_PARAMS, InFlightRequest, InitializeOptions, LspError, LspServer,
        RequestId, ServerState, cancel_request_from_notification, path_to_uri, uri_to_path,
    };
    use pdx_rules::{RuleSet, RulesError, RulesModel};
    use pdx_text::TextRange;
    use pdx_workspace::TextChange;
    use serde_json::{Value, json};

    fn eu4_server(options: InitializeOptions) -> Result<LspServer, LspError> {
        LspServer::try_new_with_profile(options, pdx_game_eu4::profile())
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

    fn decode_frames(bytes: &[u8]) -> Vec<Value> {
        let mut cursor = Cursor::new(bytes);
        let mut decoded = Vec::new();
        while let Some(value) = super::read_message(&mut cursor).expect("test frame is valid") {
            decoded.push(value);
        }
        decoded
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
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("pdx-lsp-wrong-game-{nonce}.pdxrules"));
        RuleSet::from_model(RulesModel {
            game_id: "another-game".to_owned(),
            ..RulesModel::default()
        })
        .write_sqlite(&path)
        .expect("write mismatched rules");

        let error = LspServer::try_new_for_game(
            InitializeOptions { rules_path: Some(path.clone()) },
            "eu4",
        )
        .expect_err("mismatched game must be rejected");
        assert!(matches!(
            error,
            LspError::Rules(RulesError::GameMismatch { expected, actual })
                if expected == "eu4" && actual == "another-game"
        ));
        fs::remove_file(path).expect("cleanup rules");
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
                "params":{"textDocument":{"uri":uri,"languageId":"pdx-script","version":1,"text":"a\r\n汉😀e\u{301}\r\n"}}
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
            eu4_server(InitializeOptions::default()).expect("syntax-only server should initialize");
        server.run_transport(Cursor::new(input), &mut output).expect("transport should finish");

        let responses = decode_frames(&output);
        let before_initialize =
            responses.iter().find(|value| value["id"] == 1).expect("pre-init response");
        assert_eq!(before_initialize["error"]["code"], -32002);
        let initialize =
            responses.iter().find(|value| value["id"] == 2).expect("initialize response");
        assert_eq!(initialize["result"]["capabilities"]["textDocumentSync"]["change"], 2);
        assert_eq!(initialize["result"]["capabilities"]["renameProvider"]["prepareProvider"], true);
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
        let mut server = eu4_server(InitializeOptions::default()).expect("server");

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
        let uri = "file:///tmp/phase5-events.txt";
        let text = "country_event = { id = test.1 }\nevent = test.1\nscope = nowhere\n";
        let input = frames([
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":"file:///tmp","capabilities":{}}}),
            json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
            json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":uri,"languageId":"pdx-script","version":1,"text":text}}}),
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
            eu4_server(InitializeOptions::default()).expect("syntax-only server should initialize");
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
        assert_eq!(
            responses.iter().find(|value| value["id"] == 6).expect("document symbols")["result"]
                .as_array()
                .map(Vec::len),
            Some(1)
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
        assert_eq!(rename["result"]["changes"][uri].as_array().map(Vec::len), Some(2));
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
        let target_uri = path_to_uri(&target_path);
        let references_uri = path_to_uri(&references_path);
        let rules_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../rules/eu4.pdxrules");
        let input = frames([
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":path_to_uri(&root),"capabilities":{}}}),
            json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
            json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":target_uri,"languageId":"pdx-script","version":1,"text":"country_event = { id = cross.1 }\n"}}}),
            json!({"jsonrpc":"2.0","id":2,"method":"textDocument/rename","params":{"textDocument":{"uri":target_uri},"position":{"line":0,"character":25},"newName":"renamed.1"}}),
            json!({"jsonrpc":"2.0","id":3,"method":"shutdown","params":{}}),
            json!({"jsonrpc":"2.0","method":"exit"}),
        ]);
        let mut output = Vec::new();
        let mut server = eu4_server(InitializeOptions { rules_path: Some(rules_path) })
            .expect("bundled rules should load");
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
    fn stale_diagnostics_do_not_replace_newer_results() {
        let mut server =
            eu4_server(InitializeOptions::default()).expect("syntax-only server should initialize");
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
            json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":uri,"languageId":"pdx-script","version":1,"text":"scope = nowhere\n"}}}),
            json!({"jsonrpc":"2.0","method":"textDocument/didChange","params":{"textDocument":{"uri":uri,"version":2},"contentChanges":[{"text":"scope = country\n"}]}}),
            json!({"jsonrpc":"2.0","id":2,"method":"shutdown","params":{}}),
            json!({"jsonrpc":"2.0","method":"exit"}),
        ]);
        let mut output = Vec::new();
        let mut server = eu4_server(InitializeOptions::default()).expect("server");

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
        let cancelled = Arc::new(AtomicBool::new(false));
        let in_flight = HashMap::from([(
            request_id.clone(),
            InFlightRequest { cancelled: Arc::clone(&cancelled) },
        )]);

        cancel_request_from_notification(
            &json!({
                "jsonrpc": "2.0",
                "method": "$/cancelRequest",
                "params": {"id": "active-query"},
            }),
            &in_flight,
        );

        assert!(cancelled.load(Ordering::Acquire));
        assert!(in_flight.contains_key(&request_id));
    }
}
