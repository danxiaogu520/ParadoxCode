use crate::resolution::*;
use crate::support::*;
use crate::types::*;
use pdx_engine::{AnalysisSnapshot, DocumentSource};
use pdx_parser::FileFormat;
use pdx_text::TextSize;

pub(crate) fn completion_definitions_for_kinds(
    snapshot: &AnalysisSnapshot,
    prefix: &str,
    kinds: &[&str],
    cancellation: &CancellationToken,
) -> Result<Vec<(String, String)>, Cancelled> {
    let mut definitions = Vec::new();
    for kind in kinds {
        for definition in snapshot.index().definitions_for_kind(kind) {
            cancellation.checkpoint()?;
            if completion_matches(&definition.name, prefix) {
                definitions.push((definition.kind.clone(), definition.name.clone()));
            }
        }
    }
    for document in snapshot.documents().values() {
        cancellation.checkpoint()?;
        if document.source() != DocumentSource::Overlay {
            continue;
        }
        if let Some(input) = input_for_document(snapshot, document.id()) {
            definitions.extend(
                semantic_data(snapshot, &input)
                    .definitions
                    .into_iter()
                    .filter(|definition| {
                        kinds
                            .iter()
                            .any(|kind| definition.kind.eq_ignore_ascii_case(kind))
                            && completion_matches(&definition.name, prefix)
                    })
                    .map(|definition| (definition.kind, definition.name)),
            );
        }
    }
    definitions.sort_by(|left, right| {
        (left.0.to_ascii_lowercase(), left.1.to_ascii_lowercase())
            .cmp(&(right.0.to_ascii_lowercase(), right.1.to_ascii_lowercase()))
    });
    definitions.dedup_by(|left, right| {
        left.0.eq_ignore_ascii_case(&right.0) && left.1.eq_ignore_ascii_case(&right.1)
    });
    Ok(definitions)
}
pub(crate) fn completion_value_context(input: &ParsedInput, position: TextSize) -> bool {
    if input.format == FileFormat::Script
        && let Some(hir) = input.hir.as_deref()
    {
        if hir.properties().iter().any(|property| {
            position >= property.key_range.start() && position <= property.key_range.end()
        }) {
            return false;
        }
        if hir.properties().iter().any(|property| {
            property.scalar.as_ref().is_some_and(|scalar| {
                position >= scalar.range.start() && position <= scalar.range.end()
            })
        }) {
            return true;
        }
    }
    let offset = usize::try_from(position)
        .unwrap_or(input.source.len())
        .min(input.source.len());
    let line_start = input.source[..offset]
        .rfind('\n')
        .map_or(0, |index| index + 1);
    let line = &input.source[line_start..offset];
    if input.format == FileFormat::Localisation {
        return line.contains(':') && !line.trim_start().starts_with('#');
    }
    let equals = line.rfind('=');
    let open = line.rfind('{');
    equals.is_some_and(|equals| open.is_none_or(|open| equals > open))
}

pub(crate) fn localisation_language_header(input: &ParsedInput, position: TextSize) -> bool {
    if input.format != FileFormat::Localisation {
        return false;
    }
    let offset = usize::try_from(position)
        .unwrap_or(input.source.len())
        .min(input.source.len());
    let line_start = input.source[..offset]
        .rfind('\n')
        .map_or(0, |index| index + 1);
    let line = input.source[line_start..offset].trim();
    let Some(language) = line.strip_prefix("l_") else {
        return false;
    };
    let Some(language) = language.strip_suffix(':') else {
        return false;
    };
    !language.is_empty()
        && language
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
}
/// Sort penalty applied to candidates that match the typed prefix only as a substring, so
/// exact and prefix matches stay on top of the completion list.
const FUZZY_MATCH_PENALTY: u32 = 100;

/// Sort penalty applied to deprecated rules so current commands stay on top.
const DEPRECATED_SORT_PENALTY: u32 = 500;

pub(crate) fn completion_sort_score(score: u32, deprecated: bool) -> u32 {
    if deprecated {
        score.saturating_add(DEPRECATED_SORT_PENALTY)
    } else {
        score
    }
}

pub(crate) fn push_completion(
    items: &mut Vec<CompletionItem>,
    mut item: CompletionItem,
    prefix: &str,
) {
    if !completion_matches(&item.label, prefix) {
        return;
    }
    if !starts_with_ignore_ascii_case(&item.label, prefix) {
        item.sort_score = item.sort_score.saturating_add(FUZZY_MATCH_PENALTY);
    }
    items.push(item);
}
