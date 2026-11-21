//! Symbol hovers: definition resolution, shadowing, and localisation previews.

use super::render::{HoverModel, code_span};
use super::template::template_signature_hover;
use crate::localisation::{
    localisation_previews_for_name, preview_rows, symbol_localisation_preview,
};
use crate::resolution::symbol_candidates_for_hover;
use crate::semantic::dynamic_definition_summary;
use crate::support::{root_for_path, same_location};
use crate::types::{CancellationToken, Cancelled, Location};
use engine::{AnalysisSnapshot, SourceRootKind};
use text::TextRange;

/// Maximum number of candidate paths rendered before the list is truncated.
const MAX_HOVER_CANDIDATES: usize = 8;

/// Computes the structured hover for a symbol use or definition.
///
/// Rendering to Markdown is deferred so callers can branch on resolution facts (for example
/// preferring a variant with a localisation preview) without string-matching rendered
/// headings.
pub(crate) fn hover_for_symbol(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    name: &str,
    _range: TextRange,
    cancellation: &CancellationToken,
) -> Result<HoverModel, Cancelled> {
    let candidates = symbol_candidates_for_hover(snapshot, kind, name, cancellation)?;
    let mut model = HoverModel::new(format!("### {} {}", kind, code_span(name)));
    if candidates.is_empty() {
        model.push_section(format!("#### unresolved {kind} symbol"));
        if kind == "localisation" {
            model.push_localisation(&preview_rows(snapshot, None, name, &[], true));
        }
    } else {
        let highest = candidates
            .iter()
            .map(|candidate| candidate.priority)
            .max()
            .unwrap_or(0);
        let active = candidates
            .iter()
            .filter(|candidate| candidate.priority == highest)
            .collect::<Vec<_>>();
        if active.len() == 1 {
            let definition = active[0];
            let location = definition_source_link(snapshot, &mut model, definition);
            model.push_section(location);
            let shadowed = candidates
                .iter()
                .filter(|candidate| {
                    !same_location(&candidate.location, &definition.location)
                        && candidate.priority < definition.priority
                })
                .count();
            if shadowed > 0 {
                model.push_section(format!(
                    "Overrides {shadowed} lower-priority definition(s)."
                ));
            }
            let previews =
                symbol_localisation_preview(snapshot, kind, name, definition, cancellation)?;
            if matches!(kind, "event" | "mission") {
                model.preview = Some(preview_target(
                    snapshot,
                    kind,
                    name,
                    definition,
                    &previews,
                    cancellation,
                )?);
            }
            model.push_localisation(&previews);
            if let Some(summary) = dynamic_definition_summary(snapshot, kind, name) {
                let mut signature = template_signature_hover(snapshot, &summary);
                if snapshot.ir().schemas.is_empty()
                    && crate::semantic::dynamic_definition_type(snapshot, kind)
                {
                    signature.push('\n');
                    signature.push_str(&crate::template_contracts::contract_hover_line(
                        snapshot, kind, name,
                    ));
                }
                model.push_section(signature);
            }
        } else {
            // Ambiguous symbols still deserve a preview when any candidate carries localisation
            // text; that is exactly the case where the user most wants to see the translations.
            if kind.eq_ignore_ascii_case("localisation") {
                let previews = localisation_previews_for_name(snapshot, name, cancellation)?;
                model.push_localisation(&preview_rows(snapshot, None, name, &previews, true));
            } else {
                for candidate in &candidates {
                    let previews =
                        symbol_localisation_preview(snapshot, kind, name, candidate, cancellation)?;
                    if !previews.is_empty() {
                        model.push_localisation(&previews);
                        break;
                    }
                }
            }
            let shown = active.len().min(MAX_HOVER_CANDIDATES);
            let mut lines = active
                .iter()
                .take(shown)
                .map(|candidate| {
                    format!(
                        "- {}",
                        definition_source_link(snapshot, &mut model, candidate)
                    )
                })
                .collect::<Vec<_>>();
            if active.len() > shown {
                lines.push(format!("- … and {} more", active.len() - shown));
            }
            model.push_section(format!("#### ambiguous {kind} symbol"));
            model.push_section(format!("#### Candidates:\n\n{}", lines.join("\n")));
        }
    }
    Ok(model)
}

pub(crate) fn preview_target(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    name: &str,
    definition: &crate::resolution::ResolutionDefinition,
    localisation: &[crate::localisation::LocalisationPreviewRow],
    cancellation: &CancellationToken,
) -> Result<crate::PreviewTarget, Cancelled> {
    let title_field = if kind == "mission" { "name" } else { "title" };
    let title = localisation
        .iter()
        .find(|row| row.label.as_deref() == Some(title_field) && row.location.is_some());
    let mut missing_localisation = localisation
        .iter()
        .filter(|row| row.location.is_none())
        .map(|row| row.key.clone())
        .collect::<Vec<_>>();
    missing_localisation.sort();
    missing_localisation.dedup();
    let source = definition
        .location
        .document
        .as_ref()
        .and_then(|id| crate::support::input_for_document(snapshot, id))
        .or_else(|| {
            definition
                .location
                .file
                .and_then(|file| crate::support::input_for_source_file(snapshot, file))
        });
    let source = source.as_ref().and_then(|input| input.hir.as_ref());
    let mut event_type = None;
    let mut prerequisite_issues = Vec::new();
    if let Some(hir) = source {
        if kind == "event" {
            event_type = hir
                .properties()
                .iter()
                .find(|property| property.range == definition.location.range)
                .map(|property| property.key.clone());
        } else if let Some(spec) = snapshot.game_profile().mission_view.as_ref() {
            let prerequisites = hir
                .properties()
                .iter()
                .filter(|property| {
                    property.key == spec.node_fields.required_missions
                        && crate::resolution::text_range_within(
                            property.range,
                            definition.location.range,
                        )
                })
                .filter_map(|property| property.value_range);
            let mut seen = std::collections::BTreeSet::new();
            for list in prerequisites {
                for item in hir
                    .bare_values()
                    .iter()
                    .filter(|item| crate::resolution::text_range_within(item.range, list))
                {
                    if !seen.insert(item.value.to_ascii_lowercase()) {
                        continue;
                    }
                    let candidates =
                        symbol_candidates_for_hover(snapshot, kind, &item.value, cancellation)?;
                    if candidates.is_empty() {
                        prerequisite_issues.push((item.value.to_string(), false));
                    } else if candidates
                        .get(1)
                        .is_some_and(|next| next.priority == candidates[0].priority)
                    {
                        prerequisite_issues.push((item.value.to_string(), true));
                    }
                }
            }
        }
    }
    Ok(crate::PreviewTarget {
        kind: kind.to_owned(),
        id: name.to_owned(),
        title: title.map(|row| row.value.clone()),
        location: crate::resolution::definition_selection_location(definition),
        title_source: title.and_then(|row| Some((row.key.clone(), row.location.clone()?))),
        missing_localisation,
        event_type,
        source_name: symbol_source_root(snapshot, &definition.location),
        prerequisite_issues,
    })
}

fn definition_source_link(
    snapshot: &AnalysisSnapshot,
    model: &mut HoverModel,
    definition: &crate::resolution::ResolutionDefinition,
) -> String {
    let location = crate::resolution::definition_selection_location(definition);
    let mut label = symbol_location_path(&location);
    if let Some(position) = symbol_location_position(snapshot, &location) {
        label.push_str(&format!(" {position}"));
    }
    let source = symbol_source_root(snapshot, &location);
    format!("{source} · {}", model.source_link(&label, location))
}

pub(crate) fn symbol_location_path(location: &Location) -> String {
    location.path.as_ref().map_or_else(
        || "<open document>".to_owned(),
        |path| path.as_str().to_owned(),
    )
}

/// One-based `L<line>:C<column>` for a candidate's definition start, so
/// ambiguous-symbol lists with several sites in one file stay tellable
/// apart. Returns `None` when the backing source is not loadable.
pub(crate) fn symbol_location_position(
    snapshot: &AnalysisSnapshot,
    location: &Location,
) -> Option<String> {
    if location.document.is_none()
        && let Some(position) = location
            .file
            .and_then(|file| snapshot.index().position_for(file, location.range))
    {
        return Some(format!(
            "L{}:C{}",
            position.start.line + 1,
            position.start.character + 1
        ));
    }
    let input = location
        .document
        .as_ref()
        .and_then(|document| crate::support::input_for_document(snapshot, document))
        .or_else(|| {
            location
                .file
                .and_then(|file| crate::support::input_for_source_file(snapshot, file))
        })?;
    let start = usize::try_from(location.range.start()).ok()?;
    let prefix = &input.source[..start.min(input.source.len())];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let last_line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    let column = prefix[last_line_start..].chars().count() + 1;
    Some(format!("L{line}:C{column}"))
}

pub(crate) fn symbol_source_root(snapshot: &AnalysisSnapshot, location: &Location) -> String {
    let root = location
        .file
        .and_then(|file_id| snapshot.source_files().get(&file_id))
        .and_then(|file| {
            snapshot
                .source_roots()
                .iter()
                .find(|root| root.id == file.root_id)
        })
        .or_else(|| {
            location
                .document
                .as_ref()
                .and_then(|document_id| snapshot.document(document_id))
                .and_then(|document| document.path())
                .and_then(|path| root_for_path(snapshot, path))
        });
    match root.map(|root| root.kind) {
        Some(SourceRootKind::Vanilla) => "Vanilla".to_owned(),
        Some(SourceRootKind::Dependency) => root
            .and_then(|root| {
                root.name
                    .as_deref()
                    .or_else(|| root.path.as_path().file_name()?.to_str())
            })
            .map(|name| format!("Dependency ({})", name.replace(['\n', '\r'], " ")))
            .unwrap_or_else(|| "Dependency".to_owned()),
        Some(SourceRootKind::Project) => "Project".to_owned(),
        None if location.document.is_some() => "Open overlay".to_owned(),
        None => "Unknown source root".to_owned(),
    }
}
