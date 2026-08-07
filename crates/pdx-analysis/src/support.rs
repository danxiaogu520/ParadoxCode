use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use pdx_engine::hir::HirFile;
use pdx_engine::{AnalysisSnapshot, DocumentId, DocumentSource, ParsedSource, SourceFileId};
use pdx_parser::{CstKind, CstNode, FileFormat, ParsedFile};
use pdx_rules::GameProfile;
use pdx_text::{LogicalPath, TextRange, TextSize};

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
#[derive(Clone, Debug)]
pub(crate) struct PropertyInfo {
    pub(crate) key: String,
    pub(crate) value: Option<(String, TextRange)>,
}

pub(crate) fn input_for_document(
    snapshot: &AnalysisSnapshot,
    id: &DocumentId,
) -> Option<ParsedInput> {
    let document = snapshot.document(id)?;
    let path = document
        .path()
        .and_then(|path| logical_path(snapshot, path))
        .or_else(|| {
            id.as_str()
                .split(['/', '\\'])
                .next_back()
                .filter(|name| name.contains('.'))
                .and_then(|name| LogicalPath::parse(name).ok())
        });
    let file = document
        .path()
        .and_then(|path| {
            snapshot
                .source_files()
                .values()
                .find(|file| file.physical_path == path)
        })
        .map(|file| file.id);
    let source = document.text_handle();
    let parsed = document.parsed()?;
    let format = parsed.format();
    let parsed = match parsed {
        ParsedSource::Text(parsed) => ParsedContent::Text(Arc::clone(parsed)),
    };
    let hir = document.hir_handle();
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
    let file = snapshot.source_files().get(&id)?;
    let state = snapshot.file_state(id)?;
    let parsed = match state.parsed()? {
        ParsedSource::Text(parsed) => ParsedContent::Text(Arc::clone(parsed)),
    };
    Some(ParsedInput {
        document: None,
        file: Some(id),
        path: Some(file.logical_path.clone()),
        format: state.parsed()?.format(),
        source: state.source_handle(),
        parsed,
        hir: state.hir_handle(),
        profile: snapshot.game_profile_handle(),
    })
}

pub(crate) fn logical_path(snapshot: &AnalysisSnapshot, path: &Path) -> Option<LogicalPath> {
    snapshot
        .source_roots()
        .iter()
        .filter_map(|root| path.strip_prefix(&root.path).ok())
        .filter_map(|relative| LogicalPath::parse(&relative.to_string_lossy()).ok())
        .min_by_key(|path| path.as_str().len())
        .or_else(|| {
            path.file_name()
                .and_then(|name| LogicalPath::parse(&name.to_string_lossy()).ok())
        })
}
#[derive(Clone, Debug)]
pub(crate) struct ScriptProperty {
    pub(crate) key: String,
    pub(crate) key_range: TextRange,
    pub(crate) range: TextRange,
    pub(crate) operator: Option<String>,
    pub(crate) scalar: Option<(String, TextRange)>,
    pub(crate) block_range: Option<TextRange>,
    pub(crate) block: Vec<ScriptProperty>,
    pub(crate) bare_values: Vec<(String, TextRange)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ScopeContext {
    pub(crate) profile: Arc<GameProfile>,
    pub(crate) root: String,
    pub(crate) current: String,
    pub(crate) from: Vec<String>,
    pub(crate) previous: Vec<String>,
}

impl ScopeContext {
    pub(crate) fn new(profile: Arc<GameProfile>) -> Self {
        Self {
            profile,
            root: "any".to_owned(),
            current: "any".to_owned(),
            from: Vec::new(),
            previous: Vec::new(),
        }
    }
}
pub(crate) fn script_properties(input: &ParsedInput, parent: &CstNode) -> Vec<ScriptProperty> {
    parent
        .children()
        .iter()
        .filter(|node| node.kind() == CstKind::Property)
        .filter_map(|node| {
            let (key, key_range) = property_key(input, node)?;
            let value = node
                .children()
                .iter()
                .find(|child| child.kind() == CstKind::Value);
            let block_node = value.and_then(|value| {
                value
                    .children()
                    .iter()
                    .find(|child| child.kind() == CstKind::Block)
            });
            let block = block_node.map_or_else(Vec::new, |block| script_properties(input, block));
            let bare_values = block_node.map_or_else(Vec::new, |block| {
                block
                    .children()
                    .iter()
                    .filter(|child| {
                        matches!(child.kind(), CstKind::BareValue | CstKind::QuotedString)
                    })
                    .filter_map(|child| {
                        let raw = input.source_text(child.range())?.trim();
                        let value = raw
                            .strip_prefix('"')
                            .and_then(|value| value.strip_suffix('"'))
                            .unwrap_or(raw)
                            .to_owned();
                        Some((value, child.range()))
                    })
                    .collect()
            });
            let operator = node
                .children()
                .iter()
                .find(|child| child.kind() == CstKind::Operator)
                .and_then(|child| input.source_text(child.range()))
                .map(str::to_owned);
            Some(ScriptProperty {
                key,
                key_range,
                range: node.range(),
                operator,
                scalar: property_scalar(input, node),
                block_range: block_node.map(CstNode::range),
                block,
                bare_values,
            })
        })
        .collect()
}
pub(crate) fn property_key(input: &ParsedInput, node: &CstNode) -> Option<(String, TextRange)> {
    let key = node
        .children()
        .iter()
        .find(|child| child.kind() == CstKind::Key)?;
    let text = text(input, key.range())?.trim().to_owned();
    Some((text, key.range()))
}

pub(crate) fn property_scalar(input: &ParsedInput, node: &CstNode) -> Option<(String, TextRange)> {
    let value = node
        .children()
        .iter()
        .find(|child| child.kind() == CstKind::Value)?;
    let scalar = value
        .children()
        .iter()
        .find(|child| matches!(child.kind(), CstKind::BareValue | CstKind::QuotedString))?;
    let raw = text(input, scalar.range())?.trim();
    let value = raw
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(raw)
        .to_owned();
    Some((value, scalar.range()))
}

pub(crate) fn text(input: &ParsedInput, range: TextRange) -> Option<&str> {
    input.source_text(range)
}

pub(crate) fn properties(input: &ParsedInput) -> Vec<PropertyInfo> {
    input.hir.as_deref().map_or_else(Vec::new, |hir| {
        hir.properties()
            .iter()
            .map(|property| PropertyInfo {
                key: property.key.clone(),
                value: property
                    .scalar
                    .as_ref()
                    .map(|scalar| (scalar.value.clone(), scalar.range)),
            })
            .collect()
    })
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
pub(crate) fn root_for_path<'a>(
    snapshot: &'a AnalysisSnapshot,
    path: &Path,
) -> Option<&'a pdx_engine::SourceRoot> {
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
        .flat_map(|path| {
            snapshot
                .source_files()
                .values()
                .filter(move |file| file.physical_path == path)
                .map(|file| file.id)
        })
        .collect()
}
