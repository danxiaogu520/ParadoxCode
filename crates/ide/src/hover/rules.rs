//! Semantic-rule hovers: property keys, scalar values, documentation, and provenance hints.

use super::render::{HoverModel, code_span};
use crate::support::{ParsedInput, contains};
use crate::types::{CancellationToken, Cancelled};
use engine::{AnalysisSnapshot, SourceRootKind};
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
        model.push_section(item.render());
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
            // A DLC zip member shows the archive plus the entry inside it;
            // the archive alone would not say which asset serves the reference.
            let resolved = match resolution.hit.archive_member.as_deref() {
                Some(member) => {
                    format!("{} :: {member}", resolution.hit.path.as_path().display())
                }
                None => resolution.hit.path.as_path().display().to_string(),
            };
            let fallback = if resolution.extension_fallback {
                "\n- note: resolved through the engine's `.tga`/`.dds` extension fallback"
            } else {
                ""
            };
            format!("- resolved: `{resolved}`\n- found in: {origin}{fallback}")
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
    let mut fields = ir
        .lookup(
            fact.schema,
            property_key,
            ir.shape(fact.fields[0]).unwrap_or(rules::ir::Shape::Scalar),
        )
        .filter(|id| {
            matches!(ir.matcher(ir.field(*id).key), rules::ir::Matcher::Literal(value)
                if ir.strings().resolve(*value).eq_ignore_ascii_case(property_key))
        })
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
    let state = hir
        .schema_facts()
        .iter()
        .filter(|parent| parent.schema == fact.schema && contains(parent.range, fact.range.start()))
        .min_by_key(|parent| parent.range.len())
        .map(|parent| &parent.state);
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
    let mut values = Vec::new();
    let mut documents = Vec::new();
    let mut scopes = Vec::new();
    for id in &fields {
        cancellation.checkpoint()?;
        let field = ir.field(*id);
        let value = match field.value {
            rules::ir::FieldValue::Scalar(matcher) => crate::ir_semantic::describe(ir, matcher),
            rules::ir::FieldValue::Block(schema) => {
                format!("a `{}` block", ir.strings.resolve(ir.schema(schema).name))
            }
            rules::ir::FieldValue::SelfBlock => format!(
                "a `{}` block",
                ir.strings.resolve(ir.schema(fact.schema).name)
            ),
        };
        let allowed = field.scope.as_ref().map(|scope| &scope.scopes_in);
        let available = current.is_none_or(|current| {
            allowed.is_none_or(|scopes| {
                scopes.is_empty() || crate::ir_semantic::scope_allows(ir, current, scopes)
            })
        });
        if key || available {
            if fields.len() == 1
                && let rules::ir::FieldValue::Scalar(matcher) = field.value
                && let rules::ir::Matcher::Union(alternatives) = ir.matcher(matcher)
            {
                values.extend(alternatives.iter().map(|matcher| {
                    (
                        crate::ir_semantic::describe(ir, *matcher),
                        allowed,
                        available,
                    )
                }));
            } else {
                values.push((value, allowed, available));
            }
        }
        if let Some(doc) = field.doc {
            documents.push(ir.strings.resolve(doc).to_owned());
        }
        if field.deprecated {
            model.push_section("Deprecated".to_owned());
        }
        if let Some(origin) = ir.provenance_of(*id) {
            model.push_section(format!(
                "Source: `{}` `{}`",
                ir.strings.resolve(origin.file),
                ir.strings.resolve(origin.pointer)
            ));
        }
        let query_matcher = if key {
            Some(field.key)
        } else {
            match field.value {
                rules::ir::FieldValue::Scalar(matcher) => Some(matcher),
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
                if let Some(origin) = ir.provenance_of(source) {
                    model.push_section(format!(
                        "Query source: `{}` `{}`",
                        ir.strings.resolve(origin.file),
                        ir.strings.resolve(origin.pointer)
                    ));
                }
            }
        }
        if let Some(state) = state {
            let next = hir::transition_ir_scope(ir, state.clone(), field.scope.as_ref(), word);
            if next.current != state.current
                || field
                    .scope
                    .as_ref()
                    .is_some_and(|scope| scope.push.is_some() || !scope.set.is_empty())
            {
                let target = next
                    .current
                    .first()
                    .map_or_else(|| "any".into(), scope_name);
                scopes.push(format!("- `{word}` enters `{target}`"));
            }
        }
        if !key
            && matches!(field.value, rules::ir::FieldValue::Scalar(matcher)
            if matches!(ir.matcher(matcher), rules::ir::Matcher::Path(_)))
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
    if !key
        && let Some(scalar) = hir
            .properties()
            .iter()
            .filter_map(|property| property.scalar.as_ref())
            .find(|scalar| contains(scalar.range, position))
        && !fields.iter().any(|id| match ir.field(*id).value {
            rules::ir::FieldValue::Scalar(matcher) => match state {
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
        model.push_section("The current value does not match an allowed value type.".into());
    }
    values.sort_by(|left, right| left.0.cmp(&right.0));
    values.dedup();
    let count = values.len();
    let lines = values
        .into_iter()
        .map(|(value, allowed, available)| {
            let scopes = allowed.filter(|scopes| !scopes.is_empty()).map(|scopes| {
                scopes
                    .iter()
                    .map(|scope| code_span(ir.strings().resolve(*scope)))
                    .collect::<Vec<_>>()
                    .join(", ")
            });
            match scopes {
                Some(scopes) if !available => format!(
                    "- {value}; unavailable in current scope `{}`; valid scopes: {scopes}",
                    current.map_or_else(|| "any".into(), scope_name)
                ),
                Some(scopes) => format!("- value: {value}\n- valid scopes: {scopes}"),
                None => format!("- value: {value}"),
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    model.sections.insert(
        0,
        if count > 1 {
            format!("#### Allowed value types ({count})\n\n{lines}")
        } else {
            lines
        },
    );
    documents.sort();
    documents.dedup();
    for document in documents {
        let document = document.lines().collect::<Vec<_>>().join("  \n");
        model.push_section(format!("#### Documentation\n\n{document}"));
    }
    if let Some(current) = current {
        let here = scope_name(current);
        if here != "any" || !scopes.is_empty() {
            scopes.sort();
            scopes.dedup();
            model.push_section(format!(
                "#### Scope\n\n- here: `{here}`{}",
                if scopes.is_empty() {
                    String::new()
                } else {
                    format!("\n{}", scopes.join("\n"))
                }
            ));
        }
    }
    Ok(Some(model))
}
