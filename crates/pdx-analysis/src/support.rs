use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use pdx_engine::hir::HirFile;
use pdx_engine::hir::lower_with_profile;
use pdx_engine::{AnalysisSnapshot, DocumentId, DocumentSource, ParsedSource, SourceFileId};
use pdx_parser::{CstKind, CstNode, FileFormat, ParsedFile, QuotedScript, parse};
use pdx_rules::{GameProfile, ParserKind};
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
        .and_then(|path| snapshot.source_file_id_for_path(path));
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
    if let Some(ParsedSource::Text(parsed)) = state.parsed() {
        return Some(ParsedInput {
            document: None,
            file: Some(id),
            path: Some(file.logical_path.clone()),
            format: parsed.format(),
            source: state.source_handle(),
            parsed: ParsedContent::Text(Arc::clone(parsed)),
            hir: state.hir_handle(),
            profile: snapshot.game_profile_handle(),
        });
    }
    // The scan may evict CST/HIR frontends after background validation to
    // bound resident memory; the source text stays in the file state, so the
    // tree is reparsed transiently for this one query.
    let format = match snapshot.rules().classify(&file.logical_path)?.parser {
        ParserKind::Script => FileFormat::Script,
        ParserKind::Localisation => FileFormat::Localisation,
        ParserKind::Asset | ParserKind::SyntaxOnly => return None,
    };
    let source = state.source_handle();
    let parsed = Arc::new(parse(format, &source));
    let hir = Arc::new(lower_with_profile(
        (*parsed).clone(),
        &file.logical_path,
        snapshot.rules(),
        snapshot.game_profile(),
    ));
    Some(ParsedInput {
        document: None,
        file: Some(id),
        path: Some(file.logical_path.clone()),
        format,
        source,
        parsed: ParsedContent::Text(parsed),
        hir: Some(hir),
        profile: snapshot.game_profile_handle(),
    })
}

pub(crate) fn input_for_text(
    snapshot: &AnalysisSnapshot,
    path: &LogicalPath,
    text: &str,
) -> Option<ParsedInput> {
    let format = match snapshot.rules().classify(path)?.parser {
        ParserKind::Script => FileFormat::Script,
        ParserKind::Localisation => FileFormat::Localisation,
        ParserKind::Asset | ParserKind::SyntaxOnly => return None,
    };
    let source = Arc::<str>::from(text);
    let parsed = Arc::new(parse(format, &source));
    let hir = Arc::new(lower_with_profile(
        (*parsed).clone(),
        path,
        snapshot.rules(),
        snapshot.game_profile(),
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

pub(crate) fn logical_path(snapshot: &AnalysisSnapshot, path: &Path) -> Option<LogicalPath> {
    snapshot
        .source_roots()
        .iter()
        .filter_map(|root| path.strip_prefix(&root.path).ok())
        .filter_map(|relative| LogicalPath::parse(&relative.to_string_lossy()).ok())
        .min_by_key(|path| path.as_str().len())
        .or_else(|| LogicalPath::parse(&path.to_string_lossy()).ok())
        .or_else(|| {
            path.file_name()
                .and_then(|name| LogicalPath::parse(&name.to_string_lossy()).ok())
        })
}
/// A script property tree built for one analysis query.
///
/// Keys, operators, scalars, and bare values are interned `Arc<str>` handles: the same
/// spellings recur thousands of times per workspace, and validation clones property paths
/// and scope registers on every transition, so shared allocations keep those clones at
/// reference-count cost.
#[derive(Clone, Debug)]
pub(crate) struct ScriptProperty {
    pub(crate) key: std::sync::Arc<str>,
    pub(crate) key_range: TextRange,
    pub(crate) range: TextRange,
    pub(crate) operator: Option<std::sync::Arc<str>>,
    pub(crate) scalar: Option<(std::sync::Arc<str>, TextRange)>,
    pub(crate) quoted: bool,
    pub(crate) quoted_source: Option<QuotedScalarSource>,
    pub(crate) block_range: Option<TextRange>,
    pub(crate) block: Vec<ScriptProperty>,
    pub(crate) bare_values: Vec<(std::sync::Arc<str>, TextRange)>,
}

#[derive(Clone, Debug)]
pub(crate) struct QuotedScalarSource {
    source: Arc<str>,
    source_offsets: QuotedScalarOffsets,
}

#[derive(Clone, Debug)]
enum QuotedScalarOffsets {
    /// Top-level CST offsets map directly into the document and need no per-byte allocation.
    Direct { start: TextSize, len: TextSize },
    /// Secondary CST offsets compose through the enclosing quoted Script source map.
    Mapped(Arc<[TextSize]>),
}

impl QuotedScalarSource {
    pub(crate) fn synthetic(source: Arc<str>, fallback: TextRange) -> Self {
        let offsets = (0..=source.len())
            .map(|_| fallback.start())
            .collect::<Vec<_>>();
        Self {
            source,
            source_offsets: QuotedScalarOffsets::Mapped(offsets.into()),
        }
    }

    pub(crate) fn source(&self) -> &str {
        &self.source
    }

    fn map_offset(&self, offset: TextSize) -> Option<TextSize> {
        match &self.source_offsets {
            QuotedScalarOffsets::Direct { start, len } if offset <= *len => {
                start.checked_add(offset)
            }
            QuotedScalarOffsets::Direct { .. } => None,
            QuotedScalarOffsets::Mapped(offsets) => {
                offsets.get(usize::try_from(offset).ok()?).copied()
            }
        }
    }

    pub(crate) fn map_decoded_range(
        &self,
        script: &QuotedScript,
        range: TextRange,
    ) -> Option<TextRange> {
        let relative = script.source_map().decoded_range(range)?;
        TextRange::new(
            self.map_offset(relative.start())?,
            self.map_offset(relative.end())?,
        )
    }

    pub(crate) fn decoded_position(
        &self,
        script: &QuotedScript,
        position: TextSize,
    ) -> Option<TextSize> {
        let local = match &self.source_offsets {
            QuotedScalarOffsets::Direct { start, len } => {
                usize::try_from(position.saturating_sub(*start).min(*len)).ok()?
            }
            QuotedScalarOffsets::Mapped(offsets) => offsets
                .partition_point(|candidate| *candidate <= position)
                .saturating_sub(1),
        };
        script
            .source_map()
            .source_offset(u32::try_from(local).ok()?)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ScopeContext {
    pub(crate) profile: Arc<GameProfile>,
    pub(crate) root: Arc<str>,
    pub(crate) current: Arc<str>,
    pub(crate) from: Vec<Arc<str>>,
    pub(crate) previous: Vec<Arc<str>>,
}

impl ScopeContext {
    pub(crate) fn new(profile: Arc<GameProfile>) -> Self {
        Self {
            profile,
            root: pdx_engine::intern_shard_string("any"),
            current: pdx_engine::intern_shard_string("any"),
            from: Vec::new(),
            previous: Vec::new(),
        }
    }
}
pub(crate) fn script_properties(input: &ParsedInput, parent: CstNode<'_>) -> Vec<ScriptProperty> {
    let ParsedContent::Text(parsed) = &input.parsed;
    script_properties_mapped(parsed, parent, Some, true)
}

pub(crate) fn script_bare_values(
    input: &ParsedInput,
    parent: CstNode<'_>,
) -> Vec<(std::sync::Arc<str>, TextRange)> {
    let ParsedContent::Text(parsed) = &input.parsed;
    script_bare_values_mapped(parsed, parent, Some)
}

pub(crate) fn quoted_script_container(
    script: &QuotedScript,
    origin: &QuotedScalarSource,
) -> (Vec<ScriptProperty>, Vec<(std::sync::Arc<str>, TextRange)>) {
    let parsed = script.parsed();
    let map = |offset| {
        let relative = script.source_map().decoded_offset(offset)?;
        origin.map_offset(relative)
    };
    (
        script_properties_mapped(parsed, parsed.root(), map, false),
        script_bare_values_mapped(parsed, parsed.root(), map),
    )
}

fn script_properties_mapped(
    parsed: &ParsedFile,
    parent: CstNode<'_>,
    map_offset: impl Copy + Fn(TextSize) -> Option<TextSize>,
    direct_offsets: bool,
) -> Vec<ScriptProperty> {
    let map_range =
        |range: TextRange| TextRange::new(map_offset(range.start())?, map_offset(range.end())?);
    parent
        .children()
        .filter(|node| node.kind() == CstKind::Property)
        .filter_map(|node| {
            let key_node = node.children().find(|child| child.kind() == CstKind::Key)?;
            let key = pdx_engine::intern_shard_string(parsed.text(key_node.range())?.trim());
            let key_range = map_range(key_node.range())?;
            let value = node.children().find(|child| child.kind() == CstKind::Value);
            let block_node = value.and_then(|value| {
                value
                    .children()
                    .find(|child| child.kind() == CstKind::Block)
            });
            let block = block_node.map_or_else(Vec::new, |block| {
                script_properties_mapped(parsed, block, map_offset, direct_offsets)
            });
            let bare_values = block_node.map_or_else(Vec::new, |block| {
                script_bare_values_mapped(parsed, block, map_offset)
            });
            let operator = node
                .children()
                .find(|child| child.kind() == CstKind::Operator)
                .and_then(|child| parsed.text(child.range()))
                .map(pdx_engine::intern_shard_string);
            let scalar_node = property_scalar_node(node);
            let scalar = scalar_node.and_then(|scalar| {
                let raw = parsed.text(scalar.range())?.trim();
                let value = raw
                    .strip_prefix('"')
                    .and_then(|value| value.strip_suffix('"'))
                    .unwrap_or(raw);
                Some((
                    pdx_engine::intern_shard_string(value),
                    map_range(scalar.range())?,
                ))
            });
            let quoted_source = scalar_node
                .filter(|scalar| scalar.kind() == CstKind::QuotedString)
                .and_then(|scalar| {
                    let source = Arc::<str>::from(parsed.text(scalar.range())?);
                    // A quoted token found in a decoded secondary CST is still the raw token for
                    // that Script layer. Retaining it verbatim lets the next descent decode exactly
                    // one additional layer; re-encoding here would introduce a spurious layer.
                    let source_offsets = if direct_offsets {
                        QuotedScalarOffsets::Direct {
                            start: scalar.range().start(),
                            len: scalar.range().len(),
                        }
                    } else {
                        let start = usize::try_from(scalar.range().start()).ok()?;
                        let len = usize::try_from(scalar.range().len()).ok()?;
                        let offsets = (0..=len)
                            .map(|offset| {
                                map_offset(u32::try_from(start.checked_add(offset)?).ok()?)
                            })
                            .collect::<Option<Vec<_>>>()?;
                        QuotedScalarOffsets::Mapped(offsets.into())
                    };
                    Some(QuotedScalarSource {
                        source,
                        source_offsets,
                    })
                });
            Some(ScriptProperty {
                key,
                key_range,
                range: map_range(node.range())?,
                operator,
                scalar,
                quoted: scalar_node.is_some_and(|scalar| scalar.kind() == CstKind::QuotedString),
                quoted_source,
                block_range: block_node.and_then(|block| map_range(block.range())),
                block,
                bare_values,
            })
        })
        .collect()
}

fn script_bare_values_mapped(
    parsed: &ParsedFile,
    parent: CstNode<'_>,
    map_offset: impl Copy + Fn(TextSize) -> Option<TextSize>,
) -> Vec<(std::sync::Arc<str>, TextRange)> {
    parent
        .children()
        .filter(|child| matches!(child.kind(), CstKind::BareValue | CstKind::QuotedString))
        .filter_map(|child| {
            let raw = parsed.text(child.range())?.trim();
            let value = raw
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
                .unwrap_or(raw);
            Some((
                pdx_engine::intern_shard_string(value),
                TextRange::new(
                    map_offset(child.range().start())?,
                    map_offset(child.range().end())?,
                )?,
            ))
        })
        .collect()
}
fn property_scalar_node(node: CstNode<'_>) -> Option<CstNode<'_>> {
    node.children()
        .find(|child| child.kind() == CstKind::Value)?
        .children()
        .find(|child| matches!(child.kind(), CstKind::BareValue | CstKind::QuotedString))
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
        .filter_map(|path| snapshot.source_file_id_for_path(path))
        .collect()
}
