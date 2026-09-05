use crate::dynamic_contracts;
use crate::dynamic_cycles;
use crate::lints::{
    is_boolean_container_key, is_conditional_key, lint_boolean_container, lint_conditional_block,
    lint_conditional_siblings,
};
use crate::localisation::localisation_command_diagnostics;
use crate::messages::{
    backticked_list, did_you_mean, expected_from_rules, key_description, occurrence_word,
    value_description, value_plural,
};
use crate::quoted_script::{QuotedScriptParse, QuotedScriptSession};
use crate::resolution::*;
use crate::semantic::*;
use crate::suggest::best_suggestion;
use crate::support::*;
use crate::types::*;
use pdx_engine::hir::{
    HirFile, HirParameterReferenceKind, Scope, Template, TemplateFragment, TemplateItem,
    TemplateProperty, TemplateToken, TemplateValue,
};
use pdx_engine::{
    AnalysisSnapshot, DocumentId, DocumentSource, DynamicDefinitionSummary, SourceFileId,
};
use pdx_parser::{FileFormat, SyntaxError};
use pdx_rules::{KeyMatcher, RuleShape};
use pdx_text::TextRange;

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
                for related in right.related.drain(..) {
                    if !left.related.contains(&related) {
                        left.related.push(related);
                    }
                }
                left.notes.append(&mut right.notes);
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
    // Definition-site dynamic analyses run first: they warm the per-revision
    // caches the call-site checks consult.
    diagnostics
        .values
        .extend(dynamic_cycles::dynamic_cycle_diagnostics(
            snapshot,
            input,
            cancellation,
        )?);
    diagnostics
        .values
        .extend(dynamic_contracts::dynamic_contract_diagnostics(
            snapshot,
            input,
            cancellation,
        )?);
    diagnostics
        .values
        .extend(dynamic_contracts::dynamic_call_site_diagnostics(
            snapshot,
            input,
            cancellation,
        )?);
    diagnostics
        .values
        .extend(crate::modifier_scope::modifier_scope_diagnostics(
            snapshot,
            input,
            cancellation,
        )?);
    diagnostics
        .values
        .extend(semantic_rule_diagnostics(snapshot, input, cancellation)?);
    diagnostics.values.extend(localisation_command_diagnostics(
        snapshot,
        input,
        cancellation,
    )?);
    let mission = crate::mission::mission_diagnostics(snapshot, input, cancellation)?;
    if !mission.is_empty() {
        // The mission validator explains a dangling prerequisite with full
        // context ("mission A requires unknown mission B") and underlines the
        // exact prerequisite token; drop the generic bare-value complaint
        // covering the same token so users see one diagnostic per fault.
        let dependency_ranges: std::collections::HashSet<TextRange> = mission
            .iter()
            .filter(|diagnostic| diagnostic.code == DiagnosticCode::InvalidDependency)
            .map(|diagnostic| diagnostic.range)
            .collect();
        diagnostics.values.retain(|diagnostic| {
            !(diagnostic.code == DiagnosticCode::InvalidValue
                && dependency_ranges.contains(&diagnostic.range))
        });
        diagnostics.values.extend(mission);
    }
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
            // The rule-driven scope-command check reports the same value with
            // more context whenever a rule covers this key; only report here
            // when the semantic walk said nothing at this range.
            && !diagnostics
                .values
                .iter()
                .any(|diagnostic| diagnostic.range == *range)
        {
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::InvalidValue,
                DiagnosticCode::InvalidValue.severity(),
                *range,
                format!(
                    "unknown scope `{value}`{}",
                    did_you_mean(best_suggestion(
                        value,
                        snapshot
                            .game_profile()
                            .scope_names
                            .iter()
                            .map(String::as_str),
                    ))
                ),
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
            // Scalar arguments inside a dynamic definition invocation are untyped
            // parameter values, not localisation key references.
            Resolution::Missing
                if reference.kind.eq_ignore_ascii_case("localisation")
                    && localisation_reference_is_dynamic_argument(
                        snapshot,
                        input,
                        reference.range,
                    ) => {}
            Resolution::Missing => {
                diagnostics.push(if reference.kind.eq_ignore_ascii_case("localisation") {
                    // The game renders a missing localisation key as its raw spelling,
                    // so a missing key is a data-quality warning rather than a script
                    // error.
                    Diagnostic::new(
                        DiagnosticCode::UnknownLocalisationKey,
                        DiagnosticCode::UnknownLocalisationKey.severity(),
                        reference.range,
                        format!(
                            "unknown localisation key `{}`{}",
                            reference.name,
                            did_you_mean(
                                localisation_key_suggestion(snapshot, &reference.name).as_deref()
                            )
                        ),
                    )
                } else {
                    Diagnostic::new(
                        DiagnosticCode::InvalidValue,
                        DiagnosticCode::InvalidValue.severity(),
                        reference.range,
                        format!(
                            "unknown {} `{}`{}",
                            reference.kind,
                            reference.name,
                            did_you_mean(best_suggestion(
                                &reference.name,
                                effective_workspace_member_names(snapshot, &reference.kind)
                                    .iter()
                                    .map(String::as_str)
                            ))
                        ),
                    )
                })
            }
            // Localisation is merged across languages and may be repeated by replace files.
            // Existence is enough for diagnostics; navigation retains the candidate set.
            // The game resolves same-name definitions deterministically by source priority,
            // so ambiguity is never a runtime error and is intentionally not diagnosed.
            Resolution::Ambiguous => {}
            Resolution::Unique(_) => {}
        }
    }
    for definition in &semantic.definitions {
        cancellation.checkpoint()?;
        if definition.name.contains('$') {
            continue;
        }
        // Shadowing is only meaningful for kinds the game resolves by name
        // (later definition wins). The index also harvests member definitions
        // named after structural keys (`maneuver`, `graphical_culture`, event
        // flags) that repeat legally in every instance; those never conflict.
        if !pdx_game::eu4::resolved_symbol_kind(&definition.kind) {
            continue;
        }
        // Only replacement-policy kinds can shadow; merge/unique kinds (such as
        // localisation) legally repeat the same name.
        let Some(ordered) = resolution.ordered_candidates(&definition.kind, &definition.name)
        else {
            continue;
        };
        // Candidates above were already reduced to one priority, so any earlier
        // sibling here comes from the same source root; cross-root overrides
        // (for example a mod replacing a vanilla definition) stay silent.
        let Some(index) = ordered
            .iter()
            .position(|candidate| candidate.location == definition.symbol.location)
        else {
            continue;
        };
        if index > 0 {
            let mut diagnostic = Diagnostic::new(
                DiagnosticCode::AmbiguousDefinition,
                DiagnosticCode::AmbiguousDefinition.severity(),
                definition.symbol.selection_range,
                format!(
                    "definition `{}` shadows an earlier definition of the same name; the later definition takes effect",
                    definition.name
                ),
            );
            // Point at the definition being shadowed so one click separates a
            // true collision from an intentional override.
            if let Some(earlier) = index
                .checked_sub(1)
                .and_then(|earlier| ordered.get(earlier))
            {
                let mut location = earlier.location.clone();
                location.range = earlier.selection_range;
                diagnostic = diagnostic.with_related(RelatedLocation {
                    location,
                    message: format!("earlier definition of `{}`", definition.name),
                });
            }
            diagnostics.push(diagnostic);
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

/// Returns whether a localisation-kind reference sits inside a dynamic definition
/// invocation, where scalar arguments are untyped parameter values.
fn localisation_reference_is_dynamic_argument(
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
    // nested invocations carry opaque dynamic arguments.
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
            quoted_scripts: &mut quoted_scripts,
            quoted_script_depth: 0,
            scope_diagnostics_deferred: false,
            enclosing_key: None,
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
                    quoted_scripts: &mut quoted_scripts,
                    quoted_script_depth: 0,
                    scope_diagnostics_deferred: false,
                    enclosing_key: None,
                })?;
            }
        } else {
            let root_properties: Vec<&ScriptProperty> = property.block.iter().collect();
            let root_values: Vec<&(std::sync::Arc<str>, TextRange)> =
                property.bare_values.iter().collect();
            // Definition-style type contexts may declare a wrapper rule for the entry
            // key itself whose child_context describes the entry body (e.g. `root:luck`'s
            // any_scalar country blocks validating as trigger clauses). The wrapper only
            // speaks for the entry when the context offers nothing but wildcard matchers:
            // a context that also names keys (`root:imperial_incident`'s `event`,
            // `can_stop`, ...) describes the entry body itself, and its wildcard rules
            // target entry children. Structural wrapper rules without a child-context
            // switch keep the file context, matching the engine descent.
            let context_is_wildcard_only =
                semantic_rules_for_container(snapshot, &context, &[], &scope)
                    .iter()
                    .all(|rule| !matches!(rule.key, KeyMatcher::Exact(_) | KeyMatcher::Enum(_)));
            let wrapper_matching =
                semantic_rules_for_container_key(snapshot, &context, &[], &property.key)
                    .into_iter()
                    .filter(|rule| {
                        !matches!(rule.shape, RuleShape::LeafValue)
                            && semantic_rule_key_matches(snapshot, rule, &[], &property.key)
                    })
                    .collect::<Vec<_>>();
            let wrapper_transition = semantic_selected_transition(SemanticTransitionInput {
                snapshot,
                matching: &wrapper_matching,
                selected_alternative: None,
                context: &context,
                parent_path: &[],
                property: &property,
                scope: &scope,
                transparent_wrapper: false,
            });
            let mut body_context = context;
            let mut body_scope = scope;
            if let Some(rule) = wrapper_transition
                && context_is_wildcard_only
                && matches!(rule.key, KeyMatcher::AnyScalar | KeyMatcher::Date)
                && rule
                    .child_context
                    .as_deref()
                    .is_some_and(|child| !child.eq_ignore_ascii_case(&body_context))
            {
                body_scope = semantic_child_scope(snapshot, &body_scope, rule);
                body_context = rule.child_context.clone().unwrap_or(body_context);
            }
            validate_semantic_container(SemanticValidationInput {
                snapshot,
                context: &body_context,
                parent_path: &[],
                properties: &root_properties,
                bare_values: &root_values,
                scope: &body_scope,
                hir: input.hir.as_deref(),
                diagnostics: &mut container_diagnostics,
                cancellation,
                block_container: property.block_range.is_some(),
                container_range: property.block_range.unwrap_or(property.key_range),
                quoted_scripts: &mut quoted_scripts,
                quoted_script_depth: 0,
                scope_diagnostics_deferred: false,
                enclosing_key: None,
            })?;
        }
        if fallback_context {
            // A path-only context is still enough to establish that an authored key is not
            // accepted by the selected rule set: the game silently ignores such a key. Keep
            // unknown keys as errors; only scope availability remains uncertain until a more
            // specific root/context match is available.
            for diagnostic in &mut container_diagnostics {
                if diagnostic.code == DiagnosticCode::WrongScope {
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
    quoted_scripts: &'session mut QuotedScriptSession<'cancel>,
    quoted_script_depth: usize,
    /// True when scope decisions for this subtree belong to the dynamic-contract
    /// layer (definition-site entry contracts plus call-site validation) rather
    /// than this walk. Set for quoted-script payloads of dynamic-rule
    /// invocations: their scope findings would duplicate the contract layer's,
    /// at call-site positions and once per invocation. Content validation
    /// (keys, values, cardinality, bindings) still runs.
    scope_diagnostics_deferred: bool,
    /// Key of the property owning this container, when the container is a
    /// property block. EU4 accepts `else`/`else_if` both as siblings after an
    /// `if` and nested as children of one, so the orphan check needs the
    /// enclosing key to accept the nested form.
    enclosing_key: Option<&'data str>,
}

/// Detects a key that is unknown in the current context but defined in the
/// sibling context, so the unknown-key message can say what went wrong instead
/// of only that the key is unexpected.
fn sibling_context_key_kind(
    snapshot: &AnalysisSnapshot,
    trigger_like: bool,
    effect_like: bool,
    parent_path: &[std::sync::Arc<str>],
    property: &ScriptProperty,
) -> Option<&'static str> {
    let sibling = if trigger_like && !effect_like {
        "effect"
    } else if effect_like && !trigger_like {
        "trigger"
    } else {
        return None;
    };
    let kind = if sibling == "effect" {
        "an effect"
    } else {
        "a trigger"
    };
    [parent_path, &[]]
        .iter()
        .any(|path| {
            semantic_rules_for_container_key(snapshot, sibling, path, &property.key)
                .into_iter()
                .any(|rule| {
                    matches!(rule.key, KeyMatcher::Exact(_))
                        && semantic_property_structure_matches(rule, property)
                })
        })
        .then_some(kind)
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
        quoted_scripts,
        quoted_script_depth,
        scope_diagnostics_deferred,
        enclosing_key,
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
    if trigger_like || effect_like {
        lint_conditional_siblings(properties, enclosing_key, diagnostics);
    }
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
        if trigger_like && is_boolean_container_key(&property.key) {
            lint_boolean_container(property, diagnostics);
        }
        if (trigger_like || effect_like) && is_conditional_key(&property.key) {
            lint_conditional_block(property, effect_like, diagnostics);
        }
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
            let mut diagnostic = None;
            if let Some(kind) =
                sibling_context_key_kind(snapshot, trigger_like, effect_like, parent_path, property)
            {
                let message = if kind == "an effect" {
                    format!(
                        "`{}` is an effect and cannot be used inside a trigger block",
                        property.key
                    )
                } else {
                    format!(
                        "`{}` is a trigger and cannot be used inside an effect block; conditions belong in a `limit` block",
                        property.key
                    )
                };
                diagnostic = Some(Diagnostic::new(
                    DiagnosticCode::UnknownKey,
                    DiagnosticCode::UnknownKey.severity(),
                    property.key_range,
                    message,
                ));
            }
            if diagnostic.is_none() {
                diagnostic = dynamic_invocation_parameter_message(
                    snapshot,
                    context,
                    parent_path,
                    hir,
                    property,
                )
                .map(|message| {
                    Diagnostic::new(
                        DiagnosticCode::UnknownKey,
                        DiagnosticCode::UnknownKey.severity(),
                        property.key_range,
                        message,
                    )
                });
            }
            let diagnostic = diagnostic.unwrap_or_else(|| {
                // Sibling keys of the same container rule set make a useful
                // correction vocabulary for a misspelled key.
                let mut sibling_keys = Vec::new();
                for rule in semantic_rules_for_container(snapshot, context, parent_path, scope) {
                    if let KeyMatcher::Exact(key) = &rule.key
                        && !sibling_keys
                            .iter()
                            .any(|seen: &String| seen.eq_ignore_ascii_case(key))
                    {
                        sibling_keys.push(key.clone());
                    }
                }
                let suggestion =
                    best_suggestion(&property.key, sibling_keys.iter().map(String::as_str));
                let location = context.strip_prefix("type:").map_or_else(
                    || {
                        format!(
                            "{} `{context}` block",
                            crate::messages::article_for(context)
                        )
                    },
                    |kind| format!("{} `{kind}` definition", crate::messages::article_for(kind)),
                );
                let mut diagnostic = Diagnostic::new(
                    DiagnosticCode::UnknownKey,
                    DiagnosticCode::UnknownKey.severity(),
                    property.key_range,
                    format!(
                        "unknown key `{}` in {location}{}",
                        property.key,
                        did_you_mean(suggestion)
                    ),
                );
                if let Some(candidate) = suggestion {
                    diagnostic = diagnostic.with_fix(QuickFix::replace(
                        format!("Did you mean '{candidate}'?"),
                        property.key_range,
                        (*candidate).to_owned(),
                    ));
                }
                diagnostic
            });
            diagnostics.push(diagnostic);
        } else {
            let scoped_matching = matching
                .iter()
                .filter(|rule| semantic_scope_allows(rule, scope))
                .copied()
                .collect::<Vec<_>>();
            if scoped_matching.is_empty() && !scope_diagnostics_deferred {
                let mut scopes: Vec<&str> = Vec::new();
                for rule in matching.iter() {
                    for allowed in &rule.allowed_scopes {
                        if !scopes.iter().any(|seen| seen.eq_ignore_ascii_case(allowed)) {
                            scopes.push(allowed.as_str());
                        }
                    }
                }
                let mut diagnostic = semantic_diagnostic(
                    DiagnosticCode::WrongScope,
                    semantic_rule_severity(matching.iter().copied(), DiagnosticCode::WrongScope),
                    property.key_range,
                    format!(
                        "`{}` is not available in scope `{}`",
                        property.key, scope.current
                    ),
                    matching[0],
                );
                if !scopes.is_empty() {
                    let expected = if scopes.len() == 1 {
                        format!("`{}`", scopes[0])
                    } else {
                        format!("one of {}", backticked_list(&scopes, 8))
                    };
                    diagnostic = diagnostic.with_expected(expected);
                }
                diagnostics.push(diagnostic);
            }
            let applicable = if scoped_matching.is_empty() {
                &matching
            } else {
                &scoped_matching
            };
            let dynamic_owned_value_mismatch = dynamic_layer_owns_value_diagnostic(
                snapshot,
                applicable,
                parent_path,
                property,
                scope,
            );
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
            if !valid && !dynamic_owned_value_mismatch {
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
                    Some(ScopeValueMatch::Unknown) => DiagnosticCode::InvalidValue,
                    Some(ScopeValueMatch::Known {
                        compatible: false, ..
                    }) => DiagnosticCode::WrongScope,
                    _ => DiagnosticCode::InvalidValue,
                };
                let range = property
                    .scalar
                    .as_ref()
                    .map_or(property.key_range, |(_, range)| *range);
                let value_text = property
                    .scalar
                    .as_ref()
                    .map_or(String::new(), |(value, _)| (*value).to_string());
                let scope_suggestion = || {
                    best_suggestion(
                        &value_text,
                        // Scope registers are positional keywords, not spellings
                        // a user is typoing toward; suggesting them is noise.
                        snapshot
                            .game_profile()
                            .scope_names
                            .iter()
                            .map(String::as_str)
                            .filter(|name| {
                                !matches!(
                                    name.to_ascii_lowercase().as_str(),
                                    "root" | "this" | "from" | "prev"
                                )
                            }),
                    )
                };
                let message = match scope_value.as_ref() {
                    Some(ScopeValueMatch::Unknown)
                        if property.key.eq_ignore_ascii_case("scope") =>
                    {
                        format!(
                            "invalid scope command target `{value_text}`{}",
                            did_you_mean(scope_suggestion())
                        )
                    }
                    Some(ScopeValueMatch::Unknown) => format!(
                        "invalid target `{value_text}`: scope expression is not recognised{}",
                        did_you_mean(scope_suggestion())
                    ),
                    Some(ScopeValueMatch::Known {
                        actual, expected, ..
                    }) => format!(
                        "target `{value_text}` resolves to scope `{actual}`, expected {}",
                        expected.as_deref().unwrap_or("any scope")
                    ),
                    _ => format!(
                        "invalid value `{value_text}` for `{}`{}",
                        property.key,
                        did_you_mean(
                            property
                                .scalar
                                .as_ref()
                                .and_then(|(value, _)| {
                                    enum_value_suggestion(snapshot, applicable, value)
                                })
                                .as_deref()
                        )
                    ),
                };
                let certainty = match scope_value.as_ref() {
                    // A resolved target whose scope disagrees with the rule depends
                    // on the caller's runtime scope, so it stays contextual.
                    Some(ScopeValueMatch::Known {
                        compatible: false, ..
                    }) => DiagnosticCertainty::Contextual,
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
                    && let Some(expected) =
                        expected_from_rules(snapshot, applicable.iter().copied())
                {
                    diagnostic = diagnostic.with_expected(expected);
                }
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
            if !parameterized_invocation {
                validate_dynamic_arguments(snapshot, applicable, property, scope, diagnostics);
                validate_dynamic_dispatch_keys(snapshot, applicable, property, diagnostics);
                validate_dynamic_quoted_payloads(
                    ValidationState {
                        snapshot,
                        diagnostics,
                        cancellation,
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
                        "`{}` appears {}, but at most {} {} allowed here",
                        property.key,
                        occurrence_word(count),
                        max_occurs,
                        if max_occurs == 1 { "is" } else { "are" },
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
                    quoted_scripts,
                    quoted_script_depth,
                    scope_diagnostics_deferred,
                    enclosing_key: Some(property.key.as_ref()),
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
                    quoted_scripts,
                },
                &next_context,
                &child_path,
                &next_scope,
                property,
                quoted_script_depth,
                scope_diagnostics_deferred,
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
                    quoted_scripts,
                    quoted_script_depth,
                    scope_diagnostics_deferred,
                    enclosing_key: Some(property.key.as_ref()),
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
                    quoted_scripts,
                    quoted_script_depth,
                    scope_diagnostics_deferred,
                    enclosing_key: Some(property.key.as_ref()),
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
            quoted_scripts,
            quoted_script_depth,
            scope_diagnostics_deferred,
            enclosing_key: Some(property.key.as_ref()),
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
            // Both numeric-range overflow and unrecognised scalars are invalid
            // values; the severity helper keeps bounded numeric overflows soft.
            let code = DiagnosticCode::InvalidValue;
            let severity = semantic_bare_value_severity(rules.iter().copied(), value);
            // A numeric value against bounded numeric rules is a range problem;
            // anything else is an unknown member of the accepted set.
            let numeric_overflow = value.parse::<f64>().is_ok()
                && rules.iter().any(|rule| {
                    matches!(rule.shape, RuleShape::LeafValue)
                        && matches!(
                            &rule.value,
                            pdx_rules::ValueMatcher::Int { .. }
                                | pdx_rules::ValueMatcher::Float { .. }
                        )
                });
            let message = if numeric_overflow {
                format!("value `{value}` is out of range")
            } else {
                format!("value `{value}` is not valid here")
            };
            let mut diagnostic = if let Some(rule) = rules.first() {
                semantic_diagnostic(code, severity, *value_range, message, rule)
            } else {
                Diagnostic::new(code, severity, *value_range, message)
            };
            let leaf_rules = rules
                .iter()
                .filter(|rule| matches!(rule.shape, RuleShape::LeafValue))
                .copied()
                .collect::<Vec<_>>();
            if let Some(expected) = expected_from_rules(snapshot, leaf_rules) {
                diagnostic = diagnostic.with_expected(expected);
            }
            diagnostics.push(diagnostic);
        }
    }
    // "This block ..." cardinality findings anchor on the opening brace of the
    // block itself: underlining a sibling property for a *missing* key invites
    // the wrong fix, and a whole-block squiggle hides the actual content.
    let block_anchor = if block_container {
        TextRange::new(container_range.start(), container_range.start() + 1)
            .unwrap_or(container_range)
    } else {
        container_range
    };
    let empty_range = properties.first().map_or_else(
        || {
            bare_values
                .first()
                .map_or(block_anchor, |(_, range)| *range)
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
                let plural = value_plural(snapshot, &rule.value);
                if let Some(min_occurs) = semantic_min_occurs(rule)
                    && count < min_occurs
                {
                    diagnostics.push(semantic_diagnostic(
                    DiagnosticCode::Cardinality,
                    semantic_min_cardinality_severity(rule),
                    block_anchor,
                    format!(
                        "this list must contain at least {min_occurs} {plural}, but contains {count}",
                    ),
                    rule,
                ));
                }
                if let Some(max_occurs) = rule.max_occurs
                    && count > max_occurs
                {
                    // Anchor the overflow on the first value past the quota so the
                    // squiggle names the entry that should be removed.
                    let overflow_range = bare_values
                        .iter()
                        .filter(|(value, _)| {
                            semantic_leaf_value_matches(snapshot, rule, value, scope)
                        })
                        .nth(max_occurs as usize)
                        .map_or(empty_range, |(_, range)| *range);
                    diagnostics.push(semantic_diagnostic(
                        DiagnosticCode::Cardinality,
                        Severity::Warning,
                        overflow_range,
                        format!(
                            "this list allows at most {max_occurs} {plural}, but contains {count}",
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
                let message =
                    if min_occurs == 1 && count == 0 && matches!(rule.key, KeyMatcher::Exact(_)) {
                        format!(
                            "this block is missing required key {}",
                            key_description(&rule.key)
                        )
                    } else {
                        let subject = match &rule.key {
                            KeyMatcher::Exact(value) => format!("`{value}`"),
                            KeyMatcher::Type(kind) => format!("a `{kind}` name"),
                            KeyMatcher::Enum(name) => format!("a key from `{name}`"),
                            _ => "an entry".to_owned(),
                        };
                        format!(
                            "this block requires at least {} {} of {}; it contains {}",
                            min_occurs,
                            if min_occurs == 1 { "entry" } else { "entries" },
                            subject,
                            occurrence_word(count),
                        )
                    };
                diagnostics.push(semantic_diagnostic(
                    DiagnosticCode::Cardinality,
                    semantic_min_cardinality_severity(rule),
                    block_anchor,
                    message,
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

/// Finds a unique close indexed localisation key for a missing reference.
fn localisation_key_suggestion(snapshot: &AnalysisSnapshot, name: &str) -> Option<String> {
    best_suggestion(name, localisation_key_index(snapshot).iter()).map(str::to_owned)
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
    quoted_scripts: &'session mut QuotedScriptSession<'cancel>,
}

fn validate_quoted_script(
    state: ValidationState<'_, '_, '_>,
    context: &str,
    parent_path: &[std::sync::Arc<str>],
    scope: &ScopeContext,
    property: &ScriptProperty,
    depth: usize,
    scope_diagnostics_deferred: bool,
) -> Result<(), Cancelled> {
    let ValidationState {
        snapshot,
        diagnostics,
        cancellation,
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
                limit.message(),
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
        quoted_scripts,
        quoted_script_depth: depth.saturating_add(1),
        scope_diagnostics_deferred,
        enclosing_key: None,
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

fn dynamic_invocation_summary(
    snapshot: &AnalysisSnapshot,
    rules: &[&pdx_rules::SemanticRule],
    key: &str,
) -> Option<DynamicDefinitionSummary> {
    rules.iter().find_map(|rule| {
        let type_name = match &rule.key {
            pdx_rules::KeyMatcher::Type(type_name) | pdx_rules::KeyMatcher::Dynamic(type_name)
                if dynamic_definition_type(snapshot, type_name) =>
            {
                type_name
            }
            _ => return None,
        };
        dynamic_definition_summary(snapshot, type_name, key)
    })
}

/// True when the dynamic-definition layer owns the message for this
/// property's value shape, so the generic value-mismatch diagnostic stays
/// silent: either the key is a parameter of a resolved invocation (argument
/// shape and argument values carry dedicated diagnostics), or the property
/// is a scalar invocation of a definition with required parameters (the
/// missing-parameter diagnostic fires instead). Scope-target verdicts stay
/// with the generic layer.
fn dynamic_layer_owns_value_diagnostic(
    snapshot: &AnalysisSnapshot,
    rules: &[&pdx_rules::SemanticRule],
    parent_path: &[std::sync::Arc<str>],
    property: &ScriptProperty,
    scope: &ScopeContext,
) -> bool {
    if let Some((value, _)) = property.scalar.as_ref() {
        let scope_valued = rules.iter().any(|rule| {
            !matches!(
                semantic_scope_value_match(snapshot, rule, value, scope),
                ScopeValueMatch::NotScopeRule
            )
        });
        if scope_valued {
            return false;
        }
    }
    if rules.iter().any(|rule| {
        matches!(
            qualified_parameter_domain(snapshot, rule, parent_path),
            QualifiedParameterDomain::Known(_)
        )
    }) {
        return true;
    }
    if property.block_range.is_none() {
        return dynamic_invocation_summary(snapshot, rules, &property.key).is_some_and(|summary| {
            summary.parameters.iter().any(|parameter| {
                parameter.required
                    && !dynamic_parameter_is_runtime_optional(&summary, &parameter.name)
            })
        });
    }
    false
}

/// True when `range` sits inside a dynamic-definition body in this file:
/// unknown statements there are ordinary unknown effects or triggers, not
/// invocation parameters.
fn inside_dynamic_definition_body(
    snapshot: &AnalysisSnapshot,
    hir: Option<&HirFile>,
    range: TextRange,
) -> bool {
    hir.is_some_and(|hir| {
        hir.definitions().iter().any(|definition| {
            definition.range.start() <= range.start()
                && range.end() <= definition.range.end()
                && dynamic_definition_type(snapshot, &definition.kind)
        })
    })
}

/// The unknown-key message for a key sitting directly inside a dynamic
/// invocation's argument block: name the scripted definition and its known
/// parameters instead of only the rule context.
fn dynamic_invocation_parameter_message(
    snapshot: &AnalysisSnapshot,
    context: &str,
    parent_path: &[std::sync::Arc<str>],
    hir: Option<&HirFile>,
    property: &ScriptProperty,
) -> Option<String> {
    let owner_name = parent_path.last()?;
    let kind = crate::dynamic_rules::dynamic_kind_for_context(snapshot, context)?;
    let summary = dynamic_definition_summary(snapshot, &kind, owner_name)?;
    if inside_dynamic_definition_body(snapshot, hir, property.key_range) {
        return None;
    }
    if crate::dynamic_rules::dynamic_rule_row(snapshot, &kind, owner_name)
        .is_some_and(|row| row.dispatches_dynamically)
    {
        // A dispatching invocation binds arbitrary caller-chosen keys; only
        // the dispatch-key validator can judge them.
        return None;
    }
    if summary.parameters.is_empty() {
        return Some(format!(
            "unexpected key `{}`: scripted `{}` takes no parameters",
            property.key, summary.name
        ));
    }
    let names = summary
        .parameters
        .iter()
        .map(|parameter| parameter.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "unexpected parameter `{}` of scripted `{}` (known: {names})",
        property.key, summary.name
    ))
}

fn validate_dynamic_arguments(
    snapshot: &AnalysisSnapshot,
    rules: &[&pdx_rules::SemanticRule],
    property: &ScriptProperty,
    scope: &ScopeContext,
    diagnostics: &mut Vec<Diagnostic>,
) {
    // Scalar invocations (`stable = yes`) cannot bind anything, but fall
    // through: a definition with required parameters must still get the
    // dedicated missing-parameter message instead of only the generic
    // value mismatch.
    let Some(summary) = dynamic_invocation_summary(snapshot, rules, &property.key) else {
        return;
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
                    "dynamic parameter `{}` is provided more than once",
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
                && !dynamic_parameter_is_runtime_optional(&summary, &parameter.name)
                && !counts
                    .keys()
                    .any(|name| name.eq_ignore_ascii_case(&parameter.name))
        })
        .map(|parameter| format!("`{}`", parameter.name))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        let hint = if property.block_range.is_none() {
            "; provide them in a parameter block"
        } else {
            ""
        };
        diagnostics.push(Diagnostic::new(
            DiagnosticCode::Cardinality,
            DiagnosticCode::Cardinality.severity(),
            property.key_range,
            format!(
                "dynamic definition `{}` is missing required parameter(s): {}{hint}",
                summary.name,
                missing.join(", ")
            ),
        ));
        return;
    }
    // A conditional whose guard is supplied (or, for `[!X]`, absent)
    // activates its body; parameters the body then uses without a runtime
    // guard become required for this invocation.
    let row = crate::dynamic_rules::dynamic_rule_row(snapshot, &summary.kind, &summary.name);
    if let (Some(template), Some(row)) = (summary.template.as_ref(), row.as_ref()) {
        let supplied: std::collections::BTreeSet<String> = counts.keys().cloned().collect();
        let branch_missing =
            template_branch_active_missing(snapshot, template, &row.context, &supplied);
        if !branch_missing.is_empty() {
            let names = branch_missing
                .iter()
                .map(|name| format!("`{name}`"))
                .collect::<Vec<_>>()
                .join(", ");
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::Cardinality,
                DiagnosticCode::Cardinality.severity(),
                property.key_range,
                format!(
                    "scripted `{}` requires parameter(s) {names} in the active branch",
                    summary.name
                ),
            ));
            return;
        }
    }
    // The derived dynamic row carries the usage-site value constraints that
    // the definition body infers for each parameter; check the caller's
    // arguments against them here, at the call site, without instantiating
    // the definition body.
    if let Some(row) = row {
        validate_dynamic_argument_values(snapshot, &row, property, scope, diagnostics);
    }
}

/// Parameters that activated conditional branches use without a runtime
/// guard, in body order, deduplicated. Only bodies reached through an
/// activated `[X]`/`[!X]` conditional are requirements of this invocation:
/// parameters outside conditionals belong to the static signature check,
/// runtime `if`/`else` subtrees are alternatives, and nested dynamic-rule
/// argument blocks forward rather than require.
fn template_branch_active_missing(
    snapshot: &AnalysisSnapshot,
    template: &Template,
    context: &str,
    supplied: &std::collections::BTreeSet<String>,
) -> Vec<String> {
    let mut missing = Vec::new();
    branch_conditional_items(
        snapshot,
        &template.items,
        context,
        false,
        supplied,
        &mut missing,
    );
    missing
}

/// Considers only conditionals; non-conditional items are the static
/// signature's concern.
fn branch_conditional_items(
    snapshot: &AnalysisSnapshot,
    items: &[TemplateItem],
    context: &str,
    runtime_guarded: bool,
    supplied: &std::collections::BTreeSet<String>,
    missing: &mut Vec<String>,
) {
    for item in items {
        if let TemplateItem::Conditional(conditional) = item {
            let guard_supplied = supplied.contains(&conditional.name.to_ascii_lowercase());
            if conditional.negated != guard_supplied {
                branch_active_body(
                    snapshot,
                    &conditional.items,
                    context,
                    runtime_guarded,
                    supplied,
                    missing,
                );
            }
        }
    }
}

/// One activated body: collect unguarded parameter usage, recurse into
/// nested conditionals by their own guards, and keep runtime-branch and
/// forwarding semantics.
fn branch_active_body(
    snapshot: &AnalysisSnapshot,
    items: &[TemplateItem],
    context: &str,
    runtime_guarded: bool,
    supplied: &std::collections::BTreeSet<String>,
    missing: &mut Vec<String>,
) {
    for item in items {
        match item {
            TemplateItem::Property(property) => {
                let property_guarded = runtime_guarded && !is_limit_property(property);
                branch_active_record_token(&property.key, property_guarded, supplied, missing);
                match &property.value {
                    TemplateValue::Scalar(token) => {
                        branch_active_record_token(token, property_guarded, supplied, missing);
                    }
                    TemplateValue::Block { items, .. } => {
                        if branch_active_forwards_arguments(snapshot, context, property) {
                            continue;
                        }
                        branch_active_body(
                            snapshot,
                            items,
                            context,
                            property_guarded || is_runtime_branch_key(&property.key),
                            supplied,
                            missing,
                        );
                    }
                }
            }
            TemplateItem::BareValue(token) => {
                branch_active_record_token(token, runtime_guarded, supplied, missing);
            }
            TemplateItem::Conditional(conditional) => {
                let guard_supplied = supplied.contains(&conditional.name.to_ascii_lowercase());
                if conditional.negated != guard_supplied {
                    branch_active_body(
                        snapshot,
                        &conditional.items,
                        context,
                        runtime_guarded,
                        supplied,
                        missing,
                    );
                }
            }
        }
    }
}

/// Records parameters used in a token as missing unless supplied; parameters
/// inside runtime branch alternatives are optional and not recorded.
fn branch_active_record_token(
    token: &TemplateToken,
    runtime_guarded: bool,
    supplied: &std::collections::BTreeSet<String>,
    missing: &mut Vec<String>,
) {
    if runtime_guarded {
        return;
    }
    for fragment in &token.fragments {
        let TemplateFragment::Parameter { name, .. } = fragment else {
            continue;
        };
        if !supplied.contains(&name.to_ascii_lowercase())
            && !missing
                .iter()
                .any(|recorded| recorded.eq_ignore_ascii_case(name))
        {
            missing.push(name.clone());
        }
    }
}

/// True when the property is a nested dynamic-rule call whose block assigns
/// callee parameters; forwarded parameters render at this definition's own
/// call sites, so they are not requirements here.
fn branch_active_forwards_arguments(
    snapshot: &AnalysisSnapshot,
    context: &str,
    property: &TemplateProperty,
) -> bool {
    let [TemplateFragment::Literal(key)] = property.key.fragments.as_slice() else {
        return false;
    };
    crate::dynamic_rules::dynamic_kind_for_context(snapshot, context)
        .and_then(|kind| resolve_dynamic_definition(snapshot, &kind, key.trim()))
        .is_some()
}

/// Checks one invocation's arguments against the parameter rows of its
/// derived dynamic rule: scalar shape where the body uses the parameter as a
/// value, and value matching against the inferred per-site matchers.
fn validate_dynamic_argument_values(
    snapshot: &AnalysisSnapshot,
    row: &crate::dynamic_rules::DynamicRuleRow,
    invocation: &ScriptProperty,
    scope: &ScopeContext,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for (index, argument) in invocation.block.iter().enumerate() {
        // Forwarded parameters (`K = $K$` inside another definition) render
        // at that definition's own call sites; they cannot be checked here.
        if argument
            .scalar
            .as_ref()
            .is_some_and(|(value, _)| value.contains('$'))
        {
            continue;
        }
        let Some(parameter) = row
            .parameters
            .iter()
            .find(|parameter| parameter.name.eq_ignore_ascii_case(&argument.key))
        else {
            continue;
        };
        // Bindings keep the last duplicate; earlier occurrences are stale and
        // already carry their own cardinality warning.
        let superseded = invocation.block[index + 1..]
            .iter()
            .any(|later| later.key.eq_ignore_ascii_case(&argument.key));
        if superseded {
            continue;
        }
        let sites = effective_parameter_sites(snapshot, row, parameter, &mut Vec::new());
        let Some((value, value_range)) = argument.scalar.as_ref() else {
            if sites.is_empty() {
                continue;
            }
            diagnostics.push(
                Diagnostic::new(
                    DiagnosticCode::InvalidValue,
                    DiagnosticCode::InvalidValue.severity(),
                    argument.key_range,
                    format!(
                        "parameter `{}` of scripted `{}` is used as a scalar value in its body and must be provided as one",
                        parameter.name, row.name
                    ),
                )
                .with_certainty(DiagnosticCertainty::Contextual),
            );
            continue;
        };
        if parameter.quoted_script {
            // Quoted script payloads are validated by the quoted-script
            // machinery, not by scalar matching.
            continue;
        }
        if let Some(rejected) = sites.iter().find(|site| {
            !site
                .iter()
                .any(|matcher| semantic_matcher_accepts(snapshot, matcher, value, scope))
        }) {
            let expected = rejected
                .iter()
                .map(|matcher| value_description(snapshot, matcher))
                .collect::<Vec<_>>()
                .join(" or ");
            diagnostics.push(
                Diagnostic::new(
                    DiagnosticCode::InvalidValue,
                    DiagnosticCode::InvalidValue.severity(),
                    *value_range,
                    format!(
                        "argument `{}` for parameter `{}` of scripted `{}` does not match its usage in the definition body",
                        value, parameter.name, row.name
                    ),
                )
                .with_expected(expected)
                .with_certainty(DiagnosticCertainty::Contextual),
            );
        }
        validate_forwarded_to_any_parameter(
            snapshot,
            row,
            parameter,
            value,
            *value_range,
            scope,
            diagnostics,
        );
    }
}

/// Forwards whose callee parameter name is itself rendered from a `$param$`
/// key (`helper = { $WHICH$ = $X$ }`) can land on any of the callee's
/// parameters: the binding must be acceptable to at least one of them.
fn validate_forwarded_to_any_parameter(
    snapshot: &AnalysisSnapshot,
    row: &crate::dynamic_rules::DynamicRuleRow,
    parameter: &crate::dynamic_rules::DynamicParameterRow,
    value: &str,
    value_range: TextRange,
    scope: &ScopeContext,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for edge in parameter
        .forwarded_to
        .iter()
        .filter(|edge| edge.parameter.is_none())
    {
        let Some(callee) = crate::dynamic_rules::dynamic_rule_row(snapshot, &edge.kind, &edge.name)
        else {
            continue;
        };
        let acceptable = callee.parameters.iter().any(|candidate| {
            candidate.quoted_script && value.starts_with('"')
                || candidate.sites.is_empty()
                || candidate.sites.iter().any(|site| {
                    site.iter()
                        .any(|matcher| semantic_matcher_accepts(snapshot, matcher, value, scope))
                })
        });
        if !acceptable {
            diagnostics.push(
                Diagnostic::new(
                    DiagnosticCode::InvalidValue,
                    DiagnosticCode::InvalidValue.severity(),
                    value_range,
                    format!(
                        "argument `{}` for parameter `{}` of scripted `{}` does not match any parameter of scripted `{}` it can be forwarded to",
                        value, parameter.name, row.name, callee.name
                    ),
                )
                .with_certainty(DiagnosticCertainty::Contextual),
            );
        }
    }
}

/// Validates quoted script payloads bound to payload parameters (bare
/// `$BODY$` usage, quoted template tokens, or QuotedScript-shape sites): the
/// quoted string is parsed and its statements validated in the definition's
/// body context, with diagnostics mapped onto the argument's quoted range.
fn validate_dynamic_quoted_payloads(
    state: ValidationState<'_, '_, '_>,
    rules: &[&pdx_rules::SemanticRule],
    invocation: &ScriptProperty,
    scope: &ScopeContext,
    quoted_script_depth: usize,
) -> Result<(), Cancelled> {
    if invocation.block_range.is_none() {
        return Ok(());
    }
    let ValidationState {
        snapshot,
        diagnostics,
        cancellation,
        quoted_scripts,
    } = state;
    let Some(row) = dynamic_row_for_invocation(snapshot, rules, invocation) else {
        return Ok(());
    };
    for (index, argument) in invocation.block.iter().enumerate() {
        // Bindings keep the last duplicate; earlier occurrences are stale.
        let superseded = invocation.block[index + 1..]
            .iter()
            .any(|later| later.key.eq_ignore_ascii_case(&argument.key));
        if superseded {
            continue;
        }
        let Some(parameter) = row
            .parameters
            .iter()
            .find(|parameter| parameter.name.eq_ignore_ascii_case(&argument.key))
        else {
            continue;
        };
        if !parameter.quoted_script {
            continue;
        }
        if argument.quoted_source.is_none() {
            // A payload parameter splices its raw text into a quoted script;
            // an unquoted bare scalar renders as script content it almost
            // certainly is not.
            if !argument.quoted
                && let Some((_, range)) = argument.scalar.as_ref()
            {
                diagnostics.push(Diagnostic::new(
                    DiagnosticCode::InvalidValue,
                    Severity::Warning,
                    *range,
                    format!(
                        "parameter `{}` of scripted `{}` is spliced into a quoted script payload; provide its value as a quoted script",
                        argument.key, row.name
                    ),
                ));
            }
            continue;
        }
        validate_quoted_script(
            ValidationState {
                snapshot,
                diagnostics: &mut *diagnostics,
                cancellation,
                quoted_scripts: &mut *quoted_scripts,
            },
            &row.context,
            &[],
            scope,
            argument,
            quoted_script_depth,
            // Scope authority for payload statements sits with the
            // definition-site contract, not this call-site walk.
            true,
        )?;
    }
    Ok(())
}

/// Resolves the derived dynamic row for one scripted invocation from the
/// matching rule rows' dynamic type.
fn dynamic_row_for_invocation(
    snapshot: &AnalysisSnapshot,
    rules: &[&pdx_rules::SemanticRule],
    invocation: &ScriptProperty,
) -> Option<crate::dynamic_rules::DynamicRuleRow> {
    let type_name = rules.iter().find_map(|rule| match &rule.key {
        KeyMatcher::Type(type_name) | KeyMatcher::Dynamic(type_name)
            if dynamic_definition_type(snapshot, type_name) =>
        {
            Some(type_name.as_str())
        }
        _ => None,
    })?;
    crate::dynamic_rules::dynamic_rule_row(snapshot, type_name, &invocation.key)
}

/// The sites constraining one parameter, following forwarding edges into
/// nested calls so a forwarded argument inherits the callee parameter's own
/// constraints. `visited` guards forwarding cycles.
pub(crate) fn effective_parameter_sites(
    snapshot: &AnalysisSnapshot,
    row: &crate::dynamic_rules::DynamicRuleRow,
    parameter: &crate::dynamic_rules::DynamicParameterRow,
    visited: &mut Vec<(String, String)>,
) -> Vec<Vec<pdx_rules::ValueMatcher>> {
    if !parameter.sites.is_empty() || parameter.forwarded_to.is_empty() {
        return parameter.sites.clone();
    }
    let identity = (row.kind.to_ascii_lowercase(), row.name.to_ascii_lowercase());
    if visited.contains(&identity) {
        return parameter.sites.clone();
    }
    visited.push(identity);
    let mut sites = parameter.sites.clone();
    for edge in &parameter.forwarded_to {
        let Some(callee_parameter_name) = &edge.parameter else {
            // Rendered-target edges are checked by
            // `validate_forwarded_to_any_parameter` instead.
            continue;
        };
        let Some(callee) = crate::dynamic_rules::dynamic_rule_row(snapshot, &edge.kind, &edge.name)
        else {
            continue;
        };
        let Some(callee_parameter) = callee
            .parameters
            .iter()
            .find(|candidate| candidate.name.eq_ignore_ascii_case(callee_parameter_name))
        else {
            continue;
        };
        sites.extend(effective_parameter_sites(
            snapshot,
            &callee,
            callee_parameter,
            visited,
        ));
    }
    sites
}

/// Renders the `$param$` keys of a dynamically dispatching scripted
/// definition with the caller's argument bindings and rejects bindings that
/// do not name a known key in the definition's body context. The check stays
/// string-level: no tree is instantiated and the body itself is validated at
/// its definition.
fn validate_dynamic_dispatch_keys(
    snapshot: &AnalysisSnapshot,
    rules: &[&pdx_rules::SemanticRule],
    property: &ScriptProperty,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if property.block_range.is_none() {
        return;
    }
    let Some(type_name) = rules.iter().find_map(|rule| match &rule.key {
        pdx_rules::KeyMatcher::Type(type_name) | pdx_rules::KeyMatcher::Dynamic(type_name)
            if dynamic_definition_type(snapshot, type_name) =>
        {
            Some(type_name.as_str())
        }
        _ => None,
    }) else {
        return;
    };
    let Some(row) = crate::dynamic_rules::dynamic_rule_row(snapshot, type_name, &property.key)
    else {
        return;
    };
    if !row.dispatches_dynamically {
        return;
    }
    let Some(resolved) = resolve_dynamic_definition(snapshot, type_name, &property.key) else {
        return;
    };
    let Some(template) = resolved.summary.template.as_ref() else {
        return;
    };
    let bindings = dynamic_cycles::scalar_argument_bindings(property);
    let mut walker = DispatchKeyWalker {
        snapshot,
        definition_name: row.name.clone(),
        bindings: &bindings,
        invocation: property,
        diagnostics,
    };
    walker.walk_items(&template.items, &resolved.body_context, true);
}

struct DispatchKeyWalker<'a> {
    snapshot: &'a AnalysisSnapshot,
    definition_name: String,
    bindings: &'a std::collections::BTreeMap<String, String>,
    invocation: &'a ScriptProperty,
    diagnostics: &'a mut Vec<Diagnostic>,
}

impl DispatchKeyWalker<'_> {
    fn walk_items(&mut self, items: &[TemplateItem], context: &str, key_checks: bool) {
        for item in items {
            match item {
                TemplateItem::Property(property) => {
                    if key_checks {
                        self.check_property(property, context);
                    }
                    if let TemplateValue::Block { items, .. } = &property.value {
                        if self.block_is_argument_list(property, context) {
                            // A nested dynamic-rule call's block assigns callee
                            // parameters (`helper = { AMOUNT = $X$ }`): its keys
                            // name parameters, they do not dispatch.
                            continue;
                        }
                        let lowered = self
                            .rendered_key(property)
                            .map(|key| key.to_ascii_lowercase());
                        if lowered.as_deref().is_some_and(is_display_only_block) {
                            continue;
                        }
                        let branch_keyed = lowered
                            .as_deref()
                            .is_some_and(is_value_keyed_branch_container);
                        let Some(child_context) = self.child_block_context(property, context)
                        else {
                            continue;
                        };
                        self.walk_items(items, &child_context, !branch_keyed);
                    }
                }
                TemplateItem::Conditional(conditional) => {
                    self.walk_items(&conditional.items, context, key_checks);
                }
                TemplateItem::BareValue(_) => {}
            }
        }
    }

    /// The context in which a container block's contents validate, or `None`
    /// when the walker should not descend: display-only blocks (`tooltip`)
    /// render their contents for the tooltip without executing them, and
    /// quoted-script payloads validate as quotes at their definition.
    fn child_block_context(&self, property: &TemplateProperty, context: &str) -> Option<String> {
        let rendered = self.rendered_key(property)?;
        let lowered = rendered.to_ascii_lowercase();
        if is_display_only_block(&lowered) {
            return None;
        }
        let rows = semantic_rules_for_container_key(self.snapshot, context, &[], &rendered);
        if rows
            .iter()
            .any(|rule| matches!(rule.shape, RuleShape::QuotedScript))
        {
            return None;
        }
        let switched = rows.iter().find_map(|rule| {
            rule.child_context
                .as_deref()
                .filter(|child| !child.eq_ignore_ascii_case(context))
                .map(str::to_owned)
        });
        Some(switch_child_context(&lowered, context, switched))
    }

    /// True when the property is a nested dynamic-rule call whose block is an
    /// argument list rather than statements. Covers literal callee names and
    /// callee names rendered from a fully-bound `$param$` key; an unbound or
    /// partially-rendered key makes the block unknowable, and unknowable
    /// blocks stay silent rather than report against a guessed target.
    fn block_is_argument_list(&self, property: &TemplateProperty, context: &str) -> bool {
        let Some(rendered) = self.rendered_key(property) else {
            return true;
        };
        crate::dynamic_rules::dynamic_kind_for_context(self.snapshot, context)
            .and_then(|kind| resolve_dynamic_definition(self.snapshot, &kind, &rendered))
            .is_some()
    }

    /// The property key with `$param$` fragments substituted from the
    /// invocation's bindings; `None` when a parameter is unbound or the
    /// rendered result still contains a parameter.
    fn rendered_key(&self, property: &TemplateProperty) -> Option<String> {
        let mut rendered = String::new();
        for fragment in &property.key.fragments {
            match fragment {
                TemplateFragment::Literal(literal) => rendered.push_str(literal),
                TemplateFragment::Parameter { name, .. } => {
                    let bound = self.bindings.get(&name.to_ascii_lowercase())?;
                    rendered.push_str(bound);
                }
            }
        }
        let rendered = rendered.trim();
        if rendered.is_empty() || rendered.contains('$') {
            return None;
        }
        Some(rendered.to_owned())
    }

    fn check_property(&mut self, property: &TemplateProperty, context: &str) {
        let fragments = &property.key.fragments;
        if !fragments
            .iter()
            .any(|fragment| matches!(fragment, TemplateFragment::Parameter { .. }))
        {
            return;
        }
        let mut rendered = String::new();
        let mut last_parameter = None;
        for fragment in fragments {
            match fragment {
                TemplateFragment::Literal(literal) => rendered.push_str(literal),
                TemplateFragment::Parameter { name, .. } => {
                    let Some(bound) = self.bindings.get(&name.to_ascii_lowercase()) else {
                        // An unbound optional parameter omits or defers the
                        // statement; the rendered key is unknowable.
                        return;
                    };
                    rendered.push_str(bound);
                    last_parameter = Some(name.clone());
                }
            }
        }
        let rendered = rendered.trim();
        if rendered.is_empty() || rendered.contains('$') {
            return;
        }
        if rendered.parse::<f64>().is_ok() {
            // Numeric keys are weights or id selectors (`random_list = { 0 = … }`),
            // never key lookups.
            return;
        }
        let lowered = rendered.to_ascii_lowercase();
        let is_scope_register = matches!(
            lowered.as_str(),
            "this" | "root" | "prev" | "from" | "fromfrom" | "fromfromfrom"
        ) || lowered.starts_with("event_target:")
            || lowered.starts_with("global_event_target:");
        if is_scope_register {
            return;
        }
        let known = semantic_rules_for_container_key(self.snapshot, context, &[], rendered)
            .iter()
            .any(|rule| semantic_rule_key_matches(self.snapshot, rule, &[], rendered));
        if known {
            return;
        }
        // Blame the argument that supplied the offending key fragment.
        let Some(parameter) = last_parameter else {
            return;
        };
        let Some((value, value_range)) = self.invocation.block.iter().find_map(|argument| {
            argument
                .key
                .eq_ignore_ascii_case(&parameter)
                .then(|| {
                    argument
                        .scalar
                        .as_ref()
                        .map(|(value, range)| (value.clone(), *range))
                })
                .flatten()
        }) else {
            return;
        };
        self.diagnostics.push(Diagnostic::new(
            DiagnosticCode::InvalidValue,
            DiagnosticCode::InvalidValue.severity(),
            value_range,
            format!(
                "argument `{}` for parameter `{parameter}` of scripted `{}` does not name a known {} key",
                value, self.definition_name, context
            ),
        ));
    }
}

/// Blocks whose contents render for display only; their keys reference real
/// effects but never execute, so a rendered key mismatch is cosmetic at worst.
fn is_display_only_block(lowered: &str) -> bool {
    matches!(lowered, "tooltip")
}

/// Containers whose branch keys are VALUES, not keys: `random`/`random_list`
/// branch on a rendered weight and `trigger_switch` branches on the
/// `on_trigger` value, so rendered branch keys never resolve through the
/// rule key index.
fn is_value_keyed_branch_container(lowered: &str) -> bool {
    matches!(lowered, "random" | "random_list" | "trigger_switch")
}

/// Resolves the walked child context from the row-declared switch, falling
/// back to the trigger context for structural sub-blocks (`limit`-style
/// gates) whose rows the current context does not declare.
fn switch_child_context(lowered: &str, context: &str, switched: Option<String>) -> String {
    switched.unwrap_or_else(|| {
        if crate::dynamic_rules::is_structural_sub_block(lowered)
            && !context.eq_ignore_ascii_case("trigger")
        {
            "trigger".to_owned()
        } else {
            context.to_owned()
        }
    })
}

/// Re-evaluates branch-local optionality from the persisted dynamic template. Vanilla dynamic
/// signatures can come from an older index cache whose compact `required` flags predate runtime
/// `if`/`else` branch awareness; the template remains sufficient to correct that metadata at the
/// call site without changing the cache schema.
fn dynamic_parameter_is_runtime_optional(summary: &DynamicDefinitionSummary, name: &str) -> bool {
    let Some(template) = summary.template.as_ref() else {
        return false;
    };
    template_parameter_runtime_optional(template, name)
}

fn template_parameter_runtime_optional(template: &Template, name: &str) -> bool {
    fn visit_token(
        token: &TemplateToken,
        name: &str,
        runtime_guarded: bool,
        seen: &mut bool,
        unguarded: &mut bool,
    ) {
        for fragment in &token.fragments {
            let TemplateFragment::Parameter {
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
        items: &[TemplateItem],
        name: &str,
        runtime_guarded: bool,
        seen: &mut bool,
        unguarded: &mut bool,
    ) {
        for item in items {
            match item {
                TemplateItem::Property(property) => {
                    let property_guarded = runtime_guarded && !is_limit_property(property);
                    visit_token(&property.key, name, property_guarded, seen, unguarded);
                    match &property.value {
                        TemplateValue::Scalar(token) => {
                            visit_token(token, name, property_guarded, seen, unguarded)
                        }
                        TemplateValue::Block { items, .. } => visit_items(
                            items,
                            name,
                            property_guarded || is_runtime_branch_key(&property.key),
                            seen,
                            unguarded,
                        ),
                    }
                }
                TemplateItem::BareValue(token) => {
                    visit_token(token, name, runtime_guarded, seen, unguarded);
                }
                TemplateItem::Conditional(conditional) => {
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

fn is_limit_property(property: &TemplateProperty) -> bool {
    let [TemplateFragment::Literal(key)] = property.key.fragments.as_slice() else {
        return false;
    };
    key.trim().eq_ignore_ascii_case("limit")
}

fn is_runtime_branch_key(key: &TemplateToken) -> bool {
    let [TemplateFragment::Literal(key)] = key.fragments.as_slice() else {
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
