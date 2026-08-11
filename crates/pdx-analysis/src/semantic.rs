use std::sync::Arc;

use pdx_engine::hir::semantic_root_context as hir_semantic_root_context;
use pdx_engine::hir::{HirFile, ScopeState, ScopeValue};
use pdx_engine::{
    AnalysisSnapshot, DocumentId, DocumentSource, MacroDefinitionSummary, MacroParameterSignature,
    SourceFileId,
};
use pdx_rules::{GameProfile, KeyMatcher, RuleShape, ValueMatcher};
use pdx_text::{LogicalPath, TextRange};

use crate::support::*;
use crate::types::*;

pub(crate) fn semantic_rules_for_container<'a>(
    snapshot: &'a AnalysisSnapshot,
    context: &str,
    parent_path: &[String],
    _scope: &ScopeContext,
) -> Vec<&'a pdx_rules::SemanticRule> {
    let mut candidates = snapshot
        .rules()
        .semantic_rules_for_context(context)
        .collect::<Vec<_>>();
    if let Some(type_name) = context.strip_prefix("type:") {
        candidates.extend(
            snapshot
                .rules()
                .semantic_rules_for_context(&format!("root:{type_name}")),
        );
        candidates.sort_by(|left, right| left.id.cmp(&right.id));
    }
    candidates
        .into_iter()
        .filter(|rule| semantic_parent_path_matches(snapshot, &rule.parent_path, parent_path))
        .collect()
}

pub(crate) fn semantic_initial_scope(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    context: &str,
    root_key: &str,
    key_range: TextRange,
) -> ScopeContext {
    if let Some(state) = input
        .hir
        .as_deref()
        .and_then(|hir| hir.scope_fact(key_range, context))
        .map(|fact| &fact.state)
    {
        return scope_context_from_hir(snapshot.game_profile_handle(), state);
    }
    let mut scope = ScopeContext::new(snapshot.game_profile_handle());
    if let Some(type_name) = context.strip_prefix("type:")
        && let Some(root_scope) = snapshot
            .rules()
            .model()
            .semantic
            .type_root_scopes
            .get(type_name)
            .and_then(|roots| roots.get(root_key))
    {
        scope.root.clone_from(root_scope);
        scope.current.clone_from(root_scope);
        return scope;
    }
    if let Some(root_scope) = snapshot.game_profile().root_scope(root_key) {
        scope.root = root_scope.to_owned();
        scope.current = root_scope.to_owned();
    }
    scope
}

pub(crate) fn scope_context_from_hir(
    profile: Arc<GameProfile>,
    state: &ScopeState,
) -> ScopeContext {
    fn spelling(value: &ScopeValue) -> String {
        match value {
            ScopeValue::Known(scopes) if scopes.len() == 1 => scopes[0].clone(),
            ScopeValue::Known(_) => "any".to_owned(),
            ScopeValue::Unknown => "any".to_owned(),
            ScopeValue::Invalid => "invalid".to_owned(),
        }
    }
    ScopeContext {
        profile,
        root: spelling(&state.root),
        current: state
            .current
            .first()
            .map_or_else(|| "any".to_owned(), spelling),
        from: state.from.iter().map(spelling).collect(),
        previous: state.previous.iter().map(spelling).collect(),
    }
}
pub(crate) fn semantic_root_context(
    snapshot: &AnalysisSnapshot,
    key: &str,
    logical_path: Option<&LogicalPath>,
) -> Option<String> {
    hir_semantic_root_context(snapshot.rules(), logical_path, key)
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn cached_scope_fact_for_property<'hir>(
    snapshot: &AnalysisSnapshot,
    hir: Option<&'hir HirFile>,
    context: &str,
    parent_path: &[String],
    property: &ScriptProperty,
    matching: &[&pdx_rules::SemanticRule],
    selected_alternative: Option<&str>,
    scope: &ScopeContext,
    transparent_wrapper: bool,
) -> Option<&'hir pdx_engine::hir::ScopeFact> {
    let fact = property
        .block
        .iter()
        .find_map(|child| hir.and_then(|hir| hir.scope_fact_at(child.key_range)))?;

    // HIR cannot inspect the workspace while lowering, so a cached dynamic transition is only
    // authoritative once analysis confirms the member. A missing index member is accepted only
    // when the first-party descriptor's negative/positive key filter proves it structurally.
    let mut transition_matching = matching.to_vec();
    if transition_matching.is_empty() {
        transition_matching = semantic_rules_for_container(snapshot, context, parent_path, scope)
            .into_iter()
            .filter(|rule| {
                !matches!(rule.shape, RuleShape::LeafValue)
                    && semantic_scope_allows(rule, scope)
                    && match &rule.key {
                        KeyMatcher::Type(type_name) => {
                            match workspace_type_member(snapshot, type_name, &property.key) {
                                WorkspaceTypeMember::Present => true,
                                WorkspaceTypeMember::Absent => false,
                                WorkspaceTypeMember::Unknown => {
                                    type_member_provably_valid(snapshot, type_name, &property.key)
                                }
                            }
                        }
                        _ => false,
                    }
            })
            .collect();
    }
    let selected = semantic_selected_transition(
        snapshot,
        &transition_matching,
        selected_alternative,
        context,
        parent_path,
        property,
        scope,
        transparent_wrapper,
    )?;
    let (expected_context, expected_path) = semantic_transition_destination(
        selected,
        context,
        parent_path,
        &property.key,
        transparent_wrapper,
    );
    (fact.context.eq_ignore_ascii_case(&expected_context)
        && fact.parent_path.len() == expected_path.len()
        && fact
            .parent_path
            .iter()
            .zip(expected_path)
            .all(|(actual, expected)| actual.eq_ignore_ascii_case(&expected)))
    .then_some(fact)
}
pub(crate) fn semantic_rule_is_selected(
    rule: &pdx_rules::SemanticRule,
    selected: Option<&str>,
) -> bool {
    rule.alternative_id
        .as_deref()
        .is_none_or(|alternative| selected == Some(alternative))
}

/// `required` is the declarative shorthand for one minimum occurrence.  Explicit cardinality
/// remains authoritative when both fields are present, which keeps generated rules backwards
/// compatible while making a standalone required field executable.
pub(crate) fn semantic_min_occurs(rule: &pdx_rules::SemanticRule) -> Option<u32> {
    rule.min_occurs.or(rule.required.then_some(1))
}

/// Alias-definition cardinality describes the fields inside one invocation. It must not be
/// applied to sibling invocations in the surrounding effect/trigger container.
pub(crate) fn semantic_rule_is_alias_definition(rule: &pdx_rules::SemanticRule) -> bool {
    rule.alternative_id.as_deref() == Some(rule.id.as_str())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn semantic_selected_transition<'rule>(
    snapshot: &AnalysisSnapshot,
    matching: &[&'rule pdx_rules::SemanticRule],
    selected_alternative: Option<&str>,
    context: &str,
    parent_path: &[String],
    property: &ScriptProperty,
    scope: &ScopeContext,
    transparent_wrapper: bool,
) -> Option<&'rule pdx_rules::SemanticRule> {
    let applicable =
        semantic_transition_candidates(matching, selected_alternative, property, scope);
    if semantic_transitions_equivalent(&applicable) {
        return applicable.first().copied();
    }
    if property.block.is_empty() && property.bare_values.is_empty() {
        return None;
    }

    let mut structural_path = parent_path.to_vec();
    if !transparent_wrapper {
        structural_path.push(property.key.clone());
    }
    let structural_rules = semantic_rules_for_container(snapshot, context, &structural_path, scope);
    let possible = applicable
        .iter()
        .copied()
        .filter(|candidate| {
            let (child_context, child_path) = semantic_transition_destination(
                candidate,
                context,
                parent_path,
                &property.key,
                transparent_wrapper,
            );
            let child_scope = semantic_child_scope(snapshot, scope, candidate);
            let child_rules =
                semantic_rules_for_container(snapshot, &child_context, &child_path, &child_scope);
            property.block.iter().all(|child| {
                structural_rules.iter().any(|rule| {
                    !matches!(rule.shape, RuleShape::LeafValue)
                        && semantic_rule_key_matches(snapshot, rule, &structural_path, &child.key)
                }) || child_rules.iter().any(|rule| {
                    !matches!(rule.shape, RuleShape::LeafValue)
                        && semantic_rule_key_matches(snapshot, rule, &child_path, &child.key)
                })
            }) && property.bare_values.iter().all(|(value, _)| {
                structural_rules.iter().any(|rule| {
                    matches!(rule.shape, RuleShape::LeafValue)
                        && semantic_leaf_value_matches(snapshot, rule, value, scope)
                }) || child_rules.iter().any(|rule| {
                    matches!(rule.shape, RuleShape::LeafValue)
                        && semantic_leaf_value_matches(snapshot, rule, value, &child_scope)
                })
            })
        })
        .collect::<Vec<_>>();
    if !possible.is_empty() && semantic_transitions_equivalent(&possible) {
        return possible.first().copied();
    }
    // No candidate covers every child. Keep the structural-path fallback so genuinely
    // unmatched children are reported against the parent context; alias definitions are
    // already preserved above, which keeps legitimate transitions like `if` selected.
    None
}

/// Returns the rules which may drive a property's semantic transition. `AnyScalar` is a
/// container-local fallback: once a concrete matcher accepts the key it must not compete with
/// that rule's destination. A quoted Script shape is likewise more specific than an ordinary
/// scalar leaf for the same quoted value. Keeping this policy in one helper prevents diagnostics,
/// completion and future embedded-language queries from choosing different containers.
pub(crate) fn semantic_transition_candidates<'rule>(
    matching: &[&'rule pdx_rules::SemanticRule],
    selected_alternative: Option<&str>,
    property: &ScriptProperty,
    scope: &ScopeContext,
) -> Vec<&'rule pdx_rules::SemanticRule> {
    let mut applicable = matching
        .iter()
        .copied()
        .filter(|rule| {
            // Alias definitions are concrete invocations, not competing alternatives, so
            // alternative selection must not discard them (e.g. `if` inside an effect).
            semantic_rule_is_alias_definition(rule)
                || selected_alternative.is_none()
                || semantic_rule_is_selected(rule, selected_alternative)
        })
        .filter(|rule| semantic_scope_allows(rule, scope))
        .filter(|rule| semantic_property_structure_matches(rule, property))
        .collect::<Vec<_>>();

    if applicable
        .iter()
        .any(|rule| !matches!(rule.key, KeyMatcher::AnyScalar))
    {
        applicable.retain(|rule| !matches!(rule.key, KeyMatcher::AnyScalar));
    }

    if property.quoted
        && applicable
            .iter()
            .any(|rule| matches!(rule.shape, RuleShape::QuotedScript))
    {
        applicable.retain(|rule| matches!(rule.shape, RuleShape::QuotedScript));
    }
    applicable
}

pub(crate) fn semantic_transition_destination(
    rule: &pdx_rules::SemanticRule,
    context: &str,
    parent_path: &[String],
    property_key: &str,
    transparent_wrapper: bool,
) -> (String, Vec<String>) {
    rule.child_context.as_deref().map_or_else(
        || {
            let mut child_path = parent_path.to_vec();
            if !transparent_wrapper {
                child_path.push(property_key.to_owned());
            }
            (context.to_owned(), child_path)
        },
        |child_context| (child_context.to_owned(), Vec::new()),
    )
}

pub(crate) fn semantic_transitions_equivalent(rules: &[&pdx_rules::SemanticRule]) -> bool {
    let Some(first) = rules.first() else {
        return false;
    };
    rules.iter().all(|candidate| {
        semantic_optional_text_eq(
            first.child_context.as_deref(),
            candidate.child_context.as_deref(),
        ) && semantic_optional_text_eq(first.push_scope.as_deref(), candidate.push_scope.as_deref())
            && first.replace_scope.len() == candidate.replace_scope.len()
            && first
                .replace_scope
                .iter()
                .all(|(left_register, left_scope)| {
                    candidate
                        .replace_scope
                        .iter()
                        .any(|(right_register, right_scope)| {
                            left_register.eq_ignore_ascii_case(right_register)
                                && left_scope.eq_ignore_ascii_case(right_scope)
                        })
                })
            && candidate
                .replace_scope
                .iter()
                .all(|(right_register, right_scope)| {
                    first
                        .replace_scope
                        .iter()
                        .any(|(left_register, left_scope)| {
                            left_register.eq_ignore_ascii_case(right_register)
                                && left_scope.eq_ignore_ascii_case(right_scope)
                        })
                })
    })
}

pub(crate) fn semantic_optional_text_eq(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left.eq_ignore_ascii_case(right),
        (None, None) => true,
        _ => false,
    }
}

pub(crate) fn semantic_selected_alternative(
    snapshot: &AnalysisSnapshot,
    rules: &[&pdx_rules::SemanticRule],
    parent_path: &[String],
    properties: &[ScriptProperty],
    bare_values: &[(String, TextRange)],
    scope: &ScopeContext,
) -> Option<String> {
    let mut alternatives = Vec::<String>::new();
    for rule in rules {
        if let Some(alternative) = rule.alternative_id.as_ref()
            && !alternatives.iter().any(|known| known == alternative)
        {
            alternatives.push(alternative.clone());
        }
    }
    let mut best: Option<((usize, usize), String)> = None;
    let mut tied = false;
    for alternative in alternatives {
        let group = rules
            .iter()
            .filter(|rule| rule.alternative_id.as_deref() == Some(alternative.as_str()))
            .copied()
            .collect::<Vec<_>>();
        let mut present = 0_usize;
        let mut valid = 0_usize;
        for property in properties {
            let matching = group.iter().filter(|rule| {
                !matches!(rule.shape, RuleShape::LeafValue)
                    && semantic_rule_key_matches(snapshot, rule, parent_path, &property.key)
            });
            if matching.clone().next().is_some() {
                present += 1;
            }
            if matching
                .filter(|rule| semantic_scope_allows(rule, scope))
                .any(|rule| semantic_property_matches(snapshot, rule, property, scope))
            {
                valid += 1;
            }
        }
        valid += bare_values
            .iter()
            .filter(|(value, _)| {
                group.iter().any(|rule| {
                    matches!(rule.shape, RuleShape::LeafValue)
                        && semantic_leaf_value_matches(snapshot, rule, value, scope)
                })
            })
            .count();
        let score = (valid, present);
        match best.as_ref() {
            None => {
                best = Some((score, alternative));
                tied = false;
            }
            Some((current, _)) if score > *current => {
                best = Some((score, alternative));
                tied = false;
            }
            Some((current, _)) if score == *current => tied = true,
            Some(_) => {}
        }
    }
    if tied {
        None
    } else {
        best.map(|(_, alternative)| alternative)
    }
}

pub(crate) fn semantic_leaf_value_matches(
    snapshot: &AnalysisSnapshot,
    rule: &pdx_rules::SemanticRule,
    value: &str,
    scope: &ScopeContext,
) -> bool {
    match &rule.value {
        ValueMatcher::Dynamic(kind) => semantic_dynamic_value_matches(snapshot, kind, value, scope),
        ValueMatcher::DynamicSet(_) => !value.is_empty(),
        matcher => matcher.matches(
            value,
            |type_name, member| workspace_member(snapshot, type_name, member),
            |enum_name, member| enum_member(snapshot, enum_name, member),
            |scope_name, member| scope_member(snapshot, scope_name, member, scope),
        ),
    }
}

pub(crate) fn semantic_value_matcher_label(matcher: &ValueMatcher) -> String {
    match matcher {
        ValueMatcher::AnyScalar => "scalar".to_owned(),
        ValueMatcher::Exact(value) => value.clone(),
        ValueMatcher::Bool => "bool".to_owned(),
        ValueMatcher::Int { .. } => "int".to_owned(),
        ValueMatcher::Float { .. } => "float".to_owned(),
        ValueMatcher::Date => "date".to_owned(),
        ValueMatcher::Type(value) => format!("<{value}>"),
        ValueMatcher::Enum(value) => format!("enum[{value}]"),
        ValueMatcher::Scope(value) => value
            .as_deref()
            .map_or_else(|| "scope".to_owned(), |value| format!("scope[{value}]")),
        ValueMatcher::Localisation => "localisation".to_owned(),
        ValueMatcher::Filepath => "filepath".to_owned(),
        ValueMatcher::Dynamic(value) => format!("value[{value}]"),
        ValueMatcher::DynamicSet(value) => format!("value_set[{value}]"),
        ValueMatcher::Opaque(value) => value.clone(),
    }
}

pub(crate) fn semantic_rule_provenance(rule: &pdx_rules::SemanticRule) -> String {
    format!("rule {}:{}", rule.source_file, rule.line)
}

pub(crate) fn semantic_matcher_label(matcher: &KeyMatcher) -> String {
    match matcher {
        KeyMatcher::Exact(value) => value.clone(),
        KeyMatcher::Type(value) => format!("<{value}>"),
        KeyMatcher::Enum(value) => format!("enum[{value}]"),
        KeyMatcher::AnyScalar => "scalar".to_owned(),
        KeyMatcher::Dynamic(value) => format!("value_set[{value}]"),
    }
}

pub(crate) fn semantic_parent_path_matches(
    snapshot: &AnalysisSnapshot,
    expected: &[String],
    actual: &[String],
) -> bool {
    expected.len() == actual.len()
        && expected.iter().zip(actual).all(|(expected, actual)| {
            if let Some(type_name) = expected
                .strip_prefix('<')
                .and_then(|name| name.strip_suffix('>'))
            {
                workspace_member(snapshot, type_name, actual)
            } else if let Some(enum_name) = expected
                .strip_prefix("enum[")
                .and_then(|name| name.strip_suffix(']'))
            {
                enum_member(snapshot, enum_name, actual)
            } else {
                expected.eq_ignore_ascii_case(actual)
            }
        })
}

pub(crate) fn semantic_rule_severity<'a>(
    rules: impl IntoIterator<Item = &'a pdx_rules::SemanticRule>,
    fallback: DiagnosticCode,
) -> u8 {
    rules
        .into_iter()
        .filter_map(|rule| rule.severity)
        .min()
        .unwrap_or_else(|| fallback.severity())
}

pub(crate) fn semantic_min_cardinality_severity(rule: &pdx_rules::SemanticRule) -> u8 {
    if !rule.strict_min {
        2
    } else {
        rule.severity
            .unwrap_or(DiagnosticCode::Cardinality.severity())
    }
}

pub(crate) fn semantic_scope_allows(rule: &pdx_rules::SemanticRule, scope: &ScopeContext) -> bool {
    rule.allowed_scopes.is_empty()
        || rule
            .allowed_scopes
            .iter()
            .any(|expected| scope.profile.scopes_compatible(&scope.current, expected))
}

pub(crate) fn semantic_child_scope(
    snapshot: &AnalysisSnapshot,
    parent: &ScopeContext,
    rule: &pdx_rules::SemanticRule,
) -> ScopeContext {
    let mut child = parent.clone();
    if let Some(push_scope) = &rule.push_scope
        && !push_scope.eq_ignore_ascii_case("any")
    {
        child.previous.insert(0, child.current.clone());
        child.current.clone_from(push_scope);
    }
    for (register, value) in &rule.replace_scope {
        let value = resolve_scope_expression_context(snapshot, &child, value);
        let register = register.to_ascii_lowercase().replace('_', "");
        match register.as_str() {
            "root" => child.root = value,
            "this" => child.current = value,
            _ => {
                if let Some(depth) = repeated_scope_register_depth(&register, "from") {
                    set_scope_register(&mut child.from, depth, &value);
                } else if let Some(depth) = repeated_scope_register_depth(&register, "previous")
                    .or_else(|| repeated_scope_register_depth(&register, "prev"))
                {
                    set_scope_register(&mut child.previous, depth, &value);
                }
            }
        }
    }
    child
}

pub(crate) fn resolve_scope_expression_context(
    snapshot: &AnalysisSnapshot,
    context: &ScopeContext,
    expression: &str,
) -> String {
    if expression.contains('.') {
        let mut segments = expression.split('.');
        let Some(first) = segments.next() else {
            return "any".to_owned();
        };
        let mut value = resolve_scope_expression_context(snapshot, context, first);
        for segment in segments {
            value = resolve_scope_link_context(snapshot, context, &value, segment)
                .unwrap_or_else(|| "any".to_owned());
            if value.eq_ignore_ascii_case("any") {
                break;
            }
        }
        return value;
    }

    let lowered = expression.to_ascii_lowercase().replace('_', "");
    if lowered == "root" {
        return context.root.clone();
    }
    if lowered == "this" {
        return context.current.clone();
    }
    if let Some(depth) = repeated_scope_register_depth(&lowered, "from") {
        return context
            .from
            .get(depth)
            .cloned()
            .unwrap_or_else(|| "any".to_owned());
    }
    if let Some(depth) = repeated_scope_register_depth(&lowered, "previous")
        .or_else(|| repeated_scope_register_depth(&lowered, "prev"))
    {
        return context
            .previous
            .get(depth)
            .cloned()
            .unwrap_or_else(|| "any".to_owned());
    }

    let link_expression = snapshot
        .rules()
        .exact_semantic_rules(expression)
        .any(|rule| {
            matches!(
                rule.context.to_ascii_lowercase().as_str(),
                "effect" | "trigger"
            ) && rule.push_scope.is_some()
        });
    if let Some(target) =
        resolve_scope_link_context(snapshot, context, &context.current, expression)
    {
        return target;
    }
    if expression.eq_ignore_ascii_case("any") || link_expression {
        "any".to_owned()
    } else if context.profile.is_scope(expression) {
        expression.to_owned()
    } else {
        "any".to_owned()
    }
}

pub(crate) fn resolve_scope_link_context(
    snapshot: &AnalysisSnapshot,
    context: &ScopeContext,
    current: &str,
    expression: &str,
) -> Option<String> {
    let mut targets = snapshot
        .rules()
        .exact_semantic_rules(expression)
        .filter_map(|rule| {
            if !matches!(
                rule.context.to_ascii_lowercase().as_str(),
                "effect" | "trigger"
            ) || !rule.allowed_scopes.is_empty()
                && !rule
                    .allowed_scopes
                    .iter()
                    .any(|expected| context.profile.scopes_compatible(current, expected))
            {
                return None;
            }
            rule.push_scope
                .as_deref()
                .filter(|target| !target.eq_ignore_ascii_case("any"))
                .map(str::to_owned)
        })
        .collect::<Vec<_>>();
    targets.sort_by_key(|target| target.to_ascii_lowercase());
    targets.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    if targets.len() == 1 {
        Some(targets.remove(0))
    } else {
        None
    }
}

pub(crate) fn set_scope_register(registers: &mut Vec<String>, depth: usize, value: &str) {
    if registers.len() <= depth {
        registers.resize(depth + 1, "any".to_owned());
    }
    registers[depth] = value.to_owned();
}

pub(crate) fn repeated_scope_register_depth(value: &str, token: &str) -> Option<usize> {
    let count = value.len().checked_div(token.len())?;
    if count > 0 && token.repeat(count) == value {
        Some(count - 1)
    } else {
        None
    }
}

pub(crate) fn semantic_rule_key_matches(
    snapshot: &AnalysisSnapshot,
    rule: &pdx_rules::SemanticRule,
    parent_path: &[String],
    key: &str,
) -> bool {
    match qualified_parameter_domain(snapshot, rule, parent_path) {
        QualifiedParameterDomain::NotApplicable => semantic_key_matches(snapshot, &rule.key, key),
        QualifiedParameterDomain::Known(names) => {
            names.iter().any(|name| name.eq_ignore_ascii_case(key))
        }
        QualifiedParameterDomain::OpenWorld => !key.is_empty(),
    }
}

pub(crate) enum QualifiedParameterDomain {
    NotApplicable,
    Known(Vec<String>),
    OpenWorld,
}

pub(crate) fn qualified_parameter_domain(
    snapshot: &AnalysisSnapshot,
    rule: &pdx_rules::SemanticRule,
    parent_path: &[String],
) -> QualifiedParameterDomain {
    let KeyMatcher::Enum(enum_name) = &rule.key else {
        return QualifiedParameterDomain::NotApplicable;
    };
    if !enum_name.eq_ignore_ascii_case("scripted_effect_params") {
        return QualifiedParameterDomain::NotApplicable;
    }
    let owner_kind = rule
        .parent_path
        .last()
        .and_then(|segment| segment.strip_prefix('<'))
        .and_then(|segment| segment.strip_suffix('>'));
    let Some(owner_kind) = owner_kind else {
        return QualifiedParameterDomain::NotApplicable;
    };
    if !matches!(
        owner_kind.to_ascii_lowercase().as_str(),
        "scripted_effect" | "scripted_trigger"
    ) {
        return QualifiedParameterDomain::NotApplicable;
    }
    let Some(owner_name) = parent_path.last() else {
        return QualifiedParameterDomain::OpenWorld;
    };
    parameter_names_for_owner(snapshot, owner_kind, owner_name).map_or(
        QualifiedParameterDomain::OpenWorld,
        QualifiedParameterDomain::Known,
    )
}

pub(crate) fn parameter_names_for_owner(
    snapshot: &AnalysisSnapshot,
    owner_kind: &str,
    owner_name: &str,
) -> Option<Vec<String>> {
    macro_definition_summary(snapshot, owner_kind, owner_name).map(|summary| {
        summary
            .parameters
            .into_iter()
            .map(|parameter| parameter.name)
            .collect()
    })
}

pub(crate) fn macro_definition_summary(
    snapshot: &AnalysisSnapshot,
    owner_kind: &str,
    owner_name: &str,
) -> Option<MacroDefinitionSummary> {
    resolve_macro_definition(snapshot, owner_kind, owner_name).map(|resolved| resolved.summary)
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum MacroDefinitionIdentity {
    Overlay {
        document: DocumentId,
        version: Option<i64>,
        definition_range: TextRange,
    },
    File {
        file: SourceFileId,
        revision: u64,
        definition_range: TextRange,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedMacroDefinition {
    pub(crate) identity: MacroDefinitionIdentity,
    pub(crate) summary: MacroDefinitionSummary,
    pub(crate) body_context: String,
}

pub(crate) fn resolve_macro_definition(
    snapshot: &AnalysisSnapshot,
    owner_kind: &str,
    owner_name: &str,
) -> Option<ResolvedMacroDefinition> {
    let body_context = snapshot
        .rules()
        .model()
        .semantic
        .type_descriptors
        .iter()
        .find(|(kind, _)| kind.eq_ignore_ascii_case(owner_kind))
        .and_then(|(_, descriptor)| descriptor.scripted_macro.as_ref())
        .filter(|descriptor| descriptor.macro_enabled)?
        .body_context
        .clone();
    let mut overlay_candidates = Vec::new();
    for document in snapshot
        .documents()
        .values()
        .filter(|document| document.source() == DocumentSource::Overlay)
    {
        let Some(hir) = document.hir_handle() else {
            continue;
        };
        for definition in hir.definitions().iter().filter(|definition| {
            definition.kind.eq_ignore_ascii_case(owner_kind)
                && definition.name.eq_ignore_ascii_case(owner_name)
        }) {
            overlay_candidates.push(ResolvedMacroDefinition {
                identity: MacroDefinitionIdentity::Overlay {
                    document: document.id().clone(),
                    version: document.version(),
                    definition_range: definition.range,
                },
                summary: macro_summary_in_hir(
                    &hir,
                    &definition.kind,
                    &definition.name,
                    definition.range,
                ),
                body_context: body_context.clone(),
            });
        }
    }
    if overlay_candidates.len() > 1 {
        return None;
    }
    if let Some(resolved) = overlay_candidates.pop() {
        return Some(resolved);
    }

    let definition = snapshot.index().active_definition(owner_kind, owner_name)?;
    let source_file = snapshot.source_files().get(&definition.file_id)?;
    let hidden_by_overlay = snapshot.documents().values().any(|document| {
        document.source() == DocumentSource::Overlay
            && document
                .path()
                .is_some_and(|path| path == source_file.physical_path)
    });
    if hidden_by_overlay {
        return None;
    }
    let summary = snapshot
        .index()
        .active_macro_definition(owner_kind, owner_name)
        .cloned()?;
    let state = snapshot.file_state(definition.file_id);
    Some(ResolvedMacroDefinition {
        identity: MacroDefinitionIdentity::File {
            file: definition.file_id,
            revision: state.map_or(0, pdx_engine::FileState::revision),
            definition_range: definition.range,
        },
        summary,
        body_context,
    })
}

fn macro_summary_in_hir(
    hir: &HirFile,
    kind: &str,
    name: &str,
    owner_range: TextRange,
) -> MacroDefinitionSummary {
    let parameters = hir
        .parameter_definitions_for_owner(owner_range)
        .map(|definition| MacroParameterSignature {
            name: definition.name.clone(),
            required: hir.parameter_is_required(owner_range, &definition.name),
        })
        .collect();
    MacroDefinitionSummary {
        kind: kind.to_owned(),
        name: name.to_owned(),
        definition_range: owner_range,
        parameters,
        template: hir.macro_template(kind, name, owner_range).cloned(),
    }
}

/// Builds the canonical invocation snippet for a resolved scripted macro signature.
pub(crate) fn scripted_definition_snippet(
    snapshot: &AnalysisSnapshot,
    kind_name: &str,
    definition_name: &str,
    base_indent: &str,
) -> String {
    let Some(summary) = macro_definition_summary(snapshot, kind_name, definition_name) else {
        let inner_indent = format!("{base_indent}\t");
        return format!("{definition_name} = {{\n{inner_indent}$0\n{base_indent}}}");
    };
    if summary.parameters.is_empty() {
        return format!("{definition_name} = yes");
    }
    let inner_indent = format!("{base_indent}\t");
    let mut body = String::new();
    for (index, parameter) in summary
        .parameters
        .iter()
        .filter(|parameter| parameter.required)
        .enumerate()
    {
        body.push_str(&format!(
            "{inner_indent}{} = ${}\n",
            parameter.name,
            index + 1
        ));
    }
    format!("{definition_name} = {{\n{body}{inner_indent}$0\n{base_indent}}}")
}

pub(crate) fn semantic_key_matches(
    snapshot: &AnalysisSnapshot,
    matcher: &KeyMatcher,
    key: &str,
) -> bool {
    matcher.matches(
        key,
        |type_name, member| workspace_member(snapshot, type_name, member),
        |enum_name, member| enum_member(snapshot, enum_name, member),
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WorkspaceTypeMember {
    /// The workspace has a definition for this type member.
    Present,
    /// The workspace has definitions for the type, but not this member.
    Absent,
    /// No definition for the type is indexed yet; keep the conservative open-world fallback.
    Unknown,
}

pub(crate) fn workspace_type_member(
    snapshot: &AnalysisSnapshot,
    type_name: &str,
    member: &str,
) -> WorkspaceTypeMember {
    if workspace_member(snapshot, type_name, member) {
        WorkspaceTypeMember::Present
    } else if workspace_kind_has_members(snapshot, type_name) {
        WorkspaceTypeMember::Absent
    } else {
        WorkspaceTypeMember::Unknown
    }
}

pub(crate) fn type_member_provably_valid(
    snapshot: &AnalysisSnapshot,
    type_name: &str,
    key: &str,
) -> bool {
    snapshot
        .rules()
        .model()
        .semantic
        .type_descriptors
        .get(type_name)
        .and_then(|descriptor| descriptor.type_key_filter.as_ref())
        .is_some_and(|(values, negate)| {
            values.iter().any(|value| value.eq_ignore_ascii_case(key)) != *negate
        })
}

pub(crate) fn semantic_property_matches(
    snapshot: &AnalysisSnapshot,
    rule: &pdx_rules::SemanticRule,
    property: &ScriptProperty,
    scope_context: &ScopeContext,
) -> bool {
    if let Some(matches) = scripted_macro_call_shape_matches(snapshot, rule, property) {
        return matches;
    }
    if !semantic_property_structure_matches(rule, property) {
        return false;
    }
    let Some((value, _)) = property.scalar.as_ref() else {
        return matches!(
            rule.value,
            ValueMatcher::AnyScalar | ValueMatcher::Opaque(_)
        );
    };
    if let ValueMatcher::Dynamic(kind) = &rule.value {
        return semantic_dynamic_value_matches(snapshot, kind, value, scope_context);
    }
    if let ValueMatcher::DynamicSet(_) = &rule.value {
        return !value.is_empty();
    }
    rule.value.matches(
        value,
        |type_name, member| workspace_member(snapshot, type_name, member),
        |enum_name, member| enum_member(snapshot, enum_name, member),
        |scope, member| scope_member(snapshot, scope, member, scope_context),
    ) || (matches!(
        rule.value,
        ValueMatcher::Int { .. } | ValueMatcher::Float { .. }
    ) && scope_member(snapshot, None, value, scope_context))
}

/// Returns whether a property satisfies the rule shape and operator independently of its scalar
/// spelling. Macro definition placeholders use this to defer only the binding-dependent matcher.
pub(crate) fn semantic_property_structure_matches(
    rule: &pdx_rules::SemanticRule,
    property: &ScriptProperty,
) -> bool {
    let shape_matches = match rule.shape {
        RuleShape::Node => property.block_range.is_some(),
        RuleShape::QuotedScript => property.quoted && property.scalar.is_some(),
        RuleShape::ValueClause => {
            property.block_range.is_some() && !property.bare_values.is_empty()
        }
        RuleShape::Leaf | RuleShape::LeafValue => property.scalar.is_some(),
    };
    if !shape_matches {
        return false;
    }
    if rule
        .operator
        .as_deref()
        .is_some_and(|operator| property.operator.as_deref() != Some(operator))
    {
        return false;
    }
    true
}

fn scripted_macro_call_shape_matches(
    snapshot: &AnalysisSnapshot,
    rule: &pdx_rules::SemanticRule,
    property: &ScriptProperty,
) -> Option<bool> {
    let type_name = match &rule.key {
        KeyMatcher::Type(type_name) | KeyMatcher::Dynamic(type_name)
            if scripted_macro_type(snapshot, type_name) =>
        {
            type_name
        }
        _ => return None,
    };
    let summary = macro_definition_summary(snapshot, type_name, &property.key)?;
    Some(scripted_macro_invocation_shape_matches(
        snapshot,
        type_name,
        &summary,
        property.scalar.as_ref().map(|(value, _)| value.as_str()),
        property.block_range.is_some(),
    ))
}

pub(crate) fn scripted_macro_invocation_shape_matches(
    snapshot: &AnalysisSnapshot,
    type_name: &str,
    summary: &MacroDefinitionSummary,
    scalar: Option<&str>,
    is_block: bool,
) -> bool {
    if is_block {
        return true;
    }
    let Some(value) = scalar else {
        return false;
    };
    match summary.parameters.len() {
        0 => {
            let body_context = snapshot
                .rules()
                .model()
                .semantic
                .type_descriptors
                .iter()
                .find(|(kind, _)| kind.eq_ignore_ascii_case(type_name))
                .and_then(|(_, descriptor)| descriptor.scripted_macro.as_ref())
                .map(|descriptor| descriptor.body_context.as_str());
            if body_context.is_some_and(|context| context.eq_ignore_ascii_case("trigger")) {
                value.eq_ignore_ascii_case("yes") || value.eq_ignore_ascii_case("no")
            } else {
                value.eq_ignore_ascii_case("yes")
            }
        }
        _ => false,
    }
}

pub(crate) fn semantic_dynamic_value_matches(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    value: &str,
    scope_context: &ScopeContext,
) -> bool {
    let kind = kind.to_ascii_lowercase();
    if kind == "scope_field" {
        return scope_member(snapshot, None, value, scope_context)
            || workspace_member(snapshot, "variable_name", value);
    }
    if kind == "variable" {
        return value.parse::<f64>().is_ok()
            || value.starts_with('$')
            || workspace_member(snapshot, "variable", value)
            || workspace_member(snapshot, "variable_name", value);
    }
    if kind == "value" {
        return value.parse::<f64>().is_ok()
            || value.starts_with('$')
            || workspace_member(snapshot, "variable", value)
            || workspace_member(snapshot, "variable_name", value);
    }
    if value.starts_with('$') && value.ends_with('$') {
        return true;
    }
    enum_member(snapshot, &kind, value) || workspace_member(snapshot, &kind, value)
}

fn workspace_member_kinds(snapshot: &AnalysisSnapshot, type_name: &str) -> Vec<String> {
    let base = type_name
        .split_once('.')
        .map_or(type_name, |(kind, _)| kind);
    let mut kinds = vec![type_name.to_owned(), base.to_owned()];
    if let Some(alias) = snapshot.game_profile().member_kind_alias(base) {
        kinds.push(alias.to_owned());
    }
    kinds.sort_by_key(|kind| kind.to_ascii_lowercase());
    kinds.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    kinds
}

/// Returns members visible at this snapshot, with open overlays replacing their backing files.
/// Macro names intentionally come from definitions, not from a static Vanilla name list.
pub(crate) fn effective_workspace_member_names(
    snapshot: &AnalysisSnapshot,
    type_name: &str,
) -> Vec<String> {
    let hidden_files = overlay_file_ids(snapshot);
    let kinds = workspace_member_kinds(snapshot, type_name);
    let mut names = Vec::new();
    for kind in &kinds {
        names.extend(
            snapshot
                .index()
                .definitions_for_kind(kind)
                .filter(|definition| {
                    definition.active && !hidden_files.contains(&definition.file_id)
                })
                .map(|definition| definition.name.clone()),
        );
    }
    for document in snapshot
        .documents()
        .values()
        .filter(|document| document.source() == DocumentSource::Overlay)
    {
        let Some(hir) = document.hir_handle() else {
            continue;
        };
        names.extend(
            hir.definitions()
                .iter()
                .filter(|definition| {
                    kinds
                        .iter()
                        .any(|kind| definition.kind.eq_ignore_ascii_case(kind))
                })
                .map(|definition| definition.name.clone()),
        );
    }
    names.sort_by_key(|name| name.to_ascii_lowercase());
    names.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    names
}

pub(crate) fn scripted_macro_type(snapshot: &AnalysisSnapshot, type_name: &str) -> bool {
    snapshot
        .rules()
        .model()
        .semantic
        .type_descriptors
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(type_name))
        .and_then(|(_, descriptor)| descriptor.scripted_macro.as_ref())
        .is_some_and(|descriptor| descriptor.macro_enabled)
}

pub(crate) fn workspace_member(snapshot: &AnalysisSnapshot, type_name: &str, member: &str) -> bool {
    let hidden_files = overlay_file_ids(snapshot);
    let kinds = workspace_member_kinds(snapshot, type_name);
    if kinds.iter().any(|kind| {
        snapshot
            .index()
            .definitions(kind, member)
            .into_iter()
            .any(|definition| definition.active && !hidden_files.contains(&definition.file_id))
    }) {
        return true;
    }
    snapshot
        .documents()
        .values()
        .filter(|document| document.source() == DocumentSource::Overlay)
        .filter_map(|document| document.hir_handle())
        .any(|hir| {
            hir.definitions().iter().any(|definition| {
                definition.name.eq_ignore_ascii_case(member)
                    && kinds
                        .iter()
                        .any(|kind| definition.kind.eq_ignore_ascii_case(kind))
            })
        })
}

fn workspace_kind_has_members(snapshot: &AnalysisSnapshot, type_name: &str) -> bool {
    let hidden_files = overlay_file_ids(snapshot);
    let kinds = workspace_member_kinds(snapshot, type_name);
    if kinds.iter().any(|kind| {
        snapshot
            .index()
            .definitions_for_kind(kind)
            .any(|definition| definition.active && !hidden_files.contains(&definition.file_id))
    }) {
        return true;
    }
    snapshot
        .documents()
        .values()
        .filter(|document| document.source() == DocumentSource::Overlay)
        .filter_map(|document| document.hir_handle())
        .any(|hir| {
            hir.definitions().iter().any(|definition| {
                kinds
                    .iter()
                    .any(|kind| definition.kind.eq_ignore_ascii_case(kind))
            })
        })
}

pub(crate) fn enum_member(snapshot: &AnalysisSnapshot, enum_name: &str, member: &str) -> bool {
    let static_member = snapshot
        .rules()
        .model()
        .semantic
        .enum_values
        .get(enum_name)
        .is_some_and(|values| {
            values
                .iter()
                .any(|value| value.eq_ignore_ascii_case(member))
        });
    static_member
        || snapshot.game_profile().enum_extra_member(enum_name, member)
        || snapshot
            .game_profile()
            .member_kind_alias(enum_name)
            .is_some_and(|kind| workspace_member(snapshot, kind, member))
        || workspace_member(snapshot, enum_name, member)
}

pub(crate) fn scope_member(
    snapshot: &AnalysisSnapshot,
    scope: Option<&str>,
    member: &str,
    context: &ScopeContext,
) -> bool {
    let lowered = member.to_ascii_lowercase().replace('_', "");
    let resolved = if lowered == "root" {
        Some(context.root.as_str())
    } else if lowered == "this" {
        Some(context.current.as_str())
    } else if let Some(depth) = repeated_scope_register_depth(&lowered, "from") {
        context.from.get(depth).map(String::as_str)
    } else if let Some(depth) = repeated_scope_register_depth(&lowered, "previous")
        .or_else(|| repeated_scope_register_depth(&lowered, "prev"))
    {
        context.previous.get(depth).map(String::as_str)
    } else {
        Some(member)
    };
    let Some(resolved) = resolved else {
        // FROM/PREV registers are not tracked in every context; an unknown register is
        // a legitimate scope reference that the analysis cannot disprove.
        return true;
    };
    if !context.profile.is_scope(resolved) {
        // Country tags are valid scope references, e.g. `who = TRP`.
        return workspace_member(snapshot, "country_tag", member);
    }
    scope.is_none_or(|expected| context.profile.scopes_compatible(resolved, expected))
}
