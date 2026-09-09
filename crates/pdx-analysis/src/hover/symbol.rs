//! Symbol hovers: definition resolution, shadowing, and localisation previews.

use std::collections::BTreeSet;
use std::sync::Arc;

use super::dynamic::dynamic_signature_hover;
use super::render::{HoverModel, code_span};
use crate::localisation::{localisation_preview, symbol_localisation_preview};
use crate::resolution::{symbol_candidates_for_hover, symbol_resolution_policy};
use crate::semantic::dynamic_definition_summary;
use crate::support::{root_for_path, same_location};
use crate::types::{CancellationToken, Cancelled, Location};
use pdx_engine::{AnalysisSnapshot, SourceRootKind};
use pdx_rules::SymbolResolutionPolicy;
use pdx_text::TextRange;

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
    let policy = symbol_resolution_policy(snapshot, kind);
    let mut model = HoverModel::new(format!("### {} {}", kind, code_span(name)));
    if candidates.is_empty() {
        model.push_section(format!("#### unresolved {kind} symbol"));
    } else {
        let highest = candidates
            .iter()
            .map(|candidate| candidate.priority)
            .max()
            .unwrap_or(0);
        let active = match policy {
            SymbolResolutionPolicy::ReplaceBySymbol => candidates
                .iter()
                .filter(|candidate| candidate.priority == highest)
                .collect::<Vec<_>>(),
            SymbolResolutionPolicy::Merge | SymbolResolutionPolicy::Unique => {
                if candidates.len() == 1 {
                    vec![&candidates[0]]
                } else {
                    Vec::new()
                }
            }
        };
        if active.len() == 1 {
            let definition = active[0];
            model.push_section(format!(
                "#### Resolved definition\n\n- Source root: {}\n- Defined in: `{}`",
                symbol_source_root(snapshot, &definition.location),
                symbol_location_path(&definition.location),
            ));
            let shadowed = candidates
                .iter()
                .filter(|candidate| {
                    !same_location(&candidate.location, &definition.location)
                        && candidate.priority < definition.priority
                })
                .collect::<Vec<_>>();
            if !shadowed.is_empty() {
                model.push_section(format!(
                    "#### Shadowed definitions:\n\n{}",
                    shadowed
                        .into_iter()
                        .map(|candidate| format!(
                            "- {}: `{}`",
                            symbol_source_root(snapshot, &candidate.location),
                            symbol_location_path(&candidate.location),
                        ))
                        .collect::<Vec<_>>()
                        .join("\n")
                ));
            }
            if let Some((language, value)) =
                symbol_localisation_preview(snapshot, kind, name, definition, cancellation)?
            {
                model.has_localisation_preview = true;
                model.push_section(localisation_preview_section(language, &value));
            }
            if let Some(summary) = dynamic_definition_summary(snapshot, kind, name) {
                let mut signature = dynamic_signature_hover(&summary);
                if crate::semantic::dynamic_definition_type(snapshot, kind) {
                    signature.push('\n');
                    signature.push_str(&crate::dynamic_contracts::contract_hover_line(
                        snapshot, kind, name,
                    ));
                }
                model.push_section(signature);
            }
        } else {
            // Ambiguous symbols still deserve a preview when any candidate carries localisation
            // text; that is exactly the case where the user most wants to see the translations.
            if kind.eq_ignore_ascii_case("localisation") {
                for candidate in &candidates {
                    if let Some((language, value)) = localisation_preview(snapshot, candidate) {
                        if value.is_empty() {
                            continue;
                        }
                        model.has_localisation_preview = true;
                        model.push_section(localisation_preview_section(language, &value));
                        break;
                    }
                }
            } else {
                for candidate in &candidates {
                    if let Some((language, value)) =
                        symbol_localisation_preview(snapshot, kind, name, candidate, cancellation)?
                    {
                        model.has_localisation_preview = true;
                        model.push_section(localisation_preview_section(language, &value));
                        break;
                    }
                }
            }
            let shown = candidates.len().min(MAX_HOVER_CANDIDATES);
            let mut lines = candidates
                .iter()
                .take(shown)
                .map(|candidate| {
                    format!(
                        "- {}: `{}`",
                        symbol_source_root(snapshot, &candidate.location),
                        symbol_location_path(&candidate.location),
                    )
                })
                .collect::<Vec<_>>();
            if candidates.len() > shown {
                lines.push(format!("- … and {} more", candidates.len() - shown));
            }
            model.push_section(format!("#### ambiguous {kind} symbol"));
            model.push_section(format!("#### Candidates:\n\n{}", lines.join("\n")));
        }
    }
    Ok(model)
}

/// Formats the shared localisation-preview section for one resolved (language, value) pair.
fn localisation_preview_section(language: Option<String>, value: &str) -> String {
    format!(
        "#### Localisation preview\n\n- Localisation{}: \"{}\"",
        language
            .as_deref()
            .map_or_else(String::new, |language| format!(" ({language})")),
        value
    )
}

pub(crate) fn symbol_location_path(location: &Location) -> String {
    location.path.as_ref().map_or_else(
        || "<open document>".to_owned(),
        |path| path.as_str().to_owned(),
    )
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
        Some(SourceRootKind::Dependency) => "Dependency".to_owned(),
        Some(SourceRootKind::CurrentMod) => "Current Mod".to_owned(),
        None if location.document.is_some() => "Open overlay".to_owned(),
        None => "Unknown source root".to_owned(),
    }
}

pub(crate) fn known_keys(snapshot: &AnalysisSnapshot) -> Arc<BTreeSet<String>> {
    // The key set is a pure function of the immutable snapshot but is consulted on every
    // property-key hover; memoize per revision instead of rebuilding it each time.
    let revision = snapshot.revision();
    let key = "hover-known-keys";
    if let Some(cached) = snapshot
        .query_cache()
        .get::<BTreeSet<String>>(revision, key)
    {
        return cached;
    }
    let mut keys = snapshot
        .game_profile()
        .fallback_keys
        .iter()
        .map(|key| key.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    for record in &snapshot.rules().model().records {
        keys.extend(record.fields.keys().map(|key| key.to_ascii_lowercase()));
    }
    // The imported descriptor catalog is the authoritative extension point for semantic keys.
    // Keep profile fallbacks useful in degraded mode, then admit every descriptor name supplied
    // by a validated rules artifact.
    keys.extend(
        snapshot
            .rules()
            .model()
            .symbol_descriptors
            .iter()
            .map(|descriptor| descriptor.kind_id.to_ascii_lowercase()),
    );
    let keys = Arc::new(keys);
    snapshot.query_cache().insert(
        revision,
        pdx_engine::CacheDomain::Index,
        key.to_owned(),
        Arc::clone(&keys),
    );
    keys
}
