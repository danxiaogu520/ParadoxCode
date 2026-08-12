use crate::macro_expansion::{ExpansionEnterFailure, ExpansionFailure, MacroExpansionSession};
use crate::quoted_script::{QuotedScriptParse, QuotedScriptSession};
use crate::resolution::*;
use crate::semantic::*;
use crate::support::*;
use crate::types::*;
use pdx_engine::hir::{HirFile, HirParameterReferenceKind, Scope};
use pdx_engine::{AnalysisSnapshot, DocumentId, DocumentSource, SourceFileId};
use pdx_parser::{FileFormat, SyntaxError};
use pdx_rules::RuleShape;
use pdx_text::TextRange;

const MAX_EXPANDED_DIAGNOSTICS_PER_INVOCATION: usize = 32;

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

/// Returns diagnostics for one indexed disk file while cooperatively observing cancellation.
pub fn source_file_diagnostics_with_cancellation(
    snapshot: &AnalysisSnapshot,
    file: SourceFileId,
    cancellation: &CancellationToken,
) -> Result<Vec<Diagnostic>, Cancelled> {
    cancellation.checkpoint()?;
    let Some(input) = input_for_source_file(snapshot, file) else {
        return Ok(Vec::new());
    };
    analyze_input_with_cancellation(snapshot, &input, cancellation)
        .map(|analysis| analysis.diagnostics)
}

/// Returns diagnostics for caller-supplied text classified by its logical path.
///
/// This query does not mutate the workspace or create an overlay. It is intended for bounded
/// batch tooling whose backing files already participate in the immutable snapshot index.
pub fn text_diagnostics_with_cancellation(
    snapshot: &AnalysisSnapshot,
    path: &pdx_text::LogicalPath,
    text: &str,
    cancellation: &CancellationToken,
) -> Result<Vec<Diagnostic>, Cancelled> {
    cancellation.checkpoint()?;
    let Some(input) = input_for_text(snapshot, path, text) else {
        return Ok(Vec::new());
    };
    analyze_input_with_cancellation(snapshot, &input, cancellation)
        .map(|analysis| analysis.diagnostics)
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
            && !value.contains('$')
            && !scope_member(
                snapshot,
                None,
                value,
                &ScopeContext::new(snapshot.game_profile_handle()),
            )
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
        if reference.name.contains('$') {
            continue;
        }
        match resolution.resolve(&reference.kind, &reference.name) {
            // `on_trigger` and `_trigger`-suffixed carriers may name fixed builtin
            // triggers, not only workspace scripted triggers; likewise for effects.
            Resolution::Missing
                if (reference.kind.eq_ignore_ascii_case("scripted_trigger")
                    && builtin_rule_has_key(snapshot, "trigger", &reference.name))
                    || (reference.kind.eq_ignore_ascii_case("scripted_effect")
                        && builtin_rule_has_key(snapshot, "effect", &reference.name)) => {}
            // Scalar arguments inside a scripted macro invocation are untyped
            // parameter values, not localisation key references.
            Resolution::Missing
                if reference.kind.eq_ignore_ascii_case("localisation")
                    && localisation_reference_is_macro_argument(
                        snapshot,
                        input,
                        reference.range,
                    ) => {}
            Resolution::Missing => diagnostics.push(Diagnostic {
                code: DiagnosticCode::UnknownSymbol,
                // The game renders a missing localisation key as its raw spelling, so a
                // missing key is a data-quality hint rather than a script error.
                severity: if reference.kind.eq_ignore_ascii_case("localisation") {
                    2
                } else {
                    DiagnosticCode::UnknownSymbol.severity()
                },
                range: reference.range,
                message: format!("unknown {} symbol `{}`", reference.kind, reference.name),
            }),
            // Localisation is merged across languages and may be repeated by replace files.
            // Existence is enough for diagnostics; navigation retains the candidate set.
            // The game resolves same-name definitions deterministically by source priority,
            // so ambiguity is never a runtime error and is intentionally not diagnosed.
            Resolution::Ambiguous => {}
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

/// Returns whether a fixed first-party rule in `context` accepts `key` exactly.
fn builtin_rule_has_key(snapshot: &AnalysisSnapshot, context: &str, key: &str) -> bool {
    snapshot
        .rules()
        .semantic_rules_for_context(context)
        .any(|rule| {
            matches!(
                &rule.key,
                pdx_rules::KeyMatcher::Exact(expected)
                    if expected.eq_ignore_ascii_case(key)
            )
        })
}

/// Returns whether a localisation-kind reference sits inside a scripted macro
/// invocation, where scalar arguments are untyped parameter values.
fn localisation_reference_is_macro_argument(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    range: TextRange,
) -> bool {
    let Some(hir) = input.hir.as_deref() else {
        return false;
    };
    let Some(property) = hir.properties().iter().rfind(|property| {
        property.range.start() <= range.start() && property.range.end() >= range.end()
    }) else {
        return false;
    };
    // A definition body (path length 2) keeps localisation validation; only
    // nested invocations carry opaque macro arguments.
    if property.path.len() < 3 {
        return false;
    }
    let Some(parent_key) = property.path.iter().rev().nth(1) else {
        return false;
    };
    workspace_member(snapshot, "scripted_effect", parent_key)
        || workspace_member(snapshot, "scripted_trigger", parent_key)
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
    let root_bare_values = script_bare_values(input, parsed.root());
    cancellation.checkpoint()?;
    let mut diagnostics = Vec::new();
    let mut expansion = MacroExpansionSession::default();
    let mut quoted_scripts = QuotedScriptSession::new(cancellation);
    if let Some(context) = semantic_file_root_context(snapshot, input.path.as_ref()) {
        let (root_key, key_range) = roots.first().map_or_else(
            || ("", parsed.root().range()),
            |property| (property.key.as_str(), property.key_range),
        );
        let scope = semantic_initial_scope(snapshot, input, &context, root_key, key_range);
        validate_semantic_container(
            snapshot,
            &context,
            &[],
            &roots,
            &root_bare_values,
            &scope,
            input.hir.as_deref(),
            &mut diagnostics,
            cancellation,
            true,
            parsed.root().range(),
            &mut expansion,
            &mut quoted_scripts,
            0,
        )?;
        return Ok(diagnostics);
    }
    for property in roots {
        cancellation.checkpoint()?;
        let Some(context) = semantic_root_context(snapshot, &property.key, input.path.as_ref())
        else {
            continue;
        };
        // A context guessed from the directory layout alone is less trustworthy than a
        // key/filter-declared match, so its diagnostics are downgraded one step.
        let fallback_context =
            semantic_root_context_is_fallback(snapshot, &property.key, input.path.as_ref());
        let scope =
            semantic_initial_scope(snapshot, input, &context, &property.key, property.key_range);
        let mut container_diagnostics = Vec::new();
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
                let descriptor = &snapshot.rules().model().semantic.type_descriptors[type_name];
                if descriptor
                    .type_key_filter
                    .as_ref()
                    .is_some_and(|(values, negate)| {
                        values
                            .iter()
                            .any(|value| value.eq_ignore_ascii_case(&child.key))
                            == *negate
                    })
                {
                    continue;
                }
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
                    &mut container_diagnostics,
                    cancellation,
                    child.block_range.is_some(),
                    child.block_range.unwrap_or(child.key_range),
                    &mut expansion,
                    &mut quoted_scripts,
                    0,
                )?;
            }
        } else {
            validate_semantic_container(
                snapshot,
                &context,
                &[],
                &property.block,
                &property.bare_values,
                &scope,
                input.hir.as_deref(),
                &mut container_diagnostics,
                cancellation,
                property.block_range.is_some(),
                property.block_range.unwrap_or(property.key_range),
                &mut expansion,
                &mut quoted_scripts,
                0,
            )?;
        }
        if fallback_context {
            // Only key/scope classification depends on the guessed context; value and
            // cardinality diagnostics fire from matched rules and stay trustworthy.
            for diagnostic in &mut container_diagnostics {
                if matches!(
                    diagnostic.code,
                    DiagnosticCode::UnknownKey | DiagnosticCode::WrongScope
                ) {
                    diagnostic.severity = diagnostic.severity.saturating_add(1);
                }
            }
        }
        diagnostics.extend(container_diagnostics);
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
    container_range: TextRange,
    expansion: &mut MacroExpansionSession,
    quoted_scripts: &mut QuotedScriptSession<'_>,
    quoted_script_depth: usize,
) -> Result<(), Cancelled> {
    cancellation.checkpoint()?;
    let rules = semantic_rules_for_container(snapshot, context, parent_path, scope);
    if rules.is_empty() {
        return Ok(());
    }
    let mut exact_rules =
        std::collections::BTreeMap::<String, Vec<&pdx_rules::SemanticRule>>::new();
    let mut non_exact_rules = Vec::new();
    for rule in &rules {
        if let pdx_rules::KeyMatcher::Exact(key) = &rule.key {
            exact_rules
                .entry(key.to_ascii_lowercase())
                .or_default()
                .push(*rule);
        } else {
            non_exact_rules.push(*rule);
        }
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
        let count = counts.entry(key.clone()).or_default();
        *count = count.saturating_add(1);
        let profile = snapshot.game_profile();
        let trigger_like = context.eq_ignore_ascii_case("trigger")
            || profile.semantic_context_inherits(context, "trigger");
        let effect_like = context.eq_ignore_ascii_case("effect")
            || profile.semantic_context_inherits(context, "effect");
        let transparent_wrapper = (trigger_like
            && profile.is_transparent_scope_wrapper(&property.key))
            || ((trigger_like || effect_like)
                && profile.is_dynamic_scope_expression(&property.key));
        let matching = exact_rules
            .get(&key)
            .into_iter()
            .flatten()
            .copied()
            .chain(non_exact_rules.iter().copied())
            .filter(|rule| {
                !matches!(rule.shape, RuleShape::LeafValue)
                    && semantic_rule_key_matches(snapshot, rule, parent_path, &property.key)
            })
            .collect::<Vec<_>>();
        let parameterized_key = hir.is_some_and(|hir| {
            owner_local_parameter_in_range(
                hir,
                property.key_range,
                HirParameterReferenceKind::KeySubstitution,
            )
        });
        if matching.is_empty() && (transparent_wrapper || parameterized_key) {
            // EU4 logical wrappers retain their parent context. Owner-local parameter keys are
            // likewise deferred until a call site supplies the concrete key spelling.
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
            let parameterized_scalar = property.scalar.as_ref().is_some_and(|(_, range)| {
                hir.is_some_and(|hir| {
                    owner_local_parameter_in_range(
                        hir,
                        *range,
                        HirParameterReferenceKind::Substitution,
                    )
                })
            });
            let valid = applicable.iter().any(|rule| {
                semantic_property_matches(snapshot, rule, property, scope)
                    || (parameterized_scalar && semantic_property_structure_matches(rule, property))
            });
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
            let parameterized_invocation = hir.is_some_and(|hir| {
                owner_local_parameter_in_range(
                    hir,
                    property.range,
                    HirParameterReferenceKind::Substitution,
                ) || owner_local_parameter_in_range(
                    hir,
                    property.range,
                    HirParameterReferenceKind::KeySubstitution,
                ) || (property_contains_parameter_token(property)
                    && hir.parameter_definitions().iter().any(|definition| {
                        property.range.start() >= definition.owner_range.start()
                            && property.range.end() <= definition.owner_range.end()
                    }))
            });
            let arguments_allow_expansion = parameterized_invocation
                || validate_scripted_macro_arguments(snapshot, applicable, property, diagnostics);
            if valid
                && !scoped_matching.is_empty()
                && arguments_allow_expansion
                && !parameterized_invocation
            {
                validate_scripted_macro_expansion(
                    snapshot,
                    applicable,
                    property,
                    scope,
                    diagnostics,
                    cancellation,
                    expansion,
                    quoted_scripts,
                    quoted_script_depth,
                )?;
            }
            let unlimited_occurrences = applicable.iter().any(|rule| {
                !semantic_rule_is_alias_definition(rule)
                    && semantic_rule_is_selected(rule, selected_alternative.as_deref())
                    && rule.max_occurs.is_none()
            });
            let max_occurs = if unlimited_occurrences {
                None
            } else {
                applicable
                    .iter()
                    .filter(|rule| {
                        !semantic_rule_is_alias_definition(rule)
                            && semantic_rule_is_selected(rule, selected_alternative.as_deref())
                    })
                    .filter_map(|rule| rule.max_occurs)
                    .max()
            };
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
        } else if transparent_wrapper
            && snapshot
                .game_profile()
                .is_dynamic_scope_expression(&property.key)
        {
            let mut next_scope = scope.clone();
            next_scope.previous.insert(0, next_scope.current.clone());
            next_scope.current = "any".to_owned();
            Some((context.to_owned(), parent_path.to_vec(), next_scope))
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
                    property.block_range.unwrap_or(property.key_range),
                    expansion,
                    quoted_scripts,
                    quoted_script_depth,
                )?;
            }
            continue;
        };
        let quoted_transition = semantic_selected_transition(
            snapshot,
            &matching,
            selected_alternative.as_deref(),
            context,
            parent_path,
            property,
            scope,
            transparent_wrapper,
        )
        .filter(|rule| matches!(rule.shape, RuleShape::QuotedScript));
        if quoted_transition.is_some() {
            validate_quoted_script(
                snapshot,
                &next_context,
                &child_path,
                &next_scope,
                property,
                diagnostics,
                cancellation,
                expansion,
                quoted_scripts,
                quoted_script_depth,
            )?;
            continue;
        }
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
                    property.block_range.unwrap_or(property.key_range),
                    expansion,
                    quoted_scripts,
                    quoted_script_depth,
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
                    property.block_range.unwrap_or(property.key_range),
                    expansion,
                    quoted_scripts,
                    quoted_script_depth,
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
            property.block_range.unwrap_or(property.key_range),
            expansion,
            quoted_scripts,
            quoted_script_depth,
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
        let parameterized_value = hir.is_some_and(|hir| {
            owner_local_parameter_in_range(
                hir,
                *value_range,
                HirParameterReferenceKind::Substitution,
            )
        });
        if matching.is_empty() && !parameterized_value {
            diagnostics.push(Diagnostic {
                code: DiagnosticCode::InvalidValue,
                severity: semantic_rule_severity(
                    rules.iter().copied(),
                    DiagnosticCode::InvalidValue,
                ),
                range: *value_range,
                message: format!(
                    "bare value `{value}` does not match the semantic rule value clause"
                ),
            });
        }
    }
    let empty_range = properties.first().map_or_else(
        || {
            bare_values
                .first()
                .map_or(container_range, |(_, range)| *range)
        },
        |property| property.key_range,
    );
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
            if min_occurs == 0 {
                continue;
            }
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

fn property_contains_parameter_token(property: &ScriptProperty) -> bool {
    property.key.contains('$')
        || property
            .scalar
            .as_ref()
            .is_some_and(|(value, _)| value.contains('$'))
        || property
            .bare_values
            .iter()
            .any(|(value, _)| value.contains('$'))
        || property.block.iter().any(property_contains_parameter_token)
}

#[allow(clippy::too_many_arguments)]
fn validate_quoted_script(
    snapshot: &AnalysisSnapshot,
    context: &str,
    parent_path: &[String],
    scope: &ScopeContext,
    property: &ScriptProperty,
    diagnostics: &mut Vec<Diagnostic>,
    cancellation: &CancellationToken,
    expansion: &mut MacroExpansionSession,
    quoted_scripts: &mut QuotedScriptSession<'_>,
    depth: usize,
) -> Result<(), Cancelled> {
    cancellation.checkpoint()?;
    let range = property
        .scalar
        .as_ref()
        .map_or(property.key_range, |(_, range)| *range);
    let Some(origin) = property.quoted_source.as_ref() else {
        return Ok(());
    };
    let script = match quoted_scripts.parse(origin.source(), depth)? {
        QuotedScriptParse::Parsed(script) => script,
        QuotedScriptParse::Opaque => {
            diagnostics.push(Diagnostic {
                code: DiagnosticCode::InvalidValue,
                severity: DiagnosticCode::InvalidValue.severity(),
                range,
                message: "quoted Script payload could not be decoded".to_owned(),
            });
            return Ok(());
        }
        QuotedScriptParse::Limited(limit) => {
            diagnostics.push(Diagnostic {
                code: DiagnosticCode::InvalidValue,
                severity: DiagnosticCode::InvalidValue.severity(),
                range,
                message: limit.message().to_owned(),
            });
            return Ok(());
        }
    };
    for error in script.parsed().errors() {
        cancellation.checkpoint()?;
        let Some(range) = origin.map_decoded_range(&script, error.range) else {
            continue;
        };
        diagnostics.push(Diagnostic {
            code: DiagnosticCode::Syntax,
            severity: DiagnosticCode::Syntax.severity(),
            range,
            message: error.message.clone(),
        });
    }
    let (properties, bare_values) = quoted_script_container(&script, origin);
    validate_semantic_container(
        snapshot,
        context,
        parent_path,
        &properties,
        &bare_values,
        scope,
        None,
        diagnostics,
        cancellation,
        true,
        range,
        expansion,
        quoted_scripts,
        depth.saturating_add(1),
    )
}

fn owner_local_parameter_in_range(
    hir: &HirFile,
    range: TextRange,
    expected_kind: HirParameterReferenceKind,
) -> bool {
    hir.parameter_references().iter().any(|reference| {
        reference.kind == expected_kind
            && reference.range.start() >= range.start()
            && reference.range.end() <= range.end()
            && hir
                .parameter_definitions_for_owner(reference.owner_range)
                .any(|definition| definition.name.eq_ignore_ascii_case(&reference.name))
    })
}

#[allow(clippy::too_many_arguments)]
fn validate_scripted_macro_expansion(
    snapshot: &AnalysisSnapshot,
    rules: &[&pdx_rules::SemanticRule],
    property: &ScriptProperty,
    scope: &ScopeContext,
    diagnostics: &mut Vec<Diagnostic>,
    cancellation: &CancellationToken,
    expansion: &mut MacroExpansionSession,
    quoted_scripts: &mut QuotedScriptSession<'_>,
    quoted_script_depth: usize,
) -> Result<(), Cancelled> {
    let Some(type_name) = rules.iter().find_map(|rule| match &rule.key {
        pdx_rules::KeyMatcher::Type(type_name) | pdx_rules::KeyMatcher::Dynamic(type_name)
            if scripted_macro_type(snapshot, type_name) =>
        {
            Some(type_name.as_str())
        }
        _ => None,
    }) else {
        return Ok(());
    };
    let Some(resolved) = resolve_macro_definition(snapshot, type_name, &property.key) else {
        return Ok(());
    };
    let Some(template) = resolved.summary.template.as_ref() else {
        return Ok(());
    };
    match expansion.enter(&resolved) {
        Ok(()) => {}
        Err(ExpansionEnterFailure::Cycle(chain)) => {
            diagnostics.push(Diagnostic {
                code: DiagnosticCode::MacroExpansionCycle,
                severity: DiagnosticCode::MacroExpansionCycle.severity(),
                range: property.key_range,
                message: format!("scripted macro expansion cycle: {}", chain.join(" -> ")),
            });
            return Ok(());
        }
        Err(ExpansionEnterFailure::Limit(limit)) => {
            if expansion.should_report_limit() {
                diagnostics.push(macro_expansion_limit(property.key_range, limit));
            }
            return Ok(());
        }
    }
    let expanded = match expansion.expand(
        template,
        property,
        cancellation,
        quoted_scripts,
        quoted_script_depth,
    ) {
        Ok(Ok(expanded)) => expanded,
        Ok(Err(ExpansionFailure::MissingParameter(name))) => {
            diagnostics.push(Diagnostic {
                code: DiagnosticCode::Cardinality,
                severity: DiagnosticCode::Cardinality.severity(),
                range: property.key_range,
                message: format!(
                    "macro `{}` expansion requires parameter `{name}` in the active branch",
                    resolved.summary.name
                ),
            });
            expansion.leave();
            return Ok(());
        }
        Ok(Err(ExpansionFailure::InvalidArgument { name, range })) => {
            diagnostics.push(Diagnostic {
                code: DiagnosticCode::InvalidValue,
                severity: DiagnosticCode::InvalidValue.severity(),
                range,
                message: format!("macro parameter `{name}` must be a scalar token for expansion"),
            });
            expansion.leave();
            return Ok(());
        }
        Ok(Err(ExpansionFailure::Limit(limit))) => {
            if expansion.should_report_limit() {
                diagnostics.push(macro_expansion_limit(property.key_range, limit));
            }
            expansion.leave();
            return Ok(());
        }
        Err(cancelled) => {
            expansion.leave();
            return Err(cancelled);
        }
    };
    let first_expanded_diagnostic = diagnostics.len();
    let validation = validate_semantic_container(
        snapshot,
        &resolved.body_context,
        &[],
        &expanded.properties,
        &expanded.bare_values,
        scope,
        None,
        diagnostics,
        cancellation,
        true,
        property.key_range,
        expansion,
        quoted_scripts,
        0,
    );
    expansion.leave();
    validation?;
    let expanded_diagnostic_count = diagnostics.len().saturating_sub(first_expanded_diagnostic);
    if expanded_diagnostic_count > MAX_EXPANDED_DIAGNOSTICS_PER_INVOCATION {
        let omitted = expanded_diagnostic_count - MAX_EXPANDED_DIAGNOSTICS_PER_INVOCATION;
        diagnostics.truncate(first_expanded_diagnostic + MAX_EXPANDED_DIAGNOSTICS_PER_INVOCATION);
        diagnostics.push(Diagnostic {
            code: DiagnosticCode::MacroExpansionLimit,
            severity: DiagnosticCode::MacroExpansionLimit.severity(),
            range: property.key_range,
            message: format!("scripted macro expansion omitted {omitted} additional diagnostic(s)"),
        });
    }
    for diagnostic in &mut diagnostics[first_expanded_diagnostic..] {
        diagnostic.message = format!(
            "in expansion of `{}`: {}",
            resolved.summary.name, diagnostic.message
        );
    }
    Ok(())
}

fn macro_expansion_limit(range: TextRange, limit: &'static str) -> Diagnostic {
    Diagnostic {
        code: DiagnosticCode::MacroExpansionLimit,
        severity: DiagnosticCode::MacroExpansionLimit.severity(),
        range,
        message: format!("scripted macro expansion exceeded the {limit} limit"),
    }
}

fn validate_scripted_macro_arguments(
    snapshot: &AnalysisSnapshot,
    rules: &[&pdx_rules::SemanticRule],
    property: &ScriptProperty,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    if property.block_range.is_none() {
        return true;
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
        return true;
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
        return false;
    }
    true
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
