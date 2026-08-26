use crate::support::*;
use crate::types::*;
use pdx_engine::{AnalysisSnapshot, DocumentId};
use pdx_parser::FileFormat;
use pdx_parser::encode_quoted_script_text;
use pdx_text::{TextRange, TextSize};

mod candidates;
mod context;
mod macro_constraints;
mod support;

pub(crate) use candidates::*;
pub(crate) use context::*;
pub(crate) use macro_constraints::infer_macro_quoted_script_constraints;
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
    if let Some(items) = macro_parameter_completion(snapshot, &input, position, cancellation)? {
        return Ok(CompletionResult {
            revision: snapshot.revision(),
            items,
        });
    }
    let replacement_range = word_range(&input.source, position);
    let prefix = input
        .source_text(replacement_range)
        .unwrap_or_default()
        .to_owned();
    let default_value_context = completion_value_context(&input, position);
    if input.format == FileFormat::Localisation {
        if localisation_language_header(&input, position) {
            return Ok(CompletionResult {
                revision: snapshot.revision(),
                items: Vec::new(),
            });
        }
        return if default_value_context {
            localisation_value_completion(snapshot, replacement_range, &prefix, cancellation)
        } else {
            localisation_key_completion(snapshot, replacement_range, &prefix, cancellation)
        };
    }
    let mut items = Vec::<RankedCompletionItem>::new();
    let mut member_cache = CompletionMemberCache::default();
    let semantic_context =
        semantic_completion_context_with_cancellation(snapshot, &input, position, cancellation)?;
    let value_context = semantic_context
        .as_ref()
        .and_then(|context| context.embedded_value_context)
        .unwrap_or(default_value_context);
    if let Some(context) = semantic_context.as_ref() {
        cancellation.checkpoint()?;
        if value_context {
            if let Some(property) = context.property.as_ref() {
                let inferred = add_inferred_macro_value_items(InferredMacroCompletionInput {
                    snapshot,
                    context,
                    property,
                    member_cache: &mut member_cache,
                    items: &mut items,
                    replacement_range,
                    prefix: &prefix,
                    cancellation,
                })?;
                if !inferred {
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
            }
        } else {
            let insert_assignment = context
                .property
                .as_ref()
                .is_none_or(|property| property.operator.is_none());
            // A type-instance wrapper such as `country_decisions = { … }` accepts only
            // free-form instance names; the wrapped type's keys must not be offered there.
            if !context.wrapper_container {
                add_semantic_key_items_ranked(
                    snapshot,
                    context,
                    &mut member_cache,
                    &mut items,
                    replacement_range,
                    &prefix,
                    insert_assignment,
                );
            }
        }
    } else if !value_context {
        // No rule-backed container covers the cursor and no file-root entry context exists
        // (for example an empty missions file, whose root series names are free-form).
    } else {
        // A bare `key = ` at the document root with no entry context: nothing to offer.
    }
    let mut items = finalize_completion_items(items);
    if let Some(context) = semantic_context.as_ref() {
        for _ in 0..context.quoted_depth {
            for item in &mut items {
                item.insert_text = encode_quoted_script_text(&item.insert_text);
            }
        }
    }
    cancellation.checkpoint()?;
    Ok(CompletionResult {
        revision: snapshot.revision(),
        items,
    })
}

fn macro_parameter_completion(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    position: TextSize,
    cancellation: &CancellationToken,
) -> Result<Option<Vec<CompletionItem>>, Cancelled> {
    if input.format != FileFormat::Script {
        return Ok(None);
    }
    let Some((replacement_range, prefix)) = dollar_parameter_fragment(&input.source, position)
    else {
        return Ok(None);
    };
    let Some(hir) = input.hir.as_deref() else {
        return Ok(None);
    };
    let Some(owner) = hir.definitions().iter().find(|definition| {
        position >= definition.range.start()
            && position <= definition.range.end()
            && crate::semantic::scripted_macro_type(snapshot, &definition.kind)
    }) else {
        return Ok(None);
    };
    let name_prefix = prefix.strip_prefix('$').unwrap_or_default();
    let value_context = completion_value_context(input, position);
    let mut items = Vec::new();
    for parameter in hir.parameter_definitions_for_owner(owner.range) {
        cancellation.checkpoint()?;
        if !completion_matches(&parameter.name, name_prefix) {
            continue;
        }
        let label = format!("${}$", parameter.name);
        items.push(CompletionItem {
            label: label.clone(),
            kind: CompletionKind::MacroParameter,
            detail: if value_context {
                "macro parameter (value)".to_owned()
            } else {
                "macro parameter (key)".to_owned()
            },
            documentation: None,
            replacement_range,
            insert_text: label,
            sort_score: 0,
            deprecated: false,
            resolve_data: None,
        });
    }
    items.sort_by(|left, right| completion_label_cmp(&left.label, &right.label));
    items.dedup_by(|left, right| left.label.eq_ignore_ascii_case(&right.label));
    Ok(Some(items))
}

fn dollar_parameter_fragment(source: &str, position: TextSize) -> Option<(TextRange, String)> {
    let position = usize::try_from(position).ok()?.min(source.len());
    if !source.is_char_boundary(position) {
        return None;
    }
    let word = word_range(source, u32::try_from(position).ok()?);
    let word_start = usize::try_from(word.start()).ok()?;
    let word_end = usize::try_from(word.end()).ok()?;
    let before_cursor = source.get(word_start..position)?;
    let relative_dollar = before_cursor.rfind('$')?;
    let start = word_start.checked_add(relative_dollar)?;
    let prefix = source.get(start..position)?;
    if prefix[1..].contains('$') {
        return None;
    }
    let end = source
        .get(position..word_end)?
        .find('$')
        .map_or(word_end, |offset| position + offset + 1);
    Some((
        TextRange::new(u32::try_from(start).ok()?, u32::try_from(end).ok()?)?,
        prefix.to_owned(),
    ))
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
                sort_score: 0,
                deprecated: false,
                resolve_data: None,
            },
            prefix,
            CompletionRankContext::new(
                CompletionSchemaTier::CurrentContext,
                CompletionSpecificity::Localisation,
                false,
                false,
            ),
        );
    }
    let items = finalize_completion_items(items);
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
    let mut items = Vec::<RankedCompletionItem>::new();
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
                sort_score: 0,
                deprecated: false,
                resolve_data: None,
            },
            prefix,
            CompletionRankContext::new(
                CompletionSchemaTier::CurrentContext,
                CompletionSpecificity::Localisation,
                false,
                false,
            ),
        );
    }
    let items = finalize_completion_items(items);
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
