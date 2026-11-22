//! Symbol hovers: definition links and localisation previews.

use super::render::{HoverModel, code_span};
use super::template::template_signature_hover;
use crate::localisation::{
    localisation_previews_for_name, preview_rows, symbol_localisation_preview,
};
use crate::resolution::{
    ResolutionDefinition, definition_selection_location, effective_localisation_candidate,
    retain_highest_and_order, symbol_candidates_for_hover,
};
use crate::semantic::dynamic_definition_summary;
use crate::support::root_for_path;
use crate::types::{CancellationToken, Cancelled, Location};
use engine::{AnalysisSnapshot, SourceRootKind};

/// Computes the hover for a symbol reference or the declaration under the cursor.
pub(crate) fn hover_for_symbol(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    name: &str,
    declaration: Option<ResolutionDefinition>,
    cancellation: &CancellationToken,
) -> Result<HoverModel, Cancelled> {
    let mut model = HoverModel::new(format!("### {} {}", kind, code_span(name)));
    if kind == "localisation" {
        let previews = localisation_previews_for_name(snapshot, name, cancellation)?;
        let rows = preview_rows(None, name, &previews, true);
        let location = rows
            .iter()
            .find_map(|row| row.location.clone())
            .or_else(|| previews.first().map(|preview| preview.location.clone()));
        let location = if location.is_some() {
            location
        } else {
            let candidates = symbol_candidates_for_hover(snapshot, kind, name, cancellation)?;
            effective_localisation_candidate(&candidates).map(definition_selection_location)
        };
        model.push_localisation(&rows);
        if let Some(location) = location {
            set_definition(&mut model, location);
        }
        return Ok(model);
    }

    let definition = match declaration {
        Some(definition) => Some(definition),
        None => {
            let candidates = symbol_candidates_for_hover(snapshot, kind, name, cancellation)?;
            retain_highest_and_order(candidates).pop()
        }
    };
    let Some(definition) = definition else {
        model.push_section(format!("#### unresolved {kind} symbol"));
        return Ok(model);
    };

    let previews = symbol_localisation_preview(snapshot, kind, name, &definition, cancellation)?;
    let location = definition_selection_location(&definition);
    if matches!(kind, "event" | "mission") {
        model.preview = Some(preview_target(
            snapshot,
            kind,
            name,
            &definition,
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
    set_definition(&mut model, location);
    Ok(model)
}

fn set_definition(model: &mut HoverModel, location: Location) {
    let label = location.path.as_ref().map_or_else(
        || {
            location
                .document
                .as_ref()
                .and_then(|id| id.as_str().rsplit('/').find(|part| !part.is_empty()))
                .unwrap_or("open document")
                .to_owned()
        },
        |path| path.as_str().to_owned(),
    );
    model.set_definition(&label, location);
}

fn preview_target(
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

fn symbol_source_root(snapshot: &AnalysisSnapshot, location: &Location) -> String {
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
