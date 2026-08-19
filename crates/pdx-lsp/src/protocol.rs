use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::PathBuf;

use lsp_types::{
    CancelParams, CompletionItemKind, Diagnostic as LspDiagnostic, DiagnosticSeverity,
    Location as LspLocation, MessageType, NumberOrString, Position as LspPosition,
    Range as LspRange, ShowMessageParams, SymbolKind, Uri,
};
use pdx_analysis::{
    CancellationToken, Cancelled, CompletionKind, Diagnostic, Location, RenameError, RenameFailure,
    diagnostics_with_cancellation,
};
use pdx_engine::{AnalysisSnapshot, DocumentError, DocumentId, WorkspaceError};
use pdx_rules::RulesError;
use pdx_text::{LineIndex, PositionRange, TextRange};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::server::{InFlightInitialize, InFlightRequest};
use crate::uri::{path_to_uri, uri_to_path};
use crate::{
    INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, JSON_RPC_VERSION, MAX_PUBLISHED_DIAGNOSTICS,
    REQUEST_CANCELLED,
};

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
pub(crate) enum RequestId {
    Number(i64),
    String(String),
}

impl RequestId {
    pub(crate) fn parse(value: &Value) -> Result<Self, RpcError> {
        if let Some(number) = value.as_i64() {
            return Ok(Self::Number(number));
        }
        if let Some(string) = value.as_str() {
            return Ok(Self::String(string.to_owned()));
        }
        Err(RpcError::new(
            INVALID_REQUEST,
            "request id must be a string or integer",
        ))
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RpcError {
    pub(crate) code: i64,
    pub(crate) message: String,
}
pub(crate) fn diagnostic_result_counts(total: usize, maximum: usize) -> (usize, usize) {
    if total <= maximum {
        (total, 0)
    } else {
        let retained = maximum.saturating_sub(1);
        (retained, total - retained)
    }
}

pub(crate) fn is_snapshot_request(method: &str) -> bool {
    matches!(
        method,
        "textDocument/completion"
            | "completionItem/resolve"
            | "textDocument/hover"
            | "textDocument/definition"
            | "textDocument/references"
            | "textDocument/prepareRename"
            | "textDocument/rename"
            | "textDocument/documentSymbol"
            | "textDocument/semanticTokens/full"
            | "textDocument/formatting"
            | "workspace/symbol"
            | "pdx/workspaceDiagnostics"
            | "pdx/classifyPaths"
            | "pdx/textDiagnostics"
            | "pdx/missionPreview"
    )
}

pub(crate) fn is_snapshot_request_message(message: &Value) -> bool {
    message
        .as_object()
        .and_then(|object| object.get("method"))
        .and_then(Value::as_str)
        .is_some_and(is_snapshot_request)
}

pub(crate) fn is_initialize_control_message(message: &Value) -> bool {
    message
        .as_object()
        .and_then(|object| object.get("method"))
        .and_then(Value::as_str)
        .is_some_and(|method| matches!(method, "$/cancelRequest" | "exit"))
}

pub(crate) fn cancel_request_from_notification(
    message: &Value,
    in_flight: &HashMap<RequestId, InFlightRequest>,
) {
    let Some(object) = message.as_object() else {
        return;
    };
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

pub(crate) fn cancel_initialize_from_notification(
    message: &Value,
    in_flight: Option<&InFlightInitialize>,
) {
    let Some(in_flight) = in_flight else { return };
    let Some(object) = message.as_object() else {
        return;
    };
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

pub(crate) fn diagnostic_values(
    snapshot: &AnalysisSnapshot,
    id: &DocumentId,
    cancellation: &CancellationToken,
) -> Option<Value> {
    let values = snapshot.document(id).map_or_else(Vec::new, |document| {
        let diagnostics = diagnostics_with_cancellation(snapshot, id, cancellation)
            .ok()
            .unwrap_or_default();
        diagnostic_values_for_text(diagnostics, document.line_index(), document.text())
    });
    serde_json::to_value(values).ok()
}

pub(crate) fn diagnostic_values_for_text(
    diagnostics: Vec<Diagnostic>,
    line_index: &LineIndex,
    text: &str,
) -> Vec<LspDiagnostic> {
    let (retained, omitted) =
        diagnostic_result_counts(diagnostics.len(), MAX_PUBLISHED_DIAGNOSTICS);
    let mut values = diagnostics
        .into_iter()
        .take(retained)
        .map(|diagnostic| {
            LspDiagnostic::new(
                range_to_lsp(line_index, text, diagnostic.range),
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
            Some(NumberOrString::String(
                "pdx-diagnostics-truncated".to_owned(),
            )),
            Some("pdx-lsp".to_owned()),
            format!("{omitted} additional diagnostics were omitted"),
            None,
            None,
        ));
    }
    values
}

pub(crate) fn diagnostics_notification(uri: &str, values: Value) -> Value {
    json!({
        "jsonrpc": JSON_RPC_VERSION,
        "method": "textDocument/publishDiagnostics",
        "params": {"uri": uri, "diagnostics": values},
    })
}

pub(crate) fn log_message_notification(typ: MessageType, message: String) -> Value {
    let typ = match typ {
        MessageType::ERROR => 1,
        MessageType::WARNING => 2,
        MessageType::INFO => 3,
        MessageType::LOG => 4,
        _ => 3,
    };
    json!({
        "jsonrpc": JSON_RPC_VERSION,
        "method": "window/logMessage",
        "params": {"type": typ, "message": message},
    })
}

pub(crate) fn show_warning_notification(message: String) -> Value {
    let params = ShowMessageParams {
        typ: MessageType::WARNING,
        message,
    };
    json!({
        "jsonrpc": JSON_RPC_VERSION,
        "method": "window/showMessage",
        "params": params,
    })
}

pub(crate) fn show_info_notification(message: String) -> Value {
    let params = ShowMessageParams {
        typ: MessageType::INFO,
        message,
    };
    json!({
        "jsonrpc": JSON_RPC_VERSION,
        "method": "window/showMessage",
        "params": params,
    })
}

pub(crate) fn completion_kind(kind: CompletionKind) -> CompletionItemKind {
    match kind {
        CompletionKind::Key => CompletionItemKind::PROPERTY,
        CompletionKind::Value => CompletionItemKind::VALUE,
        CompletionKind::Symbol => CompletionItemKind::FUNCTION,
        CompletionKind::Localisation => CompletionItemKind::REFERENCE,
    }
}

pub(crate) fn symbol_kind(kind: &str) -> SymbolKind {
    match kind {
        "localisation" => SymbolKind::STRING,
        "event" => SymbolKind::FUNCTION,
        "scripted_effect" | "scripted_trigger" => SymbolKind::NAMESPACE,
        _ => SymbolKind::VARIABLE,
    }
}

pub(crate) fn range_to_lsp(index: &LineIndex, text: &str, range: TextRange) -> LspRange {
    let start = index.position(text, range.start()).unwrap_or_default();
    let end = index.position(text, range.end()).unwrap_or(start);
    LspRange::new(
        LspPosition::new(start.line, start.character),
        LspPosition::new(end.line, end.character),
    )
}

pub(crate) fn cached_position_range_to_lsp(range: PositionRange) -> LspRange {
    LspRange::new(
        LspPosition::new(range.start.line, range.start.character),
        LspPosition::new(range.end.line, range.end.character),
    )
}

pub(crate) fn location_range_to_lsp(snapshot: &AnalysisSnapshot, location: &Location) -> LspRange {
    if let Some(document) = location.document.as_ref()
        && let Some(document) = snapshot.document(document)
    {
        return range_to_lsp(document.line_index(), document.text(), location.range);
    }
    if let Some(file) = location.file.and_then(|file| snapshot.source_text(file)) {
        let index = LineIndex::new(file);
        return range_to_lsp(&index, file, location.range);
    }
    if let Some(file) = location.file
        && let Some(range) = snapshot.index().position_for(file, location.range)
    {
        return cached_position_range_to_lsp(range);
    }
    LspRange::default()
}

pub(crate) fn range_to_lsp_for_location(
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
    if let Some(file) = location.file
        && let Some(position_range) = snapshot.index().position_for(file, range)
    {
        return cached_position_range_to_lsp(position_range);
    }
    LspRange::default()
}

pub(crate) fn location_to_lsp(
    snapshot: &AnalysisSnapshot,
    location: &Location,
) -> Option<LspLocation> {
    let uri = if let Some(document) = location.document.as_ref() {
        document.as_str().parse::<Uri>().ok()?
    } else if let Some(file) = location
        .file
        .and_then(|file| snapshot.source_files().get(&file))
    {
        path_to_uri(&file.physical_path).parse::<Uri>().ok()?
    } else if let (Some(root), Some(path)) = (snapshot.workspace_root(), location.path.as_ref()) {
        path_to_uri(&root.join(path.as_str())).parse::<Uri>().ok()?
    } else {
        return None;
    };
    Some(LspLocation::new(
        uri,
        location_range_to_lsp(snapshot, location),
    ))
}

pub(crate) fn document_error(error: DocumentError) -> RpcError {
    RpcError {
        code: INVALID_PARAMS,
        message: error.to_string(),
    }
}

pub(crate) fn workspace_scan_error(error: WorkspaceError) -> RpcError {
    let code = if matches!(error, WorkspaceError::Cancelled) {
        REQUEST_CANCELLED
    } else {
        INVALID_PARAMS
    };
    RpcError {
        code,
        message: error.to_string(),
    }
}

pub(crate) fn rename_error(error: RenameError) -> RpcError {
    RpcError {
        code: INVALID_PARAMS,
        message: format!("rename unavailable: {error}"),
    }
}

pub(crate) fn cancelled_error(_: Cancelled) -> RpcError {
    RpcError::new(REQUEST_CANCELLED, "request was cancelled")
}

pub(crate) fn rename_failure(error: RenameFailure) -> RpcError {
    match error {
        RenameFailure::Cancelled => cancelled_error(Cancelled),
        RenameFailure::Rejected(error) => rename_error(error),
    }
}

pub(crate) fn typed_params<T: DeserializeOwned>(
    params: Option<&Value>,
    context: &'static str,
) -> Result<T, RpcError> {
    serde_json::from_value(params.cloned().unwrap_or(Value::Null)).map_err(|error| RpcError {
        code: INVALID_PARAMS,
        message: format!("invalid {context} params: {error}"),
    })
}

pub(crate) fn typed_value<T: Serialize>(
    value: T,
    context: &'static str,
) -> Result<Value, RpcError> {
    serde_json::to_value(value).map_err(|error| RpcError {
        code: INTERNAL_ERROR,
        message: format!("failed to serialize {context}: {error}"),
    })
}

pub(crate) fn request_id_from_lsp(id: NumberOrString) -> RequestId {
    match id {
        NumberOrString::Number(value) => RequestId::Number(i64::from(value)),
        NumberOrString::String(value) => RequestId::String(value),
    }
}

pub(crate) fn parse_file_uri_str(uri: &str) -> Result<PathBuf, RpcError> {
    uri_to_path(uri).map_err(|_| RpcError::new(INVALID_PARAMS, "only file:// URIs are supported"))
}

impl RpcError {
    pub(crate) fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn response(&self, id: Value) -> Value {
        json!({
            "jsonrpc": JSON_RPC_VERSION,
            "id": id,
            "error": {"code": self.code, "message": self.message},
        })
    }
}

#[cfg(test)]
mod tests {
    use super::is_snapshot_request;
    use lsp_types::request::Request;
    use lsp_types::request::{
        Completion, DocumentSymbolRequest, Formatting, GotoDefinition, HoverRequest,
        PrepareRenameRequest, References, Rename, ResolveCompletionItem, SemanticTokensFullRequest,
        WorkspaceSymbolRequest,
    };

    /// The snapshot-request routing table must agree with the wire method names the
    /// real protocol library uses. Hardcoding a wrong spelling here silently breaks
    /// real clients (`textDocument/semanticTokens` vs the real
    /// `textDocument/semanticTokens/full`), so pin the routing strings to the
    /// protocol constants instead of trusting hand-written copies.
    #[test]
    fn snapshot_routing_matches_protocol_method_names() {
        for method in [
            Completion::METHOD,
            ResolveCompletionItem::METHOD,
            SemanticTokensFullRequest::METHOD,
            HoverRequest::METHOD,
            GotoDefinition::METHOD,
            References::METHOD,
            PrepareRenameRequest::METHOD,
            Rename::METHOD,
            DocumentSymbolRequest::METHOD,
            Formatting::METHOD,
            WorkspaceSymbolRequest::METHOD,
        ] {
            assert!(
                is_snapshot_request(method),
                "snapshot route missing {method}"
            );
        }
        // The bare, non-spec request name must not be mistaken for the full-token request.
        assert!(!is_snapshot_request("textDocument/semanticTokens"));
    }
}
