use crate::ir_semantic::{self, WorkspaceFacts};
use crate::support::{ParsedContent, ParsedInput, word_range};
use crate::types::{CancellationToken, Cancelled, CompletionItem, CompletionKind};
use engine::AnalysisSnapshot;
use hir::analysis::{Analysis, AnalysisCoverage, AnalysisLimit};
use rules::ir::{FieldId, FieldValue, Matcher, MatcherId, RefTarget, Shape};
use std::collections::BTreeSet;
use text::{TextRange, TextSize};

pub(crate) fn try_ir_completion(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    position: TextSize,
    cancellation: &CancellationToken,
) -> Result<Option<Analysis<Vec<CompletionItem>>>, Cancelled> {
    if let Some(projected) =
        projected_consumption_completion(snapshot, input, position, cancellation)?
    {
        return Ok(Some(projected));
    }
    let mut coverage = AnalysisCoverage::default();
    let items = try_ir_completion_inner(snapshot, input, position, cancellation, 0, &mut coverage)?;
    Ok(items.map(|value| Analysis { value, coverage }))
}

fn projected_consumption_completion(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    position: u32,
    cancellation: &CancellationToken,
) -> Result<Option<Analysis<Vec<CompletionItem>>>, Cancelled> {
    let Some(projection) =
        crate::ir_template::consumption_at(snapshot, input, position, cancellation)?
    else {
        return Ok(None);
    };
    let mut coverage = projection.body.coverage.clone();
    let mut items = Vec::new();
    let mut seen = BTreeSet::new();
    for point in &projection.positions {
        cancellation.checkpoint()?;
        let mut contexts = projection
            .body
            .hir
            .schema_facts()
            .iter()
            .filter(|context| contains(context.range, point.generated))
            .collect::<Vec<_>>();
        let shortest = contexts.iter().map(|context| context.range.len()).min();
        contexts.retain(|context| Some(context.range.len()) == shortest);
        for context in contexts {
            let parsed = std::sync::Arc::new(projection.body.hir.syntax().clone());
            let fragment_hir = hir::lower_ir_schema_in_range(
                parsed.clone(),
                snapshot.ir(),
                context.schema,
                context.subtypes.clone(),
                context.state.clone(),
                &WorkspaceFacts { snapshot },
                context.range,
            );
            let mut fragment = input.clone();
            fragment.source = parsed.source_handle();
            fragment.parsed = ParsedContent::Text(parsed);
            fragment.hir = Some(std::sync::Arc::new(fragment_hir));
            let candidates = try_ir_completion_inner(
                snapshot,
                &fragment,
                point.generated,
                cancellation,
                1,
                &mut coverage,
            )?
            .unwrap_or_default();
            for mut item in candidates {
                let Some((range, text)) = projection.edit(
                    point,
                    item.replacement_range,
                    &item.insert_text,
                    item.is_snippet,
                ) else {
                    continue;
                };
                item.replacement_range = range;
                item.insert_text = text;
                let mut interpretations = Vec::new();
                let checked = crate::ir_template::validate_consumption_edit(
                    snapshot,
                    input,
                    &projection,
                    &item,
                    &mut interpretations,
                    cancellation,
                )?;
                coverage.merge(&checked.coverage);
                if checked.value == hir::analysis::Validation::Invalid {
                    continue;
                }
                item.template_evidence = Some(crate::types::TemplateCompletionEvidence {
                    validation: checked.value,
                    witness: Default::default(),
                    interpretations,
                });
                if seen.insert((
                    item.label.clone(),
                    item.replacement_range,
                    item.insert_text.clone(),
                    item.is_snippet,
                )) {
                    items.push(item);
                }
            }
        }
    }
    Ok(Some(Analysis {
        value: items,
        coverage,
    }))
}

fn try_ir_completion_inner(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    position: TextSize,
    cancellation: &CancellationToken,
    depth: usize,
    coverage: &mut AnalysisCoverage,
) -> Result<Option<Vec<CompletionItem>>, Cancelled> {
    cancellation.checkpoint()?;
    if depth >= 8 {
        coverage.limits.insert(AnalysisLimit::ConsumptionDepth);
        return Ok(Some(Vec::new()));
    }
    if !ir_semantic::has_ir_schema(snapshot, input) {
        return Ok(None);
    }
    let Some(hir) = input.hir.as_deref() else {
        return Ok(None);
    };
    let ir = snapshot.ir();
    if let Some(items) = path_value_completion(snapshot, input, hir, position, cancellation)? {
        return Ok(Some(items));
    }
    let local_source = input.source.to_string();
    let local_position = position;
    let local_replacement = word_range(&local_source, local_position);
    let prefix = input.source_text(local_replacement).unwrap_or_default();
    let replacement_range = local_replacement;
    let value_property = hir.properties().iter().find(|property| {
        position > property.key_range.end()
            && (position <= property.range.end()
                || (property.value_range.is_none()
                    && input
                        .source
                        .get(property.range.end() as usize..position as usize)
                        .is_some_and(|gap| gap.trim().is_empty())))
            && property.operator.is_some()
            && !schema_fact_at_cursor(hir, position).is_some_and(|active| {
                hir.field_fact_at(property.key_range).is_some_and(|field| {
                    !hir.schema_facts()
                        .iter()
                        .any(|parent| parent.schema == field.schema && parent.range == active.range)
                })
            })
            && !property.value_range.is_some_and(|range| {
                !range.is_empty() && hir.schema_facts().iter().any(|fact| fact.range == range)
            })
    });
    let mut items = Vec::new();
    if let Some((invocation, summary)) = template_invocation_at(ir, snapshot, hir, position)
        && let Some(property) = value_property.filter(|property| {
            property.path.len() == invocation.path.len() + 1
                && property.path.starts_with(&invocation.path)
                && summary
                    .parameters
                    .iter()
                    .any(|parameter| parameter.name.eq_ignore_ascii_case(&property.key))
        })
    {
        let constraints = crate::ir_template::parameter_sites_at(
            snapshot,
            hir,
            invocation,
            &summary.kind,
            &summary.name,
            &property.key,
            cancellation,
        )?;
        {
            coverage.merge(&constraints.coverage);
            append_template_value_items(
                snapshot,
                &constraints,
                hir,
                invocation,
                &summary,
                coverage,
                replacement_range,
                prefix,
                &property.key,
                cancellation,
                &mut items,
            )?;
            for item in &mut items {
                item.insert_text = scalar_insert_text(
                    &item.insert_text,
                    property.scalar.as_ref().is_some_and(|scalar| scalar.quoted),
                );
            }
            return Ok(Some(items));
        }
    }
    if value_property.is_none()
        && let Some((_, summary)) = template_invocation_at(ir, snapshot, hir, position)
    {
        for parameter in &summary.parameters {
            cancellation.checkpoint()?;
            if !parameter
                .name
                .to_ascii_lowercase()
                .starts_with(&prefix.to_ascii_lowercase())
            {
                continue;
            }
            items.push(CompletionItem {
                is_snippet: false,
                template_evidence: None,
                label: parameter.name.clone(),
                kind: CompletionKind::DynamicParameter,
                detail: "parameter".into(),
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
        return Ok(Some(items));
    }
    let contracts = crate::dynamic_contracts::dynamic_contract_report_view(snapshot, cancellation)?;
    let scope_state = schema_fact_at_cursor(hir, position).map(|fact| {
        let mut state = fact.state.clone();
        if state.current.first().is_none_or(|current| matches!(current, hir::ScopeValue::Unknown) || matches!(current, hir::ScopeValue::Known(scopes) if scopes.iter().all(|scope| scope.eq_ignore_ascii_case("any"))))
            && let Some(owner) = hir.definitions().iter().find(|definition| contains(definition.range, position)
                && crate::semantic::dynamic_definition_type(snapshot, &definition.kind))
            && let Some(crate::dynamic_contracts::ScopeContract::Scopes(scopes)) = contracts.contract(&owner.kind, &owner.name)
            && scopes.len() == 1 {
            state.current = vec![hir::ScopeValue::known_single(&scopes[0])];
        }
        state
    });
    if let Some(property) = value_property {
        let Some(fact) = hir.field_fact_at(property.key_range) else {
            return Ok(Some(items));
        };
        let facts = WorkspaceFacts { snapshot };
        let mut candidates = ir
            .lookup(fact.schema, &property.key, Shape::Scalar)
            .filter(|field| {
                ir_semantic::matcher_matches(ir, ir.field(*field).key, &property.key, &facts)
            })
            .collect::<Vec<_>>();
        if candidates
            .iter()
            .any(|field| matches!(ir.matcher(ir.field(*field).key), Matcher::Literal(_)))
        {
            candidates
                .retain(|field| matches!(ir.matcher(ir.field(*field).key), Matcher::Literal(_)));
        }
        candidates.retain(|field_id| {
            ir.field(*field_id).scope.as_ref().is_none_or(|scope| {
                scope.scopes_in.is_empty()
                    || scope_state
                        .as_ref()
                        .and_then(|state| state.current.first())
                        .is_none_or(|current| {
                            ir_semantic::scope_allows(ir, current, &scope.scopes_in)
                        })
            })
        });
        // The current token may be incomplete or belong to another overload.
        // Offer every scalar overload available in the current scope.
        for field_id in candidates.iter().copied() {
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
                    scope_state.as_ref(),
                    cancellation,
                    &mut items,
                )?;
            }
        }
        if candidates.is_empty() {
            for field_id in ir.lookup(fact.schema, &property.key, Shape::Block) {
                let field = ir.field(field_id);
                let FieldValue::Block(schema) = field.value else {
                    continue;
                };
                let Some(matcher) = ir.schema(schema).items else {
                    continue;
                };
                let start = items.len();
                append_value_items(
                    snapshot,
                    ir,
                    matcher,
                    field_id,
                    field.deprecated,
                    field.doc,
                    replacement_range,
                    prefix,
                    scope_state.as_ref(),
                    cancellation,
                    &mut items,
                )?;
                for item in &mut items[start..] {
                    item.insert_text = format!("{{ {} }}", item.insert_text);
                }
            }
        }
    } else {
        let Some(schema_fact) = schema_fact_at_cursor(hir, position) else {
            return Ok(Some(items));
        };
        let state = scope_state.as_ref().expect("schema fact at cursor");
        if let Some(matcher) = ir.schema(schema_fact.schema).items {
            for label in
                ir_semantic::spellings_with_state(ir, matcher, snapshot, prefix, Some(state))
            {
                cancellation.checkpoint()?;
                let Some(rank) = prefix_rank(&label, prefix) else {
                    continue;
                };
                items.push(CompletionItem {
                    is_snippet: false,
                    template_evidence: None,
                    label: label.clone(),
                    kind: matcher_kind(ir.matcher(matcher)),
                    detail: ir_semantic::describe(ir, matcher),
                    documentation: None,
                    replacement_range,
                    insert_text: label,
                    sort_score: rank,
                    deprecated: false,
                    resolve_data: None,
                });
            }
        }
        for field_id in ir.fields(schema_fact.schema) {
            cancellation.checkpoint()?;
            let field = ir.field(field_id);
            if field.scope.as_ref().is_some_and(|scope| {
                !scope.scopes_in.is_empty()
                    && state.current.first().is_some_and(|current| {
                        !ir_semantic::scope_allows(ir, current, &scope.scopes_in)
                    })
            }) {
                continue;
            }
            let current_property = hir
                .properties()
                .iter()
                .find(|property| contains(property.key_range, position));
            let matcher = field.key;
            for label in
                ir_semantic::spellings_with_state(ir, matcher, snapshot, prefix, Some(state))
            {
                cancellation.checkpoint()?;
                let Some(rank) = prefix_rank(&label, prefix) else {
                    continue;
                };
                let template_kind = crate::ir_template::template_kind(ir, matcher);
                if let Some(kind) = &template_kind
                    && let Some(crate::dynamic_contracts::ScopeContract::Scopes(expected)) =
                        contracts.contract(kind, &label)
                    && let Some(current) = state.current.first()
                {
                    let expected = expected
                        .iter()
                        .filter_map(|name| ir.strings().lookup_folded(name))
                        .collect::<Vec<_>>();
                    if !ir_semantic::scope_allows(ir, current, &expected) {
                        continue;
                    }
                }
                let existing_count = hir
                    .properties()
                    .iter()
                    .filter(|property| {
                        property.key.eq_ignore_ascii_case(&label)
                            && current_property
                                .is_none_or(|current| current.key_range != property.key_range)
                            && crate::support::contains(
                                schema_fact.range,
                                property.key_range.start(),
                            )
                            && hir.field_fact_at(property.key_range).is_some_and(|fact| {
                                fact.schema == schema_fact.schema
                                    && fact
                                        .fields
                                        .iter()
                                        .any(|id| ir.shape(*id) == ir.shape(field_id))
                            })
                    })
                    .count() as u32;
                if field.card.max.is_some_and(|max| existing_count >= max) {
                    continue;
                }
                let assignment =
                    current_property.is_none_or(|property| property.operator.is_none());
                let label_text = key_insert_text(
                    snapshot,
                    ir,
                    matcher,
                    ir.shape(field_id),
                    &label,
                    assignment,
                );
                let label_text = if assignment
                    && matches!(field.value, FieldValue::Scalar(value) if matches!(ir.matcher(value), Matcher::Path(_)))
                {
                    format!("{} = \"$0\"", crate::insertion::snippet_literal(&label))
                } else {
                    label_text
                };
                items.push(CompletionItem {
                    is_snippet: assignment
                        && (ir.shape(field_id) == Some(Shape::Block) || label_text.contains("$0")),
                    template_evidence: None,
                    label: label.clone(),
                    kind: if template_kind.is_some() {
                        CompletionKind::DynamicDefinition
                    } else if matches!(ir.matcher(matcher), Matcher::Literal(_)) {
                        CompletionKind::Key
                    } else {
                        matcher_kind(ir.matcher(matcher))
                    },
                    detail: if let Matcher::Enum { id: enum_id } = ir.matcher(matcher) {
                        ir.strings()
                            .resolve(ir.enums[enum_id.index()].name)
                            .to_owned()
                    } else {
                        ir_semantic::field_context(ir, schema_fact.schema, field_id)
                    },
                    documentation: field.doc.map(|doc| ir.strings().resolve(doc).to_owned()),
                    replacement_range,
                    insert_text: label_text,
                    sort_score: rank
                        + 1000 * u32::from(field.deprecated)
                        + 100
                            * u32::from(ir.provenance_of(field_id).is_some_and(|origin| {
                                !ir.strings().resolve(origin.pointer).starts_with(&format!(
                                    "/schemas/{}/fields/",
                                    ir.strings().resolve(ir.schema(schema_fact.schema).name)
                                ))
                            })),
                    deprecated: field.deprecated,
                    resolve_data: Some(format!(
                        "ir-field:{}:{}",
                        snapshot.ir_fingerprint(),
                        field_id.index()
                    )),
                });
            }
        }
    }
    if let Some(property) = value_property.filter(|property| {
        property.scalar.is_some()
            || hir.field_fact_at(property.key_range).is_some_and(|field| {
                !hir::checking::field_candidates(
                    ir,
                    field.schema,
                    &property.key,
                    Shape::Scalar,
                    &WorkspaceFacts { snapshot },
                )
                .is_empty()
            })
    }) {
        for item in &mut items {
            if !item.is_snippet {
                item.insert_text = scalar_insert_text(
                    &item.insert_text,
                    property.scalar.as_ref().is_some_and(|scalar| scalar.quoted),
                );
            }
        }
    }
    items.sort_by(|a, b| {
        (a.sort_score, a.label.to_ascii_lowercase())
            .cmp(&(b.sort_score, b.label.to_ascii_lowercase()))
    });
    let mut seen = std::collections::BTreeSet::new();
    let mut exact_labels = std::collections::BTreeSet::new();
    items.retain(|item| {
        let block = item
            .insert_text
            .split_once('=')
            .is_some_and(|(_, value)| value.trim_start().starts_with('{'));
        exact_labels.insert(item.label.clone())
            && seen.insert((item.label.to_ascii_lowercase(), block))
    });
    Ok(Some(items))
}

#[allow(clippy::too_many_arguments)]
fn append_template_value_items(
    snapshot: &AnalysisSnapshot,
    sites: &[crate::ir_template::ParameterSite],
    source: &hir::HirFile,
    invocation: &hir::HirProperty,
    summary: &engine::DynamicDefinitionSummary,
    coverage: &mut AnalysisCoverage,
    replacement_range: TextRange,
    prefix: &str,
    parameter: &str,
    cancellation: &CancellationToken,
    items: &mut Vec<CompletionItem>,
) -> Result<(), Cancelled> {
    let mut connected = sites.to_vec();
    let mut inputs = crate::ir_template::invocation_inputs(source, invocation);
    inputs.values.remove(&parameter.to_ascii_lowercase());
    for related in summary.parameters.iter().filter(|related| {
        !related.name.eq_ignore_ascii_case(parameter)
            && !inputs
                .values
                .contains_key(&related.name.to_ascii_lowercase())
    }) {
        let other = crate::ir_template::parameter_sites_for_inputs(
            snapshot,
            &summary.kind,
            &summary.name,
            &related.name,
            &inputs,
            crate::ir_template::invocation_state(source, invocation),
            cancellation,
        )?;
        coverage.merge(&other.coverage);
        connected.extend(other.value);
    }
    let sites = connected.as_slice();
    let relations = crate::ir_template::relational_candidates(
        snapshot,
        sites,
        parameter,
        prefix,
        cancellation,
    )?;
    coverage.merge(&relations.coverage);
    let mut emitted = BTreeSet::new();
    for mut witness in relations {
        cancellation.checkpoint()?;
        let Some(label) = witness.get(&parameter.to_ascii_lowercase()).cloned() else {
            continue;
        };
        let Some(rank) = prefix_rank(&label, prefix) else {
            continue;
        };
        let mut checked = crate::ir_template::validate_candidate(
            snapshot,
            source,
            invocation,
            summary,
            parameter,
            &witness,
            cancellation,
        )?;
        let scalar_unknown = sites.iter().any(|site| {
            !matches!(site.domain, crate::ir_template::Domain::Payload { .. })
                && site.witness_validation(snapshot, &witness) == hir::analysis::Validation::Unknown
        });
        if scalar_unknown && checked.value == hir::analysis::Validation::Valid {
            checked.value = hir::analysis::Validation::Unknown;
            checked
                .coverage
                .residuals
                .insert(hir::analysis::ResidualReason::Binding);
        }
        coverage.merge(&checked.coverage);
        if checked.value == hir::analysis::Validation::Invalid {
            continue;
        }
        let interpretations = crate::ir_template::candidate_interpretations(
            snapshot,
            source,
            invocation,
            summary,
            &witness,
            parameter,
            cancellation,
        )?;
        witness.remove(&parameter.to_ascii_lowercase());
        if !emitted.insert((label.clone(), witness.clone())) {
            continue;
        }
        let conditions = witness
            .iter()
            .map(|(name, value)| format!("`{name}` = `{value}`"))
            .collect::<Vec<_>>()
            .join(", ");
        let detail = if !conditions.is_empty() {
            format!("requires {conditions}")
        } else if checked.value == hir::analysis::Validation::Unknown {
            format!("unresolved value for Template parameter `{parameter}`")
        } else {
            format!("value for Template parameter `{parameter}`")
        };
        items.push(CompletionItem {
            is_snippet: false,
            template_evidence: Some(crate::types::TemplateCompletionEvidence {
                validation: checked.value,
                witness,
                interpretations,
            }),
            label: label.clone(),
            kind: CompletionKind::Value,
            detail,
            documentation: None,
            replacement_range,
            insert_text: label,
            sort_score: rank,
            deprecated: false,
            resolve_data: None,
        });
    }
    Ok(())
}

fn template_invocation_at<'a>(
    ir: &rules::ir::RulesIr,
    snapshot: &AnalysisSnapshot,
    hir: &'a hir::HirFile,
    position: TextSize,
) -> Option<(&'a hir::HirProperty, engine::DynamicDefinitionSummary)> {
    let template = ir.trait_by_name("Template")?;
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
                    .any(|implementation| implementation.trait_id == template))
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

fn path_value_completion(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    hir: &hir::HirFile,
    position: TextSize,
    cancellation: &CancellationToken,
) -> Result<Option<Vec<CompletionItem>>, Cancelled> {
    let ir = snapshot.ir();
    let Some((scalar, field)) = hir.properties().iter().find_map(|property| {
        let scalar = property.scalar.as_ref()?;
        if !contains(scalar.range, position) { return None; }
        let fact = hir.field_fact_at(property.key_range)?;
        fact.fields.iter().find_map(|id| {
            let field = ir.field(*id);
            matches!(field.value, FieldValue::Scalar(id) if matches!(ir.matcher(id), Matcher::Path(Some(category))
                if ir.strings().resolve(*category).eq_ignore_ascii_case("gfx")))
                .then_some((scalar, field))
        })
    }) else { return Ok(None); };
    let start = scalar.range.start() + u32::from(scalar.quoted);
    let Some(range) = TextRange::new(start, position) else {
        return Ok(Some(Vec::new()));
    };
    let prefix = input.source_text(range).unwrap_or_default();
    let children = snapshot.texture_catalog().children_with_prefix(prefix);
    let mut items = Vec::new();
    for (labels, kind, detail) in [
        (children.directories, CompletionKind::Folder, "directory"),
        (children.files, CompletionKind::Value, "texture path"),
    ] {
        for label in labels {
            cancellation.checkpoint()?;
            items.push(CompletionItem {
                is_snippet: false,
                template_evidence: None,
                insert_text: label.to_owned(),
                label: label.to_owned(),
                kind,
                detail: detail.into(),
                documentation: field.doc.map(|id| ir.strings().resolve(id).to_owned()),
                replacement_range: range,
                sort_score: u32::from(kind != CompletionKind::Folder),
                deprecated: field.deprecated,
                resolve_data: None,
            });
        }
    }
    Ok(Some(items))
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
        let Some(rank) = prefix_rank(&label, prefix) else {
            continue;
        };
        items.push(CompletionItem {
            is_snippet: false,
            template_evidence: None,
            label: label.clone(),
            kind: matcher_kind(ir.matcher(matcher)),
            detail: if let Matcher::Ref(RefTarget::Type { type_id, .. }) = ir.matcher(matcher)
                && ir
                    .type_info(*type_id)
                    .builtin
                    .iter()
                    .any(|member| ir.strings().resolve(*member).eq_ignore_ascii_case(&label))
            {
                format!(
                    "engine-set {}",
                    ir.strings().resolve(ir.type_info(*type_id).name)
                )
            } else {
                ir_semantic::describe(ir, matcher)
            },
            documentation: doc.map(|doc| ir.strings().resolve(doc).to_owned()),
            replacement_range,
            insert_text: label,
            sort_score: rank + 1000 * u32::from(deprecated),
            deprecated,
            resolve_data: Some(format!(
                "ir-field:{}:{}",
                snapshot.ir_fingerprint(),
                field_id.index()
            )),
        });
    }
    Ok(())
}

fn scalar_insert_text(text: &str, quoted: bool) -> String {
    if quoted {
        return parser::encode_quoted_script_text(text);
    }
    if text.is_empty()
        || text
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '"' | '#' | '{' | '}' | '='))
    {
        format!("\"{}\"", parser::encode_quoted_script_text(text))
    } else {
        text.to_owned()
    }
}

fn matcher_kind(matcher: &Matcher) -> CompletionKind {
    match matcher {
        Matcher::Enum { .. } => CompletionKind::EnumMember,
        Matcher::Scope(_) | Matcher::Link => CompletionKind::Scope,
        Matcher::Ref(RefTarget::Type { .. }) | Matcher::Def { .. } => CompletionKind::Symbol,
        _ => CompletionKind::Value,
    }
}

fn prefix_rank(label: &str, prefix: &str) -> Option<u32> {
    let label = label.to_ascii_lowercase();
    let prefix = prefix.to_ascii_lowercase();
    if label.starts_with(&prefix) {
        Some(0)
    } else if label.contains(&prefix) {
        Some(100)
    } else {
        None
    }
}

fn key_insert_text(
    snapshot: &AnalysisSnapshot,
    ir: &rules::ir::RulesIr,
    matcher: MatcherId,
    shape: Option<Shape>,
    label: &str,
    assignment: bool,
) -> String {
    if !assignment {
        return label.to_owned();
    }
    match shape {
        Some(Shape::Block) => template_snippet(snapshot, ir, matcher, label).unwrap_or_else(|| {
            format!(
                "{} = {{\n\t$0\n}}",
                crate::insertion::snippet_literal(label)
            )
        }),
        Some(Shape::Scalar) => template_snippet(snapshot, ir, matcher, label)
            .unwrap_or_else(|| format!("{} = $0", crate::insertion::snippet_literal(label))),
        None => format!("{label} = "),
    }
}

fn template_snippet(
    snapshot: &AnalysisSnapshot,
    ir: &rules::ir::RulesIr,
    matcher: MatcherId,
    name: &str,
) -> Option<String> {
    let rules::ir::Matcher::Ref(RefTarget::Type { type_id, .. }) = ir.matcher(matcher) else {
        return None;
    };
    let template = ir.trait_by_name("Template")?;
    let info = ir.type_info(*type_id);
    if !info
        .trait_impls
        .iter()
        .any(|implementation| implementation.trait_id == template)
    {
        return None;
    }
    let kind = ir.strings().resolve(info.name);
    crate::semantic::dynamic_definition_summary(snapshot, kind, name)?;
    Some(crate::semantic::scripted_definition_snippet(
        snapshot, kind, name,
    ))
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
