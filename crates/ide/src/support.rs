use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use engine::{AnalysisSnapshot, DocumentId, DocumentSource, ParsedSource, SourceFileId};
use hir::HirFile;
use hir::lower_shared_with_ir_and_facts;
use parser::{CstKind, CstNode, FileFormat, ParsedFile, parse};
use rules::{GameProfile, ParserKind};
use text::{LogicalPath, TextRange, TextSize};

use crate::types::*;

#[derive(Clone, Debug)]
pub(crate) struct ParsedInput {
    pub(crate) document: Option<DocumentId>,
    pub(crate) file: Option<SourceFileId>,
    pub(crate) path: Option<LogicalPath>,
    pub(crate) format: FileFormat,
    pub(crate) source: Arc<str>,
    pub(crate) parsed: ParsedContent,
    pub(crate) hir: Option<Arc<HirFile>>,
    pub(crate) profile: Arc<GameProfile>,
}

#[derive(Clone, Debug)]
pub(crate) enum ParsedContent {
    Text(Arc<ParsedFile>),
}

impl ParsedInput {
    pub(crate) fn source_text(&self, range: TextRange) -> Option<&str> {
        let start = usize::try_from(range.start()).ok()?;
        let end = usize::try_from(range.end()).ok()?;
        self.source.get(start..end)
    }
}

pub(crate) fn input_for_document(
    snapshot: &AnalysisSnapshot,
    id: &DocumentId,
) -> Option<ParsedInput> {
    let document = snapshot.document(id)?;
    let path = document
        .path()
        .and_then(|path| logical_path(snapshot, path))
        .or_else(|| snapshot.rules().logical_path_for_uri(id.as_str()))
        .or_else(|| {
            id.as_str()
                .split(['/', '\\'])
                .next_back()
                .filter(|name| name.contains('.'))
                .and_then(|name| LogicalPath::parse(name).ok())
        });
    let file = document
        .path()
        .and_then(|path| snapshot.source_file_id_for_path(path));
    let source = document.text_handle();
    let parsed = document.parsed()?;
    let format = parsed.format();
    let parsed = match parsed {
        ParsedSource::Text(parsed) => ParsedContent::Text(Arc::clone(parsed)),
    };
    let hir = if let Some(hir) = document
        .hir_handle()
        .filter(|hir| !hir.depends_on_symbol_facts())
    {
        Some(hir)
    } else if !snapshot.ir().schemas.is_empty() {
        let ParsedContent::Text(parsed) = &parsed;
        path.as_ref().map(|path| {
            lower_for_snapshot(
                snapshot,
                Arc::clone(parsed),
                path,
                Some(&format!("ir-hir:{}", id.as_str())),
                true,
            )
        })
    } else {
        document.hir_handle()
    };
    let profile = snapshot.game_profile_handle();
    Some(ParsedInput {
        document: Some(id.clone()),
        file,
        path,
        format,
        source,
        parsed,
        hir,
        profile,
    })
}

pub(crate) fn input_for_source_file(
    snapshot: &AnalysisSnapshot,
    id: SourceFileId,
) -> Option<ParsedInput> {
    source_file_input(snapshot, id, true)
}

/// Borrows an existing interactive frontend but does not retain a new HIR for
/// a diagnostics-only workspace sweep.
pub(crate) fn diagnostic_input_for_source_file(
    snapshot: &AnalysisSnapshot,
    id: SourceFileId,
) -> Option<ParsedInput> {
    source_file_input(snapshot, id, false)
}

fn source_file_input(
    snapshot: &AnalysisSnapshot,
    id: SourceFileId,
    retain_frontend: bool,
) -> Option<ParsedInput> {
    let mut input = syntax_input_for_source_file(snapshot, id)?;
    let state = snapshot.file_state(id)?;
    let ParsedContent::Text(parsed) = &input.parsed;
    input.hir = if snapshot.ir().schemas.is_empty() && state.parsed().is_some() {
        state.hir_handle()
    } else {
        Some(lower_for_snapshot(
            snapshot,
            Arc::clone(parsed),
            input.path.as_ref()?,
            Some(&format!("ir-hir:file:{}", id.get())),
            retain_frontend,
        ))
    };
    Some(input)
}

/// Loads the same live source and syntax as semantic queries, without lowering
/// HIR for consumers that only inspect the property tree.
pub(crate) fn syntax_input_for_source_file(
    snapshot: &AnalysisSnapshot,
    id: SourceFileId,
) -> Option<ParsedInput> {
    let file = snapshot.source_files().get(&id)?;
    let state = snapshot.file_state(id)?;
    if let Some(ParsedSource::Text(parsed)) = state.parsed() {
        return Some(ParsedInput {
            document: None,
            file: Some(id),
            path: Some(file.logical_path.clone()),
            format: parsed.format(),
            source: state.source_handle(),
            parsed: ParsedContent::Text(Arc::clone(parsed)),
            hir: None,
            profile: snapshot.game_profile_handle(),
        });
    }
    // The scan may evict CST/HIR frontends after background validation to
    // bound resident memory; the source text stays in the file state, so the
    // tree is reparsed transiently for this one query.
    let format = format_for_path(snapshot, &file.logical_path)?;
    let source = state.source_handle();
    // Consult the persistent parse cache before reparsing: entries are
    // validated against the live source hash, and loading beats reparsing
    // by ~9x (see the engine `parse_cache_speed` example). A miss or an
    // unconfigured cache falls back to the plain reparse below.
    let parsed = snapshot
        .parse_cache()
        .and_then(|cache| cache.load(file, format, &source))
        .map_or_else(|| Arc::new(parse(format, &source)), Arc::new);
    Some(ParsedInput {
        document: None,
        file: Some(id),
        path: Some(file.logical_path.clone()),
        format,
        source,
        parsed: ParsedContent::Text(parsed),
        hir: None,
        profile: snapshot.game_profile_handle(),
    })
}

pub(crate) fn input_for_text(
    snapshot: &AnalysisSnapshot,
    path: &LogicalPath,
    text: &str,
) -> Option<ParsedInput> {
    let format = format_for_path(snapshot, path)?;
    let source = Arc::<str>::from(text);
    let parsed = Arc::new(parse(format, &source));
    let hir = Arc::new(lower_shared_with_ir_and_facts(
        Arc::clone(&parsed),
        path,
        snapshot.rules(),
        snapshot.game_profile(),
        snapshot.ir(),
        &crate::ir_queries::SnapshotSymbolFacts { snapshot },
    ));
    let file = snapshot
        .source_files()
        .values()
        .find(|file| file.logical_path == *path)
        .map(|file| file.id);
    Some(ParsedInput {
        document: None,
        file,
        path: Some(path.clone()),
        format,
        source,
        parsed: ParsedContent::Text(parsed),
        hir: Some(hir),
        profile: snapshot.game_profile_handle(),
    })
}

fn lower_for_snapshot(
    snapshot: &AnalysisSnapshot,
    syntax: Arc<ParsedFile>,
    path: &LogicalPath,
    cache_key: Option<&str>,
    retain_frontend: bool,
) -> Arc<HirFile> {
    if let Some(key) = cache_key
        && let Some(cached) = snapshot
            .query_cache()
            .get::<HirFile>(snapshot.revision(), key)
    {
        return cached;
    }
    let hir = Arc::new(lower_shared_with_ir_and_facts(
        syntax,
        path,
        snapshot.rules(),
        snapshot.game_profile(),
        snapshot.ir(),
        &crate::ir_queries::SnapshotSymbolFacts { snapshot },
    ));
    if retain_frontend && let Some(key) = cache_key {
        snapshot.query_cache().insert(
            snapshot.revision(),
            engine::CacheDomain::Frontends,
            key.to_owned(),
            Arc::clone(&hir),
        );
    }
    hir
}

fn format_for_path(snapshot: &AnalysisSnapshot, path: &LogicalPath) -> Option<FileFormat> {
    if !snapshot.ir().files.is_empty() {
        return match snapshot.ir().file_rule(path)?.1.parser {
            rules::ir::DocumentParser::Script => Some(FileFormat::Script),
            rules::ir::DocumentParser::Localisation => Some(FileFormat::Localisation),
            _ => None,
        };
    }
    match snapshot.rules().classify(path)?.parser {
        ParserKind::Script => Some(FileFormat::Script),
        ParserKind::Localisation => Some(FileFormat::Localisation),
        _ => None,
    }
}

pub(crate) fn logical_path(snapshot: &AnalysisSnapshot, path: &Path) -> Option<LogicalPath> {
    snapshot
        .source_roots()
        .iter()
        .filter_map(|root| path.strip_prefix(&root.path).ok())
        .filter_map(|relative| LogicalPath::parse(&relative.to_string_lossy()).ok())
        .min_by_key(|path| path.as_str().len())
        .or_else(|| {
            snapshot
                .rules()
                .logical_path_for_uri(&path.to_string_lossy())
        })
        .or_else(|| LogicalPath::parse(&path.to_string_lossy()).ok())
        .or_else(|| {
            path.file_name()
                .and_then(|name| LogicalPath::parse(&name.to_string_lossy()).ok())
        })
}
/// Structural properties used by the callable cycle graph.
#[derive(Clone, Debug)]
pub(crate) struct ScriptProperty {
    pub(crate) key: Arc<str>,
    pub(crate) key_range: TextRange,
    pub(crate) scalar: Option<(Arc<str>, TextRange)>,
    pub(crate) block: Vec<ScriptProperty>,
}

pub(crate) fn script_properties(input: &ParsedInput, parent: CstNode<'_>) -> Vec<ScriptProperty> {
    let ParsedContent::Text(parsed) = &input.parsed;
    parent
        .children()
        .filter(|node| node.kind() == CstKind::Property)
        .filter_map(|node| {
            let key = node.children().find(|child| child.kind() == CstKind::Key)?;
            let block = node
                .children()
                .find(|child| child.kind() == CstKind::Value)
                .and_then(|value| {
                    value
                        .children()
                        .find(|child| child.kind() == CstKind::Block)
                });
            let scalar = property_scalar_node(node).and_then(|scalar| {
                let raw = parsed.text(scalar.range())?.trim();
                let value = raw
                    .strip_prefix('"')
                    .and_then(|value| value.strip_suffix('"'))
                    .unwrap_or(raw);
                Some((engine::intern_shard_string(value), scalar.range()))
            });
            Some(ScriptProperty {
                key: engine::intern_shard_string(parsed.text(key.range())?.trim()),
                key_range: key.range(),
                scalar,
                block: block.map_or_else(Vec::new, |block| script_properties(input, block)),
            })
        })
        .collect()
}

fn property_scalar_node(node: CstNode<'_>) -> Option<CstNode<'_>> {
    node.children()
        .find(|child| child.kind() == CstKind::Value)?
        .children()
        .find(|child| matches!(child.kind(), CstKind::BareValue | CstKind::QuotedString))
}

pub(crate) fn local_location(input: &ParsedInput, range: TextRange) -> Location {
    Location {
        document: input.document.clone(),
        file: input.file,
        path: input.path.clone(),
        range,
    }
}
/// Whether a candidate label matches the typed prefix: case-insensitive prefix or substring.
pub(crate) fn completion_matches(label: &str, prefix: &str) -> bool {
    prefix.is_empty()
        || starts_with_ignore_ascii_case(label, prefix)
        || label
            .as_bytes()
            .windows(prefix.len())
            .any(|window| window.eq_ignore_ascii_case(prefix.as_bytes()))
}

pub(crate) fn starts_with_ignore_ascii_case(value: &str, prefix: &str) -> bool {
    value.len() >= prefix.len()
        && value.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}
pub(crate) fn word_range(source: &str, position: TextSize) -> TextRange {
    let mut offset = usize::try_from(position)
        .unwrap_or(source.len())
        .min(source.len());
    while offset > 0 && !source.is_char_boundary(offset) {
        offset -= 1;
    }
    let mut start = offset;
    while start > 0 && is_word_byte(source.as_bytes()[start - 1]) {
        start -= 1;
    }
    let mut end = offset;
    while end < source.len() && is_word_byte(source.as_bytes()[end]) {
        end += 1;
    }
    TextRange::new(
        u32::try_from(start).unwrap_or(u32::MAX),
        u32::try_from(end).unwrap_or(u32::MAX),
    )
    .unwrap_or_else(|| TextRange::empty(u32::try_from(start).unwrap_or(u32::MAX)))
}

pub(crate) fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'@' | b'$')
}

pub(crate) fn contains(range: TextRange, position: TextSize) -> bool {
    if range.is_empty() {
        position == range.start()
    } else {
        position >= range.start() && position < range.end()
    }
}

pub(crate) fn same_name(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

pub(crate) fn same_location(left: &Location, right: &Location) -> bool {
    left.document == right.document
        && left.file == right.file
        && left.path == right.path
        && left.range == right.range
}

/// Same bounding for localisation previews, which carry full event and description texts;
/// 1000 characters covers the longest vanilla entries while still guarding hovers against
/// pathological content.
pub(crate) fn truncate_localisation_preview(value: &str) -> String {
    truncate_text(value, 1000)
}

fn truncate_text(value: &str, max_chars: usize) -> String {
    let mut truncated = String::new();
    let mut overflow = false;
    for (index, character) in value.chars().enumerate() {
        if index == max_chars {
            overflow = true;
            break;
        }
        truncated.push(character);
    }
    if overflow {
        truncated.push('…');
    }
    truncated
}

pub(crate) fn root_for_path<'a>(
    snapshot: &'a AnalysisSnapshot,
    path: &Path,
) -> Option<&'a engine::SourceRoot> {
    snapshot
        .source_roots()
        .iter()
        .filter(|root| path.strip_prefix(&root.path).is_ok())
        .max_by_key(|root| root.path.as_os_str().len())
}

pub(crate) fn overlay_file_ids(snapshot: &AnalysisSnapshot) -> BTreeSet<SourceFileId> {
    snapshot
        .documents()
        .values()
        .filter(|document| document.source() == DocumentSource::Overlay)
        .filter_map(|document| document.path())
        .filter_map(|path| snapshot.source_file_id_for_path(path))
        .collect()
}
