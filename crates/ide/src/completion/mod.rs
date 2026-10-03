use crate::support::*;
use crate::types::*;
use engine::{AnalysisSnapshot, DocumentId};
use parser::FileFormat;
use text::{TextRange, TextSize};

mod ir;
mod support;

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
            coverage: Default::default(),
            revision: snapshot.revision(),
            items: Vec::new(),
        });
    };
    // Localisation values are prose, so automatic identifier completion is mostly noise.  The
    // file still participates in the workspace index for script-side hover and navigation;
    // only requests originating in the localisation document stay quiet.
    if input.format == FileFormat::Localisation {
        return Ok(CompletionResult {
            coverage: Default::default(),
            revision: snapshot.revision(),
            items: Vec::new(),
        });
    }
    if let Some(items) = dynamic_parameter_completion(snapshot, &input, position, cancellation)? {
        return Ok(CompletionResult {
            coverage: Default::default(),
            revision: snapshot.revision(),
            items,
        });
    }
    let result = ir::try_ir_completion(snapshot, &input, position, cancellation)?;
    let (items, coverage) = result.map_or_else(
        || (Vec::new(), Default::default()),
        |result| (result.value, result.coverage),
    );
    Ok(CompletionResult {
        revision: snapshot.revision(),
        items,
        coverage,
    })
}

fn dynamic_parameter_completion(
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
            && crate::semantic::dynamic_definition_type(snapshot, &definition.kind)
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
            kind: CompletionKind::DynamicParameter,
            detail: if value_context {
                "dynamic parameter (value)".to_owned()
            } else {
                "dynamic parameter (key)".to_owned()
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

/// Alias with the noun used by several editor adapters.
#[must_use]
pub fn completion(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
) -> CompletionResult {
    complete(snapshot, document, position)
}

/// Resolves documentation from the active compiled field.
#[must_use]
pub fn completion_resolve(snapshot: &AnalysisSnapshot, item: &CompletionItem) -> CompletionItem {
    let mut resolved = item.clone();
    if let Some(index) = item
        .resolve_data
        .as_deref()
        .and_then(|s| s.strip_prefix("ir-field:"))
        .and_then(|s| s.parse::<usize>().ok())
        && let Some(field) = snapshot.ir().fields.get(index)
        && let Some(doc) = field.doc
    {
        resolved.documentation = Some(snapshot.ir().strings().resolve(doc).to_owned());
    }
    resolved
}
