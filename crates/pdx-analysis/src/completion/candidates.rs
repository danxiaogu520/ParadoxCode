use std::collections::{BTreeMap, HashSet};

use crate::semantic::*;
use crate::support::*;
use crate::types::*;
use pdx_engine::AnalysisSnapshot;
use pdx_rules::{KeyMatcher, RuleShape, ValueMatcher};
use pdx_text::TextRange;

use super::context::SemanticCompletionContext;
use super::macro_constraints::infer_macro_value_constraints;
#[cfg(test)]
use super::support::finalize_completion_items;
use super::support::{
    CompletionRankContext, CompletionSchemaTier, CompletionSpecificity, RankedCompletionItem,
    push_completion,
};

pub(crate) struct SemanticCompletionRule<'rule, 'path> {
    pub(crate) rule: &'rule pdx_rules::SemanticRule,
    pub(crate) parent_path: &'path [String],
    pub(crate) scope: &'path ScopeContext,
    pub(crate) schema_tier: CompletionSchemaTier,
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

/// Returns whether a rule's key behaves like a callable script command or trigger predicate.
///
/// The rule source deliberately keeps these as ordinary semantic contexts.  Keeping the
/// presentation hint here lets the LSP layer stay a protocol adapter instead of having to infer
/// game semantics from a completion item's detail text.
fn is_command_context(context: &str) -> bool {
    context.eq_ignore_ascii_case("effect") || context.eq_ignore_ascii_case("trigger")
}

fn key_completion_kind(
    snapshot: &AnalysisSnapshot,
    rule: &pdx_rules::SemanticRule,
) -> CompletionKind {
    match &rule.key {
        KeyMatcher::Enum(_) => CompletionKind::EnumMember,
        KeyMatcher::Type(type_name) | KeyMatcher::Dynamic(type_name)
            if scripted_macro_type(snapshot, type_name) =>
        {
            CompletionKind::ScriptedMacro
        }
        KeyMatcher::Exact(_) | KeyMatcher::Type(_) | KeyMatcher::Dynamic(_)
            if is_command_context(&rule.context) =>
        {
            CompletionKind::Command
        }
        _ => CompletionKind::Key,
    }
}

fn key_specificity(
    snapshot: &AnalysisSnapshot,
    rule: &pdx_rules::SemanticRule,
) -> CompletionSpecificity {
    match &rule.key {
        KeyMatcher::Exact(_) => CompletionSpecificity::Exact,
        KeyMatcher::Enum(_) => CompletionSpecificity::Enum,
        KeyMatcher::Type(type_name) if scripted_macro_type(snapshot, type_name) => {
            CompletionSpecificity::ScriptedMacro
        }
        KeyMatcher::Type(_) => CompletionSpecificity::Type,
        KeyMatcher::Dynamic(_) => CompletionSpecificity::Dynamic,
        KeyMatcher::Date | KeyMatcher::AnyScalar => CompletionSpecificity::Fallback,
    }
}

fn rule_required_missing(
    snapshot: &AnalysisSnapshot,
    context: &SemanticCompletionContext,
    candidate: &SemanticCompletionRule<'_, '_>,
) -> bool {
    // `min_occurs` is populated with the parser's default cardinality for nearly every rule;
    // the explicit `required` flag is the schema's signal that a member is expected in the
    // current container.  Treating every `min_occurs = 1` row as required would bury ordinary
    // commands such as `always` behind hundreds of mandatory-looking aliases.
    if !candidate.rule.required {
        return false;
    }
    !context
        .existing_keys
        .iter()
        .any(|key| semantic_rule_key_matches(snapshot, candidate.rule, candidate.parent_path, key))
}

fn rule_rank_context(
    snapshot: &AnalysisSnapshot,
    context: &SemanticCompletionContext,
    candidate: &SemanticCompletionRule<'_, '_>,
    specificity: CompletionSpecificity,
) -> CompletionRankContext {
    CompletionRankContext::new(
        candidate.schema_tier,
        specificity,
        rule_required_missing(snapshot, context, candidate),
        candidate.rule.deprecated,
    )
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
        schema_tier: if context.macro_inferred {
            CompletionSchemaTier::MacroInferred
        } else {
            CompletionSchemaTier::CurrentContext
        },
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
                schema_tier: CompletionSchemaTier::ExplicitParentMember,
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
                schema_tier: if context.macro_inferred {
                    CompletionSchemaTier::MacroInferred
                } else {
                    CompletionSchemaTier::Alternative
                },
            }),
        );
    }
    rules.sort_by(|left, right| {
        left.rule
            .id
            .cmp(&right.rule.id)
            .then_with(|| left.schema_tier.cmp(&right.schema_tier))
    });
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

#[cfg(test)]
pub(crate) fn add_semantic_key_items(
    snapshot: &AnalysisSnapshot,
    context: &SemanticCompletionContext,
    member_cache: &mut CompletionMemberCache,
    items: &mut Vec<CompletionItem>,
    replacement_range: TextRange,
    prefix: &str,
    insert_assignment: bool,
) {
    let mut ranked = Vec::new();
    add_semantic_key_items_ranked(
        snapshot,
        context,
        member_cache,
        &mut ranked,
        replacement_range,
        prefix,
        insert_assignment,
    );
    items.extend(finalize_completion_items(ranked));
}

pub(crate) fn add_semantic_key_items_ranked(
    snapshot: &AnalysisSnapshot,
    context: &SemanticCompletionContext,
    member_cache: &mut CompletionMemberCache,
    items: &mut Vec<RankedCompletionItem>,
    replacement_range: TextRange,
    prefix: &str,
    insert_assignment: bool,
) {
    for candidate in semantic_rules_for_completion(snapshot, context) {
        let rule = candidate.rule;
        if !semantic_scope_allows(rule, candidate.scope) {
            continue;
        }
        // The file-root entry scaffold is the only container that suppresses single-instance
        // keys already declared at the document root; ordinary containers keep offering
        // declared keys so users can repeat or edit them.
        if context.root_entry_container
            && rule.max_occurs == Some(1)
            && context
                .existing_keys
                .iter()
                .any(|key| semantic_rule_key_matches(snapshot, rule, candidate.parent_path, key))
        {
            continue;
        }
        let documentation = (!rule.documentation.is_empty()).then(|| rule.documentation.join("\n"));
        if matches!(rule.shape, RuleShape::LeafValue) {
            // A leaf-value container such as `required_missions = { ... }` accepts any key as
            // an instance of the rule's value type; complete the workspace members of that
            // type instead of rule keys.
            add_leaf_value_member_items(
                snapshot,
                rule,
                member_cache,
                items,
                replacement_range,
                prefix,
                documentation,
                candidate.schema_tier,
            );
            continue;
        }
        match &rule.key {
            KeyMatcher::Exact(label) => push_completion(
                items,
                CompletionItem {
                    label: label.clone(),
                    kind: key_completion_kind(snapshot, rule),
                    detail: rule_context_detail(rule),
                    documentation,
                    replacement_range,
                    insert_text: key_insert_text(rule, label, insert_assignment),
                    sort_score: 0,
                    deprecated: rule.deprecated,
                    resolve_data: Some(format!("rule:{}", rule.id)),
                },
                prefix,
                rule_rank_context(snapshot, context, &candidate, CompletionSpecificity::Exact),
            ),
            KeyMatcher::Type(type_name) => {
                for label in member_cache.workspace_member_names(snapshot, type_name, prefix) {
                    let insert_text = if !insert_assignment {
                        label.clone()
                    } else if scripted_macro_type(snapshot, type_name) {
                        scripted_definition_snippet(snapshot, type_name, label)
                    } else {
                        key_insert_text(rule, label, true)
                    };
                    push_completion(
                        items,
                        CompletionItem {
                            label: label.clone(),
                            kind: key_completion_kind(snapshot, rule),
                            detail: type_name.clone(),
                            documentation: documentation.clone(),
                            replacement_range,
                            insert_text,
                            sort_score: 0,
                            deprecated: rule.deprecated,
                            resolve_data: Some(format!("rule:{}", rule.id)),
                        },
                        prefix,
                        rule_rank_context(
                            snapshot,
                            context,
                            &candidate,
                            key_specificity(snapshot, rule),
                        ),
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
                                    kind: CompletionKind::MacroParameter,
                                    detail: "parameter".to_owned(),
                                    documentation: documentation.clone(),
                                    replacement_range,
                                    insert_text: key_insert_text(rule, &label, insert_assignment),
                                    sort_score: 0,
                                    deprecated: rule.deprecated,
                                    resolve_data: Some(format!("rule:{}", rule.id)),
                                },
                                prefix,
                                rule_rank_context(
                                    snapshot,
                                    context,
                                    &candidate,
                                    CompletionSpecificity::Enum,
                                ),
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
                                    kind: CompletionKind::EnumMember,
                                    detail: enum_name.clone(),
                                    documentation: documentation.clone(),
                                    replacement_range,
                                    insert_text: key_insert_text(rule, label, insert_assignment),
                                    sort_score: 0,
                                    deprecated: rule.deprecated,
                                    resolve_data: Some(format!("rule:{}", rule.id)),
                                },
                                prefix,
                                rule_rank_context(
                                    snapshot,
                                    context,
                                    &candidate,
                                    CompletionSpecificity::Enum,
                                ),
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
                            kind: key_completion_kind(snapshot, rule),
                            detail: kind.clone(),
                            documentation: documentation.clone(),
                            replacement_range,
                            insert_text: key_insert_text(rule, label, insert_assignment),
                            sort_score: 0,
                            deprecated: rule.deprecated,
                            resolve_data: Some(format!("rule:{}", rule.id)),
                        },
                        prefix,
                        rule_rank_context(
                            snapshot,
                            context,
                            &candidate,
                            CompletionSpecificity::Dynamic,
                        ),
                    );
                }
            }
            // Open-ended keys accept arbitrary spellings and carry no member information. Date
            // keys are validated as a shape, but a fixed sample date is not a useful candidate.
            KeyMatcher::AnyScalar | KeyMatcher::Date => {}
        }
    }
}

/// Completes workspace/static members for a leaf-value rule's value matcher.
///
/// Used both for keys inside a leaf-value container and for bare values of a `value_clause`
/// rule whose children are leaf-value rules. Members are completed as keys (the inserted text
/// is the bare spelling, without an assignment).
#[expect(clippy::too_many_arguments)]
fn add_leaf_value_member_items(
    snapshot: &AnalysisSnapshot,
    rule: &pdx_rules::SemanticRule,
    member_cache: &mut CompletionMemberCache,
    items: &mut Vec<RankedCompletionItem>,
    replacement_range: TextRange,
    prefix: &str,
    documentation: Option<String>,
    schema_tier: CompletionSchemaTier,
) {
    match &rule.value {
        ValueMatcher::Type(type_name) => {
            for label in member_cache.workspace_member_names(snapshot, type_name, prefix) {
                push_completion(
                    items,
                    CompletionItem {
                        label: label.clone(),
                        kind: CompletionKind::Key,
                        detail: type_name.clone(),
                        documentation: documentation.clone(),
                        replacement_range,
                        insert_text: label.clone(),
                        sort_score: 0,
                        deprecated: rule.deprecated,
                        resolve_data: Some(format!("rule:{}", rule.id)),
                    },
                    prefix,
                    CompletionRankContext::new(
                        schema_tier,
                        CompletionSpecificity::Type,
                        false,
                        rule.deprecated,
                    ),
                );
            }
        }
        ValueMatcher::Enum(enum_name) => {
            for label in member_cache.enum_member_names(snapshot, enum_name, prefix) {
                push_completion(
                    items,
                    CompletionItem {
                        label: label.clone(),
                        kind: CompletionKind::EnumMember,
                        detail: enum_name.clone(),
                        documentation: documentation.clone(),
                        replacement_range,
                        insert_text: label.clone(),
                        sort_score: 0,
                        deprecated: rule.deprecated,
                        resolve_data: Some(format!("rule:{}", rule.id)),
                    },
                    prefix,
                    CompletionRankContext::new(
                        schema_tier,
                        CompletionSpecificity::Enum,
                        false,
                        rule.deprecated,
                    ),
                );
            }
        }
        ValueMatcher::Dynamic(kind) => {
            for label in member_cache.workspace_member_names(snapshot, kind, prefix) {
                push_completion(
                    items,
                    CompletionItem {
                        label: label.clone(),
                        kind: CompletionKind::Key,
                        detail: kind.clone(),
                        documentation: documentation.clone(),
                        replacement_range,
                        insert_text: label.clone(),
                        sort_score: 0,
                        deprecated: rule.deprecated,
                        resolve_data: Some(format!("rule:{}", rule.id)),
                    },
                    prefix,
                    CompletionRankContext::new(
                        schema_tier,
                        CompletionSpecificity::Dynamic,
                        false,
                        rule.deprecated,
                    ),
                );
            }
        }
        ValueMatcher::Localisation => {
            for label in member_cache.workspace_member_names(snapshot, "localisation", prefix) {
                add_localisation_value_completion_ranked(
                    items,
                    label,
                    "localisation",
                    documentation.clone(),
                    replacement_range,
                    prefix,
                    rule.deprecated,
                    schema_tier,
                );
            }
        }
        ValueMatcher::Exact(label) => {
            add_value_completion_ranked(
                items,
                label,
                &semantic_value_matcher_label(&rule.value),
                documentation,
                replacement_range,
                prefix,
                rule.deprecated,
                schema_tier,
                CompletionSpecificity::Exact,
            );
        }
        ValueMatcher::Bool => {
            for label in ["yes", "no"] {
                add_value_completion_ranked(
                    items,
                    label,
                    "bool",
                    documentation.clone(),
                    replacement_range,
                    prefix,
                    rule.deprecated,
                    schema_tier,
                    CompletionSpecificity::Value,
                );
            }
        }
        // Numeric and date matchers describe open-ended syntax/ranges, not finite candidate sets.
        // AnyScalar, DynamicSet, Filepath, Opaque, and Scope likewise carry no member
        // information.
        ValueMatcher::Int { .. }
        | ValueMatcher::Float { .. }
        | ValueMatcher::Date
        | ValueMatcher::AnyScalar
        | ValueMatcher::DynamicSet(_)
        | ValueMatcher::Filepath
        | ValueMatcher::Opaque(_)
        | ValueMatcher::Scope(_) => {}
    }
}

pub(crate) fn add_semantic_value_items(
    snapshot: &AnalysisSnapshot,
    context: &SemanticCompletionContext,
    property: &ScriptProperty,
    member_cache: &mut CompletionMemberCache,
    items: &mut Vec<RankedCompletionItem>,
    replacement_range: TextRange,
    prefix: &str,
) {
    let matching = semantic_rules_for_completion(snapshot, context)
        .into_iter()
        .filter(|candidate| {
            let rule = candidate.rule;
            semantic_rule_key_matches(snapshot, rule, candidate.parent_path, &property.key)
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
        if matches!(rule.shape, RuleShape::LeafValue) {
            // A bare `key = ` position inside a leaf-value container completes the members of
            // the value type, mirroring the key-position behavior.
            add_leaf_value_member_items(
                snapshot,
                rule,
                member_cache,
                items,
                replacement_range,
                prefix,
                documentation,
                candidate.schema_tier,
            );
            continue;
        }
        if matches!(rule.shape, RuleShape::ValueClause) {
            // A bare value of a `value_clause` rule is validated by the leaf-value rules of its
            // child container; complete their value-type members here.
            let mut child_path = candidate.parent_path.to_vec();
            child_path.push(property.key.clone());
            for child_rule in semantic_rules_for_container(
                snapshot,
                &context.context,
                &child_path,
                &context.scope,
            ) {
                if matches!(child_rule.shape, RuleShape::LeafValue) {
                    add_leaf_value_member_items(
                        snapshot,
                        child_rule,
                        member_cache,
                        items,
                        replacement_range,
                        prefix,
                        documentation.clone(),
                        candidate.schema_tier,
                    );
                }
            }
            continue;
        }
        match &rule.value {
            ValueMatcher::Exact(label) => add_value_completion_ranked(
                items,
                label,
                &semantic_value_matcher_label(&rule.value),
                documentation.clone(),
                replacement_range,
                prefix,
                rule.deprecated,
                candidate.schema_tier,
                CompletionSpecificity::Exact,
            ),
            ValueMatcher::Bool => {
                add_value_completion_ranked(
                    items,
                    "yes",
                    "bool",
                    documentation.clone(),
                    replacement_range,
                    prefix,
                    rule.deprecated,
                    candidate.schema_tier,
                    CompletionSpecificity::Value,
                );
                add_value_completion_ranked(
                    items,
                    "no",
                    "bool",
                    documentation.clone(),
                    replacement_range,
                    prefix,
                    rule.deprecated,
                    candidate.schema_tier,
                    CompletionSpecificity::Value,
                );
            }
            // Numeric and date matchers describe open-ended syntax/ranges, not finite candidate
            // sets. Their constraints remain available to diagnostics and hover.
            ValueMatcher::Int { .. } | ValueMatcher::Float { .. } | ValueMatcher::Date => {}
            ValueMatcher::Type(type_name) => {
                for label in member_cache.workspace_member_names(snapshot, type_name, prefix) {
                    add_value_completion_ranked(
                        items,
                        label,
                        type_name,
                        documentation.clone(),
                        replacement_range,
                        prefix,
                        rule.deprecated,
                        candidate.schema_tier,
                        CompletionSpecificity::Type,
                    );
                }
            }
            ValueMatcher::Enum(enum_name) => {
                for label in member_cache.enum_member_names(snapshot, enum_name, prefix) {
                    add_enum_member_completion_ranked(
                        items,
                        label,
                        enum_name,
                        documentation.clone(),
                        replacement_range,
                        prefix,
                        rule.deprecated,
                        candidate.schema_tier,
                    );
                }
            }
            ValueMatcher::Scope(expected) => {
                for (label, detail) in
                    scope_expression_candidates(snapshot, context, expected.as_deref())
                {
                    add_scope_completion_ranked(
                        items,
                        &label,
                        detail,
                        documentation.clone(),
                        replacement_range,
                        prefix,
                        rule.deprecated,
                        candidate.schema_tier,
                    );
                }
            }
            ValueMatcher::Localisation => {
                for label in member_cache.workspace_member_names(snapshot, "localisation", prefix) {
                    add_localisation_value_completion_ranked(
                        items,
                        label,
                        "localisation",
                        documentation.clone(),
                        replacement_range,
                        prefix,
                        rule.deprecated,
                        candidate.schema_tier,
                    );
                }
            }
            ValueMatcher::Dynamic(kind) => {
                // Mirror `semantic_dynamic_value_matches`: scope fields accept scope expressions
                // and variable names; every dynamic kind additionally accepts scope expressions
                // and same-named static enum members at runtime.
                if kind.eq_ignore_ascii_case("scope_field") {
                    for (label, detail) in scope_expression_candidates(snapshot, context, None) {
                        add_scope_completion_ranked(
                            items,
                            &label,
                            detail,
                            documentation.clone(),
                            replacement_range,
                            prefix,
                            rule.deprecated,
                            candidate.schema_tier,
                        );
                    }
                    for label in
                        member_cache.workspace_member_names(snapshot, "variable_name", prefix)
                    {
                        add_value_completion_ranked(
                            items,
                            label,
                            "variable_name",
                            documentation.clone(),
                            replacement_range,
                            prefix,
                            rule.deprecated,
                            candidate.schema_tier,
                            CompletionSpecificity::Type,
                        );
                    }
                    continue;
                }
                for label in member_cache.workspace_member_names(snapshot, kind, prefix) {
                    add_value_completion_ranked(
                        items,
                        label,
                        kind,
                        documentation.clone(),
                        replacement_range,
                        prefix,
                        rule.deprecated,
                        candidate.schema_tier,
                        CompletionSpecificity::Dynamic,
                    );
                }
                for (label, detail) in scope_expression_candidates(snapshot, context, None) {
                    add_scope_completion_ranked(
                        items,
                        &label,
                        detail,
                        documentation.clone(),
                        replacement_range,
                        prefix,
                        rule.deprecated,
                        candidate.schema_tier,
                    );
                }
                for label in member_cache.enum_member_names(snapshot, kind, prefix) {
                    add_enum_member_completion_ranked(
                        items,
                        label,
                        kind,
                        documentation.clone(),
                        replacement_range,
                        prefix,
                        rule.deprecated,
                        candidate.schema_tier,
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

pub(crate) struct InferredMacroCompletionInput<'a> {
    pub(crate) snapshot: &'a AnalysisSnapshot,
    pub(crate) context: &'a SemanticCompletionContext,
    pub(crate) property: &'a ScriptProperty,
    pub(crate) member_cache: &'a mut CompletionMemberCache,
    pub(crate) items: &'a mut Vec<RankedCompletionItem>,
    pub(crate) replacement_range: TextRange,
    pub(crate) prefix: &'a str,
    pub(crate) cancellation: &'a CancellationToken,
}

pub(crate) fn add_inferred_macro_value_items(
    input: InferredMacroCompletionInput<'_>,
) -> Result<bool, Cancelled> {
    let InferredMacroCompletionInput {
        snapshot,
        context,
        property,
        member_cache,
        items,
        replacement_range,
        prefix,
        cancellation,
    } = input;
    let sites = infer_macro_value_constraints(snapshot, context, property, cancellation)?;
    if sites.is_empty() {
        return Ok(false);
    }
    let mut intersection: Option<BTreeMap<(String, CompletionKind), RankedCompletionItem>> = None;
    for site in sites {
        cancellation.checkpoint()?;
        let site_context = SemanticCompletionContext {
            context: context.context.clone(),
            parent_path: context.parent_path.clone(),
            structural_containers: Vec::new(),
            alternative_containers: Vec::new(),
            existing_keys: Vec::new(),
            macro_inferred: false,
            scope: site.scope,
            container_property: None,
            property: None,
            quoted_depth: context.quoted_depth,
            embedded_value_context: context.embedded_value_context,
            wrapper_container: false,
            root_entry_container: false,
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
        let site_items = site_items.into_iter().fold(
            BTreeMap::<(String, CompletionKind), RankedCompletionItem>::new(),
            |mut known, item| {
                let key = (item.item.label.to_ascii_lowercase(), item.item.kind);
                match known.entry(key) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert(item);
                    }
                    std::collections::btree_map::Entry::Occupied(mut entry)
                        if item.rank < entry.get().rank =>
                    {
                        entry.insert(item);
                    }
                    std::collections::btree_map::Entry::Occupied(_) => {}
                }
                known
            },
        );
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

fn add_inferred_matcher_items(
    snapshot: &AnalysisSnapshot,
    context: &SemanticCompletionContext,
    matcher: &ValueMatcher,
    member_cache: &mut CompletionMemberCache,
    items: &mut Vec<RankedCompletionItem>,
    replacement_range: TextRange,
    prefix: &str,
) {
    match matcher {
        ValueMatcher::Exact(label) => add_value_completion_ranked(
            items,
            label,
            &semantic_value_matcher_label(matcher),
            None,
            replacement_range,
            prefix,
            false,
            CompletionSchemaTier::MacroInferred,
            CompletionSpecificity::Exact,
        ),
        ValueMatcher::Bool => {
            for label in ["yes", "no"] {
                add_value_completion_ranked(
                    items,
                    label,
                    "bool",
                    None,
                    replacement_range,
                    prefix,
                    false,
                    CompletionSchemaTier::MacroInferred,
                    CompletionSpecificity::Value,
                );
            }
        }
        // Numeric and date matchers are open-ended; do not turn their constraints into arbitrary
        // completion values.
        ValueMatcher::Int { .. } | ValueMatcher::Float { .. } | ValueMatcher::Date => {}
        ValueMatcher::Type(type_name) => {
            for label in member_cache.workspace_member_names(snapshot, type_name, prefix) {
                add_value_completion_ranked(
                    items,
                    label,
                    type_name,
                    None,
                    replacement_range,
                    prefix,
                    false,
                    CompletionSchemaTier::MacroInferred,
                    CompletionSpecificity::Type,
                );
            }
        }
        ValueMatcher::Enum(enum_name) => {
            for label in member_cache.enum_member_names(snapshot, enum_name, prefix) {
                add_enum_member_completion_ranked(
                    items,
                    label,
                    enum_name,
                    None,
                    replacement_range,
                    prefix,
                    false,
                    CompletionSchemaTier::MacroInferred,
                );
            }
        }
        ValueMatcher::Scope(expected) => {
            for (label, detail) in
                scope_expression_candidates(snapshot, context, expected.as_deref())
            {
                add_scope_completion_ranked(
                    items,
                    &label,
                    detail,
                    None,
                    replacement_range,
                    prefix,
                    false,
                    CompletionSchemaTier::MacroInferred,
                );
            }
        }
        ValueMatcher::Localisation => {
            for label in member_cache.workspace_member_names(snapshot, "localisation", prefix) {
                add_localisation_value_completion_ranked(
                    items,
                    label,
                    "localisation",
                    None,
                    replacement_range,
                    prefix,
                    false,
                    CompletionSchemaTier::MacroInferred,
                );
            }
        }
        ValueMatcher::Dynamic(kind) => {
            for label in member_cache.workspace_member_names(snapshot, kind, prefix) {
                add_value_completion_ranked(
                    items,
                    label,
                    kind,
                    None,
                    replacement_range,
                    prefix,
                    false,
                    CompletionSchemaTier::MacroInferred,
                    CompletionSpecificity::Dynamic,
                );
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
        let resolved = if label.eq_ignore_ascii_case("root") {
            Some(scope.root.as_str())
        } else if label.eq_ignore_ascii_case("this") {
            Some(scope.current.as_str())
        } else if label.eq_ignore_ascii_case("from") {
            scope.from.first().map(String::as_str)
        } else if label.eq_ignore_ascii_case("prev") {
            scope.previous.first().map(String::as_str)
        } else {
            None
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

#[expect(clippy::too_many_arguments)]
fn add_value_completion_ranked(
    items: &mut Vec<RankedCompletionItem>,
    label: &str,
    detail: &str,
    documentation: Option<String>,
    replacement_range: TextRange,
    prefix: &str,
    deprecated: bool,
    schema_tier: CompletionSchemaTier,
    specificity: CompletionSpecificity,
) {
    add_typed_value_completion(
        items,
        TypedValueCompletion {
            label,
            detail,
            documentation,
            replacement_range,
            prefix,
            deprecated,
            kind: CompletionKind::Value,
            rank: CompletionRankContext::new(schema_tier, specificity, false, deprecated),
        },
    );
}

#[expect(clippy::too_many_arguments)]
fn add_enum_member_completion_ranked(
    items: &mut Vec<RankedCompletionItem>,
    label: &str,
    detail: &str,
    documentation: Option<String>,
    replacement_range: TextRange,
    prefix: &str,
    deprecated: bool,
    schema_tier: CompletionSchemaTier,
) {
    add_typed_value_completion(
        items,
        TypedValueCompletion {
            label,
            detail,
            documentation,
            replacement_range,
            prefix,
            deprecated,
            kind: CompletionKind::EnumMember,
            rank: CompletionRankContext::new(
                schema_tier,
                CompletionSpecificity::Enum,
                false,
                deprecated,
            ),
        },
    );
}

#[expect(clippy::too_many_arguments)]
fn add_scope_completion_ranked(
    items: &mut Vec<RankedCompletionItem>,
    label: &str,
    detail: &str,
    documentation: Option<String>,
    replacement_range: TextRange,
    prefix: &str,
    deprecated: bool,
    schema_tier: CompletionSchemaTier,
) {
    add_typed_value_completion(
        items,
        TypedValueCompletion {
            label,
            detail,
            documentation,
            replacement_range,
            prefix,
            deprecated,
            kind: CompletionKind::Scope,
            rank: CompletionRankContext::new(
                schema_tier,
                CompletionSpecificity::Scope,
                false,
                deprecated,
            )
            .with_scope_distance(label.matches('.').count().min(99) as u8),
        },
    );
}

struct TypedValueCompletion<'a> {
    label: &'a str,
    detail: &'a str,
    documentation: Option<String>,
    replacement_range: TextRange,
    prefix: &'a str,
    deprecated: bool,
    kind: CompletionKind,
    rank: CompletionRankContext,
}

fn add_typed_value_completion(
    items: &mut Vec<RankedCompletionItem>,
    completion: TypedValueCompletion<'_>,
) {
    push_completion(
        items,
        CompletionItem {
            label: completion.label.to_owned(),
            kind: completion.kind,
            detail: completion.detail.to_owned(),
            documentation: completion.documentation,
            replacement_range: completion.replacement_range,
            insert_text: completion.label.to_owned(),
            sort_score: 0,
            deprecated: completion.deprecated,
            resolve_data: None,
        },
        completion.prefix,
        completion.rank,
    );
}

/// Value completion for localisation keys, which keeps the `Localisation` kind independent of
/// the detail text.
#[expect(clippy::too_many_arguments)]
fn add_localisation_value_completion_ranked(
    items: &mut Vec<RankedCompletionItem>,
    label: &str,
    detail: &str,
    documentation: Option<String>,
    replacement_range: TextRange,
    prefix: &str,
    deprecated: bool,
    schema_tier: CompletionSchemaTier,
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
            sort_score: 0,
            deprecated,
            resolve_data: None,
        },
        prefix,
        CompletionRankContext::new(
            schema_tier,
            CompletionSpecificity::Localisation,
            false,
            deprecated,
        ),
    );
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
) -> String {
    if !insert_assignment {
        return label.to_owned();
    }
    // Snippets carry only relative indentation: the client re-indents multi-line snippets to the
    // insertion line, so baking absolute leading whitespace in here would stack with that.
    match rule.shape {
        RuleShape::Node => format!("{label} = {{\n\t$0\n}}"),
        RuleShape::QuotedScript => {
            format!("{label} = \"\n\t$0\n\"")
        }
        RuleShape::Leaf | RuleShape::ValueClause => format!("{label} = "),
        RuleShape::LeafValue => label.to_owned(),
    }
}
