use std::collections::HashSet;
use std::sync::Arc;

use pdx_engine::hir::{HirFile, ScopeState, ScopeValue};
use pdx_engine::hir::{
    semantic_file_root_context as hir_semantic_file_root_context,
    semantic_root_context as hir_semantic_root_context,
    semantic_root_context_is_fallback as hir_semantic_root_context_is_fallback,
};
use pdx_engine::intern_shard_string;
use pdx_engine::{
    AnalysisSnapshot, DocumentId, DocumentSource, MacroDefinitionSummary, MacroParameterSignature,
    SourceFileId, SourceRootKind,
};
use pdx_rules::{GameProfile, KeyMatcher, RuleShape, ValueMatcher};
use pdx_text::{LogicalPath, TextRange};

use crate::support::*;
use crate::types::*;

/// Per-context rule lookup view shared by every container of one semantic context.
///
/// The merge (own context + inherited profile contexts + the `root:{type}` expansion) is a
/// pure function of the immutable snapshot, so the exact-key buckets and the non-exact list
/// are computed once per revision: per-key lookups become one hash probe plus a small
/// iteration instead of re-merging, sorting, and deduplicating per call.
/// One precomputed alternative group of one semantic context.
struct AlternativeGroup {
    /// Shared alternative identity.
    id: Arc<str>,
    /// Rule indices belonging to the group, rule-id ordered.
    rule_indices: Arc<[usize]>,
    /// Members of `rule_indices` that also declare no parent path; containers with an
    /// empty parent path match exactly these.
    top_rule_indices: Arc<[usize]>,
}

struct ContextRuleView {
    /// Whether more than one source contributed; merged sources return rule-id order while
    /// single sources keep exact-key rules before non-exact matchers.
    merged: bool,
    /// Every contributing rule index, rule-id ordered.
    all: Arc<[usize]>,
    /// Indices from `all` whose rule declares no parent path; top-level containers
    /// (parent path `[]`) match exactly these, so they skip the per-rule path filter and
    /// its full-size result allocation.
    all_top: Arc<[usize]>,
    /// Alternative groups in first-occurrence order: one entry per distinct alternative id
    /// among the contributing rules, each listing its rule indices in rule-id order.
    alternative_groups: Arc<[AlternativeGroup]>,
    /// Lowercased exact key -> `(rule index, group index)` pairs of alternative rules.
    alternative_exact_keys: rustc_hash::FxHashMap<Box<str>, Arc<[(usize, usize)]>>,
    /// `(rule index, group index)` pairs of alternative rules with non-exact key matchers.
    alternative_non_exact: Arc<[(usize, usize)]>,
    /// Lowercased exact key -> contributing rule indices, rule-id ordered per bucket.
    exact_by_key: rustc_hash::FxHashMap<Box<str>, Arc<[usize]>>,
    /// Indices of rules whose key matcher is not exact, rule-id ordered.
    non_exact: Arc<[usize]>,
}

fn context_rule_view(snapshot: &AnalysisSnapshot, context: &str) -> Arc<ContextRuleView> {
    let revision = snapshot.revision();
    let cache_key = format!("context-rule-view:{context}");
    if let Some(cached) = snapshot
        .query_cache()
        .get::<ContextRuleView>(revision, &cache_key)
    {
        return cached;
    }
    let rules = snapshot.rules();
    let mut merged = false;
    let mut all: Vec<usize> = Vec::new();
    let mut exact: rustc_hash::FxHashMap<Box<str>, Vec<usize>> = rustc_hash::FxHashMap::default();
    let mut non_exact: Vec<usize> = Vec::new();
    let mut push_source = |source: &str| {
        for index in rules.semantic_rule_indices_for_context(source) {
            let Some(rule) = rules.semantic_rule_at(index) else {
                continue;
            };
            all.push(index);
            match &rule.key {
                KeyMatcher::Exact(expected) => {
                    let key: Box<str> = expected.to_ascii_lowercase().into_boxed_str();
                    exact.entry(key).or_default().push(index);
                }
                _ => non_exact.push(index),
            }
        }
    };
    push_source(context);
    for inherited in snapshot.game_profile().inherited_semantic_contexts(context) {
        push_source(inherited);
        merged = true;
    }
    if let Some(type_name) = context.strip_prefix("type:") {
        let root_context = format!("root:{type_name}");
        push_source(&root_context);
        merged = true;
    }
    let dedup = |indices: &mut Vec<usize>| {
        // Rule indices ascend in rule-id order, so adjacent near-duplicate ids collapse here.
        let mut write = 0;
        for read in 0..indices.len() {
            let same = read > 0
                && rules
                    .semantic_rule_at(indices[write - 1])
                    .zip(rules.semantic_rule_at(indices[read]))
                    .is_some_and(|(left, right)| left.id.eq_ignore_ascii_case(&right.id));
            if same {
                continue;
            }
            indices[write] = indices[read];
            write += 1;
        }
        indices.truncate(write);
    };
    if merged {
        all.sort_unstable();
        dedup(&mut all);
        for bucket in exact.values_mut() {
            bucket.sort_unstable();
            dedup(bucket);
        }
        non_exact.sort_unstable();
        dedup(&mut non_exact);
    }
    let all_top: Vec<usize> = all
        .iter()
        .copied()
        .filter(|index| {
            rules
                .semantic_rule_at(*index)
                .is_some_and(|rule| rule.parent_path.is_empty())
        })
        .collect();
    let mut group_ids: Vec<Arc<str>> = Vec::new();
    let mut group_index_by_id = rustc_hash::FxHashMap::<Box<str>, usize>::default();
    let mut group_rules: Vec<Vec<usize>> = Vec::new();
    for index in all.iter().copied() {
        let Some(rule) = rules.semantic_rule_at(index) else {
            continue;
        };
        let Some(alternative) = rule.alternative_id.as_deref() else {
            continue;
        };
        // Group identity is the exact alternative spelling, mirroring the previous
        // case-sensitive `by_id` equality.
        let exact: Box<str> = Box::from(alternative);
        let group = match group_index_by_id.get(exact.as_ref()) {
            Some(group) => *group,
            None => {
                let group = group_ids.len();
                group_index_by_id.insert(exact, group);
                group_ids.push(intern_shard_string(alternative));
                group_rules.push(Vec::new());
                group
            }
        };
        group_rules[group].push(index);
    }
    // Discovery buckets over the alternative rules: exact-key rules hash straight to their
    // groups, non-exact matchers stay a small ordered scan.
    let mut alternative_exact_keys: rustc_hash::FxHashMap<Box<str>, Vec<(usize, usize)>> =
        rustc_hash::FxHashMap::default();
    let mut alternative_non_exact: Vec<(usize, usize)> = Vec::new();
    for (group_index, group) in group_rules.iter().enumerate() {
        for &rule_index in group {
            let Some(rule) = rules.semantic_rule_at(rule_index) else {
                continue;
            };
            match &rule.key {
                KeyMatcher::Exact(expected) => {
                    let key: Box<str> = expected.to_ascii_lowercase().into_boxed_str();
                    alternative_exact_keys
                        .entry(key)
                        .or_default()
                        .push((rule_index, group_index));
                }
                _ => alternative_non_exact.push((rule_index, group_index)),
            }
        }
    }
    let alternative_groups: Vec<AlternativeGroup> = group_ids
        .into_iter()
        .zip(group_rules)
        .map(|(id, rule_indices)| {
            let top_rule_indices = rule_indices
                .iter()
                .copied()
                .filter(|index| {
                    rules
                        .semantic_rule_at(*index)
                        .is_some_and(|rule| rule.parent_path.is_empty())
                })
                .collect::<Vec<_>>();
            AlternativeGroup {
                id,
                rule_indices: Arc::from(rule_indices),
                top_rule_indices: Arc::from(top_rule_indices),
            }
        })
        .collect();
    let view = Arc::new(ContextRuleView {
        merged,
        all: Arc::from(all),
        all_top: Arc::from(all_top),
        alternative_groups: Arc::from(alternative_groups),
        alternative_exact_keys: alternative_exact_keys
            .into_iter()
            .map(|(key, pairs)| (key, Arc::from(pairs)))
            .collect(),
        alternative_non_exact: Arc::from(alternative_non_exact),
        exact_by_key: exact
            .into_iter()
            .map(|(key, bucket)| (key, Arc::from(bucket)))
            .collect(),
        non_exact: Arc::from(non_exact),
    });
    snapshot.query_cache().insert(
        revision,
        pdx_engine::CacheDomain::Index,
        cache_key,
        view.clone(),
    );
    view
}

pub(crate) fn semantic_rules_for_container<'a>(
    snapshot: &'a AnalysisSnapshot,
    context: &str,
    parent_path: &[Arc<str>],
    _scope: &ScopeContext,
) -> Vec<&'a pdx_rules::SemanticRule> {
    let view = context_rule_view(snapshot, context);
    let rules = snapshot.rules();
    if parent_path.is_empty() {
        // Top-level containers match exactly the rules with no declared parent path.
        return view
            .all_top
            .iter()
            .filter_map(|index| rules.semantic_rule_at(*index))
            .collect();
    }
    view.all
        .iter()
        .filter_map(|index| rules.semantic_rule_at(*index))
        .filter(|rule| semantic_parent_path_matches(snapshot, &rule.parent_path, parent_path))
        .collect()
}

/// Returns the `LeafValue` rules of one container; used only to match bare values.
pub(crate) fn semantic_leaf_rules_for_container<'a>(
    snapshot: &'a AnalysisSnapshot,
    context: &str,
    parent_path: &[Arc<str>],
    scope: &ScopeContext,
) -> Vec<&'a pdx_rules::SemanticRule> {
    semantic_rules_for_container(snapshot, context, parent_path, scope)
        .into_iter()
        .filter(|rule| matches!(rule.shape, RuleShape::LeafValue))
        .collect()
}

/// Returns the container rules whose key can match `key`: the exact-key rules for `key` plus
/// every non-exact matcher in the context. Callers must still apply scope, path, and shape
/// filters, but they no longer scan every rule in large contexts (EU4 has ~1900 per context).
pub(crate) fn semantic_rules_for_container_key<'a>(
    snapshot: &'a AnalysisSnapshot,
    context: &str,
    parent_path: &[Arc<str>],
    key: &str,
) -> Vec<&'a pdx_rules::SemanticRule> {
    let view = context_rule_view(snapshot, context);
    let rules = snapshot.rules();
    let folded: std::borrow::Cow<'_, str> = if key.bytes().any(|byte| byte.is_ascii_uppercase()) {
        std::borrow::Cow::Owned(key.to_ascii_lowercase())
    } else {
        std::borrow::Cow::Borrowed(key)
    };
    let exact = view
        .exact_by_key
        .get(folded.as_ref())
        .map_or([].as_slice(), |bucket| bucket.as_ref());
    let mut candidates: Vec<usize> = Vec::with_capacity(exact.len() + view.non_exact.len());
    if view.merged {
        // Merged sources answer in rule-id order: merge the two id-ordered lists and drop
        // near-duplicate ids, exactly as the per-call merge used to after sorting.
        let (mut left, mut right) = (0, 0);
        while left < exact.len() || right < view.non_exact.len() {
            let take_left = right >= view.non_exact.len()
                || (left < exact.len() && exact[left] <= view.non_exact[right]);
            let index = if take_left {
                let index = exact[left];
                left += 1;
                index
            } else {
                let index = view.non_exact[right];
                right += 1;
                index
            };
            let same = candidates
                .last()
                .and_then(|previous| rules.semantic_rule_at(*previous))
                .zip(rules.semantic_rule_at(index))
                .is_some_and(|(previous, current)| previous.id.eq_ignore_ascii_case(&current.id));
            if !same {
                candidates.push(index);
            }
        }
    } else {
        // Single-source contexts keep exact-key rules ahead of the non-exact matchers.
        candidates.extend_from_slice(exact);
        candidates.extend_from_slice(&view.non_exact);
    }
    candidates
        .into_iter()
        .filter_map(|index| rules.semantic_rule_at(index))
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
    if let Some(type_name) = context.strip_prefix("type:") {
        if let Some(registers) = snapshot
            .rules()
            .type_root_scope_registers(type_name, root_key)
        {
            scope.root = initial_scope_register_value(&registers.root, None, None);
            scope.current = initial_scope_register_value(&registers.this, Some(&scope.root), None);
            scope.from = vec![initial_scope_register_value(
                &registers.from,
                Some(&scope.root),
                Some(&scope.current),
            )];
            return scope;
        }
        // Unknown/custom type roots keep the same conservative defaults as declared roots.
        scope.from.push(scope.root.clone());
        return scope;
    }
    if let Some(root_scope) = snapshot.game_profile().root_scope(root_key) {
        scope.root = intern_shard_string(root_scope);
        scope.current = scope.root.clone();
    }
    scope
}

fn initial_scope_register_value(
    expression: &str,
    root: Option<&str>,
    current: Option<&str>,
) -> Arc<str> {
    let expression = expression.trim();
    if expression.is_empty() || expression.eq_ignore_ascii_case("any") {
        return intern_shard_string("any");
    }
    if expression.eq_ignore_ascii_case("root") {
        return root.map_or_else(|| intern_shard_string("any"), intern_shard_string);
    }
    if expression.eq_ignore_ascii_case("this") {
        return current
            .or(root)
            .map_or_else(|| intern_shard_string("any"), intern_shard_string);
    }
    intern_shard_string(expression)
}

pub(crate) fn scope_context_from_hir(
    profile: Arc<GameProfile>,
    state: &ScopeState,
) -> ScopeContext {
    fn spelling(value: &ScopeValue) -> Arc<str> {
        match value {
            ScopeValue::Known(scopes) if scopes.len() == 1 => intern_shard_string(&scopes[0]),
            ScopeValue::Known(_) | ScopeValue::Unknown => intern_shard_string("any"),
            ScopeValue::Invalid => intern_shard_string("invalid"),
        }
    }
    ScopeContext {
        profile,
        root: spelling(&state.root),
        current: state
            .current
            .first()
            .map_or_else(|| intern_shard_string("any"), spelling),
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

pub(crate) fn semantic_root_context_is_fallback(
    snapshot: &AnalysisSnapshot,
    key: &str,
    logical_path: Option<&LogicalPath>,
) -> bool {
    hir_semantic_root_context_is_fallback(snapshot.rules(), logical_path, key)
}

pub(crate) fn semantic_file_root_context(
    snapshot: &AnalysisSnapshot,
    logical_path: Option<&LogicalPath>,
) -> Option<String> {
    hir_semantic_file_root_context(snapshot.rules(), logical_path)
}
pub(crate) struct CachedScopeFactInput<'data, 'hir> {
    pub(crate) snapshot: &'data AnalysisSnapshot,
    pub(crate) hir: Option<&'hir HirFile>,
    pub(crate) context: &'data str,
    pub(crate) parent_path: &'data [Arc<str>],
    pub(crate) property: &'data ScriptProperty,
    pub(crate) matching: &'data [&'hir pdx_rules::SemanticRule],
    pub(crate) selected_alternative: Option<&'data str>,
    pub(crate) scope: &'data ScopeContext,
    pub(crate) transparent_wrapper: bool,
}

pub(crate) fn cached_scope_fact_for_property<'hir>(
    input: CachedScopeFactInput<'_, 'hir>,
) -> Option<&'hir pdx_engine::hir::ScopeFact> {
    let fact = input
        .property
        .block
        .iter()
        .find_map(|child| input.hir.and_then(|hir| hir.scope_fact_at(child.key_range)))?;

    // HIR cannot inspect the workspace while lowering, so a cached dynamic transition is only
    // authoritative once analysis confirms the member. A missing index member is accepted only
    // when the first-party descriptor's negative/positive key filter proves it structurally.
    let mut transition_matching = input.matching.to_vec();
    if transition_matching.is_empty() {
        transition_matching = semantic_rules_for_container_key(
            input.snapshot,
            input.context,
            input.parent_path,
            &input.property.key,
        )
        .into_iter()
        .filter(|rule| {
            !matches!(rule.shape, RuleShape::LeafValue)
                && semantic_scope_allows(rule, input.scope)
                && match &rule.key {
                    KeyMatcher::Type(type_name) => {
                        match workspace_type_member(input.snapshot, type_name, &input.property.key)
                        {
                            WorkspaceTypeMember::Present => true,
                            WorkspaceTypeMember::Absent => false,
                            WorkspaceTypeMember::Unknown => type_member_provably_valid(
                                input.snapshot,
                                type_name,
                                &input.property.key,
                            ),
                        }
                    }
                    _ => false,
                }
        })
        .collect();
    }
    let selected = semantic_selected_transition(SemanticTransitionInput {
        snapshot: input.snapshot,
        matching: &transition_matching,
        selected_alternative: input.selected_alternative,
        context: input.context,
        parent_path: input.parent_path,
        property: input.property,
        scope: input.scope,
        transparent_wrapper: input.transparent_wrapper,
    })?;
    let (expected_context, expected_path) = semantic_transition_destination(
        selected,
        input.context,
        input.parent_path,
        &input.property.key,
        input.transparent_wrapper,
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

pub(crate) struct SemanticTransitionInput<'data, 'rule> {
    pub(crate) snapshot: &'data AnalysisSnapshot,
    pub(crate) matching: &'data [&'rule pdx_rules::SemanticRule],
    pub(crate) selected_alternative: Option<&'data str>,
    pub(crate) context: &'data str,
    pub(crate) parent_path: &'data [Arc<str>],
    pub(crate) property: &'data ScriptProperty,
    pub(crate) scope: &'data ScopeContext,
    pub(crate) transparent_wrapper: bool,
}

pub(crate) fn semantic_selected_transition<'rule>(
    input: SemanticTransitionInput<'_, 'rule>,
) -> Option<&'rule pdx_rules::SemanticRule> {
    let applicable = semantic_transition_candidates(
        input.matching,
        input.selected_alternative,
        input.property,
        input.scope,
    );
    if semantic_transitions_equivalent(&applicable) {
        return applicable.first().copied();
    }
    if input.property.block.is_empty() && input.property.bare_values.is_empty() {
        return None;
    }

    let mut structural_path = input.parent_path.to_vec();
    if !input.transparent_wrapper {
        structural_path.push(input.property.key.clone());
    }
    // Leaf-value rules are matched against bare values, which are not keys; build their lists
    // lazily because properties without bare values (the common case) never consult them.
    let has_bare_values = !input.property.bare_values.is_empty();
    let structural_leaf_rules = if has_bare_values {
        semantic_leaf_rules_for_container(
            input.snapshot,
            input.context,
            &structural_path,
            input.scope,
        )
    } else {
        Vec::new()
    };
    // Whether the structural container covers each block child is candidate-independent;
    // compute it once so the per-candidate filter only re-checks the destination container.
    let structural_child_checks = input
        .property
        .block
        .iter()
        .map(|child| {
            semantic_rules_for_container_key(
                input.snapshot,
                input.context,
                &structural_path,
                &child.key,
            )
            .iter()
            .any(|rule| {
                !matches!(rule.shape, RuleShape::LeafValue)
                    && semantic_rule_key_matches(input.snapshot, rule, &structural_path, &child.key)
            })
        })
        .collect::<Vec<_>>();
    let possible = applicable
        .iter()
        .copied()
        .filter(|candidate| {
            let (child_context, child_path) = semantic_transition_destination(
                candidate,
                input.context,
                input.parent_path,
                &input.property.key,
                input.transparent_wrapper,
            );
            let child_scope = semantic_child_scope(input.snapshot, input.scope, candidate);
            let child_leaf_rules = if has_bare_values {
                semantic_leaf_rules_for_container(
                    input.snapshot,
                    &child_context,
                    &child_path,
                    &child_scope,
                )
            } else {
                Vec::new()
            };
            input
                .property
                .block
                .iter()
                .zip(&structural_child_checks)
                .all(|(child, structural_ok)| {
                    *structural_ok
                        || semantic_rules_for_container_key(
                            input.snapshot,
                            &child_context,
                            &child_path,
                            &child.key,
                        )
                        .iter()
                        .any(|rule| {
                            !matches!(rule.shape, RuleShape::LeafValue)
                                && semantic_rule_key_matches(
                                    input.snapshot,
                                    rule,
                                    &child_path,
                                    &child.key,
                                )
                        })
                })
                && input.property.bare_values.iter().all(|(value, _)| {
                    structural_leaf_rules.iter().any(|rule| {
                        semantic_leaf_value_matches(input.snapshot, rule, value, input.scope)
                    }) || child_leaf_rules.iter().any(|rule| {
                        semantic_leaf_value_matches(input.snapshot, rule, value, &child_scope)
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
    parent_path: &[Arc<str>],
    property_key: &str,
    transparent_wrapper: bool,
) -> (Arc<str>, Vec<Arc<str>>) {
    rule.child_context.as_deref().map_or_else(
        || {
            let mut child_path = parent_path.to_vec();
            if !transparent_wrapper {
                child_path.push(intern_shard_string(property_key));
            }
            (intern_shard_string(context), child_path)
        },
        |child_context| (intern_shard_string(child_context), Vec::new()),
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
    context: &str,
    parent_path: &[Arc<str>],
    properties: &[&ScriptProperty],
    bare_values: &[&(Arc<str>, TextRange)],
    scope: &ScopeContext,
) -> Option<String> {
    // Alternatives are grouped in first-occurrence order; groups are small (one to three
    // rules), while the container holds hundreds of rules and alternatives. The context
    // view precomputes the grouping, so per-container work reduces to a parent-path
    // filter plus key matching against the handful of alternative rules.
    let view = context_rule_view(snapshot, context);
    let engine_rules = snapshot.rules();
    // Parent-path filter shared by grouping, discovery, and scoring: a rule participates
    // exactly when the container's merged rule list (`rules`) would have contained it.
    let parent_ok = |rule: &pdx_rules::SemanticRule| {
        semantic_parent_path_matches(snapshot, &rule.parent_path, parent_path)
    };
    // Group members that survive the container's parent-path filter, in group order; a
    // group with no surviving member stays empty, matching the previous inline grouping
    // that would never have created it.
    // For containers with an empty parent path the surviving members are exactly the
    // group's top-level rules; deeper paths check parent paths on demand.
    let top_level = parent_path.is_empty();
    let mut non_empty_groups: Vec<usize> = Vec::new();
    for (index, group) in view.alternative_groups.iter().enumerate() {
        let has_member = if top_level {
            !group.top_rule_indices.is_empty()
        } else {
            group.rule_indices.iter().any(|rule_index| {
                engine_rules
                    .semantic_rule_at(*rule_index)
                    .is_some_and(&parent_ok)
            })
        };
        if has_member {
            non_empty_groups.push(index);
        }
    }

    // Discover reachable groups by matching alternative rules against property keys. This
    // is the same membership predicate as a per-property key lookup (`rules` already
    // carries the container's parent-path filter), but it visits only the alternative
    // rules instead of scanning the context's non-exact matchers for every property.
    let mut relevant = Vec::<usize>::new();
    let mut seen = std::collections::BTreeSet::<usize>::new();
    fn folded_key(key: &str) -> std::borrow::Cow<'_, str> {
        if key.bytes().any(|byte| byte.is_ascii_uppercase()) {
            std::borrow::Cow::Owned(key.to_ascii_lowercase())
        } else {
            std::borrow::Cow::Borrowed(key)
        }
    }
    for property in properties {
        // Exact-key alternative rules resolve through the bucket; non-exact alternative
        // matchers stay a small ordered scan. Top-level containers consult only rules the
        // precomputed top membership retains.
        let exact = view
            .alternative_exact_keys
            .get(folded_key(&property.key).as_ref());
        for (rule_index, group_index) in exact
            .map_or(&[][..], |pairs| pairs.as_ref())
            .iter()
            .copied()
            .chain(view.alternative_non_exact.iter().copied())
        {
            let Some(rule) = engine_rules.semantic_rule_at(rule_index) else {
                continue;
            };
            if matches!(rule.shape, RuleShape::LeafValue) || !parent_ok(rule) {
                continue;
            }
            if semantic_rule_key_matches(snapshot, rule, parent_path, &property.key)
                && seen.insert(group_index)
            {
                relevant.push(group_index);
            }
        }
    }
    for (value, _) in bare_values {
        for rule in rules.iter().copied().filter(|rule| {
            matches!(rule.shape, RuleShape::LeafValue)
                && semantic_leaf_value_matches(snapshot, rule, value, scope)
        }) {
            if let Some(alternative) = rule.alternative_id.as_deref()
                && let Some(group) = view
                    .alternative_groups
                    .iter()
                    .position(|group| group.id.as_ref() == alternative)
                // A group whose members were all removed by the parent-path filter would
                // not have existed in the previous inline grouping.
                && non_empty_groups.contains(&group)
                && seen.insert(group)
            {
                relevant.push(group);
            }
        }
    }
    let mut best: Option<((usize, usize), usize)> = None;
    let mut tied = false;
    for index in relevant {
        let group = &view.alternative_groups[index];
        let group_member = |rule_index: &usize| {
            engine_rules
                .semantic_rule_at(*rule_index)
                .filter(|rule| parent_ok(rule))
        };
        let member_indices: &[usize] = if top_level {
            group.top_rule_indices.as_ref()
        } else {
            group.rule_indices.as_ref()
        };
        let members = member_indices.iter().filter_map(group_member);
        let mut present = 0_usize;
        let mut valid = 0_usize;
        for property in properties {
            let mut any_match = false;
            let mut any_valid = false;
            for rule in members.clone().filter(|rule| {
                !matches!(rule.shape, RuleShape::LeafValue)
                    && semantic_rule_key_matches(snapshot, rule, parent_path, &property.key)
            }) {
                any_match = true;
                if semantic_scope_allows(rule, scope)
                    && semantic_property_matches(snapshot, rule, property, scope)
                {
                    any_valid = true;
                    break;
                }
            }
            if any_match {
                present += 1;
            }
            if any_valid {
                valid += 1;
            }
        }
        valid += bare_values
            .iter()
            .filter(|(value, _)| {
                members.clone().any(|rule| {
                    matches!(rule.shape, RuleShape::LeafValue)
                        && semantic_leaf_value_matches(snapshot, rule, value, scope)
                })
            })
            .count();
        let score = (valid, present);
        match best.as_ref() {
            None => {
                best = Some((score, index));
                tied = false;
            }
            Some((current, _)) if score > *current => {
                best = Some((score, index));
                tied = false;
            }
            Some((current, _)) if score == *current => tied = true,
            Some(_) => {}
        }
    }
    if tied {
        None
    } else if let Some((_, index)) = best {
        Some(view.alternative_groups[index].id.to_string())
    } else if non_empty_groups.len() == 1 {
        // With no matching content the single alternative is selected by default, matching the
        // previous full-scan behavior where one all-zero alternative still won.
        Some(view.alternative_groups[non_empty_groups[0]].id.to_string())
    } else {
        None
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
        KeyMatcher::Date => "date".to_owned(),
        KeyMatcher::Dynamic(value) => format!("value_set[{value}]"),
    }
}

pub(crate) fn semantic_parent_path_matches(
    snapshot: &AnalysisSnapshot,
    expected: &[String],
    actual: &[Arc<str>],
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
            } else if expected.eq_ignore_ascii_case("int") {
                actual.parse::<i64>().is_ok()
            } else if expected.eq_ignore_ascii_case("float") {
                actual.parse::<f64>().is_ok()
            } else if expected.eq_ignore_ascii_case("date_field") {
                ValueMatcher::Date.matches(actual, |_, _| false, |_, _| false, |_, _| false)
            } else {
                expected.eq_ignore_ascii_case(actual)
            }
        })
}

pub(crate) fn semantic_rule_severity<'a>(
    rules: impl IntoIterator<Item = &'a pdx_rules::SemanticRule>,
    fallback: DiagnosticCode,
) -> Severity {
    rules
        .into_iter()
        .filter_map(|rule| rule.severity)
        .min()
        .map(Severity::from_rule_number)
        .unwrap_or_else(|| fallback.severity())
}

/// Selects the severity for a bare value that did not match any leaf-value rule.
///
/// An unmatched bare value is normally an error even when a rule carries a softer severity:
/// the value is not understood by the semantic contract. The one intentional exception is a
/// numeric value outside an explicitly bounded numeric rule. EU4 clamps those values at runtime
/// (the first-party colour rules use this to keep Vanilla files usable), so their configured
/// warning/info severity remains meaningful.
pub(crate) fn semantic_bare_value_severity<'a>(
    rules: impl IntoIterator<Item = &'a pdx_rules::SemanticRule>,
    value: &str,
) -> Severity {
    let leaf_rules = rules
        .into_iter()
        .filter(|rule| matches!(rule.shape, RuleShape::LeafValue))
        .collect::<Vec<_>>();
    let soft_severities = leaf_rules
        .iter()
        .filter_map(|rule| {
            let severity = rule.severity.filter(|severity| *severity > 1)?;
            numeric_range_overflow(&rule.value, value).then_some(severity)
        })
        .collect::<Vec<_>>();
    let has_hard_mismatch = leaf_rules.iter().any(|rule| {
        !numeric_range_overflow(&rule.value, value)
            || rule.severity.is_none_or(|severity| severity <= 1)
    });
    if !has_hard_mismatch {
        soft_severities
            .into_iter()
            .min()
            .map(Severity::from_rule_number)
            .unwrap_or(Severity::Error)
    } else {
        DiagnosticCode::UnknownBareValue.severity()
    }
}

/// Selects the diagnostic category for an unmatched bare value. Values outside an explicit
/// numeric range are known values with a bounded violation; all other unmatched scalars are
/// unknown bare values and therefore hard errors.
pub(crate) fn semantic_bare_value_code<'a>(
    rules: impl IntoIterator<Item = &'a pdx_rules::SemanticRule>,
    value: &str,
) -> DiagnosticCode {
    let has_numeric_overflow = rules
        .into_iter()
        .filter(|rule| matches!(rule.shape, RuleShape::LeafValue))
        .any(|rule| numeric_range_overflow(&rule.value, value));
    if has_numeric_overflow {
        DiagnosticCode::InvalidValue
    } else {
        DiagnosticCode::UnknownBareValue
    }
}

fn numeric_range_overflow(matcher: &ValueMatcher, value: &str) -> bool {
    match matcher {
        ValueMatcher::Int { min, max } => {
            let Ok(value) = value.parse::<i64>() else {
                return false;
            };
            min.is_some_and(|min| value < min) || max.is_some_and(|max| value > max)
        }
        ValueMatcher::Float { min, max } => {
            let Ok(value) = value.parse::<f64>() else {
                return false;
            };
            let lower = min.as_deref().and_then(|min| min.parse::<f64>().ok());
            let upper = max.as_deref().and_then(|max| max.parse::<f64>().ok());
            lower.is_some_and(|min| value < min) || upper.is_some_and(|max| value > max)
        }
        _ => false,
    }
}

pub(crate) fn semantic_min_cardinality_severity(rule: &pdx_rules::SemanticRule) -> Severity {
    if !rule.strict_min {
        Severity::Warning
    } else {
        rule.severity
            .map(Severity::from_rule_number)
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
    if let Some(push_scope) = &rule.push_scope {
        child.previous.insert(0, child.current.clone());
        if push_scope.eq_ignore_ascii_case("any") {
            child.current = intern_shard_string("any");
        } else {
            child.current = intern_shard_string(push_scope);
        }
    }
    for (register, value) in &rule.replace_scope {
        let value = resolve_scope_expression_context(snapshot, &child, value);
        let register = register.to_ascii_lowercase().replace('_', "");
        match register.as_str() {
            "root" => child.root = value,
            "this" => {
                child.previous.insert(0, child.current.clone());
                child.current = value;
            }
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
) -> Arc<str> {
    if expression.contains('.') {
        let mut segments = expression.split('.');
        let Some(first) = segments.next() else {
            return intern_shard_string("any");
        };
        let mut value = resolve_scope_expression_context(snapshot, context, first);
        for segment in segments {
            value = resolve_scope_link_context(snapshot, context, &value, segment)
                .unwrap_or_else(|| intern_shard_string("any"));
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
            .unwrap_or_else(|| intern_shard_string("any"));
    }
    if let Some(depth) = repeated_scope_register_depth(&lowered, "previous")
        .or_else(|| repeated_scope_register_depth(&lowered, "prev"))
    {
        return context
            .previous
            .get(depth)
            .cloned()
            .unwrap_or_else(|| intern_shard_string("any"));
    }

    let rules = snapshot.rules();
    let link_expression = rules
        .exact_semantic_rule_indices(expression)
        .iter()
        .any(|index| {
            rules.semantic_rule_is_effect_or_trigger(*index)
                && rules
                    .semantic_rule_at(*index)
                    .is_some_and(|rule| rule.push_scope.is_some())
        });
    if let Some(target) =
        resolve_scope_link_context(snapshot, context, &context.current, expression)
    {
        return target;
    }
    if expression.eq_ignore_ascii_case("any") || link_expression {
        intern_shard_string("any")
    } else if context.profile.is_scope(expression) {
        intern_shard_string(expression)
    } else {
        intern_shard_string("any")
    }
}

pub(crate) fn resolve_scope_link_context(
    snapshot: &AnalysisSnapshot,
    context: &ScopeContext,
    current: &str,
    expression: &str,
) -> Option<Arc<str>> {
    let rules = snapshot.rules();
    let mut targets = rules
        .exact_semantic_rule_indices(expression)
        .iter()
        .filter_map(|index| {
            let rule = rules.semantic_rule_at(*index)?;
            if !rules.semantic_rule_is_effect_or_trigger(*index)
                || !rule.allowed_scopes.is_empty()
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
                .map(intern_shard_string)
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

pub(crate) fn set_scope_register(registers: &mut Vec<Arc<str>>, depth: usize, value: &Arc<str>) {
    if registers.len() <= depth {
        registers.resize(depth + 1, intern_shard_string("any"));
    }
    registers[depth] = value.clone();
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
    parent_path: &[Arc<str>],
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
    parent_path: &[Arc<str>],
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
    // The resolution scans every open overlay document, and it is invoked once per
    // (property, rule) during diagnostics, completion, and hover. Memoize per
    // (revision, kind, name) so a revision pays for the scan only once.
    let revision = snapshot.revision();
    let key = format!("macro-definition:{owner_kind}:{owner_name}");
    if let Some(cached) = snapshot
        .query_cache()
        .get::<Option<ResolvedMacroDefinition>>(revision, &key)
    {
        return cached.as_ref().clone();
    }
    let resolved = resolve_macro_definition_uncached(snapshot, owner_kind, owner_name);
    snapshot.query_cache().insert(
        revision,
        pdx_engine::CacheDomain::Documents,
        key,
        Arc::new(resolved.clone()),
    );
    resolved
}

fn resolve_macro_definition_uncached(
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

/// Builds the canonical invocation snippet for a resolved scripted macro signature. Snippet bodies
/// use relative indentation only; the client re-indents multi-line snippets to the insertion line.
pub(crate) fn scripted_definition_snippet(
    snapshot: &AnalysisSnapshot,
    kind_name: &str,
    definition_name: &str,
) -> String {
    let Some(summary) = macro_definition_summary(snapshot, kind_name, definition_name) else {
        return format!("{definition_name} = {{\n\t$0\n}}");
    };
    if summary.parameters.is_empty() {
        return format!("{definition_name} = yes");
    }
    let inner_indent = "\t";
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
    format!("{definition_name} = {{\n{body}{inner_indent}$0\n}}")
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
    // Runtime value references such as `variable:name` are untyped at load time, so any
    // value rule accepts them once the property structure itself matched.
    if snapshot.game_profile().is_dynamic_value_reference(value) {
        return true;
    }
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
        || (matches!(rule.value, ValueMatcher::Type(_))
            && scope_member(snapshot, None, value, scope_context))
}

/// Classification used by diagnostics when a rule's value matcher is scope-based.
///
/// The ordinary matcher remains a boolean API for rule applicability, while this result keeps
/// enough information to distinguish a misspelled scope from a valid target in the wrong scope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ScopeValueMatch {
    NotScopeRule,
    Known {
        actual: Arc<str>,
        expected: Option<String>,
        compatible: bool,
    },
    Dynamic,
    Unresolved,
    Unknown,
}

pub(crate) fn semantic_scope_value_match(
    snapshot: &AnalysisSnapshot,
    rule: &pdx_rules::SemanticRule,
    value: &str,
    scope_context: &ScopeContext,
) -> ScopeValueMatch {
    let ValueMatcher::Scope(expected) = &rule.value else {
        return ScopeValueMatch::NotScopeRule;
    };
    match resolve_scope_member(snapshot, value, scope_context) {
        ScopeResolution::Known { scope: actual } => ScopeValueMatch::Known {
            compatible: expected
                .as_deref()
                .is_none_or(|expected| scope_context.profile.scopes_compatible(&actual, expected)),
            actual,
            expected: expected.clone(),
        },
        ScopeResolution::Dynamic => ScopeValueMatch::Dynamic,
        ScopeResolution::Unresolved => ScopeValueMatch::Unresolved,
        ScopeResolution::Unknown => ScopeValueMatch::Unknown,
    }
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
            property.block_range.is_some()
                && (!property.bare_values.is_empty() || rule.min_occurs.unwrap_or(0) == 0)
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
        property.scalar.as_ref().map(|(value, _)| value.as_ref()),
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
        // EU4 variables may be introduced by runtime effects, scripted macro arguments, and
        // define-style constants that are not enumerable from a workspace snapshot.
        return !value.is_empty();
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
    // Scope expressions and scope keywords are valid dynamic members at runtime
    // (for example `kill_mercenary_leader = THIS` or `set_ruler = ROOT`).
    if scope_member(snapshot, None, value, scope_context) {
        return true;
    }
    if snapshot.game_profile().is_open_world_value_kind(&kind) {
        return !value.is_empty();
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
    workspace_member_names_cached(snapshot, type_name)
        .as_ref()
        .clone()
}

/// Returns the workspace member names for one type as a shared snapshot-owned vector.
///
/// The source index and live overlays are immutable for a snapshot revision.  Keeping this
/// normalized name vector in the snapshot query cache avoids rewalking every definition for each
/// completion request and gives the prefix index below one stable backing allocation.
fn workspace_member_names_cached(snapshot: &AnalysisSnapshot, type_name: &str) -> Arc<Vec<String>> {
    let revision = snapshot.revision();
    let key = format!("workspace-member-names:{}", type_name.to_ascii_lowercase());
    if let Some(cached) = snapshot.query_cache().get::<Vec<String>>(revision, &key) {
        return cached;
    }
    let names = Arc::new(effective_workspace_member_names_uncached(
        snapshot, type_name,
    ));
    snapshot.query_cache().insert(
        revision,
        pdx_engine::CacheDomain::Index,
        key,
        Arc::clone(&names),
    );
    names
}

fn effective_workspace_member_names_uncached(
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
                    definition.active
                        && !hidden_files.contains(&definition.file_id)
                        && completion_source_file_allowed(snapshot, definition.file_id)
                })
                .map(|definition| definition.name.to_string()),
        );
    }
    for document in snapshot
        .documents()
        .values()
        .filter(|document| document.source() == DocumentSource::Overlay)
    {
        if !completion_overlay_allowed(snapshot) {
            break;
        }
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
                .map(|definition| definition.name.to_string()),
        );
    }
    names.sort_by_key(|name| name.to_ascii_lowercase());
    names.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    names
}

/// Prefix/sub-string lookup over the immutable workspace member names for one snapshot.
///
/// CWTools-rs uses a sorted compact localisation index for the same workload.  This variant
/// keeps the existing completion contract (case-insensitive prefix *or* contiguous substring)
/// while making the common prefix bucket a binary search and rejecting most fallback candidates
/// with a cheap character mask.
pub(crate) struct WorkspaceMemberIndex {
    names: Arc<Vec<String>>,
    masks: Vec<u64>,
}

impl WorkspaceMemberIndex {
    fn new(names: Arc<Vec<String>>) -> Self {
        let masks = names
            .iter()
            .map(|name| member_char_mask(name, u64::MAX))
            .collect();
        Self { names, masks }
    }

    pub(crate) fn select(&self, prefix: &str) -> Vec<String> {
        if prefix.is_empty() {
            return self.names.as_ref().clone();
        }
        let folded = prefix.to_ascii_lowercase();
        let start = self
            .names
            .partition_point(|name| name.to_ascii_lowercase().as_str() < folded.as_str());
        let mut selected = Vec::new();
        for name in self.names.iter().skip(start) {
            if !starts_with_ignore_ascii_case(name, prefix) {
                break;
            }
            selected.push(name.clone());
        }
        let needle = member_char_mask(prefix, 0);
        for (index, mask) in self.masks.iter().enumerate() {
            if mask & needle != needle {
                continue;
            }
            let name = &self.names[index];
            if starts_with_ignore_ascii_case(name, prefix)
                || !contains_ignore_ascii_case(name, prefix)
            {
                continue;
            }
            selected.push(name.clone());
        }
        selected.sort_by_key(|name| name.to_ascii_lowercase());
        selected.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
        selected
    }
}

/// Returns the shared name index for one type and snapshot revision.
pub(crate) fn workspace_member_index(
    snapshot: &AnalysisSnapshot,
    type_name: &str,
) -> Arc<WorkspaceMemberIndex> {
    let revision = snapshot.revision();
    let key = format!("workspace-member-index:{}", type_name.to_ascii_lowercase());
    if let Some(cached) = snapshot
        .query_cache()
        .get::<WorkspaceMemberIndex>(revision, &key)
    {
        return cached;
    }
    let index = Arc::new(WorkspaceMemberIndex::new(workspace_member_names_cached(
        snapshot, type_name,
    )));
    snapshot.query_cache().insert(
        revision,
        pdx_engine::CacheDomain::Index,
        key,
        Arc::clone(&index),
    );
    index
}

fn contains_ignore_ascii_case(value: &str, needle: &str) -> bool {
    !needle.is_empty()
        && value
            .as_bytes()
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

fn member_char_mask(text: &str, non_ascii: u64) -> u64 {
    if !text.is_ascii() {
        return non_ascii;
    }
    text.bytes().fold(0_u64, |mask, byte| {
        mask | 1_u64 << (byte.to_ascii_lowercase() & 63)
    })
}

/// Compact prefix/sub-string index for the localisation namespace.
///
/// Localisation keys are commonly the largest symbol set in an EU4 workspace (Vanilla plus a
/// mod can contain hundreds of thousands of entries).  Keeping the labels in one contiguous
/// blob avoids the pointer chasing of a `Vec<String>` during the fallback substring sweep.  The
/// index is deliberately query-only: labels remain owned by the snapshot's ordinary workspace
/// index, and a corrupt/missing cache can always rebuild this derived view.
pub(crate) struct LocalisationKeyIndex {
    blob: String,
    offsets: Vec<usize>,
    masks: Vec<u64>,
}

impl LocalisationKeyIndex {
    fn new(names: &[String]) -> Self {
        let mut sorted = names.iter().map(String::as_str).collect::<Vec<_>>();
        sorted.sort_by(|left, right| {
            left.to_ascii_lowercase()
                .cmp(&right.to_ascii_lowercase())
                .then_with(|| left.cmp(right))
        });
        sorted.dedup_by(|left, right| left.eq_ignore_ascii_case(right));

        let capacity = sorted.iter().map(|name| name.len()).sum();
        let mut blob = String::with_capacity(capacity);
        let mut offsets = Vec::with_capacity(sorted.len() + 1);
        let mut masks = Vec::with_capacity(sorted.len());
        for name in sorted {
            offsets.push(blob.len());
            masks.push(member_char_mask(name, u64::MAX));
            blob.push_str(name);
        }
        offsets.push(blob.len());
        Self {
            blob,
            offsets,
            masks,
        }
    }

    fn len(&self) -> usize {
        self.offsets.len().saturating_sub(1)
    }

    fn key(&self, index: usize) -> &str {
        &self.blob[self.offsets[index]..self.offsets[index + 1]]
    }

    fn prefix_start(&self, prefix: &str) -> usize {
        let folded = prefix.to_ascii_lowercase();
        let (mut low, mut high) = (0, self.len());
        while low < high {
            let middle = low + (high - low) / 2;
            if self.key(middle).to_ascii_lowercase() < folded {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        low
    }

    /// Selects every key matching the existing completion contract.  The cancellable variant
    /// checks once per small batch, so a large localisation namespace cannot make an obsolete
    /// request run to completion before the worker notices cancellation.
    pub(crate) fn select_with_cancellation(
        &self,
        prefix: &str,
        cancellation: &CancellationToken,
    ) -> Result<Vec<String>, Cancelled> {
        if prefix.is_empty() {
            let mut names = Vec::with_capacity(self.len());
            for index in 0..self.len() {
                if index & 255 == 0 {
                    cancellation.checkpoint()?;
                }
                names.push(self.key(index).to_owned());
            }
            return Ok(names);
        }

        let mut selected = Vec::new();
        let start = self.prefix_start(prefix);
        for index in start..self.len() {
            if index & 255 == 0 {
                cancellation.checkpoint()?;
            }
            let key = self.key(index);
            if !starts_with_ignore_ascii_case(key, prefix) {
                break;
            }
            selected.push(key.to_owned());
        }

        let needle = member_char_mask(prefix, 0);
        for (index, mask) in self.masks.iter().enumerate() {
            if index & 255 == 0 {
                cancellation.checkpoint()?;
            }
            if mask & needle != needle {
                continue;
            }
            let key = self.key(index);
            if starts_with_ignore_ascii_case(key, prefix)
                || !contains_ignore_ascii_case(key, prefix)
            {
                continue;
            }
            selected.push(key.to_owned());
        }
        selected.sort_by_key(|name| name.to_ascii_lowercase());
        selected.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
        Ok(selected)
    }
}

/// Returns the compact localisation-key index for this immutable snapshot revision.
pub(crate) fn localisation_key_index(snapshot: &AnalysisSnapshot) -> Arc<LocalisationKeyIndex> {
    let revision = snapshot.revision();
    let key = "localisation-key-index";
    if let Some(cached) = snapshot
        .query_cache()
        .get::<LocalisationKeyIndex>(revision, key)
    {
        return cached;
    }
    let names = workspace_member_names_cached(snapshot, "localisation");
    let index = Arc::new(LocalisationKeyIndex::new(names.as_ref()));
    snapshot.query_cache().insert(
        revision,
        pdx_engine::CacheDomain::Index,
        key.to_owned(),
        Arc::clone(&index),
    );
    index
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

/// Per-revision view of every definition identity visible to membership queries.
///
/// `workspace_member` is called once per (rule, property) pair — hundreds of times per
/// document. The previous per-call memo key (`format!` + two lowercase strings + a global
/// cache probe per query) cost more than the membership check itself. The index-derived
/// name counts live in the Index cache domain; definitions hidden by open overlays are
/// tracked separately in the Documents domain so overlay edits stay correct without
/// rebuilding the workspace-wide set on every keystroke.
pub(crate) struct WorkspaceMembership {
    /// Definition kind spelling as stored by the workspace index.
    kinds: rustc_hash::FxHashMap<Box<str>, rustc_hash::FxHashMap<Box<str>, u32>>,
    /// Lowercased kind -> name suffixes probed in addition to the raw member spelling.
    suffixes: rustc_hash::FxHashMap<Box<str>, Vec<Box<str>>>,
}

/// `(kind, folded name)` definition counts hidden by open overlays, per document revision.
struct OverlayHiddenCounts(rustc_hash::FxHashMap<Box<str>, rustc_hash::FxHashMap<Box<str>, u32>>);

fn overlay_hidden_counts(snapshot: &AnalysisSnapshot) -> Arc<OverlayHiddenCounts> {
    let revision = snapshot.revision();
    let cache_key = "workspace-membership-hidden";
    if let Some(cached) = snapshot
        .query_cache()
        .get::<OverlayHiddenCounts>(revision, cache_key)
    {
        return cached;
    }
    let mut counts = OverlayHiddenCounts(rustc_hash::FxHashMap::default());
    for file_id in overlay_file_ids(snapshot) {
        let Some(shard) = snapshot.index().shard(file_id) else {
            continue;
        };
        for definition in &shard.definitions {
            if !definition.active || !completion_source_file_allowed(snapshot, file_id) {
                continue;
            }
            let folded = snapshot
                .index()
                .definition_name_key(&definition.kind, &definition.name);
            counts
                .0
                .entry(Box::from(definition.kind.as_ref()))
                .or_default()
                .entry(Box::from(folded.as_ref()))
                .and_modify(|count| *count += 1)
                .or_insert(1);
        }
    }
    let counts = Arc::new(counts);
    snapshot.query_cache().insert(
        revision,
        pdx_engine::CacheDomain::Documents,
        cache_key.to_owned(),
        counts.clone(),
    );
    counts
}

impl WorkspaceMembership {
    /// Whether one `(kind, name)` pair has a definition not hidden by an overlay.
    fn indexed(&self, hidden: &OverlayHiddenCounts, kind: &str, folded: &str) -> bool {
        let Some(total) = self
            .kinds
            .get::<str>(kind)
            .and_then(|names| names.get::<str>(folded))
        else {
            return false;
        };
        let hidden_count = hidden
            .0
            .get::<str>(kind)
            .and_then(|names| names.get::<str>(folded))
            .copied()
            .unwrap_or(0);
        *total > hidden_count
    }

    fn contains(&self, snapshot: &AnalysisSnapshot, type_name: &str, member: &str) -> bool {
        let profile = snapshot.game_profile();
        let base = type_name
            .split_once('.')
            .map_or(type_name, |(kind, _)| kind);
        // The kind set matches `workspace_member_kinds` up to case-insensitive dedup; the
        // boolean result is order-insensitive, so no sorted materialisation is needed.
        let mut kinds: Vec<&str> = vec![type_name];
        if base != type_name {
            kinds.push(base);
        }
        if let Some(alias) = profile.member_kind_alias(base)
            && !kinds.iter().any(|kind| kind.eq_ignore_ascii_case(alias))
        {
            kinds.push(alias);
        }
        let mut names = vec![member.to_owned()];
        for kind in &kinds {
            let kind_lower = kind.to_ascii_lowercase();
            if let Some(suffixes) = self.suffixes.get(kind_lower.as_str()) {
                for suffix in suffixes {
                    names.push(format!("{member}{suffix}"));
                }
            }
        }
        let hidden = overlay_hidden_counts(snapshot);
        if names.iter().any(|name| {
            kinds.iter().any(|kind| {
                let folded = snapshot.index().definition_name_key(kind, name);
                self.indexed(&hidden, kind, folded.as_ref())
            })
        }) {
            return true;
        }
        if !completion_overlay_allowed(snapshot) {
            return false;
        }
        let members = overlay_members(snapshot);
        if members.names.is_empty() {
            return false;
        }
        names.iter().any(|name| {
            kinds.iter().any(|kind| {
                members
                    .names
                    .contains(&(kind.to_ascii_lowercase(), name.to_ascii_lowercase()))
            })
        })
    }
}

/// Returns the shared membership view for this snapshot revision.
fn workspace_membership(snapshot: &AnalysisSnapshot) -> Arc<WorkspaceMembership> {
    let revision = snapshot.revision();
    let cache_key = "workspace-membership";
    if let Some(cached) = snapshot
        .query_cache()
        .get::<WorkspaceMembership>(revision, cache_key)
    {
        return cached;
    }
    let mut kinds: rustc_hash::FxHashMap<Box<str>, rustc_hash::FxHashMap<Box<str>, u32>> =
        rustc_hash::FxHashMap::default();
    for (definition, active) in snapshot.index().definition_identities() {
        if !active || !completion_source_file_allowed(snapshot, definition.file_id) {
            continue;
        }
        let folded = snapshot
            .index()
            .definition_name_key(&definition.kind, &definition.name);
        *kinds
            .entry(Box::from(definition.kind.as_ref()))
            .or_default()
            .entry(Box::from(folded.as_ref()))
            .or_insert(0) += 1;
    }
    let mut suffixes: rustc_hash::FxHashMap<Box<str>, Vec<Box<str>>> =
        rustc_hash::FxHashMap::default();
    for rule in &snapshot.game_profile().member_name_suffixes {
        for kind in &rule.kinds {
            suffixes
                .entry(Box::from(kind.to_ascii_lowercase().as_str()))
                .or_default()
                .push(Box::from(rule.suffix.as_str()));
        }
    }
    let membership = Arc::new(WorkspaceMembership { kinds, suffixes });
    snapshot.query_cache().insert(
        revision,
        pdx_engine::CacheDomain::Index,
        cache_key.to_owned(),
        Arc::clone(&membership),
    );
    membership
}

pub(crate) fn workspace_member(snapshot: &AnalysisSnapshot, type_name: &str, member: &str) -> bool {
    workspace_membership(snapshot).contains(snapshot, type_name, member)
}

/// Returns whether an indexed definition's source layer is enabled for completion members.
/// Resolution, diagnostics, navigation, and hover intentionally continue to see every layer;
/// this preference only narrows the candidate list offered while typing.
pub(crate) fn completion_source_file_allowed(
    snapshot: &AnalysisSnapshot,
    file_id: SourceFileId,
) -> bool {
    let Some(file) = snapshot.source_files().get(&file_id) else {
        return false;
    };
    let Some(root) = snapshot
        .source_roots()
        .iter()
        .find(|root| root.id == file.root_id)
    else {
        return false;
    };
    snapshot.completion_source_layer_enabled(root.kind)
}

/// Open overlays are always owned by the Current Mod layer for completion filtering.
pub(crate) fn completion_overlay_allowed(snapshot: &AnalysisSnapshot) -> bool {
    snapshot.completion_source_layer_enabled(SourceRootKind::CurrentMod)
}

fn workspace_kind_has_members(snapshot: &AnalysisSnapshot, type_name: &str) -> bool {
    let kinds = workspace_member_kinds(snapshot, type_name);
    kinds
        .iter()
        .any(|kind| !workspace_member_names_cached(snapshot, kind).is_empty())
}

/// Lowercased overlay definition identity, built once per snapshot revision and shared by every
/// workspace-membership check in that revision.
#[derive(Default)]
pub(crate) struct OverlayMembers {
    /// Lowercased `(kind, name)` pairs of every overlay definition.
    pub(crate) names: HashSet<(String, String)>,
    /// Lowercased kinds present in any overlay definition.
    pub(crate) kinds: HashSet<String>,
}

/// Returns the overlay definition view for this revision, computing it at most once.
pub(crate) fn overlay_members(snapshot: &AnalysisSnapshot) -> Arc<OverlayMembers> {
    let revision = snapshot.revision();
    let key = "overlay-members";
    if let Some(cached) = snapshot.query_cache().get::<OverlayMembers>(revision, key) {
        return cached;
    }
    let mut members = OverlayMembers::default();
    for document in snapshot
        .documents()
        .values()
        .filter(|document| document.source() == DocumentSource::Overlay)
        .filter_map(|document| document.hir_handle())
    {
        for definition in document.definitions() {
            let kind = definition.kind.to_ascii_lowercase();
            members.kinds.insert(kind.clone());
            members
                .names
                .insert((kind, definition.name.to_ascii_lowercase()));
        }
    }
    let members = Arc::new(members);
    snapshot.query_cache().insert(
        revision,
        pdx_engine::CacheDomain::Documents,
        key.to_owned(),
        Arc::clone(&members),
    );
    members
}

pub(crate) fn enum_member(snapshot: &AnalysisSnapshot, enum_name: &str, member: &str) -> bool {
    // Static enum membership is a BTreeMap probe plus small scans; the previous per-call
    // memo key (format! + two lowercase strings + a global cache probe) cost more than the
    // check itself now that `workspace_member` consults a per-revision membership set.
    enum_member_uncached(snapshot, enum_name, member)
}

fn enum_member_uncached(snapshot: &AnalysisSnapshot, enum_name: &str, member: &str) -> bool {
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

/// Result of resolving a scope expression without applying an expected target scope.
///
/// This deliberately distinguishes a runtime-dynamic expression and an untracked register from
/// an actually unknown spelling. Callers can therefore suppress false positives while still
/// reporting a misspelled static scope as an error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ScopeResolution {
    Known { scope: Arc<str> },
    Dynamic,
    Unresolved,
    Unknown,
}

pub(crate) fn resolve_scope_member(
    snapshot: &AnalysisSnapshot,
    member: &str,
    context: &ScopeContext,
) -> ScopeResolution {
    let member = member.trim();
    if context.profile.is_dynamic_scope_expression(member) {
        // Runtime event targets may resolve to any concrete scope. Their existence and concrete
        // type cannot be disproved from a static workspace snapshot.
        return ScopeResolution::Dynamic;
    }
    if let Some(scope) = context.profile.scope_member_alias(member) {
        return ScopeResolution::Known {
            scope: intern_shard_string(scope),
        };
    }
    let lowered = member.to_ascii_lowercase().replace('_', "");
    if let Some(destination) = snapshot
        .rules()
        .exact_semantic_rules(member)
        .find_map(|rule| {
            rule.push_scope
                .as_deref()
                .or_else(|| (!rule.replace_scope.is_empty()).then_some("any"))
        })
    {
        return ScopeResolution::Known {
            scope: intern_shard_string(destination),
        };
    }
    let resolved = if lowered == "root" {
        Some(context.root.as_ref())
    } else if lowered == "this" {
        Some(context.current.as_ref())
    } else if let Some(depth) = repeated_scope_register_depth(&lowered, "from") {
        context.from.get(depth).map(|value| value.as_ref())
    } else if let Some(depth) = repeated_scope_register_depth(&lowered, "previous")
        .or_else(|| repeated_scope_register_depth(&lowered, "prev"))
    {
        context.previous.get(depth).map(|value| value.as_ref())
    } else {
        Some(member)
    };
    let Some(resolved) = resolved else {
        // FROM/PREV registers are not tracked in every context; an unknown register is
        // a legitimate scope reference that the analysis cannot disprove.
        return ScopeResolution::Unresolved;
    };
    if !context.profile.is_scope(resolved) {
        // Country tags are valid scope references, e.g. `who = TRP`.
        return if context.profile.enum_extra_member("country_tags", member)
            || workspace_member(snapshot, "country_tag", member)
        {
            ScopeResolution::Known {
                scope: intern_shard_string("country"),
            }
        } else {
            ScopeResolution::Unknown
        };
    }
    ScopeResolution::Known {
        scope: intern_shard_string(resolved),
    }
}

pub(crate) fn scope_member(
    snapshot: &AnalysisSnapshot,
    expected: Option<&str>,
    member: &str,
    context: &ScopeContext,
) -> bool {
    match resolve_scope_member(snapshot, member, context) {
        ScopeResolution::Known { scope } => {
            expected.is_none_or(|expected| context.profile.scopes_compatible(&scope, expected))
        }
        // Dynamic targets and untracked registers are valid but cannot be checked statically.
        ScopeResolution::Dynamic | ScopeResolution::Unresolved => true,
        ScopeResolution::Unknown => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{LocalisationKeyIndex, WorkspaceMemberIndex};
    use std::sync::Arc;

    fn linear(names: &[String], prefix: &str) -> Vec<String> {
        let mut selected = names
            .iter()
            .filter(|name| {
                prefix.is_empty()
                    || name
                        .as_bytes()
                        .windows(prefix.len())
                        .any(|window| window.eq_ignore_ascii_case(prefix.as_bytes()))
            })
            .cloned()
            .collect::<Vec<_>>();
        selected.sort_by_key(|name| name.to_ascii_lowercase());
        selected.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
        selected
    }

    #[test]
    fn workspace_member_index_matches_linear_substring_selection() {
        let mut sorted = vec![
            "Zed_event".to_owned(),
            "country_event".to_owned(),
            "EVENT_special".to_owned(),
            "province".to_owned(),
            "country_flag".to_owned(),
            "nonascii_é".to_owned(),
        ];
        sorted.sort_by_key(|name| name.to_ascii_lowercase());
        let names = Arc::new(sorted);
        let index = WorkspaceMemberIndex::new(Arc::clone(&names));
        for prefix in ["", "e", "EVENT", "event_", "flag", "zzz", "é"] {
            assert_eq!(
                index.select(prefix),
                linear(names.as_ref(), prefix),
                "{prefix:?}"
            );
        }
    }

    #[test]
    fn localisation_key_index_matches_linear_substring_selection() {
        let names = vec![
            "Zed_event".to_owned(),
            "country_event".to_owned(),
            "EVENT_special".to_owned(),
            "province".to_owned(),
            "country_flag".to_owned(),
            "nonascii_é".to_owned(),
            "event_special".to_owned(),
        ];
        let index = LocalisationKeyIndex::new(&names);
        let cancellation = crate::CancellationToken::new();
        for prefix in ["", "e", "EVENT", "event_", "flag", "zzz", "é"] {
            let expected = linear(&names, prefix);
            assert_eq!(
                index
                    .select_with_cancellation(prefix, &cancellation)
                    .expect("uncancelled selection"),
                expected,
                "{prefix:?}"
            );
        }
    }
}
