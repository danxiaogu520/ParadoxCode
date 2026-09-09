//! Hover pipeline: dispatch from a document position to a structured hover model.
//!
//! Producers live in responsibility modules (`symbol`, `rules`, `dynamic`) and all return
//! [`HoverModel`] values; Markdown rendering is owned by [`render`].

mod dynamic;
mod render;
mod rules;
mod symbol;

pub(crate) use rules::semantic_pattern_rule_hint;
pub(crate) use symbol::known_keys;

use self::render::{HoverModel, code_span};
use crate::resolution::{local_parameter_target, semantic_data};
use crate::support::{ParsedInput, contains, input_for_document, word_range};
use crate::types::{CancellationToken, Cancelled, Hover, uncancelled};
use pdx_engine::{AnalysisSnapshot, DocumentId};
use pdx_text::TextSize;

/// Computes hover information without reading the full contents of another file.
#[must_use]
pub fn hover(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
) -> Option<Hover> {
    uncancelled(hover_with_cancellation(
        snapshot,
        document,
        position,
        &CancellationToken::new(),
    ))
}

/// Computes hover information with cooperative cancellation checkpoints.
pub fn hover_with_cancellation(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
    cancellation: &CancellationToken,
) -> Result<Option<Hover>, Cancelled> {
    cancellation.checkpoint()?;
    let Some(input) = input_for_document(snapshot, document) else {
        return Ok(None);
    };
    if let Some((definition, reference)) = local_parameter_target(&input, position) {
        // The owner name lives directly on the HIR definition with the same range; going through
        // `semantic_data` here would lower the whole file just to recover one spelling.
        let owner_name = input.hir.as_deref().and_then(|hir| {
            hir.definitions()
                .iter()
                .find(|candidate| candidate.range == definition.owner_range)
                .map(|candidate| candidate.name.clone())
        });
        let occurrences = input
            .hir
            .as_deref()
            .map(|hir| {
                hir.parameter_references_for_owner(definition.owner_range)
                    .filter(|reference| reference.name.eq_ignore_ascii_case(&definition.name))
                    .count()
            })
            .unwrap_or(0);
        let optional = input.hir.as_deref().is_some_and(|hir| {
            !hir.parameter_is_required(definition.owner_range, &definition.name)
        });
        let syntax = match reference.kind {
            pdx_engine::hir::HirParameterReferenceKind::Substitution => "substitution",
            pdx_engine::hir::HirParameterReferenceKind::KeySubstitution => "key substitution",
            pdx_engine::hir::HirParameterReferenceKind::OpaqueTextSubstitution => {
                "opaque text substitution"
            }
            pdx_engine::hir::HirParameterReferenceKind::Conditional => "conditional",
        };
        let owner = owner_name.as_deref().map_or_else(
            || "scripted definition".to_owned(),
            |name| format!("scripted definition {}", code_span(name)),
        );
        let mut section = format!(
            "- Local to {owner}; inferred from its first use\n- Presence: `{}`\n- Syntax: `{syntax}`\n- Occurrences in owner: {occurrences}",
            if optional {
                "optional"
            } else {
                "required/inferred"
            },
        );
        if let Some(owner) = owner_name.as_deref()
            && let Some(contract) =
                dynamic::dynamic_parameter_contract_lines(snapshot, None, owner, &definition.name)
        {
            section.push('\n');
            section.push_str(&contract);
        }
        let mut model = HoverModel::new(format!("### parameter {}", code_span(&definition.name)));
        model.push_section(section);
        return Ok(Some(model.into_hover_with_range(reference.name_range)));
    }
    let range = word_range(&input.source, position);
    let Some(word) = input
        .source_text(range)
        .map(|word| word.trim_matches('"').to_owned())
    else {
        return Ok(None);
    };
    if word.is_empty() {
        return Ok(None);
    }
    let semantic = semantic_data(snapshot, &input);
    let mut references = semantic.references.iter().filter(|reference| {
        reference.document.as_ref() == Some(document) && contains(reference.range, position)
    });
    if let Some(first) = references.next() {
        let mut best =
            symbol::hover_for_symbol(snapshot, &first.kind, &first.name, range, cancellation)?;
        if !best.has_localisation_preview {
            for reference in references {
                let hover = symbol::hover_for_symbol(
                    snapshot,
                    &reference.kind,
                    &reference.name,
                    range,
                    cancellation,
                )?;
                if hover.has_localisation_preview {
                    best = hover;
                    break;
                }
            }
        }
        return Ok(Some(best.into_hover_with_range(range)));
    }
    if let Some(definition) = semantic.definitions.iter().find(|definition| {
        definition.document.as_ref() == Some(document)
            && contains(definition.symbol.selection_range, position)
    }) {
        return Ok(Some(
            symbol::hover_for_symbol(
                snapshot,
                &definition.kind,
                &definition.name,
                range,
                cancellation,
            )?
            .into_hover_with_range(range),
        ));
    }
    cancellation.checkpoint()?;
    if let Some(model) =
        dynamic::dynamic_invocation_parameter_hover(snapshot, &input, position, cancellation)?
    {
        return Ok(Some(model.into_hover_with_range(range)));
    }
    if let Some(model) =
        rules::semantic_rule_hover_at(snapshot, &input, position, &word, cancellation)?
    {
        return Ok(Some(model.into_hover_with_range(range)));
    }
    if let Some(model) =
        rules::semantic_value_hover_at(snapshot, &input, position, &word, cancellation)?
    {
        return Ok(Some(model.into_hover_with_range(range)));
    }
    if is_property_key_at(&input, position) {
        if known_keys(snapshot)
            .iter()
            .any(|key| key.eq_ignore_ascii_case(&word))
        {
            let mut model = HoverModel::new(known_key_hover_title(snapshot, &word));
            if let Some(details) = rules::semantic_rule_documentation(snapshot, &word) {
                model.push_section(details);
            }
            return Ok(Some(model.into_hover_with_range(range)));
        }
        // The key may still be covered by a non-exact first-party matcher (type member, enum
        // member, date, or dynamic set). Surface that provenance instead of returning nothing.
        if let Some(hint) = semantic_pattern_rule_hint(snapshot, &word) {
            let mut model = HoverModel::new(format!("### {}", code_span(&word)));
            model.push_section(hint);
            return Ok(Some(model.into_hover_with_range(range)));
        }
    }
    // Do not manufacture a tooltip for every bare word in a script or comment.  A hover is only
    // useful when the parser/HIR/rules have established a semantic role for the token.
    Ok(None)
}

pub(crate) fn is_property_key_at(input: &ParsedInput, position: TextSize) -> bool {
    input.hir.as_deref().is_some_and(|hir| {
        hir.properties()
            .iter()
            .any(|property| contains(property.key_range, position))
    })
}

/// Title for the known-key fallback hover: the category of the rule family
/// covering the key, or the bare symbol-hover pattern when no category is
/// established (mixed contexts or none).
fn known_key_hover_title(snapshot: &AnalysisSnapshot, word: &str) -> String {
    rules::semantic_rule_key_category(snapshot, word).map_or_else(
        || format!("### {}", code_span(word)),
        |category| format!("### {category} {}", code_span(word)),
    )
}
