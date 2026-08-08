use crate::support::*;
use crate::types::*;
use pdx_engine::{AnalysisSnapshot, DocumentId};
use pdx_parser::FileFormat;
use pdx_text::{TextRange, TextSize};

mod candidates;
mod context;
mod support;

pub(crate) use candidates::*;
pub(crate) use context::*;
pub(crate) use support::*;

/// Computes key, value, localisation, and symbol completion.
#[must_use]
pub fn complete(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
) -> CompletionResult {
    uncancelled(complete_with_cancellation(
        snapshot,
        document,
        position,
        &CancellationToken::new(),
    ))
}

/// Computes completion with cooperative cancellation checkpoints.
pub fn complete_with_cancellation(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
    cancellation: &CancellationToken,
) -> Result<CompletionResult, Cancelled> {
    cancellation.checkpoint()?;
    let Some(input) = input_for_document(snapshot, document) else {
        return Ok(CompletionResult {
            revision: snapshot.revision(),
            items: Vec::new(),
        });
    };
    let replacement_range = word_range(&input.source, position);
    let prefix = input
        .source_text(replacement_range)
        .unwrap_or_default()
        .to_owned();
    let base_indent = line_indent(&input.source, replacement_range.start());
    let value_context = completion_value_context(&input, position);
    if input.format == FileFormat::Localisation {
        if localisation_language_header(&input, position) {
            return Ok(CompletionResult {
                revision: snapshot.revision(),
                items: Vec::new(),
            });
        }
        return if value_context {
            localisation_value_completion(snapshot, replacement_range, &prefix, cancellation)
        } else {
            localisation_key_completion(snapshot, replacement_range, &prefix, cancellation)
        };
    }
    let mut items = Vec::new();
    let mut member_cache = CompletionMemberCache::default();
    let semantic_context = semantic_completion_context(snapshot, &input, position);
    if let Some(context) = semantic_context.as_ref() {
        cancellation.checkpoint()?;
        if value_context {
            if let Some(property) = context.property.as_ref() {
                add_semantic_value_items(
                    snapshot,
                    context,
                    property,
                    &mut member_cache,
                    &mut items,
                    replacement_range,
                    &prefix,
                );
            }
        } else {
            let insert_assignment = context
                .property
                .as_ref()
                .is_none_or(|property| property.operator.is_none());
            add_semantic_key_items(
                snapshot,
                context,
                &mut member_cache,
                &mut items,
                replacement_range,
                &prefix,
                insert_assignment,
                &base_indent,
            );
        }
    }
    items.sort_by_key(|item| (item.sort_score, item.label.to_ascii_lowercase()));
    items.dedup_by(|left, right| left.label == right.label && left.kind == right.kind);
    cancellation.checkpoint()?;
    Ok(CompletionResult {
        revision: snapshot.revision(),
        items,
    })
}

/// Completes localisation entry keys in a localisation document: workspace keys plus keys
/// already defined in the open file. The value side of an entry is free text; see
/// `localisation_value_completion`.
pub(crate) fn localisation_key_completion(
    snapshot: &AnalysisSnapshot,
    replacement_range: TextRange,
    prefix: &str,
    cancellation: &CancellationToken,
) -> Result<CompletionResult, Cancelled> {
    let mut items = Vec::new();
    for (kind_name, definition_name) in
        completion_definitions_for_kinds(snapshot, prefix, &["localisation"], cancellation)?
    {
        cancellation.checkpoint()?;
        debug_assert_eq!(kind_name, "localisation");
        push_completion(
            &mut items,
            CompletionItem {
                label: definition_name.clone(),
                kind: CompletionKind::Localisation,
                detail: "localisation".to_owned(),
                documentation: None,
                replacement_range,
                insert_text: definition_name,
                sort_score: 10,
                deprecated: false,
                resolve_data: None,
            },
            prefix,
        );
    }
    items.sort_by_key(|item| (item.sort_score, item.label.to_ascii_lowercase()));
    items.dedup_by(|left, right| left.label == right.label && left.kind == right.kind);
    cancellation.checkpoint()?;
    Ok(CompletionResult {
        revision: snapshot.revision(),
        items,
    })
}

/// Completes localisation keys referenced from the value side of a localisation entry. Only
/// `localisation` kind members are offered; unrelated workspace definitions and generic scalars
/// are not candidates here.
pub(crate) fn localisation_value_completion(
    snapshot: &AnalysisSnapshot,
    replacement_range: TextRange,
    prefix: &str,
    cancellation: &CancellationToken,
) -> Result<CompletionResult, Cancelled> {
    let mut items = Vec::new();
    for (kind_name, definition_name) in
        completion_definitions_for_kinds(snapshot, prefix, &["localisation"], cancellation)?
    {
        cancellation.checkpoint()?;
        debug_assert_eq!(kind_name, "localisation");
        push_completion(
            &mut items,
            CompletionItem {
                label: definition_name.clone(),
                kind: CompletionKind::Localisation,
                detail: "localisation".to_owned(),
                documentation: None,
                replacement_range,
                insert_text: definition_name,
                sort_score: 20,
                deprecated: false,
                resolve_data: None,
            },
            prefix,
        );
    }
    items.sort_by_key(|item| (item.sort_score, item.label.to_ascii_lowercase()));
    items.dedup_by(|left, right| left.label == right.label && left.kind == right.kind);
    cancellation.checkpoint()?;
    Ok(CompletionResult {
        revision: snapshot.revision(),
        items,
    })
}
/// Alias with the noun used by several editor adapters.
#[must_use]
pub fn completion(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
) -> CompletionResult {
    complete(snapshot, document, position)
}

/// Re-derives the documentation for a completion item that carries `resolve_data` without
/// re-running the completion query. Items without `resolve_data` resolve to themselves.
#[must_use]
pub fn completion_resolve(snapshot: &AnalysisSnapshot, item: &CompletionItem) -> CompletionItem {
    let Some(id) = item
        .resolve_data
        .as_deref()
        .and_then(|data| data.strip_prefix("rule:"))
    else {
        return item.clone();
    };
    let Some(rule) = snapshot
        .rules()
        .model()
        .semantic
        .rules
        .iter()
        .find(|rule| rule.id == id)
    else {
        return item.clone();
    };
    let mut resolved = item.clone();
    if !rule.documentation.is_empty() {
        resolved.documentation = Some(rule.documentation.join("\n"));
    }
    resolved
}
