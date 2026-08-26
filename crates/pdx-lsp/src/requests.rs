use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use lsp_types::{
    CompletionItem, CompletionList, CompletionResponse, CompletionTextEdit,
    DocumentFormattingParams, DocumentSymbol as LspDocumentSymbol, DocumentSymbolParams,
    Documentation, Hover as LspHover, HoverContents, InsertTextFormat, MarkupContent, MarkupKind,
    PrepareRenameResponse, ReferenceParams, RenameParams, SemanticTokens as LspSemanticTokens,
    SemanticTokensParams, SymbolInformation, TextDocumentPositionParams, TextEdit, Uri,
    WorkspaceEdit, WorkspaceSymbolParams,
};
use pdx_analysis::{
    CancellationToken, Cancelled, CompletionKind, SemanticToken, SemanticTokenType,
    complete_with_cancellation, completion_resolve, definition_with_cancellation,
    document_symbols_with_cancellation, hover_with_cancellation, localisation_values_by_key,
    prepare_rename_with_cancellation, references_with_cancellation, rename_with_cancellation,
    semantic_tokens_with_cancellation, source_file_diagnostics_with_cancellation,
    text_diagnostics_with_cancellation, workspace_symbols_with_cancellation,
};
use pdx_engine::{AnalysisSnapshot, DocumentId, ParsedSource, SourceRootKind};
use pdx_game::eu4::mission::Severity;
use pdx_game::eu4::mission::geometry::{self, ArrowGlyph};
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

fn glyph_name(glyph: ArrowGlyph) -> &'static str {
    match glyph {
        ArrowGlyph::VerticalTile => "verticalTile",
        ArrowGlyph::VerticalSkipTier => "verticalSkipTier",
        ArrowGlyph::HorizontalSkipSlot => "horizontalSkipSlot",
        ArrowGlyph::LeftOut => "leftOut",
        ArrowGlyph::LeftIn => "leftIn",
        ArrowGlyph::RightOut => "rightOut",
        ArrowGlyph::RightIn => "rightIn",
        ArrowGlyph::End => "end",
    }
}

const DEFAULT_WORKSPACE_DIAGNOSTIC_FILES: usize = 16;
const MAX_CLASSIFIED_PATHS: usize = 4_096;
const MAX_WORKSPACE_FILES: usize = 32_768;
const MAX_TEXT_DIAGNOSTIC_FILES: usize = 16;
const MAX_TEXT_DIAGNOSTIC_BYTES: usize = 16 * 1024 * 1024;

fn completion_sort_text(sort_score: u32, ordinal: usize) -> String {
    // `ordinal` preserves the analysis order when a client (such as VS Code) receives several
    // candidates with the same packed rank and applies its own label tie-breaker.
    format!("{sort_score:08}{ordinal:04}")
}

#[cfg(test)]
mod completion_sort_tests {
    use super::completion_sort_text;

    #[test]
    fn completion_sort_text_orders_rank_before_response_ordinal() {
        assert_eq!(completion_sort_text(22_010_000, 7), "220100000007");
        assert!(completion_sort_text(22_010_000, 0) < completion_sort_text(22_010_000, 1));
        assert!(completion_sort_text(22_010_000, 511) < completion_sort_text(22_010_001, 0));
    }
}

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

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MissionPreviewParams {
    path: String,
    text: String,
    /// The client document identity is echoed into the preview payload so a click can never
    /// accidentally navigate the currently focused, but unrelated, editor.
    uri: Option<String>,
    /// The client document version captured together with `text`.  The VS Code client uses this
    /// to discard a response that was computed for an older edit.
    version: Option<i32>,
}

#[derive(Clone, Debug)]
pub(crate) struct SnapshotRequestContext {
    snapshot: AnalysisSnapshot,
    cancellation: CancellationToken,
    /// Whether the client advertises snippet support for completion items.
    client_snippets: bool,
    /// Game sprite textures for the mission preview, when a game installation
    /// is available. `None` renders a texture-less preview.
    textures: Option<Arc<pdx_game::eu4::mission::TextureAssets>>,
}

impl SnapshotRequestContext {
    pub(crate) fn new(
        snapshot: AnalysisSnapshot,
        cancellation: CancellationToken,
        client_snippets: bool,
        textures: Option<Arc<pdx_game::eu4::mission::TextureAssets>>,
    ) -> Self {
        Self {
            snapshot,
            cancellation,
            client_snippets,
            textures,
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
            "textDocument/semanticTokens/full" => self.semantic_tokens(params),
            "textDocument/formatting" => self.formatting(params),
            "workspace/symbol" => self.workspace_symbols(params),
            "pdx/workspaceDiagnostics" => self.workspace_diagnostics(params),
            "pdx/workspaceFiles" => self.workspace_files(params),
            "pdx/classifyPaths" => self.classify_paths(params),
            "pdx/textDiagnostics" => self.text_diagnostics(params),
            "pdx/missionPreview" => self.mission_preview(params),
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

    /// Mission-tree preview for caller-supplied document text: the same
    /// literal grid layout and EMT arrow geometry the game uses, returned as
    /// renderer-ready world coordinates and UTF-16 source ranges. This is the data
    /// behind the VSCode mission-tree webview; renderers never recompute
    /// layout semantics.
    fn mission_preview(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let params = typed_params::<MissionPreviewParams>(params, "mission preview")?;
        if params.text.is_empty() {
            return Err(RpcError::new(
                INVALID_PARAMS,
                "mission preview requires document text",
            ));
        }
        if params.text.len() > MAX_TEXT_DIAGNOSTIC_BYTES {
            return Err(RpcError::new(
                INVALID_PARAMS,
                format!("mission preview is limited to {MAX_TEXT_DIAGNOSTIC_BYTES} bytes"),
            ));
        }
        self.ensure_active()?;
        if !self.snapshot.game_profile().allows_scan_file(&params.path) {
            return Err(RpcError::new(
                INVALID_PARAMS,
                format!("path is outside the active game profile: {}", params.path),
            ));
        }
        let loaded = pdx_game::eu4::mission::parse_file(&params.text);
        let file = &loaded.file;
        let layout = geometry::layout_file(file);
        let diagnostics = pdx_game::eu4::mission::validate(file);
        let line_index = LineIndex::new(&params.text);

        // Resolve all `{mission_id}_title` keys in one workspace pass with the
        // same English-preferring symbol resolution as hover; missing keys
        // simply fall back to the raw id in the renderer.
        // Different nodes can produce the same localisation key (and malformed
        // files may repeat ids). Deduplicating here avoids resolving the same
        // definition repeatedly while preserving the response for every node.
        let mut title_keys = Vec::with_capacity(layout.len());
        let mut seen_title_keys = HashSet::with_capacity(layout.len());
        for pos in &layout {
            let key = format!(
                "{}_title",
                file.trees[pos.tree_index].missions[pos.mission_index].id
            );
            if seen_title_keys.insert(key.clone()) {
                title_keys.push(key);
            }
        }
        let title_refs = title_keys.iter().map(String::as_str).collect::<Vec<_>>();
        let titles = localisation_values_by_key(&self.snapshot, &title_refs, &self.cancellation)
            .map_err(cancelled_error)?;

        // Build the mission-id and diagnostic lookup sets once. The previous
        // per-node closures scanned every tree and every diagnostic, which made
        // preview latency grow quadratically on large mission files.
        let in_file: HashSet<&str> = file
            .trees
            .iter()
            .flat_map(|tree| tree.missions.iter().map(|mission| mission.id.as_str()))
            .collect();
        let mut error_missions = HashSet::new();
        let mut warning_missions = HashSet::new();
        for diagnostic in &diagnostics {
            let Some(mission) = diagnostic.mission.as_deref() else {
                continue;
            };
            match diagnostic.severity {
                Severity::Error => {
                    error_missions.insert(mission);
                }
                Severity::Warning => {
                    warning_missions.insert(mission);
                }
            }
        }

        let nodes = layout
            .iter()
            .map(|pos| {
                let tree = &file.trees[pos.tree_index];
                let mission = &tree.missions[pos.mission_index];
                let (x, y) = geometry::world_position(pos);
                let is_root = mission
                    .required
                    .iter()
                    .all(|required| !in_file.contains(required.as_str()));
                // The game renders `{mission_id}_title`; resolve it through the
                // active workspace localisation definition so mod overrides and
                // Vanilla keys both work. The raw id remains as the fallback.
                let title_key = format!("{}_title", mission.id);
                let title = titles.get(&title_key).map(
                    |(language, value)| serde_json::json!({ "language": language, "value": value }),
                );
                let source_range =
                    line_index
                        .position_range(&params.text, mission.span)
                        .map(|range| {
                            serde_json::json!({
                                "start": {
                                    "line": range.start.line,
                                    "character": range.start.character,
                                },
                                "end": {
                                    "line": range.end.line,
                                    "character": range.end.character,
                                },
                            })
                        });
                serde_json::json!({
                    "tree": pos.tree_index,
                    "mission": pos.mission_index,
                    "id": mission.id,
                    "icon": mission.icon,
                    "titleKey": title_key,
                    "title": title,
                    "required": mission.required,
                    "x": x,
                    "y": y,
                    "sourceRange": source_range,
                    "isRoot": is_root,
                    "hasError": error_missions.contains(mission.id.as_str()),
                    "hasWarning": warning_missions.contains(mission.id.as_str()),
                })
            })
            .collect::<Vec<_>>();

        let segments = geometry::arrow_geometry(file, &layout);
        let arrows = segments
            .iter()
            .map(|segment| {
                serde_json::json!({
                    "glyph": glyph_name(segment.glyph),
                    "texture": pdx_game::eu4::mission::arrow_sprite_name(glyph_name(segment.glyph)),
                    "x": segment.x,
                    "y": segment.y,
                })
            })
            .collect::<Vec<_>>();

        // Game sprites the renderer needs: the mission frame, every node icon,
        // and every arrow glyph, deduplicated and resolved to data URLs.
        let mut wanted = vec![pdx_game::eu4::mission::FRAME_SPRITE];
        wanted.extend(layout.iter().filter_map(|pos| {
            file.trees[pos.tree_index].missions[pos.mission_index]
                .icon
                .as_deref()
        }));
        wanted.extend(segments.iter().filter_map(|segment| {
            pdx_game::eu4::mission::arrow_sprite_name(glyph_name(segment.glyph))
        }));
        wanted.sort_unstable();
        wanted.dedup();
        let mut textures = serde_json::Map::new();
        if let Some(assets) = &self.textures {
            for name in wanted {
                if let Some(url) = assets.data_url(name) {
                    textures.insert(name.to_owned(), Value::String(url));
                }
            }
        }

        // Group labels above each column, stacked for same-column groups —
        // identical placement to the editor canvas.
        let mut per_column: HashMap<u32, Vec<usize>> = HashMap::new();
        for (i, tree) in file.trees.iter().enumerate() {
            per_column.entry(tree.slot).or_default().push(i);
        }
        let mut columns: Vec<(u32, Vec<usize>)> = per_column.into_iter().collect();
        columns.sort_by_key(|(slot, _)| *slot);
        let mut groups = Vec::new();
        for (slot, trees) in columns {
            for (i, tree_index) in trees.iter().enumerate() {
                let tree = &file.trees[*tree_index];
                let source_range =
                    line_index
                        .position_range(&params.text, tree.span)
                        .map(|range| {
                            serde_json::json!({
                                "start": {
                                    "line": range.start.line,
                                    "character": range.start.character,
                                },
                                "end": {
                                    "line": range.end.line,
                                    "character": range.end.character,
                                },
                            })
                        });
                groups.push(serde_json::json!({
                    "tree": *tree_index,
                    "label": tree.id,
                    "x": geometry::ORIGIN.0
                        + (slot - 1) as f32 * (geometry::NODE_WIDTH + geometry::GAP_X),
                    "y": geometry::ORIGIN.1 - 30.0 - i as f32 * 18.0,
                    "sourceRange": source_range,
                }));
            }
        }

        // Cross-file prerequisite stubs ("↥ id" above the dependent node).
        let mut external = Vec::new();
        for (tree_index, tree) in file.trees.iter().enumerate() {
            for (mission_index, mission) in tree.missions.iter().enumerate() {
                for required in &mission.required {
                    if !in_file.contains(required.as_str()) {
                        external.push(serde_json::json!({
                            "tree": tree_index,
                            "mission": mission_index,
                            "label": required,
                        }));
                    }
                }
            }
        }

        let diagnostics = diagnostics
            .iter()
            .map(|d| {
                serde_json::json!({
                    "severity": if d.severity == Severity::Error { 1 } else { 2 },
                    "code": d.code,
                    "message": d.message,
                    "tree": d.tree,
                    "mission": d.mission,
                })
            })
            .collect::<Vec<_>>();

        Ok(serde_json::json!({
            "documentUri": params.uri,
            "documentVersion": params.version,
            "nodes": nodes,
            "arrows": arrows,
            "groups": groups,
            "external": external,
            "diagnostics": diagnostics,
            "textures": textures,
        }))
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

    /// Returns the immutable source-root/file view used by the VS Code Explorer contribution.
    /// The response contains no file contents: it is only a stable, read-only navigation model
    /// for current Mod, dependency, and Vanilla roots.
    fn workspace_files(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        if params.is_some() {
            // Keep this request intentionally parameterless so clients cannot turn it into an
            // unbounded file enumeration protocol.
            return Err(RpcError::new(
                INVALID_PARAMS,
                "workspace files does not accept parameters",
            ));
        }
        self.ensure_active()?;
        let roots = self
            .snapshot
            .source_roots()
            .iter()
            .map(|root| {
                let kind = match root.kind {
                    SourceRootKind::Vanilla => "vanilla",
                    SourceRootKind::Dependency => "dependency",
                    SourceRootKind::CurrentMod => "currentMod",
                };
                serde_json::json!({
                    "id": root.id.get(),
                    "kind": kind,
                    "path": root.path,
                    "order": root.order,
                    "writable": root.writable,
                })
            })
            .collect::<Vec<_>>();
        let mut files = self.snapshot.source_files().values().collect::<Vec<_>>();
        if files.len() > MAX_WORKSPACE_FILES {
            return Err(RpcError::new(
                INVALID_PARAMS,
                format!("workspace files are limited to {MAX_WORKSPACE_FILES} entries"),
            ));
        }
        files.sort_by(|left, right| {
            left.root_id
                .cmp(&right.root_id)
                .then_with(|| left.logical_path.as_str().cmp(right.logical_path.as_str()))
                .then_with(|| left.physical_path.cmp(&right.physical_path))
        });
        let mut resolved_paths = HashSet::new();
        let mut active_files = HashSet::new();
        for file in &files {
            if resolved_paths.insert(file.logical_path.clone()) {
                for candidate in self.snapshot.resolve(&file.logical_path) {
                    if candidate.active
                        && let Some(file_id) = candidate.file_id
                    {
                        active_files.insert(file_id);
                    }
                }
            }
        }
        let items = files
            .into_iter()
            .map(|file| {
                let active = active_files.contains(&file.id);
                serde_json::json!({
                    "id": file.id.get(),
                    "rootId": file.root_id.get(),
                    "logicalPath": file.logical_path.as_str(),
                    "uri": path_to_uri(&file.physical_path),
                    "category": file.category_id,
                    "active": active,
                })
            })
            .collect::<Vec<_>>();
        Ok(serde_json::json!({
            "revision": self.snapshot.revision(),
            "roots": roots,
            "files": items,
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
            .enumerate()
            .map(|(ordinal, item)| {
                let snippet_supported = self.client_snippets;
                let insert_text = if snippet_supported {
                    item.insert_text
                } else {
                    let start = usize::try_from(item.replacement_range.start())
                        .unwrap_or(0)
                        .min(document.text().len());
                    let base_indent = line_base_indent(document.text(), start);
                    strip_snippet_placeholders(&item.insert_text, &base_indent)
                };
                CompletionItem {
                    label: item.label,
                    kind: Some(completion_kind(item.kind)),
                    detail: Some(item.detail),
                    documentation: item.documentation.map(Documentation::String),
                    deprecated: Some(item.deprecated),
                    // `sort_score` is a packed lexicographic rank. The ordinal makes the
                    // fixed-width sort key unique within a response, so clients cannot replace
                    // the analysis tie-break with a case-sensitive label comparison.
                    sort_text: Some(completion_sort_text(item.sort_score, ordinal)),
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

    #[expect(clippy::mutable_key_type)] // URI is the protocol's stable document key; lsp_types::Uri contains an internal parse cache.
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

    #[expect(deprecated)] // LSP response types retain this optional field for wire-shape completeness.
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

    fn semantic_tokens(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let params = typed_params::<SemanticTokensParams>(params, "semantic tokens")?;
        let id = DocumentId::new(params.text_document.uri.as_str());
        let document = self
            .snapshot
            .document(&id)
            .ok_or_else(|| RpcError::new(INVALID_PARAMS, "document is not open"))?;
        let result = semantic_tokens_with_cancellation(&self.snapshot, &id, &self.cancellation)
            .map_err(cancelled_error)?;
        self.ensure_active()?;
        let line_index = document.line_index();
        let text = document.text();
        let data = encode_semantic_tokens(line_index, text, &result);
        typed_value(
            LspSemanticTokens {
                result_id: None,
                data,
            },
            "semantic tokens response",
        )
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

    #[expect(deprecated)] // LSP response types retain this optional field for wire-shape completeness.
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

/// Encodes editor-neutral semantic tokens as LSP relative-encoding rows
/// (delta line, delta character, length, token type index, modifier bitmask).
/// Tokens are guaranteed single-line; any defensive multi-line token is skipped.
pub(crate) fn encode_semantic_tokens(
    line_index: &LineIndex,
    text: &str,
    tokens: &[SemanticToken],
) -> Vec<lsp_types::SemanticToken> {
    let mut data = Vec::with_capacity(tokens.len());
    let mut previous_line = 0u32;
    let mut previous_start = 0u32;
    for token in tokens {
        let (Some(start), Some(end)) = (
            line_index.position(text, token.range.start()),
            line_index.position(text, token.range.end()),
        ) else {
            continue;
        };
        if start.line != end.line {
            continue;
        }
        let delta_line = start.line.saturating_sub(previous_line);
        let delta_start = if start.line == previous_line {
            start.character.saturating_sub(previous_start)
        } else {
            start.character
        };
        let length = end.character.saturating_sub(start.character);
        data.push(lsp_types::SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type: semantic_token_type_index(token.token_type),
            token_modifiers_bitset: u32::from(token.definition),
        });
        previous_line = start.line;
        previous_start = start.character;
    }
    data
}

fn semantic_token_type_index(token_type: SemanticTokenType) -> u32 {
    SemanticTokenType::ALL
        .iter()
        .position(|candidate| *candidate == token_type)
        .map_or(0, |index| u32::try_from(index).expect("legend fits u32"))
}

/// Removes LSP snippet placeholders (`$0`, `$1`, …) so a snippet-shaped insert text can be
/// delivered as plain text to clients without snippet support. Placeholder lines left empty by
/// the removal are dropped so the block skeleton stays tidy.
///
/// Snippet bodies carry relative indentation only, because snippet-capable clients re-indent
/// multi-line snippets to the insertion line. A plain-text edit has no such re-indenting, so the
/// absolute leading whitespace of the insertion line is re-applied to every continuation line.
pub(crate) fn strip_snippet_placeholders(text: &str, base_indent: &str) -> String {
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
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            if index == 0 {
                line.to_owned()
            } else {
                format!("{base_indent}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Returns the leading spaces and tabs on the line containing a byte offset.
fn line_base_indent(source: &str, position: usize) -> String {
    let mut position = position.min(source.len());
    while position > 0 && !source.is_char_boundary(position) {
        position -= 1;
    }
    let line_start = source[..position].rfind('\n').map_or(0, |index| index + 1);
    let prefix = &source[line_start..position];
    if prefix.bytes().all(|byte| matches!(byte, b' ' | b'\t')) {
        prefix.to_owned()
    } else {
        String::new()
    }
}
