//! Semantic-rule hovers: property keys, scalar values, documentation, and provenance hints.

use super::render::{HoverModel, code_span};
use crate::support::{ParsedInput, contains};
use crate::types::{CancellationToken, Cancelled};
use engine::{AnalysisSnapshot, SourceRootKind};
use rules::ir::{FieldId, FieldValue, Matcher, MatcherId, RefTarget, RulesIr, ValueType};
use std::collections::{BTreeMap, BTreeSet};
use text::TextSize;

/// Projects only actual Template script consumptions into ordinary rule hover.
pub(crate) fn template_consumption_hover(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    position: TextSize,
    _word: &str,
    cancellation: &CancellationToken,
) -> Result<Option<HoverModel>, Cancelled> {
    let Some(projection) =
        crate::ir_template::consumption_at(snapshot, input, position, cancellation)?
    else {
        return Ok(None);
    };
    let mut models = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for point in &projection.positions {
        let word_range =
            crate::support::word_range(&projection.body.rendered.text, point.generated);
        let word = projection
            .body
            .hir
            .syntax()
            .text(word_range)
            .unwrap_or_default();
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
            let hir = hir::lower_ir_schema_in_range(
                parsed.clone(),
                snapshot.ir(),
                context.schema,
                context.subtypes.clone(),
                context.state.clone(),
                &crate::ir_semantic::WorkspaceFacts { snapshot },
                context.range,
            );
            let mut fragment = input.clone();
            fragment.source = parsed.source_handle();
            fragment.parsed = crate::support::ParsedContent::Text(parsed);
            fragment.hir = Some(std::sync::Arc::new(hir));
            let mut model = ir_field_hover(
                snapshot,
                &fragment,
                point.generated,
                word,
                true,
                cancellation,
            )?;
            if model.is_none() {
                model = ir_field_hover(
                    snapshot,
                    &fragment,
                    point.generated,
                    word,
                    false,
                    cancellation,
                )?;
            }
            if let Some(mut model) = model {
                model.coverage.merge(&projection.body.coverage);
                if seen.insert(model.render()) {
                    models.push(model);
                }
            }
        }
    }
    if models.len() == 1 {
        let mut model = models.pop().expect("one model");
        model.push_section(format!(
            "- Consumed through Template parameter `{}`",
            projection.parameter
        ));
        return Ok(Some(model));
    }
    if models.is_empty() {
        return Ok(None);
    }
    let mut model = HoverModel::new(format!(
        "### Template consumption `{}`",
        projection.parameter
    ));
    model.coverage = projection.body.coverage.clone();
    for item in models {
        model.push_model(item);
    }
    Ok(Some(model))
}

pub(crate) fn semantic_rule_hover_at(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    position: TextSize,
    word: &str,
    cancellation: &CancellationToken,
) -> Result<Option<HoverModel>, Cancelled> {
    ir_field_hover(snapshot, input, position, word, true, cancellation)
}

pub(crate) fn semantic_value_hover_at(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    position: TextSize,
    word: &str,
    cancellation: &CancellationToken,
) -> Result<Option<HoverModel>, Cancelled> {
    ir_field_hover(snapshot, input, position, word, false, cancellation)
}

/// Keeps rule-query source documentation alongside the symbol reached through it.
pub(crate) fn query_source_hover(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    position: TextSize,
    word: &str,
    cancellation: &CancellationToken,
) -> Result<Option<HoverModel>, Cancelled> {
    let Some(hir) = input.hir.as_deref() else {
        return Ok(None);
    };
    let Some(property) = hir
        .properties()
        .iter()
        .filter(|property| contains(property.range, position))
        .min_by_key(|property| property.range.len())
    else {
        return Ok(None);
    };
    let Some(fact) = hir.field_fact_at(property.key_range) else {
        return Ok(None);
    };
    let key = contains(property.key_range, position);
    let context = hir::query::PropertyQueryContext::for_property(hir.properties(), property);
    let spelling = if key {
        property.key.as_str()
    } else {
        property
            .scalar
            .as_ref()
            .map_or(word, |scalar| scalar.value.as_str())
    };
    let ir = snapshot.ir();
    let projected = fact.fields.iter().any(|id| {
        let field = ir.field(*id);
        let matcher = if key {
            Some(field.key)
        } else {
            match field.value {
                rules::ir::FieldValue::Scalar(matcher) => Some(matcher),
                _ => None,
            }
        };
        matcher.is_some_and(|matcher| {
            crate::ir_semantic::projected_source(ir, matcher, spelling, snapshot, &context)
                .is_some()
        })
    });
    if projected {
        ir_field_hover(snapshot, input, position, word, key, cancellation)
    } else {
        Ok(None)
    }
}

/// Provenance for a `texture_path` value hover: the resolved absolute path,
/// the source root serving it, and whether only the engine's extension
/// fallback saved the reference.
fn texture_resolution_section(snapshot: &AnalysisSnapshot, value: &str) -> String {
    match snapshot.resolve_texture_path(value) {
        Some(resolution) => {
            let origin = match resolution.hit.root_kind {
                SourceRootKind::Project => "the project",
                SourceRootKind::Dependency => "a dependency mod",
                SourceRootKind::Vanilla => "the game or a DLC pack",
            };
            let origin = snapshot
                .source_roots()
                .iter()
                .find(|root| root.id == resolution.hit.root_id)
                .and_then(|root| root.name.as_deref())
                .map_or_else(
                    || origin.to_owned(),
                    |name| format!("dependency {}", code_span(name)),
                );
            let path = code_span(&resolution.logical_path);
            if resolution.extension_fallback {
                format!(
                    "- resolved: {} → {path} (extension fallback) · {origin}",
                    code_span(value)
                )
            } else {
                format!("- resolved: {path} · {origin}")
            }
        }
        None => "- resolved: not found in any mod, game, or DLC pack root".to_owned(),
    }
}

/// Display category for a rule context: the three command namespaces keep
/// their canonical names; everything else humanizes the context identifier
/// (compound `root:` contexts use their tail segment). Callers that cannot
/// establish a category fall back to the bare symbol-hover title.
pub(crate) fn semantic_context_category(context: &str) -> String {
    let context = context.strip_prefix("root:").unwrap_or(context);
    match context {
        "trigger" => "Trigger".to_owned(),
        "effect" => "Effect".to_owned(),
        "modifier" => "Modifier".to_owned(),
        other => {
            let mut name = other.replace('_', " ");
            let mut characters = name.chars();
            match characters.next() {
                Some(first) => {
                    name = first.to_uppercase().collect::<String>() + characters.as_str();
                    name
                }
                None => name,
            }
        }
    }
}

fn ir_field_hover(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    position: TextSize,
    word: &str,
    key: bool,
    cancellation: &CancellationToken,
) -> Result<Option<HoverModel>, Cancelled> {
    let Some(hir) = input.hir.as_deref() else {
        return Ok(None);
    };
    let fact = if key {
        // Quoted-script key facts already carry ranges mapped into the
        // containing document, even though the outer property list is flat.
        hir.field_facts()
            .iter()
            .find(|fact| contains(fact.range, position))
    } else {
        hir.properties()
            .iter()
            .find(|property| {
                property
                    .scalar
                    .as_ref()
                    .is_some_and(|scalar| contains(scalar.range, position))
            })
            .and_then(|property| hir.field_fact_at(property.key_range))
    };
    let Some(fact) = fact else {
        return Ok(None);
    };
    let ir = snapshot.ir();
    if fact.fields.is_empty() {
        return Ok(None);
    }
    let property_key = if key {
        word
    } else {
        hir.properties()
            .iter()
            .find(|property| {
                property
                    .scalar
                    .as_ref()
                    .is_some_and(|scalar| contains(scalar.range, position))
            })
            .map_or(word, |property| property.key.as_str())
    };
    let is_exact = |id: &FieldId| {
        matches!(ir.matcher(ir.field(*id).key), Matcher::Literal(value)
            if ir.strings().resolve(*value).eq_ignore_ascii_case(property_key))
    };
    let mut fields = ir
        .lookup(
            fact.schema,
            property_key,
            ir.shape(fact.fields[0]).unwrap_or(rules::ir::Shape::Scalar),
        )
        .filter(is_exact)
        .collect::<Vec<_>>();
    if fields.is_empty() {
        fields = fact.fields.clone();
    }
    let schema_name = ir.strings().resolve(ir.schema(fact.schema).name);
    let context = schema_name.split("__").next().unwrap_or(schema_name);
    let context = context
        .strip_suffix("_body")
        .or_else(|| context.strip_suffix("_file"))
        .unwrap_or(context);
    let mut model = HoverModel::new(format!(
        "### {}{} {}",
        semantic_context_category(context),
        if key { "" } else { " value" },
        code_span(word)
    ));
    let states = hir
        .schema_facts()
        .iter()
        .filter(|parent| parent.schema == fact.schema && contains(parent.range, fact.range.start()))
        .collect::<Vec<_>>();
    let shortest = states.iter().map(|entry| entry.range.len()).min();
    let mut states = states
        .into_iter()
        .filter(|entry| Some(entry.range.len()) == shortest);
    let state = states
        .next()
        .map(|entry| &entry.state)
        .filter(|state| states.all(|entry| &entry.state == *state));
    let current = state.and_then(|state| state.current.first());
    let scope_name = |value: &hir::ScopeValue| match value {
        hir::ScopeValue::Known(scopes) => scopes
            .iter()
            .map(|value| value.as_ref())
            .collect::<Vec<_>>()
            .join(" or "),
        hir::ScopeValue::Unknown => "any".into(),
        hir::ScopeValue::Invalid => "invalid".into(),
    };
    let applicable_fields = fields
        .iter()
        .copied()
        .filter(|id| {
            current.is_none_or(|current| {
                ir.field(*id).scope.as_ref().is_none_or(|scope| {
                    scope.scopes_in.is_empty()
                        || crate::ir_semantic::scope_allows(ir, current, &scope.scopes_in)
                })
            })
        })
        .collect::<Vec<_>>();
    let mut documents = Vec::new();
    let mut display_keys = BTreeSet::new();
    for id in &fields {
        cancellation.checkpoint()?;
        let field = ir.field(*id);
        display_keys.extend(ir.field_localisation_keys(*id, key, word));
        if let Some(doc) = field.doc {
            documents.push(ir.strings.resolve(doc).to_owned());
        }
        if field.deprecated {
            model.push_section("Deprecated".to_owned());
        }
        let query_matcher = if key {
            Some(field.key)
        } else {
            match field.value {
                FieldValue::Scalar(matcher) => Some(matcher),
                _ => None,
            }
        };
        if let Some(property) = hir
            .properties()
            .iter()
            .find(|property| property.key_range == fact.range)
        {
            let query_context =
                hir::query::PropertyQueryContext::for_property(hir.properties(), property)
                    .defer_templates(
                        hir.parameter_references()
                            .iter()
                            .any(|reference| contains(reference.owner_range, position)),
                    );
            let spelling = if key {
                property.key.as_str()
            } else {
                property
                    .scalar
                    .as_ref()
                    .map_or(word, |scalar| scalar.value.as_str())
            };
            if let Some(source) = query_matcher.and_then(|matcher| {
                crate::ir_semantic::projected_source(
                    ir,
                    matcher,
                    spelling,
                    snapshot,
                    &query_context,
                )
            }) {
                let source_field = ir.field(source);
                if let Some(doc) = source_field.doc {
                    documents.push(ir.strings.resolve(doc).to_owned());
                }
            }
        }
        if !key
            && matches!(field.value, FieldValue::Scalar(matcher)
            if matches!(ir.matcher(matcher), Matcher::Path(_)))
        {
            let value = hir
                .properties()
                .iter()
                .filter_map(|property| property.scalar.as_ref())
                .find(|scalar| contains(scalar.range, position))
                .map_or(word, |scalar| scalar.value.as_str());
            model.push_section(texture_resolution_section(snapshot, value));
        }
    }
    let mut problem = None;
    if let Some(current) = current
        && applicable_fields.is_empty()
    {
        let required = fields
            .iter()
            .filter_map(|id| ir.field(*id).scope.as_ref())
            .flat_map(|scope| scope.scopes_in.iter())
            .map(|scope| ir.strings().resolve(*scope))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(code_span)
            .collect::<Vec<_>>()
            .join(" or ");
        problem = Some(format!(
            "**Invalid scope.** {} requires {required}. Current scope is {}.",
            code_span(property_key),
            code_span(&scope_name(current)),
        ));
    } else if !key
        && let Some(scalar) = hir
            .properties()
            .iter()
            .filter_map(|property| property.scalar.as_ref())
            .find(|scalar| contains(scalar.range, position))
        && !applicable_fields
            .iter()
            .any(|id| match ir.field(*id).value {
                FieldValue::Scalar(matcher) => match state {
                    Some(state) => crate::ir_semantic::matcher_matches_with_state(
                        snapshot,
                        ir,
                        matcher,
                        &scalar.value,
                        &crate::ir_queries::SnapshotSymbolFacts { snapshot },
                        state,
                    ),
                    None => crate::ir_semantic::matcher_matches_in_snapshot(
                        snapshot,
                        ir,
                        matcher,
                        &scalar.value,
                        &crate::ir_queries::SnapshotSymbolFacts { snapshot },
                    ),
                },
                _ => true,
            })
    {
        let expected = applicable_fields
            .iter()
            .filter_map(|id| match ir.field(*id).value {
                FieldValue::Scalar(matcher) => Some(crate::ir_semantic::describe(ir, matcher)),
                _ => None,
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(" or ");
        problem = Some(format!(
            "**Invalid value.** {} must be {expected}.",
            code_span(&scalar.value),
        ));
    }
    let key_fields = key.then(|| {
        ir.lookup_key(fact.schema, property_key)
            .filter(is_exact)
            .collect::<Vec<_>>()
    });
    let displayed_fields = match key_fields.as_deref() {
        Some(exact) if !exact.is_empty() => exact,
        _ if key || applicable_fields.is_empty() => &fields,
        _ => &applicable_fields,
    };
    model
        .sections
        .insert(0, value_scope_table(ir, displayed_fields));
    if let Some(problem) = problem {
        model.sections.insert(0, problem);
    }
    let mut rows = Vec::new();
    for (label, key) in display_keys {
        let previews =
            crate::localisation::localisation_previews_for_name(snapshot, &key, cancellation)?;
        rows.extend(crate::localisation::preview_rows(
            snapshot,
            label.map(str::to_owned),
            &key,
            &previews,
            false,
        ));
    }
    model.push_localisation(&rows);
    documents.sort();
    documents.dedup();
    for document in documents {
        let document = document.lines().collect::<Vec<_>>().join("  \n");
        model.push_section(format!("#### Documentation\n\n{document}"));
    }
    if let Some(scope) = scope_section(ir, hir, position) {
        model.push_section(scope);
    }
    Ok(Some(model))
}

fn value_scope_table(ir: &RulesIr, fields: &[FieldId]) -> String {
    // Collect every scope for each value before grouping rows. Shared alternatives
    // then appear once without granting a scope access to the other alternatives.
    let mut scopes_by_value = BTreeMap::<String, Option<BTreeSet<&str>>>::new();
    for id in fields {
        let field = ir.field(*id);
        let scopes = field
            .scope
            .iter()
            .flat_map(|scope| scope.scopes_in.iter())
            .map(|scope| ir.strings().resolve(*scope))
            .collect::<Vec<_>>();
        let unrestricted = scopes.is_empty() || scopes.contains(&"any");
        let mut add_value = |value| {
            let allowed = scopes_by_value
                .entry(value)
                .or_insert_with(|| Some(BTreeSet::new()));
            if unrestricted {
                *allowed = None;
            } else if let Some(allowed) = allowed {
                allowed.extend(scopes.iter().copied());
            }
        };
        if !field.value_types.is_empty() {
            for value_type in &field.value_types {
                add_value(match *value_type {
                    ValueType::Alias(name) => code_span(ir.strings().resolve(name)),
                    ValueType::Matcher(matcher) => value_label(ir, matcher),
                });
            }
            continue;
        }
        match field.value {
            FieldValue::Scalar(matcher) => match ir.matcher(matcher) {
                Matcher::Union(alternatives) => {
                    for alternative in alternatives {
                        add_value(value_label(ir, *alternative));
                    }
                }
                _ => add_value(value_label(ir, matcher)),
            },
            FieldValue::Block(_) | FieldValue::SelfBlock => add_value("script block".into()),
        }
    }

    let mut values_by_scope = BTreeMap::<_, Vec<_>>::new();
    for (value, scopes) in scopes_by_value {
        values_by_scope.entry(scopes).or_default().push(value);
    }
    let mut lines = vec![
        "| Valid scopes | Allowed values |".to_owned(),
        "| --- | --- |".to_owned(),
    ];
    for (scopes, values) in values_by_scope {
        let scopes = scopes.map_or_else(
            || "Any".to_owned(),
            |scopes| {
                scopes
                    .into_iter()
                    .map(code_span)
                    .collect::<Vec<_>>()
                    .join(", ")
            },
        );
        let values = values.join(" · ");
        lines.push(format!(
            "| {} | {} |",
            table_cell(&scopes),
            table_cell(&values)
        ));
    }
    lines.join("\n")
}

fn value_label(ir: &RulesIr, matcher: MatcherId) -> String {
    match ir.matcher(matcher) {
        Matcher::Bool => "`yes` or `no`".to_owned(),
        Matcher::Ref(RefTarget::Type {
            type_id,
            subtype: None,
            ..
        }) => code_span(ir.strings().resolve(ir.type_info(*type_id).name)),
        _ => {
            let label = crate::ir_semantic::describe(ir, matcher);
            label
                .strip_prefix("an ")
                .or_else(|| label.strip_prefix("a "))
                .map(str::to_owned)
                .unwrap_or(label)
        }
    }
}

fn table_cell(text: &str) -> String {
    text.replace(['\r', '\n'], " ").replace('|', "\\|")
}

/// Show only register types shared by every applicable scope at this position.
pub(crate) fn scope_section(
    ir: &rules::ir::RulesIr,
    hir: &hir::HirFile,
    position: TextSize,
) -> Option<String> {
    let body = hir.properties().iter().find_map(|property| {
        (property.scalar.is_none() && contains(property.key_range, position))
            .then_some(property.value_range)
            .flatten()
    });
    let mut scopes = body.map_or_else(Vec::new, |body| {
        hir.schema_facts()
            .iter()
            .filter(|fact| fact.range == body)
            .collect()
    });
    let title = if scopes.is_empty() {
        scopes = hir
            .schema_facts()
            .iter()
            .filter(|fact| contains(fact.range, position))
            .collect();
        let shortest = scopes.iter().map(|fact| fact.range.len()).min()?;
        scopes.retain(|fact| fact.range.len() == shortest);
        "Scope at this position"
    } else {
        "Scope inside this block"
    };
    let known_register =
        |state: &hir::ScopeState, name| match hir::ir_scope_register_value(ir, state, name) {
            Some(hir::ScopeValue::Known(types))
                if types.len() == 1 && types[0].as_ref() != "any" =>
            {
                Some(types[0].clone())
            }
            _ => None,
        };
    let first = &scopes.first()?.state;
    let mut rows = Vec::new();
    for name in ["ROOT", "FROM", "THIS", "PREV", "PREV_PREV"] {
        if let Some(scope) = known_register(first, name)
            && scopes
                .iter()
                .skip(1)
                .all(|fact| known_register(&fact.state, name).as_ref() == Some(&scope))
        {
            let scope = table_cell(&code_span(&scope));
            rows.push(format!("| `{name}` | {scope} |"));
        }
    }
    (!rows.is_empty()).then(|| {
        format!(
            "#### {title}\n\n| Register | Scope |\n| --- | --- |\n{}",
            rows.join("\n")
        )
    })
}
