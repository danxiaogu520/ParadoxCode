use crate::resolution::*;
use crate::semantic::*;
use crate::support::*;
use crate::types::*;
use pdx_engine::hir::{HirFile, Scope};
use pdx_engine::{AnalysisSnapshot, DocumentId, DocumentSource, SourceFileId};
use pdx_parser::{FileFormat, SyntaxError};
use pdx_rules::RuleShape;
use pdx_text::TextRange;

/// Runs diagnostics for all open overlays.  Disk-only files are intentionally excluded from push
/// diagnostics; they still participate in navigation and workspace-symbol queries.
#[must_use]
pub fn analyze(snapshot: &AnalysisSnapshot) -> AnalysisResult {
    let mut diagnostics = Vec::new();
    for document in snapshot.documents().values() {
        if document.source() != DocumentSource::Overlay {
            continue;
        }
        if let Some(analysis) = analyze_document(snapshot, document.id()) {
            diagnostics.extend(analysis.diagnostics);
        }
    }
    diagnostics.sort_by_key(|diagnostic| {
        (
            diagnostic.range.start(),
            diagnostic.range.end(),
            diagnostic.code,
        )
    });
    AnalysisResult {
        revision: snapshot.revision(),
        scope: Scope::Unknown,
        diagnostics,
    }
}

/// Analyses one open or disk-backed document.
#[must_use]
pub fn analyze_document(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
) -> Option<FileAnalysis> {
    let input = input_for_document(snapshot, document)?;
    Some(analyze_input(snapshot, &input))
}

/// Analyses one indexed disk file.
#[must_use]
pub fn analyze_source_file(
    snapshot: &AnalysisSnapshot,
    file: SourceFileId,
) -> Option<FileAnalysis> {
    let input = input_for_source_file(snapshot, file)?;
    Some(analyze_input(snapshot, &input))
}

/// Returns diagnostics for one document, or an empty vector for unsupported/nonexistent files.
#[must_use]
pub fn diagnostics(snapshot: &AnalysisSnapshot, document: &DocumentId) -> Vec<Diagnostic> {
    uncancelled(diagnostics_with_cancellation(
        snapshot,
        document,
        &CancellationToken::new(),
    ))
}

/// Returns diagnostics while cooperatively stopping when `cancellation` is marked.
pub fn diagnostics_with_cancellation(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    cancellation: &CancellationToken,
) -> Result<Vec<Diagnostic>, Cancelled> {
    cancellation.checkpoint()?;
    let Some(input) = input_for_document(snapshot, document) else {
        return Ok(Vec::new());
    };
    analyze_input_with_cancellation(snapshot, &input, cancellation)
        .map(|analysis| analysis.diagnostics)
}
pub(crate) fn analyze_input(snapshot: &AnalysisSnapshot, input: &ParsedInput) -> FileAnalysis {
    uncancelled(analyze_input_with_cancellation(
        snapshot,
        input,
        &CancellationToken::new(),
    ))
}

pub(crate) fn analyze_input_with_cancellation(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    cancellation: &CancellationToken,
) -> Result<FileAnalysis, Cancelled> {
    cancellation.checkpoint()?;
    let semantic = semantic_data(snapshot, input);
    cancellation.checkpoint()?;
    let resolution = DirectResolutionContext::new(snapshot);
    let mut diagnostics = syntax_diagnostics(input);
    diagnostics.extend(semantic_rule_diagnostics(snapshot, input, cancellation)?);
    let mut unknown_scope_reported = false;
    for property in properties(input) {
        cancellation.checkpoint()?;
        if property.key.eq_ignore_ascii_case("scope")
            && let Some((value, range)) = property.value.as_ref()
            && !input.profile.is_scope(value)
            && !unknown_scope_reported
        {
            diagnostics.push(Diagnostic {
                code: DiagnosticCode::UnknownScope,
                severity: DiagnosticCode::UnknownScope.severity(),
                range: *range,
                message: format!("unknown scope `{value}`"),
            });
            unknown_scope_reported = true;
        }
    }
    for reference in &semantic.references {
        cancellation.checkpoint()?;
        match resolution.resolve(&reference.kind, &reference.name) {
            Resolution::Missing => diagnostics.push(Diagnostic {
                code: DiagnosticCode::UnknownSymbol,
                severity: DiagnosticCode::UnknownSymbol.severity(),
                range: reference.range,
                message: format!("unknown {} symbol `{}`", reference.kind, reference.name),
            }),
            Resolution::Ambiguous => diagnostics.push(Diagnostic {
                code: DiagnosticCode::AmbiguousSymbol,
                severity: DiagnosticCode::AmbiguousSymbol.severity(),
                range: reference.range,
                message: format!("ambiguous {} symbol `{}`", reference.kind, reference.name),
            }),
            Resolution::Unique(_) => {}
        }
    }
    diagnostics.sort_by_key(|diagnostic| {
        (
            diagnostic.range.start(),
            diagnostic.range.end(),
            diagnostic.code,
        )
    });
    diagnostics.dedup_by(|left, right| {
        left.code == right.code
            && left.severity == right.severity
            && left.range == right.range
            && left.message == right.message
    });
    cancellation.checkpoint()?;
    Ok(FileAnalysis {
        revision: snapshot.revision(),
        document: input.document.clone(),
        file: input.file,
        format: Some(input.format),
        scope: Scope::Unknown,
        diagnostics,
        symbols: semantic
            .definitions
            .into_iter()
            .map(|definition| definition.symbol)
            .collect(),
        references: semantic
            .references
            .into_iter()
            .map(|reference| {
                let location = reference.location();
                ReferenceInfo {
                    kind: reference.kind,
                    name: reference.name,
                    location,
                }
            })
            .collect(),
    })
}
pub(crate) fn semantic_rule_diagnostics(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    cancellation: &CancellationToken,
) -> Result<Vec<Diagnostic>, Cancelled> {
    cancellation.checkpoint()?;
    if input.format != FileFormat::Script || snapshot.rules().model().semantic.rules.is_empty() {
        return Ok(Vec::new());
    }
    let ParsedContent::Text(parsed) = &input.parsed;
    let roots = script_properties(input, parsed.root());
    cancellation.checkpoint()?;
    let mut diagnostics = Vec::new();
    for property in roots {
        cancellation.checkpoint()?;
        let Some(context) = semantic_root_context(snapshot, &property.key, input.path.as_ref())
        else {
            continue;
        };
        let scope =
            semantic_initial_scope(snapshot, input, &context, &property.key, property.key_range);
        if let Some(type_name) = context.strip_prefix("type:")
            && snapshot
                .rules()
                .model()
                .semantic
                .type_descriptors
                .get(type_name)
                .is_some_and(|descriptor| {
                    descriptor.skip_root_paths.iter().any(|path| {
                        path.first().is_some_and(|key| {
                            key.eq_ignore_ascii_case("any")
                                || key.eq_ignore_ascii_case(&property.key)
                        })
                    })
                })
        {
            for child in &property.block {
                let child_scope =
                    semantic_initial_scope(snapshot, input, &context, &child.key, child.key_range);
                validate_semantic_container(
                    snapshot,
                    &context,
                    &[],
                    &child.block,
                    &child.bare_values,
                    &child_scope,
                    input.hir.as_deref(),
                    &mut diagnostics,
                    cancellation,
                    child.block_range.is_some(),
                )?;
            }
            continue;
        }
        validate_semantic_container(
            snapshot,
            &context,
            &[],
            &property.block,
            &property.bare_values,
            &scope,
            input.hir.as_deref(),
            &mut diagnostics,
            cancellation,
            property.block_range.is_some(),
        )?;
    }
    Ok(diagnostics)
}
#[allow(clippy::too_many_arguments)] // Recursive validation carries explicit semantic state.
pub(crate) fn validate_semantic_container(
    snapshot: &AnalysisSnapshot,
    context: &str,
    parent_path: &[String],
    properties: &[ScriptProperty],
    bare_values: &[(String, TextRange)],
    scope: &ScopeContext,
    hir: Option<&HirFile>,
    diagnostics: &mut Vec<Diagnostic>,
    cancellation: &CancellationToken,
    block_container: bool,
) -> Result<(), Cancelled> {
    cancellation.checkpoint()?;
    let rules = semantic_rules_for_container(snapshot, context, parent_path, scope);
    if rules.is_empty() {
        return Ok(());
    }
    let selected_alternative = semantic_selected_alternative(
        snapshot,
        &rules,
        parent_path,
        properties,
        bare_values,
        scope,
    );
    let mut counts = std::collections::BTreeMap::<String, u32>::new();
    for property in properties {
        cancellation.checkpoint()?;
        let fact_scope = hir
            .and_then(|hir| hir.scope_fact(property.key_range, context))
            .map(|fact| scope_context_from_hir(snapshot.game_profile_handle(), &fact.state));
        let scope = fact_scope.as_ref().unwrap_or(scope);
        let key = property.key.to_ascii_lowercase();
        let count = counts.entry(key).or_default();
        *count = count.saturating_add(1);
        let transparent_wrapper = context.eq_ignore_ascii_case("trigger")
            && snapshot
                .game_profile()
                .is_transparent_scope_wrapper(&property.key);
        let matching = rules
            .iter()
            .filter(|rule| {
                !matches!(rule.shape, RuleShape::LeafValue)
                    && semantic_rule_key_matches(snapshot, rule, parent_path, &property.key)
            })
            .copied()
            .collect::<Vec<_>>();
        if matching.is_empty() && transparent_wrapper {
            // EU4 logical wrappers (AND/OR/NOT) do not introduce a new rule context or
            // scope. Their children are validated as siblings of the wrapper itself.
        } else if matching.is_empty() {
            diagnostics.push(Diagnostic {
                code: DiagnosticCode::UnknownKey,
                severity: DiagnosticCode::UnknownKey.severity(),
                range: property.key_range,
                message: format!(
                    "unexpected key `{}` in rule context `{context}`",
                    property.key
                ),
            });
        } else {
            let scoped_matching = matching
                .iter()
                .filter(|rule| semantic_scope_allows(rule, scope))
                .copied()
                .collect::<Vec<_>>();
            if scoped_matching.is_empty() {
                diagnostics.push(Diagnostic {
                    code: DiagnosticCode::WrongScope,
                    severity: semantic_rule_severity(
                        matching.iter().copied(),
                        DiagnosticCode::WrongScope,
                    ),
                    range: property.key_range,
                    message: format!(
                        "`{}` is not available in game scope `{}` ({})",
                        property.key,
                        scope.current,
                        semantic_rule_provenance(matching[0])
                    ),
                });
            }
            let applicable = if scoped_matching.is_empty() {
                &matching
            } else {
                &scoped_matching
            };
            let valid = applicable
                .iter()
                .any(|rule| semantic_property_matches(snapshot, rule, property, scope));
            if !valid {
                diagnostics.push(Diagnostic {
                    code: DiagnosticCode::InvalidValue,
                    severity: semantic_rule_severity(
                        applicable.iter().copied(),
                        DiagnosticCode::InvalidValue,
                    ),
                    range: property
                        .scalar
                        .as_ref()
                        .map_or(property.key_range, |(_, range)| *range),
                    message: format!(
                        "value of `{}` does not match the semantic rule ({})",
                        property.key,
                        semantic_rule_provenance(applicable[0])
                    ),
                });
            }
            validate_scripted_macro_arguments(snapshot, applicable, property, diagnostics);
            let max_occurs = applicable
                .iter()
                .filter(|rule| {
                    !semantic_rule_is_alias_definition(rule)
                        && semantic_rule_is_selected(rule, selected_alternative.as_deref())
                })
                .filter_map(|rule| rule.max_occurs)
                .max();
            if let Some(max_occurs) = max_occurs
                && *count > max_occurs
            {
                diagnostics.push(Diagnostic {
                    code: DiagnosticCode::Cardinality,
                    severity: 2,
                    range: property.key_range,
                    message: format!(
                        "`{}` occurs {} times, but rule cardinality allows at most {} ({})",
                        property.key,
                        count,
                        max_occurs,
                        semantic_rule_provenance(applicable[0])
                    ),
                });
            }
        }
        let cached_child_fact = cached_scope_fact_for_property(
            snapshot,
            hir,
            context,
            parent_path,
            property,
            &matching,
            selected_alternative.as_deref(),
            scope,
            transparent_wrapper,
        );
        let destination = if let Some(fact) = cached_child_fact {
            Some((
                fact.context.clone(),
                fact.parent_path.clone(),
                scope_context_from_hir(snapshot.game_profile_handle(), &fact.state),
            ))
        } else {
            semantic_selected_transition(
                snapshot,
                &matching,
                selected_alternative.as_deref(),
                context,
                parent_path,
                property,
                scope,
                transparent_wrapper,
            )
            .map(|rule| {
                let (next_context, child_path) = semantic_transition_destination(
                    rule,
                    context,
                    parent_path,
                    &property.key,
                    transparent_wrapper,
                );
                let next_scope = semantic_child_scope(snapshot, scope, rule);
                (next_context, child_path, next_scope)
            })
        };
        let mut structural_path = parent_path.to_vec();
        if !transparent_wrapper {
            structural_path.push(property.key.clone());
        }
        let Some((next_context, child_path, next_scope)) = destination else {
            let structural_rules =
                semantic_rules_for_container(snapshot, context, &structural_path, scope);
            if !structural_rules.is_empty() {
                validate_semantic_container(
                    snapshot,
                    context,
                    &structural_path,
                    &property.block,
                    &property.bare_values,
                    scope,
                    hir,
                    diagnostics,
                    cancellation,
                    property.block_range.is_some(),
                )?;
            }
            continue;
        };
        let destination_is_structural = next_context.eq_ignore_ascii_case(context)
            && child_path.len() == structural_path.len()
            && child_path
                .iter()
                .zip(&structural_path)
                .all(|(left, right)| left.eq_ignore_ascii_case(right));
        if !destination_is_structural {
            let structural_rules =
                semantic_rules_for_container(snapshot, context, &structural_path, scope);
            if !structural_rules.is_empty() {
                // Clauses such as `limit` are evaluated after the enclosing scope link has
                // moved to its target, so structural and transitioned children share next_scope.
                let (structural_properties, transition_properties): (Vec<_>, Vec<_>) =
                    property.block.iter().cloned().partition(|child| {
                        structural_rules.iter().any(|rule| {
                            !matches!(rule.shape, RuleShape::LeafValue)
                                && semantic_rule_key_matches(
                                    snapshot,
                                    rule,
                                    &structural_path,
                                    &child.key,
                                )
                        })
                    });
                let (structural_values, transition_values): (Vec<_>, Vec<_>) = property
                    .bare_values
                    .iter()
                    .cloned()
                    .partition(|(value, _)| {
                        structural_rules.iter().any(|rule| {
                            matches!(rule.shape, RuleShape::LeafValue)
                                && semantic_leaf_value_matches(snapshot, rule, value, &next_scope)
                        })
                    });
                validate_semantic_container(
                    snapshot,
                    context,
                    &structural_path,
                    &structural_properties,
                    &structural_values,
                    &next_scope,
                    hir,
                    diagnostics,
                    cancellation,
                    true,
                )?;
                validate_semantic_container(
                    snapshot,
                    &next_context,
                    &child_path,
                    &transition_properties,
                    &transition_values,
                    &next_scope,
                    hir,
                    diagnostics,
                    cancellation,
                    true,
                )?;
                continue;
            }
        }
        validate_semantic_container(
            snapshot,
            &next_context,
            &child_path,
            &property.block,
            &property.bare_values,
            &next_scope,
            hir,
            diagnostics,
            cancellation,
            property.block_range.is_some(),
        )?;
    }
    for (value, value_range) in bare_values {
        cancellation.checkpoint()?;
        let matching = rules
            .iter()
            .filter(|rule| {
                matches!(rule.shape, RuleShape::LeafValue)
                    && semantic_leaf_value_matches(snapshot, rule, value, scope)
            })
            .copied()
            .collect::<Vec<_>>();
        if matching.is_empty() {
            diagnostics.push(Diagnostic {
                code: DiagnosticCode::InvalidValue,
                severity: DiagnosticCode::InvalidValue.severity(),
                range: *value_range,
                message: format!(
                    "bare value `{value}` does not match the semantic rule value clause"
                ),
            });
        }
    }
    let empty_range = properties
        .first()
        .map_or_else(|| TextRange::empty(0), |property| property.key_range);
    if block_container {
        for rule in rules
            .iter()
            .filter(|rule| semantic_scope_allows(rule, scope))
        {
            cancellation.checkpoint()?;
            if !semantic_rule_is_selected(rule, selected_alternative.as_deref()) {
                continue;
            }
            if semantic_rule_is_alias_definition(rule) {
                continue;
            }
            if matches!(rule.shape, RuleShape::LeafValue) {
                let count = bare_values
                    .iter()
                    .filter(|(value, _)| semantic_leaf_value_matches(snapshot, rule, value, scope))
                    .count();
                let count = u32::try_from(count).unwrap_or(u32::MAX);
                if let Some(min_occurs) = semantic_min_occurs(rule)
                    && count < min_occurs
                {
                    diagnostics.push(Diagnostic {
                    code: DiagnosticCode::Cardinality,
                    severity: semantic_min_cardinality_severity(rule),
                    range: empty_range,
                    message: format!(
                        "semantic rule value clause requires at least {min_occurs} value(s), but `{}` occurs {count} times ({})",
                        semantic_value_matcher_label(&rule.value),
                        semantic_rule_provenance(rule)
                    ),
                });
                }
                if let Some(max_occurs) = rule.max_occurs
                    && count > max_occurs
                {
                    diagnostics.push(Diagnostic {
                    code: DiagnosticCode::Cardinality,
                    severity: 2,
                    range: bare_values.first().map_or(empty_range, |(_, range)| *range),
                    message: format!(
                        "semantic rule value clause allows at most {max_occurs} value(s), but found {count} ({})",
                        semantic_rule_provenance(rule)
                    ),
                });
                }
                continue;
            }
            let Some(min_occurs) = semantic_min_occurs(rule) else {
                continue;
            };
            let count = properties
                .iter()
                .filter(|property| {
                    semantic_rule_key_matches(snapshot, rule, parent_path, &property.key)
                        && !matches!(rule.shape, RuleShape::LeafValue)
                })
                .count();
            let count = u32::try_from(count).unwrap_or(u32::MAX);
            if count < min_occurs {
                diagnostics.push(Diagnostic {
                code: DiagnosticCode::Cardinality,
                severity: semantic_min_cardinality_severity(rule),
                range: empty_range,
                message: format!(
                    "semantic rule requires at least {min_occurs} occurrence(s), but `{}` occurs {count} times ({})",
                    semantic_matcher_label(&rule.key),
                    semantic_rule_provenance(rule)
                ),
            });
            }
        }
    }
    Ok(())
}

fn validate_scripted_macro_arguments(
    snapshot: &AnalysisSnapshot,
    rules: &[&pdx_rules::SemanticRule],
    property: &ScriptProperty,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if property.block_range.is_none() {
        return;
    }
    let summary = rules.iter().find_map(|rule| {
        let type_name = match &rule.key {
            pdx_rules::KeyMatcher::Type(type_name) | pdx_rules::KeyMatcher::Dynamic(type_name)
                if scripted_macro_type(snapshot, type_name) =>
            {
                type_name
            }
            _ => return None,
        };
        macro_definition_summary(snapshot, type_name, &property.key)
    });
    let Some(summary) = summary else {
        return;
    };

    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    for argument in &property.block {
        let count = counts.entry(argument.key.to_ascii_lowercase()).or_default();
        *count = count.saturating_add(1);
        if *count > 1 {
            diagnostics.push(Diagnostic {
                code: DiagnosticCode::Cardinality,
                severity: 2,
                range: argument.key_range,
                message: format!(
                    "macro parameter `{}` is provided more than once",
                    argument.key
                ),
            });
        }
    }
    let missing = summary
        .parameters
        .iter()
        .filter(|parameter| {
            parameter.required
                && !counts
                    .keys()
                    .any(|name| name.eq_ignore_ascii_case(&parameter.name))
        })
        .map(|parameter| format!("`{}`", parameter.name))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        diagnostics.push(Diagnostic {
            code: DiagnosticCode::Cardinality,
            severity: DiagnosticCode::Cardinality.severity(),
            range: property.key_range,
            message: format!(
                "macro `{}` is missing required parameter(s): {}",
                summary.name,
                missing.join(", ")
            ),
        });
    }
}
pub(crate) fn syntax_diagnostics(input: &ParsedInput) -> Vec<Diagnostic> {
    match &input.parsed {
        ParsedContent::Text(parsed) => parsed.errors().iter().map(diagnostic_from_syntax).collect(),
    }
}

pub(crate) fn diagnostic_from_syntax(error: &SyntaxError) -> Diagnostic {
    Diagnostic {
        code: DiagnosticCode::Syntax,
        severity: DiagnosticCode::Syntax.severity(),
        range: error.range,
        message: error.message.clone(),
    }
}
