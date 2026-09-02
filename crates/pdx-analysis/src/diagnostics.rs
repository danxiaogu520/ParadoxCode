use crate::localisation::localisation_command_diagnostics;
use crate::macro_expansion::{ExpansionEnterFailure, ExpansionFailure, MacroExpansionSession};
use crate::quoted_script::{QuotedScriptParse, QuotedScriptSession};
use crate::resolution::*;
use crate::semantic::*;
use crate::suggest::best_suggestion;
use crate::support::*;
use crate::types::*;
use pdx_engine::hir::{
    HirFile, HirParameterReferenceKind, MacroTemplate, MacroTemplateFragment, MacroTemplateItem,
    MacroTemplateProperty, MacroTemplateToken, MacroTemplateValue, Scope,
};
use pdx_engine::{
    AnalysisSnapshot, DocumentId, DocumentSource, MacroDefinitionSummary, SourceFileId,
};
use pdx_parser::{FileFormat, SyntaxError};
use pdx_rules::RuleShape;
use pdx_text::TextRange;

const MAX_EXPANDED_DIAGNOSTICS_PER_INVOCATION: usize = 32;

/// Single finalization point for diagnostics emitted by syntax, semantic, and reference passes.
///
/// Keeping deduplication here prevents each producer from inventing slightly different identity
/// rules. Until every producer exposes a structured subject (the exact symbol or key), the
/// message remains part of identity so two independent symbol failures sharing a range survive.
#[derive(Default)]
struct DiagnosticCollector {
    values: Vec<Diagnostic>,
}

impl DiagnosticCollector {
    fn new(values: Vec<Diagnostic>) -> Self {
        Self { values }
    }

    fn push(&mut self, diagnostic: Diagnostic) {
        self.values.push(diagnostic);
    }

    fn finish(mut self) -> Vec<Diagnostic> {
        self.values.sort_by_key(|diagnostic| {
            (
                diagnostic.range.start(),
                diagnostic.range.end(),
                diagnostic.code,
                diagnostic.severity,
            )
        });
        self.values.dedup_by(|left, right| {
            if left.code == right.code && left.range == right.range && left.message == right.message
            {
                for fix in right.fixes.drain(..) {
                    if !left.fixes.contains(&fix) {
                        left.fixes.push(fix);
                    }
                }
                true
            } else {
                false
            }
        });
        self.values
    }
}

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
    let mut diagnostics = DiagnosticCollector::new(syntax_diagnostics(input));
    diagnostics
        .values
        .extend(semantic_rule_diagnostics(snapshot, input, cancellation)?);
    diagnostics.values.extend(localisation_command_diagnostics(
        snapshot,
        input,
        cancellation,
    )?);
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
            && !diagnostics.values.iter().any(|diagnostic| {
                diagnostic.code == DiagnosticCode::UnknownScope && diagnostic.range == *range
            })
        {
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::UnknownScope,
                DiagnosticCode::UnknownScope.severity(),
                *range,
                format!("unknown scope `{value}`"),
            ));
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
            Resolution::Missing => diagnostics.push(Diagnostic::new(
                DiagnosticCode::UnknownSymbol,
                // The game renders a missing localisation key as its raw spelling, so a
                // missing key is a data-quality hint rather than a script error.
                if reference.kind.eq_ignore_ascii_case("localisation") {
                    Severity::Warning
                } else {
                    DiagnosticCode::UnknownSymbol.severity()
                },
                reference.range,
                format!("unknown {} symbol `{}`", reference.kind, reference.name),
            )),
            // Localisation is merged across languages and may be repeated by replace files.
            // Existence is enough for diagnostics; navigation retains the candidate set.
            // The game resolves same-name definitions deterministically by source priority,
            // so ambiguity is never a runtime error and is intentionally not diagnosed.
            Resolution::Ambiguous => {}
            Resolution::Unique(_) => {}
        }
    }
    let diagnostics = diagnostics.finish();
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
        let root_refs: Vec<&ScriptProperty> = roots.iter().collect();
        let root_value_refs: Vec<&(std::sync::Arc<str>, TextRange)> =
            root_bare_values.iter().collect();
        let (root_key, key_range) = roots.first().map_or_else(
            || ("", parsed.root().range()),
            |property| (property.key.as_ref(), property.key_range),
        );
        let scope = semantic_initial_scope(snapshot, input, &context, root_key, key_range);
        validate_semantic_container(SemanticValidationInput {
            snapshot,
            context: &context,
            parent_path: &[],
            properties: &root_refs,
            bare_values: &root_value_refs,
            scope: &scope,
            hir: input.hir.as_deref(),
            diagnostics: &mut diagnostics,
            cancellation,
            block_container: true,
            container_range: parsed.root().range(),
            expansion: &mut expansion,
            quoted_scripts: &mut quoted_scripts,
            quoted_script_depth: 0,
        })?;
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
                let child_properties: Vec<&ScriptProperty> = child.block.iter().collect();
                let child_values: Vec<&(std::sync::Arc<str>, TextRange)> =
                    child.bare_values.iter().collect();
                validate_semantic_container(SemanticValidationInput {
                    snapshot,
                    context: &context,
                    parent_path: &[],
                    properties: &child_properties,
                    bare_values: &child_values,
                    scope: &child_scope,
                    hir: input.hir.as_deref(),
                    diagnostics: &mut container_diagnostics,
                    cancellation,
                    block_container: child.block_range.is_some(),
                    container_range: child.block_range.unwrap_or(child.key_range),
                    expansion: &mut expansion,
                    quoted_scripts: &mut quoted_scripts,
                    quoted_script_depth: 0,
                })?;
            }
        } else {
            let root_properties: Vec<&ScriptProperty> = property.block.iter().collect();
            let root_values: Vec<&(std::sync::Arc<str>, TextRange)> =
                property.bare_values.iter().collect();
            validate_semantic_container(SemanticValidationInput {
                snapshot,
                context: &context,
                parent_path: &[],
                properties: &root_properties,
                bare_values: &root_values,
                scope: &scope,
                hir: input.hir.as_deref(),
                diagnostics: &mut container_diagnostics,
                cancellation,
                block_container: property.block_range.is_some(),
                container_range: property.block_range.unwrap_or(property.key_range),
                expansion: &mut expansion,
                quoted_scripts: &mut quoted_scripts,
                quoted_script_depth: 0,
            })?;
        }
        if fallback_context {
            // A path-only context is still enough to establish that an authored key is not
            // accepted by the selected rule set: the game silently ignores such a key. Keep
            // unknown keys as errors; only scope availability remains uncertain until a more
            // specific root/context match is available.
            for diagnostic in &mut container_diagnostics {
                if diagnostic.code == DiagnosticCode::RuleWrongScope {
                    diagnostic.severity = diagnostic.severity.saturating_add(1);
                    diagnostic.certainty = DiagnosticCertainty::Inferred;
                }
            }
        }
        diagnostics.extend(container_diagnostics);
    }
    Ok(diagnostics)
}

fn semantic_diagnostic(
    code: DiagnosticCode,
    severity: Severity,
    range: TextRange,
    message: String,
    rule: &pdx_rules::SemanticRule,
) -> Diagnostic {
    Diagnostic::new(code, severity, range, message).with_provenance(DiagnosticProvenance {
        rule_id: Some(rule.id.clone()),
        context: Some(rule.context.clone()),
        source_file: Some(rule.source_file.clone()),
        source_line: Some(rule.line),
    })
}

struct SemanticValidationInput<'data, 'hir, 'session, 'cancel> {
    snapshot: &'data AnalysisSnapshot,
    context: &'data str,
    parent_path: &'data [std::sync::Arc<str>],
    properties: &'data [&'data ScriptProperty],
    bare_values: &'data [&'data (std::sync::Arc<str>, TextRange)],
    scope: &'data ScopeContext,
    hir: Option<&'hir HirFile>,
    diagnostics: &'data mut Vec<Diagnostic>,
    cancellation: &'data CancellationToken,
    block_container: bool,
    container_range: TextRange,
    expansion: &'data mut MacroExpansionSession,
    quoted_scripts: &'session mut QuotedScriptSession<'cancel>,
    quoted_script_depth: usize,
}

fn validate_semantic_container(
    input: SemanticValidationInput<'_, '_, '_, '_>,
) -> Result<(), Cancelled> {
    let SemanticValidationInput {
        snapshot,
        context,
        parent_path,
        properties,
        bare_values,
        scope,
        hir,
        diagnostics,
        cancellation,
        block_container,
        container_range,
        expansion,
        quoted_scripts,
        quoted_script_depth,
    } = input;
    cancellation.checkpoint()?;
    let rules = semantic_rules_for_container(snapshot, context, parent_path, scope);
    if rules.is_empty() {
        return Ok(());
    }
    let selected_alternative = semantic_selected_alternative(
        snapshot,
        &rules,
        context,
        parent_path,
        properties,
        bare_values,
        scope,
    );
    let profile = snapshot.game_profile();
    let trigger_like = context.eq_ignore_ascii_case("trigger")
        || profile.semantic_context_inherits(context, "trigger");
    let effect_like = context.eq_ignore_ascii_case("effect")
        || profile.semantic_context_inherits(context, "effect");
    // Case-folded occurrence counting with a linear scan: containers repeat a handful of
    // distinct keys, and the scan avoids lowercasing and cloning a String per property.
    let mut counts: Vec<(&str, u32)> = Vec::new();
    for property in properties {
        cancellation.checkpoint()?;
        let fact_scope = hir
            .and_then(|hir| hir.scope_fact(property.key_range, context))
            .map(|fact| scope_context_from_hir(snapshot.game_profile_handle(), &fact.state));
        let scope = fact_scope.as_ref().unwrap_or(scope);
        let count = match counts
            .iter_mut()
            .find(|(seen, _)| seen.eq_ignore_ascii_case(&property.key))
        {
            Some(entry) => {
                entry.1 = entry.1.saturating_add(1);
                entry.1
            }
            None => {
                counts.push((property.key.as_ref(), 1));
                1
            }
        };
        let transparent_wrapper = (trigger_like
            && profile.is_transparent_scope_wrapper(&property.key))
            || ((trigger_like || effect_like)
                && profile.is_dynamic_scope_expression(&property.key));
        let matching =
            semantic_rules_for_container_key(snapshot, context, parent_path, &property.key)
                .into_iter()
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
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::UnknownKey,
                DiagnosticCode::UnknownKey.severity(),
                property.key_range,
                format!(
                    "unexpected key `{}` in rule context `{context}`",
                    property.key
                ),
            ));
        } else {
            let scoped_matching = matching
                .iter()
                .filter(|rule| semantic_scope_allows(rule, scope))
                .copied()
                .collect::<Vec<_>>();
            if scoped_matching.is_empty() {
                diagnostics.push(semantic_diagnostic(
                    DiagnosticCode::RuleWrongScope,
                    semantic_rule_severity(
                        matching.iter().copied(),
                        DiagnosticCode::RuleWrongScope,
                    ),
                    property.key_range,
                    format!(
                        "`{}` is not available in game scope `{}` ({})",
                        property.key,
                        scope.current,
                        semantic_rule_provenance(matching[0])
                    ),
                    matching[0],
                ));
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
                let scope_value = property.scalar.as_ref().and_then(|(value, _)| {
                    applicable
                        .iter()
                        .map(|rule| semantic_scope_value_match(snapshot, rule, value, scope))
                        .find(|result| !matches!(result, ScopeValueMatch::NotScopeRule))
                });
                let diagnostic_code = match scope_value.as_ref() {
                    Some(ScopeValueMatch::Unknown)
                        if property.key.eq_ignore_ascii_case("scope") =>
                    {
                        DiagnosticCode::InvalidScopeCommand
                    }
                    Some(ScopeValueMatch::Unknown) => DiagnosticCode::InvalidTarget,
                    Some(ScopeValueMatch::Known {
                        compatible: false, ..
                    }) => DiagnosticCode::TargetWrongScope,
                    _ => DiagnosticCode::InvalidValue,
                };
                let range = property
                    .scalar
                    .as_ref()
                    .map_or(property.key_range, |(_, range)| *range);
                let message = match scope_value.as_ref() {
                    Some(ScopeValueMatch::Unknown)
                        if property.key.eq_ignore_ascii_case("scope") =>
                    {
                        format!(
                            "invalid scope command target `{}` ({})",
                            property.scalar.as_ref().map_or("", |(value, _)| value),
                            semantic_rule_provenance(applicable[0])
                        )
                    }
                    Some(ScopeValueMatch::Unknown) => format!(
                        "invalid target `{}`: scope expression is not recognised ({})",
                        property.scalar.as_ref().map_or("", |(value, _)| value),
                        semantic_rule_provenance(applicable[0])
                    ),
                    Some(ScopeValueMatch::Known {
                        actual, expected, ..
                    }) => format!(
                        "target `{}` resolves to scope `{actual}`, expected {} ({})",
                        property.scalar.as_ref().map_or("", |(value, _)| value),
                        expected.as_deref().unwrap_or("any scope"),
                        semantic_rule_provenance(applicable[0])
                    ),
                    _ => format!(
                        "value of `{}` does not match the semantic rule ({})",
                        property.key,
                        semantic_rule_provenance(applicable[0])
                    ),
                };
                let certainty = match diagnostic_code {
                    DiagnosticCode::UnknownScope
                    | DiagnosticCode::InvalidTarget
                    | DiagnosticCode::InvalidScopeCommand => DiagnosticCertainty::Certain,
                    DiagnosticCode::TargetWrongScope => DiagnosticCertainty::Contextual,
                    _ => DiagnosticCertainty::Certain,
                };
                let mut diagnostic = Diagnostic::new(
                    diagnostic_code,
                    semantic_rule_severity(applicable.iter().copied(), diagnostic_code),
                    range,
                    message,
                )
                .with_certainty(certainty);
                diagnostic.provenance = Some(DiagnosticProvenance {
                    rule_id: Some(applicable[0].id.clone()),
                    context: Some(applicable[0].context.clone()),
                    source_file: Some(applicable[0].source_file.clone()),
                    source_line: Some(applicable[0].line),
                });
                if diagnostic_code == DiagnosticCode::InvalidValue
                    && let Some((value, value_range)) = property.scalar.as_ref()
                    && let Some(candidate) = enum_value_suggestion(snapshot, applicable, value)
                {
                    let replacement = format!(
                        "\"{}\"",
                        candidate.replace('\\', "\\\\").replace('"', "\\\"")
                    );
                    diagnostic = diagnostic.with_fix(QuickFix::replace(
                        format!("Did you mean '{candidate}'?"),
                        *value_range,
                        replacement,
                    ));
                }
                diagnostics.push(diagnostic);
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
                    ValidationState {
                        snapshot,
                        diagnostics,
                        cancellation,
                        expansion,
                        quoted_scripts,
                    },
                    applicable,
                    property,
                    scope,
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
                && count > max_occurs
            {
                diagnostics.push(semantic_diagnostic(
                    DiagnosticCode::Cardinality,
                    Severity::Warning,
                    property.key_range,
                    format!(
                        "`{}` occurs {} times, but rule cardinality allows at most {} ({})",
                        property.key,
                        count,
                        max_occurs,
                        semantic_rule_provenance(applicable[0])
                    ),
                    applicable[0],
                ));
            }
        }
        let cached_child_fact = cached_scope_fact_for_property(CachedScopeFactInput {
            snapshot,
            hir,
            context,
            parent_path,
            property,
            matching: &matching,
            selected_alternative: selected_alternative.as_deref(),
            scope,
            transparent_wrapper,
        });
        // The structural-path fallback and the quoted-script probe below need the same
        // selected transition; compute it once and reuse the rule for both consumers.
        let mut transition_rule: Option<&pdx_rules::SemanticRule> = None;
        let mut transition_resolved = false;
        let destination = if let Some(fact) = cached_child_fact {
            Some((
                pdx_engine::intern_shard_string(&fact.context),
                fact.parent_path
                    .iter()
                    .map(|segment| pdx_engine::intern_shard_string(segment))
                    .collect::<Vec<_>>(),
                scope_context_from_hir(snapshot.game_profile_handle(), &fact.state),
            ))
        } else if transparent_wrapper
            && snapshot
                .game_profile()
                .is_dynamic_scope_expression(&property.key)
        {
            let mut next_scope = scope.clone();
            next_scope.previous.insert(0, next_scope.current.clone());
            next_scope.current = pdx_engine::intern_shard_string("any");
            Some((
                pdx_engine::intern_shard_string(context),
                parent_path.to_vec(),
                next_scope,
            ))
        } else {
            transition_resolved = true;
            let selected = semantic_selected_transition(SemanticTransitionInput {
                snapshot,
                matching: &matching,
                selected_alternative: selected_alternative.as_deref(),
                context,
                parent_path,
                property,
                scope,
                transparent_wrapper,
            });
            transition_rule = selected;
            selected.map(|rule| {
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
                let child_properties: Vec<&ScriptProperty> = property.block.iter().collect();
                let child_values: Vec<&(std::sync::Arc<str>, TextRange)> =
                    property.bare_values.iter().collect();
                validate_semantic_container(SemanticValidationInput {
                    snapshot,
                    context,
                    parent_path: &structural_path,
                    properties: &child_properties,
                    bare_values: &child_values,
                    scope,
                    hir,
                    diagnostics,
                    cancellation,
                    block_container: property.block_range.is_some(),
                    container_range: property.block_range.unwrap_or(property.key_range),
                    expansion,
                    quoted_scripts,
                    quoted_script_depth,
                })?;
            }
            continue;
        };
        let quoted_transition = if transition_resolved {
            transition_rule
        } else {
            semantic_selected_transition(SemanticTransitionInput {
                snapshot,
                matching: &matching,
                selected_alternative: selected_alternative.as_deref(),
                context,
                parent_path,
                property,
                scope,
                transparent_wrapper,
            })
        }
        .filter(|rule| matches!(rule.shape, RuleShape::QuotedScript));
        if quoted_transition.is_some() {
            validate_quoted_script(
                ValidationState {
                    snapshot,
                    diagnostics,
                    cancellation,
                    expansion,
                    quoted_scripts,
                },
                &next_context,
                &child_path,
                &next_scope,
                property,
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
                // Children are partitioned by reference: cloning whole property subtrees here
                // dominated allocation traffic for deeply nested files.
                let mut structural_properties = Vec::new();
                let mut transition_properties = Vec::new();
                for child in &property.block {
                    let structural = semantic_rules_for_container_key(
                        snapshot,
                        context,
                        &structural_path,
                        &child.key,
                    )
                    .iter()
                    .any(|rule| {
                        !matches!(rule.shape, RuleShape::LeafValue)
                            && semantic_rule_key_matches(
                                snapshot,
                                rule,
                                &structural_path,
                                &child.key,
                            )
                    });
                    if structural {
                        structural_properties.push(child);
                    } else {
                        transition_properties.push(child);
                    }
                }
                let mut structural_values = Vec::new();
                let mut transition_values = Vec::new();
                for value in &property.bare_values {
                    let structural = structural_rules.iter().any(|rule| {
                        matches!(rule.shape, RuleShape::LeafValue)
                            && semantic_leaf_value_matches(snapshot, rule, &value.0, &next_scope)
                    });
                    if structural {
                        structural_values.push(value);
                    } else {
                        transition_values.push(value);
                    }
                }
                validate_semantic_container(SemanticValidationInput {
                    snapshot,
                    context,
                    parent_path: &structural_path,
                    properties: &structural_properties,
                    bare_values: &structural_values,
                    scope: &next_scope,
                    hir,
                    diagnostics,
                    cancellation,
                    block_container: true,
                    container_range: property.block_range.unwrap_or(property.key_range),
                    expansion,
                    quoted_scripts,
                    quoted_script_depth,
                })?;
                validate_semantic_container(SemanticValidationInput {
                    snapshot,
                    context: &next_context,
                    parent_path: &child_path,
                    properties: &transition_properties,
                    bare_values: &transition_values,
                    scope: &next_scope,
                    hir,
                    diagnostics,
                    cancellation,
                    block_container: true,
                    container_range: property.block_range.unwrap_or(property.key_range),
                    expansion,
                    quoted_scripts,
                    quoted_script_depth,
                })?;
                continue;
            }
        }
        let child_properties: Vec<&ScriptProperty> = property.block.iter().collect();
        let child_values: Vec<&(std::sync::Arc<str>, TextRange)> =
            property.bare_values.iter().collect();
        validate_semantic_container(SemanticValidationInput {
            snapshot,
            context: &next_context,
            parent_path: &child_path,
            properties: &child_properties,
            bare_values: &child_values,
            scope: &next_scope,
            hir,
            diagnostics,
            cancellation,
            block_container: property.block_range.is_some(),
            container_range: property.block_range.unwrap_or(property.key_range),
            expansion,
            quoted_scripts,
            quoted_script_depth,
        })?;
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
            let code = semantic_bare_value_code(rules.iter().copied(), value);
            let severity = semantic_bare_value_severity(rules.iter().copied(), value);
            let message =
                format!("bare value `{value}` does not match the semantic rule value clause");
            let diagnostic = rules.first().map_or_else(
                || Diagnostic::new(code, severity, *value_range, message.clone()),
                |rule| semantic_diagnostic(code, severity, *value_range, message.clone(), rule),
            );
            diagnostics.push(diagnostic);
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
        for rule in rules.iter() {
            cancellation.checkpoint()?;
            // Cardinality gates run before the scope/selection checks for non-leaf rules so
            // the hundreds of unrelated rules in a container skip the scope filter entirely.
            let min_occurs = if matches!(rule.shape, RuleShape::LeafValue) {
                None
            } else {
                semantic_min_occurs(rule).filter(|min_occurs| *min_occurs > 0)
            };
            if min_occurs.is_none() && !matches!(rule.shape, RuleShape::LeafValue) {
                continue;
            }
            if !semantic_scope_allows(rule, scope) {
                continue;
            }
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
                    diagnostics.push(semantic_diagnostic(
                    DiagnosticCode::Cardinality,
                    semantic_min_cardinality_severity(rule),
                    empty_range,
                    format!(
                        "semantic rule value clause requires at least {min_occurs} value(s), but `{}` occurs {count} times ({})",
                        semantic_value_matcher_label(&rule.value),
                        semantic_rule_provenance(rule)
                    ),
                    rule,
                ));
                }
                if let Some(max_occurs) = rule.max_occurs
                    && count > max_occurs
                {
                    diagnostics.push(semantic_diagnostic(
                    DiagnosticCode::Cardinality,
                    Severity::Warning,
                    bare_values.first().map_or(empty_range, |(_, range)| *range),
                    format!(
                        "semantic rule value clause allows at most {max_occurs} value(s), but found {count} ({})",
                        semantic_rule_provenance(rule)
                    ),
                    rule,
                ));
                }
                continue;
            }
            let min_occurs = min_occurs.expect("non-leaf rules carry a positive cardinality gate");
            let count = properties
                .iter()
                .filter(|property| {
                    semantic_rule_key_matches(snapshot, rule, parent_path, &property.key)
                        && !matches!(rule.shape, RuleShape::LeafValue)
                })
                .count();
            let count = u32::try_from(count).unwrap_or(u32::MAX);
            if count < min_occurs {
                diagnostics.push(semantic_diagnostic(
                DiagnosticCode::Cardinality,
                semantic_min_cardinality_severity(rule),
                empty_range,
                format!(
                    "semantic rule requires at least {min_occurs} occurrence(s), but `{}` occurs {count} times ({})",
                    semantic_matcher_label(&rule.key),
                    semantic_rule_provenance(rule)
                ),
                rule,
            ));
            }
        }
    }
    Ok(())
}

/// Finds a unique close static/workspace enum member for an invalid scalar value.
///
/// Enum values are normally sourced from the first-party model.  Workspace members are included
/// as well because several EU4 enums intentionally alias dynamic definition kinds (for example
/// country tags); the same visibility rules used by completion and semantic matching apply.
fn enum_value_suggestion(
    snapshot: &AnalysisSnapshot,
    rules: &[&pdx_rules::SemanticRule],
    value: &str,
) -> Option<String> {
    let mut candidates = Vec::new();
    for rule in rules {
        let pdx_rules::ValueMatcher::Enum(enum_name) = &rule.value else {
            continue;
        };
        if let Some((_, values)) = snapshot
            .rules()
            .model()
            .semantic
            .enum_values
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(enum_name))
        {
            candidates.extend(values.iter().cloned());
        }
        candidates.extend(effective_workspace_member_names(snapshot, enum_name));
    }
    candidates.sort_by_key(|candidate| candidate.to_ascii_lowercase());
    candidates.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    best_suggestion(value, candidates.iter().map(String::as_str)).map(str::to_owned)
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

struct ValidationState<'data, 'session, 'cancel> {
    snapshot: &'data AnalysisSnapshot,
    diagnostics: &'data mut Vec<Diagnostic>,
    cancellation: &'data CancellationToken,
    expansion: &'data mut MacroExpansionSession,
    quoted_scripts: &'session mut QuotedScriptSession<'cancel>,
}

fn validate_quoted_script(
    state: ValidationState<'_, '_, '_>,
    context: &str,
    parent_path: &[std::sync::Arc<str>],
    scope: &ScopeContext,
    property: &ScriptProperty,
    depth: usize,
) -> Result<(), Cancelled> {
    let ValidationState {
        snapshot,
        diagnostics,
        cancellation,
        expansion,
        quoted_scripts,
    } = state;
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
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::InvalidValue,
                DiagnosticCode::InvalidValue.severity(),
                range,
                "quoted Script payload could not be decoded".to_owned(),
            ));
            return Ok(());
        }
        QuotedScriptParse::Limited(limit) => {
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::InvalidValue,
                DiagnosticCode::InvalidValue.severity(),
                range,
                limit.message().to_owned(),
            ));
            return Ok(());
        }
    };
    for error in script.parsed().errors() {
        cancellation.checkpoint()?;
        let Some(range) = origin.map_decoded_range(&script, error.range) else {
            continue;
        };
        diagnostics.push(Diagnostic::new(
            DiagnosticCode::Syntax,
            DiagnosticCode::Syntax.severity(),
            range,
            error.message.clone(),
        ));
    }
    let (expanded_properties, expanded_values) = quoted_script_container(&script, origin);
    let property_refs: Vec<&ScriptProperty> = expanded_properties.iter().collect();
    let value_refs: Vec<&(std::sync::Arc<str>, TextRange)> = expanded_values.iter().collect();
    validate_semantic_container(SemanticValidationInput {
        snapshot,
        context,
        parent_path,
        properties: &property_refs,
        bare_values: &value_refs,
        scope,
        hir: None,
        diagnostics,
        cancellation,
        block_container: true,
        container_range: range,
        expansion,
        quoted_scripts,
        quoted_script_depth: depth.saturating_add(1),
    })
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

fn validate_scripted_macro_expansion(
    state: ValidationState<'_, '_, '_>,
    rules: &[&pdx_rules::SemanticRule],
    property: &ScriptProperty,
    scope: &ScopeContext,
    quoted_script_depth: usize,
) -> Result<(), Cancelled> {
    let ValidationState {
        snapshot,
        diagnostics,
        cancellation,
        expansion,
        quoted_scripts,
    } = state;
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
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::MacroExpansionCycle,
                DiagnosticCode::MacroExpansionCycle.severity(),
                property.key_range,
                format!("scripted macro expansion cycle: {}", chain.join(" -> ")),
            ));
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
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::Cardinality,
                DiagnosticCode::Cardinality.severity(),
                property.key_range,
                format!(
                    "macro `{}` expansion requires parameter `{name}` in the active branch",
                    resolved.summary.name
                ),
            ));
            expansion.leave();
            return Ok(());
        }
        Ok(Err(ExpansionFailure::InvalidArgument { name, range })) => {
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::InvalidValue,
                DiagnosticCode::InvalidValue.severity(),
                range,
                format!("macro parameter `{name}` must be a scalar token for expansion"),
            ));
            expansion.leave();
            return Ok(());
        }
        Ok(Err(ExpansionFailure::OmitOptionalProperty)) => {
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
    let expanded_property_refs: Vec<&ScriptProperty> = expanded.properties.iter().collect();
    let expanded_value_refs: Vec<&(std::sync::Arc<str>, TextRange)> =
        expanded.bare_values.iter().collect();
    let validation = validate_semantic_container(SemanticValidationInput {
        snapshot,
        context: &resolved.body_context,
        parent_path: &[],
        properties: &expanded_property_refs,
        bare_values: &expanded_value_refs,
        scope,
        hir: None,
        diagnostics,
        cancellation,
        block_container: true,
        container_range: property.key_range,
        expansion,
        quoted_scripts,
        quoted_script_depth: 0,
    });
    expansion.leave();
    validation?;
    let expanded_diagnostic_count = diagnostics.len().saturating_sub(first_expanded_diagnostic);
    if expanded_diagnostic_count > MAX_EXPANDED_DIAGNOSTICS_PER_INVOCATION {
        let omitted = expanded_diagnostic_count - MAX_EXPANDED_DIAGNOSTICS_PER_INVOCATION;
        diagnostics.truncate(first_expanded_diagnostic + MAX_EXPANDED_DIAGNOSTICS_PER_INVOCATION);
        diagnostics.push(
            Diagnostic::new(
                DiagnosticCode::AnalysisIncomplete,
                DiagnosticCode::AnalysisIncomplete.severity(),
                property.key_range,
                format!("scripted macro expansion omitted {omitted} additional diagnostic(s)"),
            )
            .with_certainty(DiagnosticCertainty::Unresolved),
        );
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
    Diagnostic::new(
        DiagnosticCode::AnalysisIncomplete,
        DiagnosticCode::AnalysisIncomplete.severity(),
        range,
        format!("scripted macro expansion exceeded the {limit} limit"),
    )
    .with_certainty(DiagnosticCertainty::Unresolved)
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
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::Cardinality,
                Severity::Warning,
                argument.key_range,
                format!(
                    "macro parameter `{}` is provided more than once",
                    argument.key
                ),
            ));
        }
    }
    let missing = summary
        .parameters
        .iter()
        .filter(|parameter| {
            parameter.required
                && !macro_parameter_is_runtime_optional(&summary, &parameter.name)
                && !counts
                    .keys()
                    .any(|name| name.eq_ignore_ascii_case(&parameter.name))
        })
        .map(|parameter| format!("`{}`", parameter.name))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        diagnostics.push(Diagnostic::new(
            DiagnosticCode::Cardinality,
            DiagnosticCode::Cardinality.severity(),
            property.key_range,
            format!(
                "macro `{}` is missing required parameter(s): {}",
                summary.name,
                missing.join(", ")
            ),
        ));
        return false;
    }
    true
}

/// Re-evaluates branch-local optionality from the persisted macro template. Vanilla macro
/// signatures can come from an older index cache whose compact `required` flags predate runtime
/// `if`/`else` branch awareness; the template remains sufficient to correct that metadata at the
/// call site without changing the cache schema.
fn macro_parameter_is_runtime_optional(summary: &MacroDefinitionSummary, name: &str) -> bool {
    let Some(template) = summary.template.as_ref() else {
        return false;
    };
    template_parameter_runtime_optional(template, name)
}

fn template_parameter_runtime_optional(template: &MacroTemplate, name: &str) -> bool {
    fn visit_token(
        token: &MacroTemplateToken,
        name: &str,
        runtime_guarded: bool,
        seen: &mut bool,
        unguarded: &mut bool,
    ) {
        for fragment in &token.fragments {
            let MacroTemplateFragment::Parameter {
                name: parameter, ..
            } = fragment
            else {
                continue;
            };
            if parameter.eq_ignore_ascii_case(name) {
                *seen = true;
                *unguarded |= !runtime_guarded;
            }
        }
    }

    fn visit_items(
        items: &[MacroTemplateItem],
        name: &str,
        runtime_guarded: bool,
        seen: &mut bool,
        unguarded: &mut bool,
    ) {
        for item in items {
            match item {
                MacroTemplateItem::Property(property) => {
                    let property_guarded = runtime_guarded && !is_limit_property(property);
                    visit_token(&property.key, name, property_guarded, seen, unguarded);
                    match &property.value {
                        MacroTemplateValue::Scalar(token) => {
                            visit_token(token, name, property_guarded, seen, unguarded)
                        }
                        MacroTemplateValue::Block { items, .. } => visit_items(
                            items,
                            name,
                            property_guarded || is_runtime_branch_key(&property.key),
                            seen,
                            unguarded,
                        ),
                    }
                }
                MacroTemplateItem::BareValue(token) => {
                    visit_token(token, name, runtime_guarded, seen, unguarded);
                }
                MacroTemplateItem::Conditional(conditional) => {
                    visit_items(&conditional.items, name, runtime_guarded, seen, unguarded)
                }
            }
        }
    }

    let mut seen = false;
    let mut unguarded = false;
    visit_items(&template.items, name, false, &mut seen, &mut unguarded);
    seen && !unguarded
}

fn is_limit_property(property: &MacroTemplateProperty) -> bool {
    let [MacroTemplateFragment::Literal(key)] = property.key.fragments.as_slice() else {
        return false;
    };
    key.trim().eq_ignore_ascii_case("limit")
}

fn is_runtime_branch_key(key: &MacroTemplateToken) -> bool {
    let [MacroTemplateFragment::Literal(key)] = key.fragments.as_slice() else {
        return false;
    };
    matches!(
        key.trim().to_ascii_lowercase().as_str(),
        "if" | "else_if" | "else"
    )
}
pub(crate) fn syntax_diagnostics(input: &ParsedInput) -> Vec<Diagnostic> {
    match &input.parsed {
        ParsedContent::Text(parsed) => parsed.errors().iter().map(diagnostic_from_syntax).collect(),
    }
}

pub(crate) fn diagnostic_from_syntax(error: &SyntaxError) -> Diagnostic {
    Diagnostic::new(
        DiagnosticCode::Syntax,
        DiagnosticCode::Syntax.severity(),
        error.range,
        error.message.clone(),
    )
}
