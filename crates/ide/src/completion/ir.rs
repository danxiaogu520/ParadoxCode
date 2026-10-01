use crate::ir_semantic::{self, WorkspaceFacts};
use crate::quoted_script::{QuotedScriptParse, QuotedScriptSession};
use crate::support::{ParsedContent, ParsedInput, word_range};
use crate::types::{CancellationToken, Cancelled, CompletionItem, CompletionKind};
use engine::AnalysisSnapshot;
use parser::{CstKind, CstNode, QuotedScript, encode_quoted_script_text};
use rules::ir::{FieldId, FieldValue, Matcher, MatcherId, RefTarget, Shape};
use text::{TextRange, TextSize};

pub(crate) fn try_ir_completion(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    position: TextSize,
    cancellation: &CancellationToken,
) -> Result<Option<Vec<CompletionItem>>, Cancelled> {
    if !ir_semantic::has_ir_schema(snapshot, input) {
        return Ok(None);
    }
    let Some(hir) = input.hir.as_deref() else {
        return Ok(None);
    };
    let ir = snapshot.ir();
    let (local_source, local_position, quoted_layers) =
        completion_coordinates(input, position, cancellation)?;
    let local_replacement = word_range(&local_source, local_position);
    let prefix = local_source
        .get(
            usize::try_from(local_replacement.start()).unwrap_or(local_source.len())
                ..usize::try_from(local_replacement.end()).unwrap_or(local_source.len()),
        )
        .unwrap_or_default();
    let replacement_range =
        map_completion_range(local_replacement, &quoted_layers).unwrap_or(local_replacement);
    let value_property = hir.properties().iter().find(|property| {
        position > property.key_range.end()
            && position <= property.range.end()
            && property.operator.is_some()
            && !schema_fact_at_cursor(hir, position).is_some_and(|active| {
                hir.field_fact_at(property.key_range).is_some_and(|field| {
                    !hir.schema_facts()
                        .iter()
                        .any(|parent| parent.schema == field.schema && parent.range == active.range)
                })
            })
            && !property
                .value_range
                .is_some_and(|range| hir.schema_facts().iter().any(|fact| fact.range == range))
    });
    let mut items = Vec::new();
    if let Some((invocation, summary)) = callable_invocation_at(ir, snapshot, hir, position)
        && let Some(property) = value_property.filter(|property| {
            property.path.len() == invocation.path.len() + 1
                && property.path.starts_with(&invocation.path)
                && summary
                    .parameters
                    .iter()
                    .any(|parameter| parameter.name.eq_ignore_ascii_case(&property.key))
        })
    {
        let constraints = ir_semantic::callable_parameter_matchers(
            snapshot,
            &summary.kind,
            &summary.name,
            cancellation,
        )?;
        if let Some(matchers) = constraints.get(&property.key.to_ascii_lowercase()) {
            append_callable_value_items(
                snapshot,
                ir,
                matchers,
                replacement_range,
                prefix,
                &property.key,
                cancellation,
                &mut items,
            )?;
            for item in &mut items {
                for _ in 0..quoted_layers.len() {
                    item.insert_text = encode_quoted_script_text(&item.insert_text);
                }
            }
            return Ok(Some(items));
        }
    }
    if value_property.is_none()
        && let Some((invocation, summary)) = callable_invocation_at(ir, snapshot, hir, position)
    {
        let current = hir
            .properties()
            .iter()
            .find(|property| contains(property.key_range, position));
        for parameter in &summary.parameters {
            cancellation.checkpoint()?;
            if !parameter
                .name
                .to_ascii_lowercase()
                .starts_with(&prefix.to_ascii_lowercase())
                || hir.properties().iter().any(|property| {
                    property.path.len() == invocation.path.len() + 1
                        && property.path.starts_with(&invocation.path)
                        && current.is_none_or(|current| current.key_range != property.key_range)
                        && property.key.eq_ignore_ascii_case(&parameter.name)
                })
            {
                continue;
            }
            items.push(CompletionItem {
                label: parameter.name.clone(),
                kind: CompletionKind::DynamicParameter,
                detail: if parameter.required {
                    "required callable parameter".into()
                } else {
                    "optional callable parameter".into()
                },
                documentation: None,
                replacement_range,
                insert_text: format!("{} = ", parameter.name),
                sort_score: u32::from(!parameter.required),
                deprecated: false,
                resolve_data: None,
            });
        }
        items.sort_by(|a, b| {
            (a.sort_score, a.label.to_ascii_lowercase())
                .cmp(&(b.sort_score, b.label.to_ascii_lowercase()))
        });
        for item in &mut items {
            for _ in 0..quoted_layers.len() {
                item.insert_text = encode_quoted_script_text(&item.insert_text);
            }
        }
        return Ok(Some(items));
    }
    if let Some(property) = value_property {
        let Some(fact) = hir.field_fact_at(property.key_range) else {
            return Ok(Some(items));
        };
        let mut candidates = fact
            .fields
            .iter()
            .copied()
            .filter(|field| ir.shape(*field) == Some(Shape::Scalar))
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            candidates.extend(
                ir.lookup(fact.schema, &property.key, Shape::Scalar)
                    .filter(|field| ir.gate_holds(ir.field(*field).gate, &fact.subtypes)),
            );
        }
        let facts = WorkspaceFacts { snapshot };
        let selected = property
            .scalar
            .as_ref()
            .and_then(|scalar| {
                candidates
                    .iter()
                    .copied()
                    .find(|candidate| match ir.field(*candidate).value {
                        FieldValue::Scalar(matcher) => {
                            ir_semantic::matcher_matches(ir, matcher, &scalar.value, &facts)
                        }
                        _ => false,
                    })
            })
            .or_else(|| candidates.first().copied());
        if let Some(field_id) = selected {
            let field = ir.field(field_id);
            if let FieldValue::Scalar(matcher) = field.value {
                append_value_items(
                    snapshot,
                    ir,
                    matcher,
                    field_id,
                    field.deprecated,
                    field.doc,
                    replacement_range,
                    prefix,
                    schema_fact_at_cursor(hir, position).map(|fact| &fact.state),
                    cancellation,
                    &mut items,
                )?;
            }
        }
    } else {
        let Some(schema_fact) = schema_fact_at_cursor(hir, position) else {
            return Ok(Some(items));
        };
        for field_id in ir.fields(schema_fact.schema, &schema_fact.subtypes) {
            cancellation.checkpoint()?;
            let field = ir.field(field_id);
            let current_property = hir
                .properties()
                .iter()
                .find(|property| contains(property.key_range, position));
            let existing_count = hir
                .properties()
                .iter()
                .filter(|property| {
                    if current_property
                        .is_some_and(|current| current.key_range == property.key_range)
                    {
                        return false;
                    }
                    crate::support::contains(schema_fact.range, property.key_range.start())
                        && hir.field_fact_at(property.key_range).is_some_and(|fact| {
                            fact.schema == schema_fact.schema && fact.fields.contains(&field_id)
                        })
                })
                .count() as u32;
            if field.card.max.is_some_and(|max| existing_count >= max) {
                continue;
            }
            let matcher = field.key;
            for label in ir_semantic::spellings_with_state(
                ir,
                matcher,
                snapshot,
                prefix,
                schema_fact_at_cursor(hir, position).map(|fact| &fact.state),
            ) {
                cancellation.checkpoint()?;
                if !label
                    .to_ascii_lowercase()
                    .starts_with(&prefix.to_ascii_lowercase())
                {
                    continue;
                }
                let label_text = key_insert_text(
                    snapshot,
                    ir,
                    matcher,
                    &schema_fact.subtypes,
                    ir.shape(field_id),
                    &label,
                    current_property.is_none_or(|property| property.operator.is_none()),
                );
                items.push(CompletionItem {
                    label: label.clone(),
                    kind: matcher_kind(ir.matcher(matcher)),
                    detail: ir_semantic::describe(ir, matcher),
                    documentation: field.doc.map(|doc| ir.strings().resolve(doc).to_owned()),
                    replacement_range,
                    insert_text: label_text,
                    sort_score: u32::from(field.deprecated),
                    deprecated: field.deprecated,
                    resolve_data: Some(format!("ir-field:{}", field_id.index())),
                });
            }
        }
    }
    items.sort_by(|a, b| {
        (a.sort_score, a.label.to_ascii_lowercase())
            .cmp(&(b.sort_score, b.label.to_ascii_lowercase()))
    });
    items.dedup_by(|a, b| a.label.eq_ignore_ascii_case(&b.label));
    for item in &mut items {
        for _ in 0..quoted_layers.len() {
            item.insert_text = encode_quoted_script_text(&item.insert_text);
        }
    }
    Ok(Some(items))
}

#[allow(clippy::too_many_arguments)]
fn append_callable_value_items(
    snapshot: &AnalysisSnapshot,
    ir: &rules::ir::RulesIr,
    matchers: &[MatcherId],
    replacement_range: TextRange,
    prefix: &str,
    parameter: &str,
    cancellation: &CancellationToken,
    items: &mut Vec<CompletionItem>,
) -> Result<(), Cancelled> {
    let Some(first) = matchers.first().copied() else {
        return Ok(());
    };
    let mut candidates = ir_semantic::spellings(ir, first, snapshot, prefix);
    let facts = WorkspaceFacts { snapshot };
    candidates.retain(|candidate| {
        matchers.iter().all(|matcher| {
            ir_semantic::matcher_matches_in_snapshot(snapshot, ir, *matcher, candidate, &facts)
        })
    });
    for label in candidates {
        cancellation.checkpoint()?;
        if !label
            .to_ascii_lowercase()
            .starts_with(&prefix.to_ascii_lowercase())
        {
            continue;
        }
        items.push(CompletionItem {
            label: label.clone(),
            kind: matcher_kind(ir.matcher(first)),
            detail: format!("value for Callable parameter `{parameter}`"),
            documentation: Some(ir_semantic::describe(ir, first)),
            replacement_range,
            insert_text: label,
            sort_score: 0,
            deprecated: false,
            resolve_data: None,
        });
    }
    Ok(())
}

fn callable_invocation_at<'a>(
    ir: &rules::ir::RulesIr,
    snapshot: &AnalysisSnapshot,
    hir: &'a hir::HirFile,
    position: TextSize,
) -> Option<(&'a hir::HirProperty, engine::DynamicDefinitionSummary)> {
    let callable = ir.trait_by_name("Callable")?;
    hir.properties()
        .iter()
        .filter(|property| property.scalar.is_none() && contains(property.range, position))
        .filter_map(|property| {
            let fact = hir.field_fact_at(property.key_range)?;
            let kind = fact.fields.iter().find_map(|field_id| {
                let field = ir.field(*field_id);
                let rules::ir::Matcher::Ref(RefTarget::Type { type_id, .. }) =
                    ir.matcher(field.key)
                else {
                    return None;
                };
                let info = ir.type_info(*type_id);
                (info
                    .trait_impls
                    .iter()
                    .any(|implementation| implementation.trait_id == callable)
                    || info.subtypes.iter().any(|subtype| {
                        fact.subtypes.contains(*type_id, subtype.name)
                            && subtype
                                .trait_impls
                                .iter()
                                .any(|implementation| implementation.trait_id == callable)
                    }))
                .then(|| ir.strings().resolve(info.name).to_owned())
            })?;
            if hir.definitions().iter().any(|definition| {
                definition.kind.eq_ignore_ascii_case(&kind)
                    && definition.name.eq_ignore_ascii_case(&property.key)
                    && definition.range.start() <= property.range.start()
                    && property.range.end() <= definition.range.end()
            }) {
                return None;
            }
            let summary =
                crate::semantic::dynamic_definition_summary(snapshot, &kind, &property.key)?;
            Some((property, summary))
        })
        .min_by_key(|(property, _)| property.range.len())
}

type CompletionCoordinates = (String, TextSize, Vec<(TextSize, QuotedScript)>);

fn completion_coordinates(
    input: &ParsedInput,
    position: TextSize,
    cancellation: &CancellationToken,
) -> Result<CompletionCoordinates, Cancelled> {
    let mut source = input.source.to_string();
    let mut local_position = position;
    let mut parsed = match &input.parsed {
        ParsedContent::Text(parsed) => parsed.clone(),
    };
    let mut layers = Vec::new();
    let mut session = QuotedScriptSession::new(cancellation);
    loop {
        cancellation.checkpoint()?;
        let Some(node) = quoted_at(parsed.root(), local_position) else {
            break;
        };
        let Some(raw) = parsed.text(node.range()) else {
            break;
        };
        let script = match session.parse(raw, layers.len())? {
            QuotedScriptParse::Parsed(script) => script,
            QuotedScriptParse::Opaque | QuotedScriptParse::Limited(_) => break,
        };
        let relative = local_position.saturating_sub(node.range().start());
        let Some(decoded) = script.source_map().source_offset(relative) else {
            break;
        };
        layers.push((node.range().start(), script.clone()));
        local_position = decoded;
        source = script.parsed().source().to_owned();
        parsed = std::sync::Arc::new(script.parsed().clone());
    }
    Ok((source, local_position, layers))
}

fn quoted_at(node: CstNode<'_>, position: TextSize) -> Option<CstNode<'_>> {
    if node.kind() == CstKind::QuotedString
        && position >= node.range().start()
        && position <= node.range().end()
    {
        return Some(node);
    }
    node.children().find_map(|child| quoted_at(child, position))
}

fn map_completion_range(
    mut range: TextRange,
    layers: &[(TextSize, QuotedScript)],
) -> Option<TextRange> {
    for (token_start, script) in layers.iter().rev() {
        let relative = script.source_map().decoded_range(range)?;
        range = TextRange::new(
            token_start.checked_add(relative.start())?,
            token_start.checked_add(relative.end())?,
        )?;
    }
    Some(range)
}

#[allow(clippy::too_many_arguments)]
fn append_value_items(
    snapshot: &AnalysisSnapshot,
    ir: &rules::ir::RulesIr,
    matcher: MatcherId,
    field_id: FieldId,
    deprecated: bool,
    doc: Option<rules::ir::Symbol>,
    replacement_range: TextRange,
    prefix: &str,
    scope_state: Option<&hir::ScopeState>,
    cancellation: &CancellationToken,
    items: &mut Vec<CompletionItem>,
) -> Result<(), Cancelled> {
    for label in ir_semantic::spellings_with_state(ir, matcher, snapshot, prefix, scope_state) {
        cancellation.checkpoint()?;
        if !label
            .to_ascii_lowercase()
            .starts_with(&prefix.to_ascii_lowercase())
        {
            continue;
        }
        items.push(CompletionItem {
            label: label.clone(),
            kind: matcher_kind(ir.matcher(matcher)),
            detail: ir_semantic::describe(ir, matcher),
            documentation: doc.map(|doc| ir.strings().resolve(doc).to_owned()),
            replacement_range,
            insert_text: label,
            sort_score: u32::from(deprecated),
            deprecated,
            resolve_data: Some(format!("ir-field:{}", field_id.index())),
        });
    }
    Ok(())
}

fn matcher_kind(matcher: &Matcher) -> CompletionKind {
    match matcher {
        Matcher::Enum { .. } => CompletionKind::EnumMember,
        Matcher::Scope(_) | Matcher::Link => CompletionKind::Scope,
        Matcher::Ref(RefTarget::Type { .. })
        | Matcher::Ref(RefTarget::Trait(_))
        | Matcher::Def { .. } => CompletionKind::Symbol,
        _ => CompletionKind::Value,
    }
}

fn key_insert_text(
    snapshot: &AnalysisSnapshot,
    ir: &rules::ir::RulesIr,
    matcher: MatcherId,
    subtypes: &rules::ir::SubtypeSet,
    shape: Option<Shape>,
    label: &str,
    assignment: bool,
) -> String {
    if !assignment {
        return label.to_owned();
    }
    match shape {
        Some(Shape::Block) => callable_snippet(snapshot, ir, matcher, subtypes, label)
            .unwrap_or_else(|| format!("{label} = {{\n\t$0\n}}")),
        Some(Shape::Quoted) => format!("{label} = \"\n\t$0\n\""),
        Some(Shape::Scalar) => callable_snippet(snapshot, ir, matcher, subtypes, label)
            .filter(|snippet| snippet.contains("$1"))
            .unwrap_or_else(|| format!("{label} = ")),
        None => format!("{label} = "),
    }
}

fn callable_snippet(
    snapshot: &AnalysisSnapshot,
    ir: &rules::ir::RulesIr,
    matcher: MatcherId,
    subtypes: &rules::ir::SubtypeSet,
    name: &str,
) -> Option<String> {
    let rules::ir::Matcher::Ref(RefTarget::Type { type_id, .. }) = ir.matcher(matcher) else {
        return None;
    };
    let callable = ir.trait_by_name("Callable")?;
    let info = ir.type_info(*type_id);
    if !info
        .trait_impls
        .iter()
        .any(|implementation| implementation.trait_id == callable)
        && !info.subtypes.iter().any(|subtype| {
            subtypes.contains(*type_id, subtype.name)
                && subtype
                    .trait_impls
                    .iter()
                    .any(|implementation| implementation.trait_id == callable)
        })
    {
        return None;
    }
    let kind = ir.strings().resolve(info.name);
    let summary = crate::semantic::dynamic_definition_summary(snapshot, kind, name)?;
    let required = summary
        .parameters
        .iter()
        .filter(|parameter| parameter.required)
        .collect::<Vec<_>>();
    if required.is_empty() {
        return Some(format!("{name} = {{\n\t$0\n}}"));
    }
    let mut body = format!("{name} = {{\n");
    for (index, parameter) in required.iter().enumerate() {
        body.push_str(&format!("\t{} = ${}\n", parameter.name, index + 1));
    }
    body.push_str(&format!("\t${}\n}}", required.len() + 1));
    Some(body)
}

fn contains(range: TextRange, position: TextSize) -> bool {
    if range.is_empty() {
        range.start() == position
    } else {
        position >= range.start() && position <= range.end()
    }
}

fn schema_fact_at_cursor(hir: &hir::HirFile, position: TextSize) -> Option<&hir::SchemaFact> {
    hir.schema_facts()
        .iter()
        .filter(|fact| {
            crate::support::contains(fact.range, position) || fact.range.end() == position
        })
        .min_by_key(|fact| fact.range.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matcher_kind_covers_ir_value_families() {
        assert_eq!(matcher_kind(&Matcher::Bool), CompletionKind::Value);
        assert_eq!(matcher_kind(&Matcher::Scope(None)), CompletionKind::Scope);
    }
}
