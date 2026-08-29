use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::io;
use std::path::PathBuf;

use lsp_types::{
    CancelParams, CompletionItemKind, Diagnostic as LspDiagnostic, DiagnosticSeverity,
    Location as LspLocation, MessageType, NumberOrString, Position as LspPosition,
    Range as LspRange, ShowMessageParams, SymbolKind, Uri,
};
use pdx_analysis::{
    CancellationToken, Cancelled, CompletionKind, Diagnostic, DiagnosticCode, Location,
    RenameError, RenameFailure, Severity, diagnostics_with_cancellation,
};
use pdx_engine::{AnalysisSnapshot, DocumentError, DocumentId};
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
            | "textDocument/codeAction"
            | "textDocument/documentSymbol"
            | "textDocument/inlayHint"
            | "textDocument/semanticTokens/full"
            | "textDocument/semanticTokens/full/delta"
            | "textDocument/semanticTokens/range"
            | "textDocument/formatting"
            | "workspace/symbol"
            | "pdx/workspaceDiagnostics"
            | "pdx/workspaceFiles"
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

pub(crate) fn is_execute_command_message(message: &Value) -> bool {
    message
        .as_object()
        .and_then(|object| object.get("method"))
        .and_then(Value::as_str)
        == Some("workspace/executeCommand")
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

/// Converts diagnostics for one snapshot document while applying both category suppression and
/// user-selected severity remapping. The raw analysis values are adjusted before publication so
/// truncation and all downstream aggregate counts observe the effective severity.
pub(crate) fn diagnostic_values_with_ignored_and_overrides(
    snapshot: &AnalysisSnapshot,
    id: &DocumentId,
    cancellation: &CancellationToken,
    ignored: &HashSet<String>,
    overrides: &BTreeMap<String, Option<Severity>>,
) -> Option<Value> {
    let values = snapshot.document(id).map_or_else(Vec::new, |document| {
        let diagnostics = diagnostics_with_cancellation(snapshot, id, cancellation)
            .ok()
            .unwrap_or_default();
        diagnostic_values_for_text_with_ignored_and_overrides(
            diagnostics,
            document.line_index(),
            document.text(),
            ignored,
            overrides,
        )
    });
    serde_json::to_value(values).ok()
}

#[cfg(test)]
pub(crate) fn diagnostic_values_for_text(
    diagnostics: Vec<Diagnostic>,
    line_index: &LineIndex,
    text: &str,
) -> Vec<LspDiagnostic> {
    diagnostic_values_for_text_with_ignored_and_overrides(
        diagnostics,
        line_index,
        text,
        &HashSet::new(),
        &BTreeMap::new(),
    )
}

#[cfg(test)]
pub(crate) fn diagnostic_values_for_text_with_ignored(
    diagnostics: Vec<Diagnostic>,
    line_index: &LineIndex,
    text: &str,
    ignored: &HashSet<String>,
) -> Vec<LspDiagnostic> {
    diagnostic_values_for_text_with_ignored_and_overrides(
        diagnostics,
        line_index,
        text,
        ignored,
        &BTreeMap::new(),
    )
}

pub(crate) fn diagnostic_values_for_text_with_ignored_and_overrides(
    diagnostics: Vec<Diagnostic>,
    line_index: &LineIndex,
    text: &str,
    ignored: &HashSet<String>,
    overrides: &BTreeMap<String, Option<Severity>>,
) -> Vec<LspDiagnostic> {
    let diagnostics = filter_diagnostics_with_ignored_and_overrides(
        diagnostics,
        line_index,
        text,
        ignored,
        overrides,
    );
    let (retained, omitted) =
        diagnostic_result_counts(diagnostics.len(), MAX_PUBLISHED_DIAGNOSTICS);
    let mut values = diagnostics
        .into_iter()
        .take(retained)
        .map(|diagnostic| {
            let severity = match diagnostic.severity.lsp_number() {
                1 => Some(DiagnosticSeverity::ERROR),
                2 => Some(DiagnosticSeverity::WARNING),
                3 => Some(DiagnosticSeverity::INFORMATION),
                4 => Some(DiagnosticSeverity::HINT),
                _ => None,
            };
            let fix_data = diagnostic
                .fixes
                .iter()
                .map(|fix| {
                    json!({
                        "title": fix.title,
                        "range": range_to_lsp(line_index, text, fix.range),
                        "newText": fix.new_text,
                    })
                })
                .collect::<Vec<_>>();
            let mut value = LspDiagnostic::new(
                range_to_lsp(line_index, text, diagnostic.range),
                severity,
                Some(NumberOrString::String(diagnostic.code.as_str().to_owned())),
                Some("pdx-analysis".to_owned()),
                diagnostic.message,
                None,
                None,
            );
            // Certainty is current client-facing metadata; internal rule provenance never
            // crosses the LSP boundary.
            let mut metadata = json!({
                "certainty": diagnostic.certainty.as_str(),
            });
            if !fix_data.is_empty() {
                metadata["fixes"] = Value::Array(fix_data);
            }
            value.data = Some(metadata);
            value
        })
        .collect::<Vec<_>>();
    if omitted > 0 {
        values.push(LspDiagnostic::new(
            LspRange::default(),
            Some(DiagnosticSeverity::INFORMATION),
            Some(NumberOrString::String("DiagnosticsTruncated".to_owned())),
            Some("pdx-lsp".to_owned()),
            format!("{omitted} additional diagnostics were omitted"),
            None,
            None,
        ));
    }
    values
}

/// Applies workspace and source-level diagnostic suppressions before publication. Keeping this
/// as a reusable raw-diagnostic pass lets workspace-wide validation aggregate the same filtered
/// categories without duplicating the inline-directive rules.
pub(crate) fn filter_diagnostics_with_ignored_and_overrides(
    diagnostics: Vec<Diagnostic>,
    line_index: &LineIndex,
    text: &str,
    ignored: &HashSet<String>,
    overrides: &BTreeMap<String, Option<Severity>>,
) -> Vec<Diagnostic> {
    let inline_ignored = extract_inline_ignored_codes(text);
    diagnostics
        .into_iter()
        .filter_map(|diagnostic| {
            if ignored.contains(diagnostic.code.as_str()) {
                return None;
            }
            let Some(inline_ignored) = inline_ignored.as_ref() else {
                return apply_severity_override(diagnostic, overrides);
            };
            let Some(line) = line_index
                .position(text, diagnostic.range.start())
                .map(|position| position.line.saturating_add(1))
            else {
                return apply_severity_override(diagnostic, overrides);
            };
            if inline_diagnostic_suppressed(inline_ignored, line, diagnostic.code.as_str()) {
                None
            } else {
                apply_severity_override(diagnostic, overrides)
            }
        })
        .collect()
}

fn apply_severity_override(
    mut diagnostic: Diagnostic,
    overrides: &BTreeMap<String, Option<Severity>>,
) -> Option<Diagnostic> {
    match overrides.get(diagnostic.code.as_str()) {
        Some(None) => None,
        Some(Some(severity)) => {
            diagnostic.severity = *severity;
            Some(diagnostic)
        }
        None => Some(diagnostic),
    }
}

/// The inline suppression directive accepted by CWTools-compatible source comments.
const INLINE_IGNORE_DIRECTIVE: &str = "cwtools-ignore";

/// Extracts one-based source-line directives without requiring comments to survive parsing.
/// `None` is returned for the common case where the directive substring is absent, avoiding a
/// per-diagnostic allocation on ordinary files.
fn extract_inline_ignored_codes(text: &str) -> Option<HashMap<u32, HashSet<String>>> {
    let needle = INLINE_IGNORE_DIRECTIVE.as_bytes();
    if !text
        .as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
    {
        return None;
    }
    let mut directives = HashMap::new();
    for (line_index, line) in text.lines().enumerate() {
        let mut codes = None;
        for (index, _) in line.match_indices('#') {
            let rest = &line[index + 1..];
            let after = rest.trim_start();
            let Some(head) = after.get(..INLINE_IGNORE_DIRECTIVE.len()) else {
                continue;
            };
            if !head.eq_ignore_ascii_case(INLINE_IGNORE_DIRECTIVE) {
                continue;
            }
            let trailing = after
                .get(INLINE_IGNORE_DIRECTIVE.len()..)
                .unwrap_or_default();
            if trailing
                .chars()
                .next()
                .is_some_and(|character| !character.is_whitespace() && character != '#')
            {
                continue;
            }
            let values = trailing
                .split('#')
                .next()
                .unwrap_or_default()
                .split_whitespace()
                .filter_map(|value| {
                    DiagnosticCode::parse_name(value).map(|code| code.as_str().to_ascii_lowercase())
                })
                .collect::<HashSet<_>>();
            codes = Some(values);
            break;
        }
        if let Some(codes) = codes {
            directives.insert(
                u32::try_from(line_index)
                    .unwrap_or(u32::MAX)
                    .saturating_add(1),
                codes,
            );
        }
    }
    Some(directives)
}

/// A directive on a line suppresses the named category on that line and its immediate neighbours.
/// This supports both trailing comments and a standalone comment beside the offending line.
fn inline_diagnostic_suppressed(
    directives: &HashMap<u32, HashSet<String>>,
    line: u32,
    code: &str,
) -> bool {
    if line == 0 || code.is_empty() {
        return false;
    }
    let code = code.to_ascii_lowercase();
    let start = line.saturating_sub(1);
    let end = line.saturating_add(1);
    let mut candidate = start;
    while candidate <= end {
        if directives
            .get(&candidate)
            .is_some_and(|codes| codes.contains(&code))
        {
            return true;
        }
        if candidate == u32::MAX {
            break;
        }
        candidate = candidate.saturating_add(1);
    }
    false
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
        CompletionKind::Command => CompletionItemKind::METHOD,
        // LSP has no dedicated macro kind; scripted macros are callable definitions.
        CompletionKind::ScriptedMacro => CompletionItemKind::FUNCTION,
        CompletionKind::Value => CompletionItemKind::VALUE,
        CompletionKind::EnumMember => CompletionItemKind::ENUM_MEMBER,
        CompletionKind::Scope => CompletionItemKind::VARIABLE,
        CompletionKind::Symbol => CompletionItemKind::FUNCTION,
        CompletionKind::Localisation => CompletionItemKind::REFERENCE,
        CompletionKind::MacroParameter => CompletionItemKind::VARIABLE,
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
    use std::collections::{BTreeMap, HashSet};

    use super::{
        LineIndex, completion_kind, diagnostic_values_for_text,
        diagnostic_values_for_text_with_ignored,
        diagnostic_values_for_text_with_ignored_and_overrides, is_snapshot_request,
    };
    use lsp_types::request::Request;
    use lsp_types::request::{
        CodeActionRequest, Completion, DocumentSymbolRequest, Formatting, GotoDefinition,
        HoverRequest, InlayHintRequest, PrepareRenameRequest, References, Rename,
        ResolveCompletionItem, SemanticTokensFullDeltaRequest, SemanticTokensFullRequest,
        SemanticTokensRangeRequest, WorkspaceSymbolRequest,
    };
    use lsp_types::{CompletionItemKind, NumberOrString};
    use pdx_analysis::{
        CompletionKind, Diagnostic, DiagnosticCode, DiagnosticProvenance, QuickFix, Severity,
    };
    use pdx_text::TextRange;

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
            SemanticTokensFullDeltaRequest::METHOD,
            SemanticTokensRangeRequest::METHOD,
            InlayHintRequest::METHOD,
            HoverRequest::METHOD,
            GotoDefinition::METHOD,
            References::METHOD,
            PrepareRenameRequest::METHOD,
            Rename::METHOD,
            CodeActionRequest::METHOD,
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

    #[test]
    fn completion_kinds_preserve_semantic_icon_categories() {
        assert_eq!(
            completion_kind(CompletionKind::Key),
            CompletionItemKind::PROPERTY
        );
        assert_eq!(
            completion_kind(CompletionKind::Command),
            CompletionItemKind::METHOD
        );
        assert_eq!(
            completion_kind(CompletionKind::ScriptedMacro),
            CompletionItemKind::FUNCTION
        );
        assert_eq!(
            completion_kind(CompletionKind::Value),
            CompletionItemKind::VALUE
        );
        assert_eq!(
            completion_kind(CompletionKind::EnumMember),
            CompletionItemKind::ENUM_MEMBER
        );
        assert_eq!(
            completion_kind(CompletionKind::Scope),
            CompletionItemKind::VARIABLE
        );
        assert_eq!(
            completion_kind(CompletionKind::Symbol),
            CompletionItemKind::FUNCTION
        );
        assert_eq!(
            completion_kind(CompletionKind::Localisation),
            CompletionItemKind::REFERENCE
        );
        assert_eq!(
            completion_kind(CompletionKind::MacroParameter),
            CompletionItemKind::VARIABLE
        );
    }

    #[test]
    fn lsp_diagnostics_do_not_expose_internal_rule_provenance() {
        let diagnostic = Diagnostic::new(
            DiagnosticCode::UnknownKey,
            Severity::Error,
            TextRange::new(0, 1).expect("range"),
            "unknown key".to_owned(),
        )
        .with_fix(QuickFix::replace(
            "Rename key".to_owned(),
            TextRange::new(0, 1).expect("fix range"),
            "known_key".to_owned(),
        ))
        .with_provenance(DiagnosticProvenance {
            rule_id: Some("internal:rule".to_owned()),
            context: Some("internal-context".to_owned()),
            source_file: Some("internal.json".to_owned()),
            source_line: Some(7),
        });
        let values = diagnostic_values_for_text(vec![diagnostic], &LineIndex::new("x"), "x");
        let value = serde_json::to_value(&values[0]).expect("diagnostic JSON");

        assert_eq!(value["data"]["certainty"], "certain");
        assert_eq!(value["data"]["fixes"][0]["title"], "Rename key");
        assert_eq!(value["data"]["fixes"][0]["newText"], "known_key");
        assert!(value["data"].get("legacyCode").is_none());
        assert!(value["data"].get("provenance").is_none());
        let serialized = value.to_string();
        assert!(!serialized.contains("ruleId"));
        assert!(!serialized.contains("internal.json"));
    }

    #[test]
    fn ignored_diagnostic_codes_are_filtered_before_publication_limits() {
        let ignored = HashSet::from(["UnknownKey".to_owned()]);
        let diagnostics = vec![
            Diagnostic::new(
                DiagnosticCode::UnknownKey,
                Severity::Error,
                TextRange::new(0, 1).expect("range"),
                "hidden".to_owned(),
            ),
            Diagnostic::new(
                DiagnosticCode::UnknownSymbol,
                Severity::Error,
                TextRange::new(0, 1).expect("range"),
                "visible".to_owned(),
            ),
        ];
        let values = diagnostic_values_for_text_with_ignored(
            diagnostics,
            &LineIndex::new("x"),
            "x",
            &ignored,
        );
        assert_eq!(values.len(), 1);
        assert_eq!(
            values[0].code,
            Some(NumberOrString::String("UnknownSymbol".to_owned()))
        );
    }

    #[test]
    fn severity_overrides_remap_and_suppress_before_lsp_conversion() {
        let diagnostics = vec![
            Diagnostic::new(
                DiagnosticCode::UnknownScope,
                Severity::Error,
                TextRange::new(0, 1).expect("range"),
                "downgraded".to_owned(),
            ),
            Diagnostic::new(
                DiagnosticCode::UnknownKey,
                Severity::Error,
                TextRange::new(1, 2).expect("range"),
                "hidden".to_owned(),
            ),
        ];
        let overrides = BTreeMap::from([
            ("UnknownScope".to_owned(), Some(Severity::Warning)),
            ("UnknownKey".to_owned(), None),
        ]);
        let values = diagnostic_values_for_text_with_ignored_and_overrides(
            diagnostics,
            &LineIndex::new("xy"),
            "xy",
            &HashSet::new(),
            &overrides,
        );
        assert_eq!(values.len(), 1);
        assert_eq!(
            values[0].severity,
            Some(lsp_types::DiagnosticSeverity::WARNING)
        );
        assert_eq!(
            values[0].code,
            Some(NumberOrString::String("UnknownScope".to_owned()))
        );
    }

    #[test]
    fn inline_cwtools_ignore_covers_adjacent_lines_and_known_codes_only() {
        let text =
            "scope = nowhere\n😀 # cwtools-ignore UnknownScope UnknownKey # note\nother = value\n";
        let directive = u32::try_from(text.find("# cwtools-ignore").expect("directive"))
            .expect("directive offset");
        let below = u32::try_from(text.find("other").expect("below line")).expect("below offset");
        let diagnostics = vec![
            Diagnostic::new(
                DiagnosticCode::UnknownScope,
                Severity::Error,
                TextRange::new(0, 5).expect("range"),
                "above".to_owned(),
            ),
            Diagnostic::new(
                DiagnosticCode::UnknownScope,
                Severity::Error,
                TextRange::new(directive, directive + 1).expect("range"),
                "same".to_owned(),
            ),
            Diagnostic::new(
                DiagnosticCode::UnknownKey,
                Severity::Error,
                TextRange::new(below, below + 5).expect("range"),
                "below".to_owned(),
            ),
            Diagnostic::new(
                DiagnosticCode::UnknownSymbol,
                Severity::Error,
                TextRange::new(below, below + 5).expect("range"),
                "visible".to_owned(),
            ),
        ];
        let values = diagnostic_values_for_text_with_ignored(
            diagnostics,
            &LineIndex::new(text),
            text,
            &HashSet::new(),
        );
        assert_eq!(values.len(), 1);
        assert_eq!(
            values[0].code,
            Some(NumberOrString::String("UnknownSymbol".to_owned()))
        );
    }

    #[test]
    fn inline_cwtools_ignore_does_not_match_partial_directive_words() {
        let text = "scope = nowhere # cwtools-ignore-typo UnknownScope\n";
        let diagnostics = vec![Diagnostic::new(
            DiagnosticCode::UnknownScope,
            Severity::Error,
            TextRange::new(0, 5).expect("range"),
            "visible".to_owned(),
        )];
        let values = diagnostic_values_for_text_with_ignored(
            diagnostics,
            &LineIndex::new(text),
            text,
            &HashSet::new(),
        );
        assert_eq!(values.len(), 1);
    }
}
