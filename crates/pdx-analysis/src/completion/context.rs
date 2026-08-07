use crate::semantic::*;
use crate::support::*;
use pdx_engine::AnalysisSnapshot;
use pdx_engine::hir::HirFile;
use pdx_rules::RuleShape;
use pdx_text::{TextRange, TextSize};

use crate::semantic::{
    cached_scope_fact_for_property, scope_context_from_hir, semantic_child_scope,
    semantic_initial_scope, semantic_root_context, semantic_rule_key_matches,
    semantic_scope_allows,
};

#[derive(Clone, Debug)]
pub(crate) struct SemanticCompletionContext {
    pub(crate) context: String,
    pub(crate) parent_path: Vec<String>,
    pub(crate) structural_containers: Vec<(String, Vec<String>)>,
    pub(crate) alternative_containers: Vec<SemanticCompletionContainer>,
    pub(crate) scope: ScopeContext,
    pub(crate) property: Option<ScriptProperty>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SemanticCompletionContainer {
    pub(crate) context: String,
    pub(crate) parent_path: Vec<String>,
    pub(crate) scope: ScopeContext,
}

pub(crate) fn semantic_completion_context(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    position: TextSize,
) -> Option<SemanticCompletionContext> {
    let ParsedContent::Text(parsed) = &input.parsed;
    for root in script_properties(input, parsed.root()) {
        let Some(context) = semantic_root_context(snapshot, &root.key, input.path.as_ref()) else {
            continue;
        };
        let Some(block_range) = root.block_range else {
            continue;
        };
        if !contains(block_range, position) {
            continue;
        }
        let scope = semantic_initial_scope(snapshot, input, &context, &root.key, root.key_range);
        return Some(semantic_completion_container(
            snapshot,
            input.hir.as_deref(),
            context,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            root.block,
            root.bare_values,
            scope,
            position,
        ));
    }
    None
}

#[allow(clippy::too_many_arguments)] // Recursive traversal carries immutable HIR and semantic state.
pub(crate) fn semantic_completion_container(
    snapshot: &AnalysisSnapshot,
    hir: Option<&HirFile>,
    context: String,
    parent_path: Vec<String>,
    structural_containers: Vec<(String, Vec<String>)>,
    alternative_containers: Vec<SemanticCompletionContainer>,
    properties: Vec<ScriptProperty>,
    _bare_values: Vec<(String, TextRange)>,
    scope: ScopeContext,
    position: TextSize,
) -> SemanticCompletionContext {
    for property in &properties {
        let Some(block_range) = property.block_range else {
            continue;
        };
        if !contains(block_range, position) {
            continue;
        }
        let transparent_wrapper = context.eq_ignore_ascii_case("trigger")
            && snapshot
                .game_profile()
                .is_transparent_scope_wrapper(&property.key);
        let next_rules = semantic_rules_for_container(snapshot, &context, &parent_path, &scope)
            .into_iter()
            .filter(|rule| {
                !matches!(rule.shape, RuleShape::LeafValue)
                    && semantic_rule_key_matches(snapshot, rule, &parent_path, &property.key)
                    && semantic_scope_allows(rule, &scope)
            })
            .collect::<Vec<_>>();
        let cached_child_fact = cached_scope_fact_for_property(
            snapshot,
            hir,
            &context,
            &parent_path,
            property,
            &next_rules,
            None,
            &scope,
            transparent_wrapper,
        );
        if let Some(fact) = cached_child_fact {
            let structural_containers = completion_structural_containers(
                snapshot,
                &context,
                &parent_path,
                &property.key,
                transparent_wrapper,
                &fact.context,
                &fact.parent_path,
                &scope,
            );
            return semantic_completion_container(
                snapshot,
                hir,
                fact.context.clone(),
                fact.parent_path.clone(),
                structural_containers,
                Vec::new(),
                property.block.clone(),
                property.bare_values.clone(),
                scope_context_from_hir(snapshot.game_profile_handle(), &fact.state),
                position,
            );
        }
        let mut destinations = Vec::<SemanticCompletionContainer>::new();
        for rule in next_rules {
            let (destination_context, destination_path) =
                rule.child_context.as_deref().map_or_else(
                    || {
                        let mut path = parent_path.clone();
                        if !transparent_wrapper {
                            path.push(property.key.clone());
                        }
                        (context.clone(), path)
                    },
                    |child_context| (child_context.to_owned(), Vec::new()),
                );
            let destination = SemanticCompletionContainer {
                context: destination_context,
                parent_path: destination_path,
                scope: semantic_child_scope(snapshot, &scope, rule),
            };
            if !destinations
                .iter()
                .any(|known| semantic_completion_containers_equal(known, &destination))
            {
                destinations.push(destination);
            }
        }
        let primary = destinations.first().cloned().unwrap_or_else(|| {
            let mut path = parent_path.clone();
            if !transparent_wrapper {
                path.push(property.key.clone());
            }
            SemanticCompletionContainer {
                context: context.clone(),
                parent_path: path,
                scope: scope.clone(),
            }
        });
        let alternative_containers = destinations.into_iter().skip(1).collect::<Vec<_>>();
        let structural_containers = completion_structural_containers(
            snapshot,
            &context,
            &parent_path,
            &property.key,
            transparent_wrapper,
            &primary.context,
            &primary.parent_path,
            &scope,
        );
        return semantic_completion_container(
            snapshot,
            hir,
            primary.context,
            primary.parent_path,
            structural_containers,
            alternative_containers,
            property.block.clone(),
            property.bare_values.clone(),
            primary.scope,
            position,
        );
    }
    let property = properties
        .into_iter()
        .find(|property| contains(property.range, position));
    SemanticCompletionContext {
        context,
        parent_path,
        structural_containers,
        alternative_containers,
        scope,
        property,
    }
}

pub(crate) fn semantic_completion_containers_equal(
    left: &SemanticCompletionContainer,
    right: &SemanticCompletionContainer,
) -> bool {
    left.context.eq_ignore_ascii_case(&right.context)
        && left.parent_path.len() == right.parent_path.len()
        && left
            .parent_path
            .iter()
            .zip(&right.parent_path)
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
        && left.scope == right.scope
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn completion_structural_containers(
    snapshot: &AnalysisSnapshot,
    context: &str,
    parent_path: &[String],
    property_key: &str,
    transparent_wrapper: bool,
    next_context: &str,
    next_path: &[String],
    scope: &ScopeContext,
) -> Vec<(String, Vec<String>)> {
    let mut structural_path = parent_path.to_vec();
    if !transparent_wrapper {
        structural_path.push(property_key.to_owned());
    }
    let destination_is_structural = next_context.eq_ignore_ascii_case(context)
        && next_path.len() == structural_path.len()
        && next_path
            .iter()
            .zip(&structural_path)
            .all(|(left, right)| left.eq_ignore_ascii_case(right));
    if destination_is_structural
        || semantic_rules_for_container(snapshot, context, &structural_path, scope).is_empty()
    {
        Vec::new()
    } else {
        vec![(context.to_owned(), structural_path)]
    }
}

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
