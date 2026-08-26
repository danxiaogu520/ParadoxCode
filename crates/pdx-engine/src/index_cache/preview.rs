//! Bounded localisation previews retained by Vanilla cache.

use std::collections::BTreeMap;

use pdx_parser::{CstKind, FileFormat};

use super::IndexCacheError;
use crate::{AnalysisSnapshot, LocalisationPreview, LocalisationPreviewMap, ParsedSource};

const MAX_LOCALISATION_PREVIEW_CHARS: usize = 240;

pub(super) fn collect_localisation_previews(
    snapshot: &AnalysisSnapshot,
) -> Result<LocalisationPreviewMap, IndexCacheError> {
    let mut previews = BTreeMap::new();
    for (file_id, file) in snapshot.source_files() {
        let state = snapshot.file_state(*file_id).ok_or_else(|| {
            IndexCacheError::InvalidData(format!(
                "Vanilla file {} has no materialized file state",
                file.logical_path.as_str()
            ))
        })?;
        if let Some(cached) = state.cached_localisation_previews() {
            for (range, preview) in cached {
                if previews
                    .insert((*file_id, *range), preview.clone())
                    .is_some()
                {
                    return Err(IndexCacheError::InvalidData(format!(
                        "duplicate localisation preview in {}",
                        file.logical_path.as_str()
                    )));
                }
            }
            continue;
        }
        let Some(ParsedSource::Text(parsed)) = state.parsed() else {
            continue;
        };
        if parsed.format() != FileFormat::Localisation {
            continue;
        }
        let mut language = None;
        for node in parsed.root().children() {
            match node.kind() {
                CstKind::LanguageHeader => {
                    language = node
                        .children()
                        .iter()
                        .find(|child| child.kind() == CstKind::LocalisationKey)
                        .and_then(|child| parsed.text(child.range()))
                        .map(|value| value.trim().to_owned())
                        .filter(|value| !value.is_empty());
                }
                CstKind::LocalisationEntry => {
                    let Some(value_node) = node.children().iter().find(|child| {
                        matches!(
                            child.kind(),
                            CstKind::LocalisationString | CstKind::UnquotedValue
                        )
                    }) else {
                        continue;
                    };
                    let Some(raw) = parsed.text(value_node.range()).map(str::trim) else {
                        continue;
                    };
                    let value = raw
                        .strip_prefix('"')
                        .and_then(|value| value.strip_suffix('"'))
                        .unwrap_or(raw);
                    let value = truncate_localisation_preview(value);
                    if value.is_empty() {
                        continue;
                    }
                    if previews
                        .insert(
                            (*file_id, node.range()),
                            LocalisationPreview {
                                language: language.clone(),
                                value,
                            },
                        )
                        .is_some()
                    {
                        return Err(IndexCacheError::InvalidData(format!(
                            "duplicate localisation preview in {}",
                            file.logical_path.as_str()
                        )));
                    }
                }
                _ => {}
            }
        }
    }
    Ok(LocalisationPreviewMap::from(previews))
}

fn truncate_localisation_preview(value: &str) -> String {
    let mut truncated = value
        .chars()
        .take(MAX_LOCALISATION_PREVIEW_CHARS)
        .collect::<String>();
    if value.chars().count() > MAX_LOCALISATION_PREVIEW_CHARS {
        truncated.push('…');
    }
    truncated
}
