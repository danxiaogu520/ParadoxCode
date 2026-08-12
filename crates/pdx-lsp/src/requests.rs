use std::collections::HashMap;

use lsp_types::{
    CompletionItem, CompletionList, CompletionResponse, CompletionTextEdit,
    DocumentFormattingParams, DocumentSymbol as LspDocumentSymbol, DocumentSymbolParams,
    Documentation, Hover as LspHover, HoverContents, InsertTextFormat, MarkupContent, MarkupKind,
    PrepareRenameResponse, ReferenceParams, RenameParams, SymbolInformation,
    TextDocumentPositionParams, TextEdit, Uri, WorkspaceEdit, WorkspaceSymbolParams,
};
use pdx_analysis::{
    CancellationToken, Cancelled, CompletionKind, complete_with_cancellation, completion_resolve,
    definition_with_cancellation, document_symbols_with_cancellation, hover_with_cancellation,
    prepare_rename_with_cancellation, references_with_cancellation, rename_with_cancellation,
    source_file_diagnostics_with_cancellation, text_diagnostics_with_cancellation,
    workspace_symbols_with_cancellation,
};
use pdx_engine::{AnalysisSnapshot, DocumentId, ParsedSource, SourceRootKind};
use pdx_parser::format::format;
use pdx_rules::ParserKind;
use pdx_text::{LineIndex, LogicalPath, Position, TextRange};
use serde::Deserialize;
use serde_json::Value;

use crate::protocol::{
    RpcError, cancelled_error, completion_kind, diagnostic_values_for_text, location_range_to_lsp,
    location_to_lsp, range_to_lsp, range_to_lsp_for_location, rename_failure, symbol_kind,
    typed_params, typed_value,
};
use crate::uri::path_to_uri;
use crate::{
    INVALID_PARAMS, MAX_COMPLETION_RESULTS, MAX_WORKSPACE_DIAGNOSTIC_FILES,
    MAX_WORKSPACE_SYMBOL_RESULTS, METHOD_NOT_FOUND,
};

const DEFAULT_WORKSPACE_DIAGNOSTIC_FILES: usize = 16;
const MAX_CLASSIFIED_PATHS: usize = 4_096;
const MAX_TEXT_DIAGNOSTIC_FILES: usize = 16;
const MAX_TEXT_DIAGNOSTIC_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
struct WorkspaceDiagnosticsParams {
    offset: usize,
    limit: Option<usize>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ClassifyPathsParams {
    paths: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct TextDiagnosticInput {
    path: String,
    text: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct TextDiagnosticsParams {
    files: Vec<TextDiagnosticInput>,
}

#[derive(Clone, Debug)]
pub(crate) struct SnapshotRequestContext {
    snapshot: AnalysisSnapshot,
    cancellation: CancellationToken,
    /// Whether the client advertises snippet support for completion items.
    client_snippets: bool,
}

impl SnapshotRequestContext {
    pub(crate) fn new(
        snapshot: AnalysisSnapshot,
        cancellation: CancellationToken,
        client_snippets: bool,
    ) -> Self {
        Self {
            snapshot,
            cancellation,
            client_snippets,
        }
    }

    pub(crate) fn dispatch(&self, method: &str, params: Option<&Value>) -> Result<Value, RpcError> {
        match method {
            "textDocument/completion" => self.completion(params),
            "completionItem/resolve" => self.completion_resolve(params),
            "textDocument/hover" => self.hover(params),
            "textDocument/definition" => self.definition(params),
            "textDocument/references" => self.references(params),
            "textDocument/prepareRename" => self.prepare_rename(params),
            "textDocument/rename" => self.rename(params),
            "textDocument/documentSymbol" => self.document_symbols(params),
            "textDocument/formatting" => self.formatting(params),
            "workspace/symbol" => self.workspace_symbols(params),
            "pdx/workspaceDiagnostics" => self.workspace_diagnostics(params),
            "pdx/classifyPaths" => self.classify_paths(params),
            "pdx/textDiagnostics" => self.text_diagnostics(params),
            _ => Err(RpcError::new(METHOD_NOT_FOUND, "method is not implemented")),
        }
    }

    fn text_diagnostics(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let params = typed_params::<TextDiagnosticsParams>(params, "text diagnostics")?;
        if params.files.is_empty() || params.files.len() > MAX_TEXT_DIAGNOSTIC_FILES {
            return Err(RpcError::new(
                INVALID_PARAMS,
                format!("text diagnostics require between 1 and {MAX_TEXT_DIAGNOSTIC_FILES} files"),
            ));
        }
        let total_bytes = params
            .files
            .iter()
            .try_fold(0usize, |total, file| total.checked_add(file.text.len()))
            .ok_or_else(|| {
                RpcError::new(INVALID_PARAMS, "text diagnostics payload is too large")
            })?;
        if total_bytes > MAX_TEXT_DIAGNOSTIC_BYTES {
            return Err(RpcError::new(
                INVALID_PARAMS,
                format!("text diagnostics are limited to {MAX_TEXT_DIAGNOSTIC_BYTES} bytes"),
            ));
        }

        let mut results = Vec::with_capacity(params.files.len());
        for file in params.files {
            self.ensure_active()?;
            if !self.snapshot.game_profile().allows_scan_file(&file.path) {
                return Err(RpcError::new(
                    INVALID_PARAMS,
                    format!("path is outside the active game profile: {}", file.path),
                ));
            }
            let logical_path = LogicalPath::parse(&file.path).map_err(|error| {
                RpcError::new(
                    INVALID_PARAMS,
                    format!("invalid logical path {}: {error}", file.path),
                )
            })?;
            let diagnostics = text_diagnostics_with_cancellation(
                &self.snapshot,
                &logical_path,
                &file.text,
                &self.cancellation,
            )
            .map_err(cancelled_error)?;
            let line_index = LineIndex::new(&file.text);
            results.push(serde_json::json!({
                "path": file.path,
                "diagnostics": diagnostic_values_for_text(diagnostics, &line_index, &file.text),
            }));
        }
        Ok(Value::Array(results))
    }

    fn classify_paths(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let params = typed_params::<ClassifyPathsParams>(params, "path classification")?;
        if params.paths.len() > MAX_CLASSIFIED_PATHS {
            return Err(RpcError::new(
                INVALID_PARAMS,
                format!("path classification is limited to {MAX_CLASSIFIED_PATHS} paths"),
            ));
        }
        let mut accepted = Vec::new();
        for path in params.paths {
            self.ensure_active()?;
            if !self.snapshot.game_profile().allows_scan_file(&path) {
                continue;
            }
            let Ok(logical) = LogicalPath::parse(&path) else {
                continue;
            };
            let Some(category) = self.snapshot.rules().classify(&logical) else {
                continue;
            };
            if matches!(
                category.parser,
                ParserKind::Script | ParserKind::Localisation
            ) {
                accepted.push(path);
            }
        }
        typed_value(accepted, "path classification response")
    }

    fn workspace_diagnostics(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let params = params.map_or_else(
            || Ok(WorkspaceDiagnosticsParams::default()),
            |value| {
                typed_params::<WorkspaceDiagnosticsParams>(Some(value), "workspace diagnostics")
            },
        )?;
        let limit = params.limit.unwrap_or(DEFAULT_WORKSPACE_DIAGNOSTIC_FILES);
        if limit == 0 || limit > MAX_WORKSPACE_DIAGNOSTIC_FILES {
            return Err(RpcError::new(
                INVALID_PARAMS,
                format!(
                    "workspace diagnostics limit must be between 1 and {MAX_WORKSPACE_DIAGNOSTIC_FILES}"
                ),
            ));
        }

        let current_root_ids = self
            .snapshot
            .source_roots()
            .iter()
            .filter(|root| root.kind == SourceRootKind::CurrentMod)
            .map(|root| root.id)
            .collect::<std::collections::BTreeSet<_>>();
        let mut files = self
            .snapshot
            .source_files()
            .values()
            .filter(|file| current_root_ids.contains(&file.root_id))
            .filter(|file| {
                self.snapshot
                    .file_state(file.id)
                    .is_some_and(|state| state.parsed().is_some())
            })
            .collect::<Vec<_>>();
        files.sort_by(|left, right| {
            left.logical_path
                .as_str()
                .cmp(right.logical_path.as_str())
                .then_with(|| left.physical_path.cmp(&right.physical_path))
        });
        let total = files.len();
        let end = params.offset.saturating_add(limit).min(total);
        let mut items = Vec::with_capacity(end.saturating_sub(params.offset));
        if params.offset < total {
            for file in &files[params.offset..end] {
                self.ensure_active()?;
                let state = self
                    .snapshot
                    .file_state(file.id)
                    .expect("filtered source file has state");
                let diagnostics = source_file_diagnostics_with_cancellation(
                    &self.snapshot,
                    file.id,
                    &self.cancellation,
                )
                .map_err(cancelled_error)?;
                let line_index = LineIndex::new(state.source());
                items.push(serde_json::json!({
                    "uri": path_to_uri(&file.physical_path),
                    "logicalPath": file.logical_path.as_str(),
                    "diagnostics": diagnostic_values_for_text(
                        diagnostics,
                        &line_index,
                        state.source(),
                    ),
                }));
            }
        }
        self.ensure_active()?;
        Ok(serde_json::json!({
            "offset": params.offset,
            "nextOffset": (end < total).then_some(end),
            "total": total,
            "items": items,
        }))
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
                let snippet_supported = self.client_snippets;
                let insert_text = if snippet_supported {
                    item.insert_text
                } else {
                    strip_snippet_placeholders(&item.insert_text)
                };
                CompletionItem {
                    label: item.label,
                    kind: Some(completion_kind(item.kind)),
                    detail: Some(item.detail),
                    documentation: item.documentation.map(Documentation::String),
                    deprecated: Some(item.deprecated),
                    sort_text: Some(format!("{:03}", item.sort_score)),
                    insert_text: Some(insert_text.clone()),
                    insert_text_format: Some(if snippet_supported && insert_text.contains('$') {
                        InsertTextFormat::SNIPPET
                    } else {
                        InsertTextFormat::PLAIN_TEXT
                    }),
                    data: item.resolve_data.map(Value::String),
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
            CompletionResponse::List(CompletionList {
                is_incomplete,
                items,
            }),
            "completion response",
        )
    }

    fn completion_resolve(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let params = typed_params::<CompletionItem>(params, "completionItem/resolve")?;
        self.ensure_active()?;
        let data = params
            .data
            .as_ref()
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let item = pdx_analysis::CompletionItem {
            label: params.label.clone(),
            kind: CompletionKind::Key,
            detail: String::new(),
            documentation: None,
            replacement_range: TextRange::empty(0),
            insert_text: params.label.clone(),
            sort_score: 0,
            deprecated: false,
            resolve_data: (!data.is_empty()).then_some(data),
        };
        let mut resolved = completion_resolve(&self.snapshot, &item);
        if let Some(documentation) = resolved.documentation.take() {
            let mut result = params;
            result.documentation = Some(Documentation::String(documentation));
            return typed_value(result, "completionItem/resolve response");
        }
        typed_value(params, "completionItem/resolve response")
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
            changes.entry(location.uri).or_default().push(TextEdit {
                range: location.range,
                new_text: edit.new_text,
            });
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
        if self.cancellation.is_cancelled() {
            Err(cancelled_error(Cancelled))
        } else {
            Ok(())
        }
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

pub(crate) fn bounded_results<T>(mut values: Vec<T>, maximum: usize) -> (Vec<T>, bool) {
    let incomplete = values.len() > maximum;
    values.truncate(maximum);
    (values, incomplete)
}

/// Removes LSP snippet placeholders (`$0`, `$1`, …) so a snippet-shaped insert text can be
/// delivered as plain text to clients without snippet support. Placeholder lines left empty by
/// the removal are dropped so the block skeleton stays tidy.
pub(crate) fn strip_snippet_placeholders(text: &str) -> String {
    let mut stripped = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '$' {
            while chars.peek().is_some_and(|next| next.is_ascii_digit()) {
                chars.next();
            }
        } else {
            stripped.push(character);
        }
    }
    stripped
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}
