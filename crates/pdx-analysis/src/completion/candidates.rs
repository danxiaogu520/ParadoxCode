use std::collections::{BTreeMap, HashSet};

use crate::semantic::*;
use crate::support::*;
use crate::types::*;
use pdx_engine::AnalysisSnapshot;
use pdx_rules::{KeyMatcher, RuleShape, ValueMatcher};
use pdx_text::TextRange;

use super::context::SemanticCompletionContext;
use super::macro_constraints::infer_macro_value_constraints;
use super::support::{completion_sort_score, push_completion};

pub(crate) struct SemanticCompletionRule<'rule, 'path> {
    pub(crate) rule: &'rule pdx_rules::SemanticRule,
    pub(crate) parent_path: &'path [String],
    pub(crate) scope: &'path ScopeContext,
}

#[derive(Default)]
pub(crate) struct CompletionMemberCache {
    pub(crate) workspace: BTreeMap<(String, String), Vec<String>>,
    pub(crate) enums: BTreeMap<(String, String), Vec<String>>,
}

impl CompletionMemberCache {
    fn workspace_member_names(
        &mut self,
        snapshot: &AnalysisSnapshot,
        type_name: &str,
        prefix: &str,
    ) -> &[String] {
        let cache_key = (type_name.to_ascii_lowercase(), prefix.to_ascii_lowercase());
        self.workspace.entry(cache_key).or_insert_with(|| {
            let mut names = effective_workspace_member_names(snapshot, type_name)
                .into_iter()
                .filter(|name| completion_matches(name, prefix))
                .collect::<Vec<_>>();
            names.sort_by_key(|name| name.to_ascii_lowercase());
            names.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
            names
        })
    }

    fn enum_member_names(
        &mut self,
        snapshot: &AnalysisSnapshot,
        enum_name: &str,
        prefix: &str,
    ) -> &[String] {
        let cache_key = (enum_name.to_ascii_lowercase(), prefix.to_ascii_lowercase());
        if !self.enums.contains_key(&cache_key) {
            let mut names = snapshot
                .rules()
                .model()
                .semantic
                .enum_values
                .get(enum_name)
                .cloned()
                .unwrap_or_default();
            names.extend(
                self.workspace_member_names(snapshot, enum_name, prefix)
                    .iter()
                    .cloned(),
            );
            names.retain(|name| completion_matches(name, prefix));
            names.sort_by_key(|name| name.to_ascii_lowercase());
            names.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
            self.enums.insert(cache_key.clone(), names);
        }
        self.enums.get(&cache_key).map_or(&[], Vec::as_slice)
    }
}

pub(crate) fn semantic_rules_for_completion<'rule, 'path>(
    snapshot: &'rule AnalysisSnapshot,
    context: &'path SemanticCompletionContext,
) -> Vec<SemanticCompletionRule<'rule, 'path>> {
    let mut rules = semantic_rules_for_container(
        snapshot,
        &context.context,
        &context.parent_path,
        &context.scope,
    )
    .into_iter()
    .map(|rule| SemanticCompletionRule {
        rule,
        parent_path: &context.parent_path,
        scope: &context.scope,
    })
    .collect::<Vec<_>>();
    for (structural_context, structural_path) in &context.structural_containers {
        rules.extend(
            semantic_rules_for_container(
                snapshot,
                structural_context,
                structural_path,
                &context.scope,
            )
            .into_iter()
            .map(|rule| SemanticCompletionRule {
                rule,
                parent_path: structural_path,
                scope: &context.scope,
            }),
        );
    }
    for alternative in &context.alternative_containers {
        rules.extend(
            semantic_rules_for_container(
                snapshot,
                &alternative.context,
                &alternative.parent_path,
                &alternative.scope,
            )
            .into_iter()
            .map(|rule| SemanticCompletionRule {
                rule,
                parent_path: &alternative.parent_path,
                scope: &alternative.scope,
            }),
        );
    }
    rules.sort_by(|left, right| left.rule.id.cmp(&right.rule.id));
    rules.dedup_by(|left, right| {
        left.rule.id == right.rule.id
            && left.parent_path.len() == right.parent_path.len()
            && left
                .parent_path
                .iter()
                .zip(right.parent_path)
                .all(|(left, right)| left.eq_ignore_ascii_case(right))
            && left.scope == right.scope
    });
    rules
}

#[allow(clippy::too_many_arguments)] // Candidate construction keeps the query context and output state explicit.
pub(crate) fn add_semantic_key_items(
    snapshot: &AnalysisSnapshot,
    context: &SemanticCompletionContext,
    member_cache: &mut CompletionMemberCache,
    items: &mut Vec<CompletionItem>,
    replacement_range: TextRange,
    prefix: &str,
    insert_assignment: bool,
    base_indent: &str,
) {
    for candidate in semantic_rules_for_completion(snapshot, context) {
        let rule = candidate.rule;
        if matches!(rule.shape, RuleShape::LeafValue)
            || !semantic_scope_allows(rule, candidate.scope)
        {
            continue;
        }
        let documentation = (!rule.documentation.is_empty()).then(|| rule.documentation.join("\n"));
        match &rule.key {
            KeyMatcher::Exact(label) => push_completion(
                items,
                CompletionItem {
                    label: label.clone(),
                    kind: CompletionKind::Key,
                    detail: rule_context_detail(rule),
                    documentation,
                    replacement_range,
                    insert_text: key_insert_text(rule, label, insert_assignment, base_indent),
                    sort_score: completion_sort_score(
                        if rule.required { 2 } else { 5 },
                        rule.deprecated,
                    ),
                    deprecated: rule.deprecated,
                    resolve_data: Some(format!("rule:{}", rule.id)),
                },
                prefix,
            ),
            KeyMatcher::Type(type_name) => {
                for label in member_cache.workspace_member_names(snapshot, type_name, prefix) {
                    let insert_text = if !insert_assignment {
                        label.clone()
                    } else if scripted_macro_type(snapshot, type_name) {
                        scripted_definition_snippet(snapshot, type_name, label, base_indent)
                    } else {
                        key_insert_text(rule, label, true, base_indent)
                    };
                    push_completion(
                        items,
                        CompletionItem {
                            label: label.clone(),
                            kind: CompletionKind::Key,
                            detail: type_name.clone(),
                            documentation: documentation.clone(),
                            replacement_range,
                            insert_text,
                            sort_score: completion_sort_score(8, rule.deprecated),
                            deprecated: rule.deprecated,
                            resolve_data: Some(format!("rule:{}", rule.id)),
                        },
                        prefix,
                    );
                }
            }
            KeyMatcher::Enum(enum_name) => {
                match qualified_parameter_domain(snapshot, rule, candidate.parent_path) {
                    QualifiedParameterDomain::Known(labels) => {
                        for label in labels {
                            push_completion(
                                items,
                                CompletionItem {
                                    label: label.clone(),
                                    kind: CompletionKind::Key,
                                    detail: "parameter".to_owned(),
                                    documentation: documentation.clone(),
                                    replacement_range,
                                    insert_text: key_insert_text(
                                        rule,
                                        &label,
                                        insert_assignment,
                                        base_indent,
                                    ),
                                    sort_score: completion_sort_score(8, rule.deprecated),
                                    deprecated: rule.deprecated,
                                    resolve_data: Some(format!("rule:{}", rule.id)),
                                },
                                prefix,
                            );
                        }
                    }
                    QualifiedParameterDomain::OpenWorld => {}
                    QualifiedParameterDomain::NotApplicable => {
                        for label in member_cache.enum_member_names(snapshot, enum_name, prefix) {
                            push_completion(
                                items,
                                CompletionItem {
                                    label: label.clone(),
                                    kind: CompletionKind::Key,
                                    detail: enum_name.clone(),
                                    documentation: documentation.clone(),
                                    replacement_range,
                                    insert_text: key_insert_text(
                                        rule,
                                        label,
                                        insert_assignment,
                                        base_indent,
                                    ),
                                    sort_score: completion_sort_score(8, rule.deprecated),
                                    deprecated: rule.deprecated,
                                    resolve_data: Some(format!("rule:{}", rule.id)),
                                },
                                prefix,
                            );
                        }
                    }
                }
            }
            KeyMatcher::Dynamic(kind) => {
                for label in member_cache.workspace_member_names(snapshot, kind, prefix) {
                    push_completion(
                        items,
                        CompletionItem {
                            label: label.clone(),
                            kind: CompletionKind::Key,
                            detail: kind.clone(),
                            documentation: documentation.clone(),
                            replacement_range,
                            insert_text: key_insert_text(
                                rule,
                                label,
                                insert_assignment,
                                base_indent,
                            ),
                            sort_score: completion_sort_score(8, rule.deprecated),
                            deprecated: rule.deprecated,
                            resolve_data: Some(format!("rule:{}", rule.id)),
                        },
                        prefix,
                    );
                }
            }
            KeyMatcher::AnyScalar | KeyMatcher::Date => {}
        }
    }
}

pub(crate) fn add_semantic_value_items(
    snapshot: &AnalysisSnapshot,
    context: &SemanticCompletionContext,
    property: &ScriptProperty,
    member_cache: &mut CompletionMemberCache,
    items: &mut Vec<CompletionItem>,
    replacement_range: TextRange,
    prefix: &str,
) {
    let matching = semantic_rules_for_completion(snapshot, context)
        .into_iter()
        .filter(|candidate| {
            let rule = candidate.rule;
            !matches!(rule.shape, RuleShape::LeafValue)
                && semantic_rule_key_matches(snapshot, rule, candidate.parent_path, &property.key)
                && rule
                    .operator
                    .as_deref()
                    .is_none_or(|operator| property.operator.as_deref() == Some(operator))
        })
        .filter(|candidate| semantic_scope_allows(candidate.rule, candidate.scope))
        .collect::<Vec<_>>();
    for candidate in matching {
        let rule = candidate.rule;
        let documentation = (!rule.documentation.is_empty()).then(|| rule.documentation.join("\n"));
        match &rule.value {
            ValueMatcher::Exact(label) => add_value_completion(
                items,
                label,
                &semantic_value_matcher_label(&rule.value),
                documentation.clone(),
                replacement_range,
                prefix,
                rule.deprecated,
            ),
            ValueMatcher::Bool => {
                add_value_completion(
                    items,
                    "yes",
                    "bool",
                    documentation.clone(),
                    replacement_range,
                    prefix,
                    rule.deprecated,
                );
                add_value_completion(
                    items,
                    "no",
                    "bool",
                    documentation.clone(),
                    replacement_range,
                    prefix,
                    rule.deprecated,
                );
            }
            ValueMatcher::Int { min, max } => {
                add_numeric_completion(
                    items,
                    min.map(|value| value.to_string()).as_deref(),
                    "int",
                    documentation.clone(),
                    replacement_range,
                    prefix,
                    rule.deprecated,
                );
                add_numeric_completion(
                    items,
                    max.map(|value| value.to_string()).as_deref(),
                    "int",
                    documentation.clone(),
                    replacement_range,
                    prefix,
                    rule.deprecated,
                );
            }
            ValueMatcher::Date => {
                add_value_completion(
                    items,
                    "1444.11.11",
                    "date",
                    documentation.clone(),
                    replacement_range,
                    prefix,
                    rule.deprecated,
                );
            }
            ValueMatcher::Float { min, max } => {
                add_value_completion(
                    items,
                    "0",
                    "float",
                    documentation.clone(),
                    replacement_range,
                    prefix,
                    rule.deprecated,
                );
                if min.is_some() || max.is_some() {
                    add_value_completion(
                        items,
                        min.as_deref().unwrap_or("1"),
                        "float",
                        documentation.clone(),
                        replacement_range,
                        prefix,
                        rule.deprecated,
                    );
                    add_value_completion(
                        items,
                        max.as_deref().unwrap_or("1"),
                        "float",
                        documentation.clone(),
                        replacement_range,
                        prefix,
                        rule.deprecated,
                    );
                }
            }
            ValueMatcher::Type(type_name) => {
                for label in member_cache.workspace_member_names(snapshot, type_name, prefix) {
                    add_value_completion(
                        items,
                        label,
                        type_name,
                        documentation.clone(),
                        replacement_range,
                        prefix,
                        rule.deprecated,
                    );
                }
            }
            ValueMatcher::Enum(enum_name) => {
                for label in member_cache.enum_member_names(snapshot, enum_name, prefix) {
                    add_value_completion(
                        items,
                        label,
                        enum_name,
                        documentation.clone(),
                        replacement_range,
                        prefix,
                        rule.deprecated,
                    );
                }
            }
            ValueMatcher::Scope(expected) => {
                for (label, detail) in
                    scope_expression_candidates(snapshot, context, expected.as_deref())
                {
                    add_value_completion(
                        items,
                        &label,
                        detail,
                        documentation.clone(),
                        replacement_range,
                        prefix,
                        rule.deprecated,
                    );
                }
            }
            ValueMatcher::Localisation => {
                for label in member_cache.workspace_member_names(snapshot, "localisation", prefix) {
                    add_localisation_value_completion(
                        items,
                        label,
                        "localisation",
                        documentation.clone(),
                        replacement_range,
                        prefix,
                        rule.deprecated,
                    );
                }
            }
            ValueMatcher::Dynamic(kind) => {
                for label in member_cache.workspace_member_names(snapshot, kind, prefix) {
                    add_value_completion(
                        items,
                        label,
                        kind,
                        documentation.clone(),
                        replacement_range,
                        prefix,
                        rule.deprecated,
                    );
                }
                if matches!(kind.as_str(), "variable" | "value") {
                    add_value_completion(
                        items,
                        "$0",
                        kind,
                        documentation.clone(),
                        replacement_range,
                        prefix,
                        rule.deprecated,
                    );
                }
            }
            ValueMatcher::DynamicSet(_)
            | ValueMatcher::AnyScalar
            | ValueMatcher::Filepath
            | ValueMatcher::Opaque(_) => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn add_inferred_macro_value_items(
    snapshot: &AnalysisSnapshot,
    context: &SemanticCompletionContext,
    property: &ScriptProperty,
    member_cache: &mut CompletionMemberCache,
    items: &mut Vec<CompletionItem>,
    replacement_range: TextRange,
    prefix: &str,
    cancellation: &CancellationToken,
) -> Result<bool, Cancelled> {
    let sites = infer_macro_value_constraints(snapshot, context, property, cancellation)?;
    if sites.is_empty() {
        return Ok(false);
    }
    let mut intersection: Option<BTreeMap<(String, CompletionKind), CompletionItem>> = None;
    for site in sites {
        cancellation.checkpoint()?;
        let site_context = SemanticCompletionContext {
            context: context.context.clone(),
            parent_path: context.parent_path.clone(),
            structural_containers: Vec::new(),
            alternative_containers: Vec::new(),
            scope: site.scope,
            container_property: None,
            property: None,
            quoted_depth: context.quoted_depth,
            embedded_value_context: context.embedded_value_context,
        };
        let mut site_items = Vec::new();
        for matcher in &site.matchers {
            add_inferred_matcher_items(
                snapshot,
                &site_context,
                matcher,
                member_cache,
                &mut site_items,
                replacement_range,
                prefix,
            );
        }
        let site_items = site_items
            .into_iter()
            .map(|item| ((item.label.to_ascii_lowercase(), item.kind), item))
            .collect::<BTreeMap<_, _>>();
        if let Some(known) = &mut intersection {
            known.retain(|key, _| site_items.contains_key(key));
        } else {
            intersection = Some(site_items);
        }
    }
    if let Some(intersection) = intersection {
        items.extend(intersection.into_values());
    }
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
fn add_inferred_matcher_items(
    snapshot: &AnalysisSnapshot,
    context: &SemanticCompletionContext,
    matcher: &ValueMatcher,
    member_cache: &mut CompletionMemberCache,
    items: &mut Vec<CompletionItem>,
    replacement_range: TextRange,
    prefix: &str,
) {
    match matcher {
        ValueMatcher::Exact(label) => add_value_completion(
            items,
            label,
            &semantic_value_matcher_label(matcher),
            None,
            replacement_range,
            prefix,
            false,
        ),
        ValueMatcher::Bool => {
            for label in ["yes", "no"] {
                add_value_completion(items, label, "bool", None, replacement_range, prefix, false);
            }
        }
        ValueMatcher::Int { min, max } => {
            add_numeric_completion(
                items,
                min.map(|value| value.to_string()).as_deref(),
                "int",
                None,
                replacement_range,
                prefix,
                false,
            );
            add_numeric_completion(
                items,
                max.map(|value| value.to_string()).as_deref(),
                "int",
                None,
                replacement_range,
                prefix,
                false,
            );
        }
        ValueMatcher::Float { min, max } => {
            add_value_completion(items, "0", "float", None, replacement_range, prefix, false);
            if min.is_some() || max.is_some() {
                for label in [min.as_deref().unwrap_or("1"), max.as_deref().unwrap_or("1")] {
                    add_value_completion(
                        items,
                        label,
                        "float",
                        None,
                        replacement_range,
                        prefix,
                        false,
                    );
                }
            }
        }
        ValueMatcher::Date => add_value_completion(
            items,
            "1444.11.11",
            "date",
            None,
            replacement_range,
            prefix,
            false,
        ),
        ValueMatcher::Type(type_name) => {
            for label in member_cache.workspace_member_names(snapshot, type_name, prefix) {
                add_value_completion(
                    items,
                    label,
                    type_name,
                    None,
                    replacement_range,
                    prefix,
                    false,
                );
            }
        }
        ValueMatcher::Enum(enum_name) => {
            for label in member_cache.enum_member_names(snapshot, enum_name, prefix) {
                add_value_completion(
                    items,
                    label,
                    enum_name,
                    None,
                    replacement_range,
                    prefix,
                    false,
                );
            }
        }
        ValueMatcher::Scope(expected) => {
            for (label, detail) in
                scope_expression_candidates(snapshot, context, expected.as_deref())
            {
                add_value_completion(
                    items,
                    &label,
                    detail,
                    None,
                    replacement_range,
                    prefix,
                    false,
                );
            }
        }
        ValueMatcher::Localisation => {
            for label in member_cache.workspace_member_names(snapshot, "localisation", prefix) {
                add_localisation_value_completion(
                    items,
                    label,
                    "localisation",
                    None,
                    replacement_range,
                    prefix,
                    false,
                );
            }
        }
        ValueMatcher::Dynamic(kind) => {
            for label in member_cache.workspace_member_names(snapshot, kind, prefix) {
                add_value_completion(items, label, kind, None, replacement_range, prefix, false);
            }
            if matches!(kind.as_str(), "variable" | "value") {
                add_value_completion(items, "$0", kind, None, replacement_range, prefix, false);
            }
        }
        ValueMatcher::DynamicSet(_)
        | ValueMatcher::AnyScalar
        | ValueMatcher::Filepath
        | ValueMatcher::Opaque(_) => {}
    }
}

/// Maximum number of multi-segment scope chains offered as completion candidates.
pub(crate) const SCOPE_CHAIN_LIMIT: usize = 16;

/// Scope expression candidates for a value position: base scope names and intrinsics whose
/// resolved scope is compatible with the expectation, plus scope links reachable from the
/// current scope (single links and, when the first hop does not already satisfy, one-hop chains).
pub(crate) fn scope_expression_candidates(
    snapshot: &AnalysisSnapshot,
    context: &SemanticCompletionContext,
    expected: Option<&str>,
) -> Vec<(String, &'static str)> {
    let profile = snapshot.game_profile();
    let scope = &context.scope;
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();
    let compatible = |scope_name: &str| -> bool {
        expected.is_none_or(|expected| profile.scopes_compatible(scope_name, expected))
    };
    // Intrinsics with an unknown resolved scope stay visible; they may still be legal here.
    let intrinsic_compatible = |resolved: &str| -> bool {
        resolved.eq_ignore_ascii_case("any")
            || resolved.eq_ignore_ascii_case("invalid")
            || compatible(resolved)
    };
    for label in &profile.scope_completions {
        let resolved = match label.as_str() {
            "root" => Some(scope.root.as_str()),
            "this" => Some(scope.current.as_str()),
            "from" => scope.from.first().map(String::as_str),
            "prev" => scope.previous.first().map(String::as_str),
            _ => None,
        };
        let keep = match resolved {
            Some(resolved) => intrinsic_compatible(resolved),
            None => compatible(label),
        };
        if keep && seen.insert(label.clone()) {
            candidates.push((label.clone(), "scope"));
        }
    }
    let links = scope_link_rules(snapshot);
    for (label, allowed, target) in &links {
        let reachable = allowed.is_empty()
            || allowed
                .iter()
                .any(|allowed| profile.scopes_compatible(&scope.current, allowed));
        if reachable && compatible(target) && seen.insert(label.clone()) {
            candidates.push((label.clone(), "scope link"));
        }
    }
    let mut chains = Vec::new();
    for (label1, allowed1, target1) in &links {
        let reachable1 = allowed1.is_empty()
            || allowed1
                .iter()
                .any(|allowed| profile.scopes_compatible(&scope.current, allowed));
        if !reachable1 || compatible(target1) {
            // The single link already satisfies the expectation; a chain adds no value.
            continue;
        }
        for (label2, allowed2, target2) in &links {
            if label1 == label2 {
                continue;
            }
            let second_hop = allowed2.is_empty()
                || allowed2
                    .iter()
                    .any(|allowed| profile.scopes_compatible(target1, allowed));
            if second_hop && compatible(target2) {
                chains.push(format!("{label1}.{label2}"));
            }
        }
    }
    chains.sort();
    chains.dedup();
    for chain in chains.into_iter().take(SCOPE_CHAIN_LIMIT) {
        if seen.insert(chain.clone()) {
            candidates.push((chain, "scope link"));
        }
    }
    candidates
}

/// Exact-key effect/trigger rules that push a scope: `(label, allowed_scopes, push_scope)`.
pub(crate) fn scope_link_rules(snapshot: &AnalysisSnapshot) -> Vec<(String, Vec<String>, String)> {
    let mut links = snapshot
        .rules()
        .model()
        .semantic
        .rules
        .iter()
        .filter(|rule| {
            matches!(
                rule.context.to_ascii_lowercase().as_str(),
                "effect" | "trigger"
            ) && matches!(&rule.key, KeyMatcher::Exact(label) if !label.contains('.'))
                && rule.push_scope.is_some()
        })
        .map(|rule| {
            let label = match &rule.key {
                KeyMatcher::Exact(label) => label.clone(),
                _ => unreachable!("filtered for exact keys"),
            };
            (
                label,
                rule.allowed_scopes.clone(),
                rule.push_scope.clone().expect("filtered for push scope"),
            )
        })
        .collect::<Vec<_>>();
    links.sort();
    links.dedup();
    links
}

pub(crate) fn add_value_completion(
    items: &mut Vec<CompletionItem>,
    label: &str,
    detail: &str,
    documentation: Option<String>,
    replacement_range: TextRange,
    prefix: &str,
    deprecated: bool,
) {
    push_completion(
        items,
        CompletionItem {
            label: label.to_owned(),
            kind: CompletionKind::Value,
            detail: detail.to_owned(),
            documentation,
            replacement_range,
            insert_text: label.to_owned(),
            sort_score: completion_sort_score(4, deprecated),
            deprecated,
            resolve_data: None,
        },
        prefix,
    );
}

/// Value completion for localisation keys, which keeps the `Localisation` kind independent of
/// the detail text.
pub(crate) fn add_localisation_value_completion(
    items: &mut Vec<CompletionItem>,
    label: &str,
    detail: &str,
    documentation: Option<String>,
    replacement_range: TextRange,
    prefix: &str,
    deprecated: bool,
) {
    push_completion(
        items,
        CompletionItem {
            label: label.to_owned(),
            kind: CompletionKind::Localisation,
            detail: detail.to_owned(),
            documentation,
            replacement_range,
            insert_text: label.to_owned(),
            sort_score: completion_sort_score(4, deprecated),
            deprecated,
            resolve_data: None,
        },
        prefix,
    );
}

pub(crate) fn add_numeric_completion(
    items: &mut Vec<CompletionItem>,
    label: Option<&str>,
    detail: &str,
    documentation: Option<String>,
    replacement_range: TextRange,
    prefix: &str,
    deprecated: bool,
) {
    if let Some(label) = label {
        add_value_completion(
            items,
            label,
            detail,
            documentation,
            replacement_range,
            prefix,
            deprecated,
        );
    }
}

/// Short detail for a rule-backed key: the semantic context the rule belongs to. `effect` and
/// `trigger` keep their short names; other contexts (including `type:`/`root:` prefixed roots)
/// are shown by their bare name.
pub(crate) fn rule_context_detail(rule: &pdx_rules::SemanticRule) -> String {
    if rule.context.eq_ignore_ascii_case("effect") || rule.context.eq_ignore_ascii_case("trigger") {
        return rule.context.clone();
    }
    rule.context
        .strip_prefix("type:")
        .or_else(|| rule.context.strip_prefix("root:"))
        .unwrap_or(&rule.context)
        .to_owned()
}

/// Builds the text inserted for a rule-backed key completion. Scalar and value-clause keys
/// insert the `=` operator so the cursor lands on the value; block keys insert an empty block
/// skeleton as a snippet. Existing assignments only replace the key spelling.
pub(crate) fn key_insert_text(
    rule: &pdx_rules::SemanticRule,
    label: &str,
    insert_assignment: bool,
    base_indent: &str,
) -> String {
    if !insert_assignment {
        return label.to_owned();
    }
    match rule.shape {
        RuleShape::Node => format!("{label} = {{\n{base_indent}\t$0\n{base_indent}}}"),
        RuleShape::QuotedScript => {
            format!("{label} = \"\n{base_indent}\t$0\n{base_indent}\"")
        }
        RuleShape::Leaf | RuleShape::ValueClause => format!("{label} = "),
        RuleShape::LeafValue => label.to_owned(),
    }
}
