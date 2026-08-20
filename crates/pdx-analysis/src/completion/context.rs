use crate::semantic::*;
use crate::support::*;
use crate::{
    completion::macro_constraints::infer_macro_quoted_script_constraints,
    quoted_script::{QuotedScriptParse, QuotedScriptSession},
    types::{CancellationToken, Cancelled},
};
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
    pub(crate) container_property: Option<ScriptProperty>,
    pub(crate) property: Option<ScriptProperty>,
    pub(crate) quoted_depth: usize,
    pub(crate) embedded_value_context: Option<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SemanticCompletionContainer {
    pub(crate) context: String,
    pub(crate) parent_path: Vec<String>,
    pub(crate) scope: ScopeContext,
}

#[cfg(test)]
pub(crate) fn semantic_completion_context(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    position: TextSize,
) -> Option<SemanticCompletionContext> {
    crate::types::uncancelled(semantic_completion_context_with_cancellation(
        snapshot,
        input,
        position,
        &CancellationToken::new(),
    ))
}

pub(crate) fn semantic_completion_context_with_cancellation(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    position: TextSize,
    cancellation: &CancellationToken,
) -> Result<Option<SemanticCompletionContext>, Cancelled> {
    cancellation.checkpoint()?;
    let mut quoted_scripts = QuotedScriptSession::new(cancellation);
    let ParsedContent::Text(parsed) = &input.parsed;
    for root in script_properties(input, parsed.root()) {
        let Some(context) = semantic_root_context(snapshot, &root.key, input.path.as_ref()) else {
            continue;
        };
        let Some(block_range) = root.block_range else {
            continue;
        };
        // Inclusive end: a cursor directly after an unfinished block (`key = { ` at end of
        // file) sits on the half-open range end and must still resolve to the block.
        if position < block_range.start() || position > block_range.end() {
            continue;
        }
        let scope = semantic_initial_scope(snapshot, input, &context, &root.key, root.key_range);
        return semantic_completion_container(
            snapshot,
            input.hir.as_deref(),
            context,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            None,
            root.block,
            root.bare_values,
            scope,
            position,
            0,
            None,
            &mut quoted_scripts,
        )
        .map(Some);
    }
    Ok(None)
}

#[allow(clippy::too_many_arguments)] // Recursive traversal carries immutable HIR and semantic state.
pub(crate) fn semantic_completion_container(
    snapshot: &AnalysisSnapshot,
    hir: Option<&HirFile>,
    context: String,
    parent_path: Vec<String>,
    structural_containers: Vec<(String, Vec<String>)>,
    alternative_containers: Vec<SemanticCompletionContainer>,
    container_property: Option<ScriptProperty>,
    properties: Vec<ScriptProperty>,
    _bare_values: Vec<(String, TextRange)>,
    scope: ScopeContext,
    position: TextSize,
    quoted_depth: usize,
    embedded_value_context: Option<bool>,
    quoted_scripts: &mut QuotedScriptSession<'_>,
) -> Result<SemanticCompletionContext, Cancelled> {
    for property in &properties {
        let container_range = property.block_range.or_else(|| {
            property
                .quoted_source
                .as_ref()
                .and_then(|_| property.scalar.as_ref().map(|(_, range)| *range))
        });
        let Some(block_range) = container_range else {
            continue;
        };
        if !contains(block_range, position) {
            continue;
        }
        let transparent_wrapper = context.eq_ignore_ascii_case("trigger")
            && snapshot
                .game_profile()
                .is_transparent_scope_wrapper(&property.key);
        // Transition rules for this property come from the current container *and* the
        // structural containers. After a scope link such as `if` moves to its target, the
        // block's `parent_path` is reset, but clause rules like `limit` (parent_path
        // `["if"]`) live under the pushed structural path; without those, descending into
        // such a clause block yields no candidates. Each rule keeps the source path used to
        // compute its destination, mirroring the diagnostics structural/transition split.
        let next_rules = property_transition_rules(
            snapshot,
            &context,
            &parent_path,
            &structural_containers,
            &scope,
            property,
        );
        let next_rule_refs = next_rules.iter().map(|(rule, _)| *rule).collect::<Vec<_>>();
        let cached_child_fact = cached_scope_fact_for_property(
            snapshot,
            hir,
            &context,
            &parent_path,
            property,
            &next_rule_refs,
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
                Some(property.clone()),
                property.block.clone(),
                property.bare_values.clone(),
                scope_context_from_hir(snapshot.game_profile_handle(), &fact.state),
                position,
                quoted_depth,
                embedded_value_context,
                quoted_scripts,
            );
        }
        if property.block_range.is_none()
            && let Some(origin) = property.quoted_source.as_ref()
        {
            let argument_context = SemanticCompletionContext {
                context: context.clone(),
                parent_path: parent_path.clone(),
                structural_containers: structural_containers.clone(),
                alternative_containers: alternative_containers.clone(),
                scope: scope.clone(),
                container_property: container_property.clone(),
                property: Some(property.clone()),
                quoted_depth,
                embedded_value_context,
            };
            let inferred = infer_macro_quoted_script_constraints(
                snapshot,
                &argument_context,
                property,
                quoted_scripts.cancellation(),
            )?;
            if let Some(primary) = inferred.first()
                && let QuotedScriptParse::Parsed(script) =
                    quoted_scripts.parse(origin.source(), quoted_depth)?
                && let Some(decoded_position) = origin.decoded_position(&script, position)
            {
                let (quoted_properties, quoted_values) = quoted_script_container(&script, origin);
                let value_context =
                    script_line_value_context(script.parsed().source(), decoded_position);
                let alternatives = inferred
                    .iter()
                    .skip(1)
                    .map(|site| SemanticCompletionContainer {
                        context: site.context.clone(),
                        parent_path: site.parent_path.clone(),
                        scope: site.scope.clone(),
                    })
                    .collect();
                return semantic_completion_container(
                    snapshot,
                    None,
                    primary.context.clone(),
                    primary.parent_path.clone(),
                    Vec::new(),
                    alternatives,
                    Some(property.clone()),
                    quoted_properties,
                    quoted_values,
                    primary.scope.clone(),
                    position,
                    quoted_depth.saturating_add(1),
                    Some(value_context),
                    quoted_scripts,
                );
            }
        }
        let selected = semantic_transition_candidates(&next_rule_refs, None, property, &scope);
        let next_rules = next_rules
            .into_iter()
            .filter(|(rule, _)| selected.iter().any(|selected| selected.id == rule.id))
            .collect::<Vec<_>>();
        let quoted_script_rule = next_rules
            .iter()
            .find(|(rule, _)| matches!(rule.shape, RuleShape::QuotedScript))
            .map(|(rule, _)| *rule);
        let mut destinations = Vec::<SemanticCompletionContainer>::new();
        for (rule, source_path) in &next_rules {
            let (destination_context, destination_path) =
                rule.child_context.as_deref().map_or_else(
                    || {
                        let mut path = source_path.clone();
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
        let quoted_script = if quoted_script_rule.is_some()
            && property.block_range.is_none()
            && let Some(origin) = property.quoted_source.as_ref()
        {
            match quoted_scripts.parse(origin.source(), quoted_depth)? {
                QuotedScriptParse::Parsed(script) => Some((origin, script)),
                QuotedScriptParse::Opaque | QuotedScriptParse::Limited(_) => None,
            }
        } else {
            None
        };
        if let Some((origin, script)) = quoted_script
            && let Some(decoded_position) = origin.decoded_position(&script, position)
        {
            let (quoted_properties, quoted_values) = quoted_script_container(&script, origin);
            let value_context =
                script_line_value_context(script.parsed().source(), decoded_position);
            return semantic_completion_container(
                snapshot,
                None,
                primary.context,
                primary.parent_path,
                structural_containers,
                alternative_containers,
                Some(property.clone()),
                quoted_properties,
                quoted_values,
                primary.scope,
                position,
                quoted_depth.saturating_add(1),
                Some(value_context),
                quoted_scripts,
            );
        }
        return semantic_completion_container(
            snapshot,
            hir,
            primary.context,
            primary.parent_path,
            structural_containers,
            alternative_containers,
            Some(property.clone()),
            property.block.clone(),
            property.bare_values.clone(),
            primary.scope,
            position,
            quoted_depth,
            embedded_value_context,
            quoted_scripts,
        );
    }
    let property = properties.into_iter().find(|property| {
        contains(property.range, position)
            // A value position directly after an unfinished `key = ` sits on the half-open
            // end of the property range; accept it so the value completion still fires.
            || (position >= property.key_range.start() && property.range.end() == position)
    });
    Ok(SemanticCompletionContext {
        context,
        parent_path,
        structural_containers,
        alternative_containers,
        scope,
        container_property,
        property,
        quoted_depth,
        embedded_value_context,
    })
}

fn script_line_value_context(source: &str, position: TextSize) -> bool {
    let offset = usize::try_from(position)
        .unwrap_or(source.len())
        .min(source.len());
    let line_start = source[..offset].rfind('\n').map_or(0, |index| index + 1);
    let line = &source[line_start..offset];
    let equals = line.rfind('=');
    let open = line.rfind('{');
    equals.is_some_and(|equals| open.is_none_or(|open| equals > open))
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

/// Returns the transition rules that can drive one property's block, drawing on both the
/// current container and the structural containers carried by the enclosing block. After a
/// scope link such as `if` moves to its target, `parent_path` is reset, so clause rules like
/// `limit` (parent_path `["if"]`) are only reachable through the pushed structural path. Each
/// rule is paired with the source path used to compute its destination.
pub(crate) fn property_transition_rules<'rule>(
    snapshot: &'rule AnalysisSnapshot,
    context: &str,
    parent_path: &[String],
    structural_containers: &[(String, Vec<String>)],
    scope: &ScopeContext,
    property: &ScriptProperty,
) -> Vec<(&'rule pdx_rules::SemanticRule, Vec<String>)> {
    let mut out = Vec::new();
    let mut seen = Vec::new();
    for (source_context, source_path) in structural_containers
        .iter()
        .map(|(container_context, container_path)| {
            (container_context.as_str(), container_path.as_slice())
        })
        .chain(std::iter::once((context, parent_path)))
    {
        for rule in semantic_rules_for_container(snapshot, source_context, source_path, scope) {
            if matches!(rule.shape, RuleShape::LeafValue)
                || !semantic_rule_key_matches(snapshot, rule, source_path, &property.key)
                || !semantic_scope_allows(rule, scope)
                || seen.iter().any(|id| id == &rule.id)
            {
                continue;
            }
            seen.push(rule.id.clone());
            out.push((rule, source_path.to_vec()));
        }
    }
    out
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
