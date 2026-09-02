//! Scope facts and conservative scope-register evaluation.

use pdx_rules::{GameProfile, KeyMatcher, RuleSet, RuleShape, SemanticRule};
use pdx_text::LogicalPath;

use super::semantics::semantic_root_context;
use super::{HirProperty, ScopeFact, ScopeState, ScopeValue, range_within};

pub(super) fn lower_scope_facts(
    properties: &[HirProperty],
    logical_path: Option<&LogicalPath>,
    rules: &RuleSet,
    profile: Option<&GameProfile>,
) -> Vec<ScopeFact> {
    let Some(profile) = profile else {
        return Vec::new();
    };
    let property_children = property_children(properties);
    let mut lowering = ScopeFactLowering {
        properties,
        property_children: &property_children,
        rules,
        profile,
        facts: Vec::new(),
        transition_buckets: std::collections::HashMap::new(),
        child_matches: std::collections::HashMap::new(),
    };
    // Root-context resolution lowercases the whole logical path per call, and
    // files commonly repeat one root key per definition; memoize per key.
    let mut root_contexts: rustc_hash::FxHashMap<Box<str>, Option<String>> =
        rustc_hash::FxHashMap::default();
    for (property_index, property) in properties
        .iter()
        .enumerate()
        .filter(|(_, property)| property.top_level)
    {
        let Some(context) = root_contexts
            .entry(Box::from(property.key.to_ascii_lowercase().as_str()))
            .or_insert_with(|| semantic_root_context(rules, logical_path, &property.key))
            .clone()
        else {
            continue;
        };
        lowering.facts.push(ScopeFact {
            range: property.key_range,
            state: initial_scope_state(rules, profile, &context, &property.key),
            context: context.clone(),
            parent_path: Vec::new(),
            transition: None,
        });
        let initial = lowering
            .facts
            .last()
            .expect("root fact was just inserted")
            .state
            .clone();
        let Some(type_name) = context.strip_prefix("type:") else {
            lowering.lower_nested(property_index, &context, &[], &initial);
            continue;
        };
        let skip_root = rules
            .model()
            .semantic
            .type_descriptors
            .get(type_name)
            .is_some_and(|descriptor| {
                descriptor.skip_root_paths.iter().any(|path| {
                    path.first().is_some_and(|key| {
                        key.eq_ignore_ascii_case("any") || key.eq_ignore_ascii_case(&property.key)
                    })
                })
            });
        if !skip_root {
            lowering.lower_nested(property_index, &context, &[], &initial);
            continue;
        }
        for &child_index in &property_children[property_index] {
            let child = &properties[child_index];
            let child_state = initial_scope_state(rules, profile, &context, &child.key);
            lowering.facts.push(ScopeFact {
                range: child.key_range,
                state: child_state.clone(),
                context: context.clone(),
                parent_path: Vec::new(),
                transition: None,
            });
            lowering.lower_nested(child_index, &context, &[], &child_state);
        }
    }
    lowering.facts.sort_by_key(|fact| fact.range);
    lowering.facts
}

pub(crate) fn property_children(properties: &[HirProperty]) -> Vec<Vec<usize>> {
    let mut children = vec![Vec::new(); properties.len()];
    let mut ancestors = Vec::<usize>::new();
    for (index, property) in properties.iter().enumerate() {
        while ancestors.last().is_some_and(|ancestor_index| {
            let ancestor = &properties[*ancestor_index];
            property.path.len() <= ancestor.path.len()
                || !property.path.starts_with(&ancestor.path)
                || !range_within(property.range, ancestor.range)
        }) {
            ancestors.pop();
        }
        if let Some(&parent_index) = ancestors.last()
            && property.path.len() == properties[parent_index].path.len().saturating_add(1)
        {
            children[parent_index].push(index);
        }
        ancestors.push(index);
    }
    children
}

/// Collects transition rules for one property. Concrete key matches (exact/enum/any-scalar) are
/// strong; dynamic matchers (`<mission>`, `KeyMatcher::Type`, `KeyMatcher::Dynamic`) only apply
/// when no concrete rule selects the property, so scope facts descend into dynamic blocks.
/// Rules for one (context, parent path) that pass the path/shape gates.
///
/// The path and shape filters do not depend on the property being visited,
/// so they run once per distinct pair instead of once per property; effect
/// and trigger contexts carry nearly two thousand rules each.
fn transition_candidates<'rule>(
    rules: &'rule RuleSet,
    context: &str,
    parent_path: &[String],
) -> Vec<&'rule SemanticRule> {
    let root_context = context
        .strip_prefix("type:")
        .map(|type_name| format!("root:{type_name}"));
    let mut candidates = Vec::new();
    for lookup_context in std::iter::once(context).chain(root_context.as_deref()) {
        for rule in rules.semantic_rules_for_context(lookup_context) {
            if paths_equal(&rule.parent_path, parent_path)
                && matches!(rule.shape, RuleShape::Node | RuleShape::ValueClause)
            {
                candidates.push(rule);
            }
        }
    }
    candidates
}

/// Matcher-classified transition rules for one (context, parent path).
///
/// The path/shape-filtered candidate list is property-independent, so it is
/// classified once per distinct pair and shared by every property under it;
/// effect and trigger contexts carry nearly two thousand rules each, and the
/// previous full scan per property dominated lowering. Each entry keeps its
/// original candidate position because `equivalent_transition` returns the
/// first matching rule: bucket probes restore candidate order before use.
struct TransitionBuckets<'rule> {
    candidates: Vec<&'rule SemanticRule>,
    /// Exact-key rules by lowercased key.
    exact: rustc_hash::FxHashMap<Box<str>, Vec<(u32, &'rule SemanticRule)>>,
    /// Enum rules with their member lists resolved once.
    enums: Vec<(&'rule [String], &'rule SemanticRule, u32)>,
    any_scalar: Vec<(&'rule SemanticRule, u32)>,
    date: Vec<(&'rule SemanticRule, u32)>,
    weak: Vec<(&'rule SemanticRule, u32)>,
}

impl<'rule> TransitionBuckets<'rule> {
    fn build(rules: &'rule RuleSet, candidates: Vec<&'rule SemanticRule>) -> Self {
        let mut exact: rustc_hash::FxHashMap<Box<str>, Vec<(u32, &'rule SemanticRule)>> =
            rustc_hash::FxHashMap::default();
        let mut enums = Vec::new();
        let mut any_scalar = Vec::new();
        let mut date = Vec::new();
        let mut weak = Vec::new();
        for (position, rule) in candidates.iter().copied().enumerate() {
            let position = position as u32;
            match &rule.key {
                KeyMatcher::Exact(expected) => {
                    exact
                        .entry(Box::from(expected.to_ascii_lowercase().as_str()))
                        .or_default()
                        .push((position, rule));
                }
                KeyMatcher::Enum(enum_name) => {
                    // Enum rules whose enum has no values never match any key;
                    // resolve the member list once here instead of per property.
                    let members =
                        rules
                            .model()
                            .semantic
                            .enum_values
                            .iter()
                            .find_map(|(name, values)| {
                                name.eq_ignore_ascii_case(enum_name)
                                    .then_some(values.as_slice())
                            });
                    if let Some(members) = members {
                        enums.push((members, rule, position));
                    }
                }
                KeyMatcher::AnyScalar => any_scalar.push((rule, position)),
                KeyMatcher::Date => date.push((rule, position)),
                KeyMatcher::Type(_) | KeyMatcher::Dynamic(_) => weak.push((rule, position)),
            }
        }
        Self {
            candidates,
            exact,
            enums,
            any_scalar,
            date,
            weak,
        }
    }

    /// Concrete (strong) and dynamic (weak) matches for one property key in
    /// original candidate order, mirroring the previous per-rule scan.
    fn matches_for(
        &self,
        key: &str,
        profile: &GameProfile,
        state: &ScopeState,
    ) -> Vec<&'rule SemanticRule> {
        let mut positions: Vec<u32> = Vec::new();
        let lowered = key.to_ascii_lowercase();
        if let Some(hits) = self.exact.get(lowered.as_str()) {
            positions.extend(
                hits.iter()
                    .filter(|(_, rule)| scope_allows(profile, state, rule))
                    .map(|(position, _)| *position),
            );
        }
        for (members, rule, position) in &self.enums {
            if members
                .iter()
                .any(|member| member.eq_ignore_ascii_case(key))
                && scope_allows(profile, state, rule)
            {
                positions.push(*position);
            }
        }
        for (rule, position) in &self.any_scalar {
            if scope_allows(profile, state, rule) {
                positions.push(*position);
            }
        }
        for (rule, position) in &self.date {
            if rule.key.matches(key, |_, _| false, |_, _| false)
                && scope_allows(profile, state, rule)
            {
                positions.push(*position);
            }
        }
        if positions.is_empty() {
            return self
                .weak
                .iter()
                .filter(|(rule, _)| scope_allows(profile, state, rule))
                .map(|(rule, _)| *rule)
                .collect();
        }
        positions.sort_unstable();
        positions
            .into_iter()
            .map(|position| self.candidates[position as usize])
            .collect()
    }
}

struct ScopeFactLowering<'a> {
    properties: &'a [HirProperty],
    property_children: &'a [Vec<usize>],
    rules: &'a RuleSet,
    profile: &'a GameProfile,
    facts: Vec<ScopeFact>,
    /// Matcher-bucketed rule lists per (context, parent path), shared by
    /// sibling subtrees. Effect and trigger contexts repeat heavily, so the
    /// bucket build cost amortizes across the whole file.
    transition_buckets:
        std::collections::HashMap<(String, Vec<String>), std::rc::Rc<TransitionBuckets<'a>>>,
    /// Key-acceptance buckets per transition destination (context, parent path).
    child_matches: ChildMatchMemo<'a>,
}

impl<'a> ScopeFactLowering<'a> {
    /// Buckets for one (context, parent path), built on first use.
    fn buckets_for(
        &mut self,
        context: &str,
        parent_path: &[String],
    ) -> std::rc::Rc<TransitionBuckets<'a>> {
        if let Some(buckets) = self
            .transition_buckets
            .get(&(context.to_owned(), parent_path.to_vec()))
        {
            return std::rc::Rc::clone(buckets);
        }
        let buckets = std::rc::Rc::new(TransitionBuckets::build(
            self.rules,
            transition_candidates(self.rules, context, parent_path),
        ));
        self.transition_buckets.insert(
            (context.to_owned(), parent_path.to_vec()),
            std::rc::Rc::clone(&buckets),
        );
        buckets
    }

    fn lower_nested(
        &mut self,
        parent_index: usize,
        context: &str,
        parent_path: &[String],
        state: &ScopeState,
    ) {
        let buckets = self.buckets_for(context, parent_path);
        // Disjoint borrows: `facts`/`child_matches` stay mutable while the
        // immutable inputs feed the transition selection; recursion happens
        // after the scope ends so `self` is whole again.
        let mut recurse: Vec<(usize, String, Vec<String>, ScopeState)> = Vec::new();
        {
            let ScopeFactLowering {
                properties,
                property_children,
                rules,
                profile,
                facts,
                child_matches,
                ..
            } = self;
            for &property_index in &property_children[parent_index] {
                let property = &properties[property_index];
                let matching = buckets.matches_for(&property.key, profile, state);
                let fact_index = facts.len();
                facts.push(ScopeFact {
                    range: property.key_range,
                    context: context.to_owned(),
                    parent_path: parent_path.to_vec(),
                    state: state.clone(),
                    transition: None,
                });
                let transparent = context.eq_ignore_ascii_case("trigger")
                    && profile.is_transparent_scope_wrapper(&property.key);
                let Some(rule) = statically_selected_transition_with_memo(
                    &matching,
                    properties,
                    property_children,
                    property_index,
                    rules,
                    context,
                    parent_path,
                    transparent,
                    child_matches,
                ) else {
                    continue;
                };
                let (next_context, next_path) =
                    transition_destination(rule, context, parent_path, &property.key, transparent);
                let next_state = child_scope_state(state, rule, rules, profile);
                facts[fact_index].transition = Some(next_state.clone());
                recurse.push((property_index, next_context, next_path, next_state));
            }
        }
        for (property_index, next_context, next_path, next_state) in recurse {
            self.lower_nested(property_index, &next_context, &next_path, &next_state);
        }
    }
}

/// Key-acceptance buckets answering the old `child_key_may_match` scan for one
/// (context, parent path) without rescanning the context's full rule list.
/// `statically_selected_transition` probes one destination per candidate rule
/// per child key, so the unbucketed scan volume was
/// rules-in-context × children × properties.
struct ChildMatchBuckets<'rule> {
    exact: rustc_hash::FxHashSet<Box<str>>,
    enum_members: Vec<&'rule [String]>,
    any_scalar: bool,
    date: Vec<&'rule SemanticRule>,
    dynamic: bool,
}

impl<'rule> ChildMatchBuckets<'rule> {
    fn build(rules: &'rule RuleSet, context: &str, parent_path: &[String]) -> Self {
        let root_context = context
            .strip_prefix("type:")
            .map(|type_name| format!("root:{type_name}"));
        let mut exact = rustc_hash::FxHashSet::default();
        let mut enum_members = Vec::new();
        let mut any_scalar = false;
        let mut date = Vec::new();
        let mut dynamic = false;
        for lookup_context in std::iter::once(context).chain(root_context.as_deref()) {
            for rule in rules
                .semantic_rules_for_context(lookup_context)
                .filter(|rule| {
                    paths_equal(&rule.parent_path, parent_path)
                        && !matches!(rule.shape, RuleShape::LeafValue)
                })
            {
                match &rule.key {
                    KeyMatcher::Exact(expected) => {
                        exact.insert(Box::from(expected.to_ascii_lowercase().as_str()));
                    }
                    KeyMatcher::Enum(enum_name) => {
                        match rules.model().semantic.enum_values.iter().find_map(
                            |(name, values)| {
                                name.eq_ignore_ascii_case(enum_name)
                                    .then_some(values.as_slice())
                            },
                        ) {
                            Some(members) => enum_members.push(members),
                            None => dynamic = true,
                        }
                    }
                    KeyMatcher::AnyScalar => any_scalar = true,
                    KeyMatcher::Date => date.push(rule),
                    KeyMatcher::Type(_) | KeyMatcher::Dynamic(_) => dynamic = true,
                }
            }
        }
        Self {
            exact,
            enum_members,
            any_scalar,
            date,
            dynamic,
        }
    }

    /// Whether any bucketed rule accepts `key`; `lowered` is the caller's
    /// lowercased key so repeated probes need not re-allocate it. An
    /// `AnyScalar` rule decides the whole question exactly as the scan did
    /// (non-empty key accepts, empty rejects); keys are never empty here.
    fn may_match(&self, key: &str, lowered: &str) -> bool {
        if self.any_scalar {
            return !key.is_empty();
        }
        if self.exact.contains(lowered) {
            return true;
        }
        for members in &self.enum_members {
            if members
                .iter()
                .any(|member| member.eq_ignore_ascii_case(key))
            {
                return true;
            }
        }
        for rule in &self.date {
            if rule.key.matches(key, |_, _| false, |_, _| false) {
                return true;
            }
        }
        self.dynamic
    }
}

/// Memoized [`ChildMatchBuckets`] per (context, parent path).
type ChildMatchMemo<'rule> =
    std::collections::HashMap<(String, Vec<String>), std::rc::Rc<ChildMatchBuckets<'rule>>>;

fn child_match_buckets_for<'rule>(
    memo: &mut ChildMatchMemo<'rule>,
    rules: &'rule RuleSet,
    context: &str,
    parent_path: &[String],
) -> std::rc::Rc<ChildMatchBuckets<'rule>> {
    if let Some(buckets) = memo.get(&(context.to_owned(), parent_path.to_vec())) {
        return std::rc::Rc::clone(buckets);
    }
    let buckets = std::rc::Rc::new(ChildMatchBuckets::build(rules, context, parent_path));
    memo.insert(
        (context.to_owned(), parent_path.to_vec()),
        std::rc::Rc::clone(&buckets),
    );
    buckets
}

#[cfg(test)]
pub(crate) struct StaticTransitionInput<'data, 'rule> {
    pub(crate) matching: &'data [&'rule SemanticRule],
    pub(crate) properties: &'data [HirProperty],
    pub(crate) property_children: &'data [Vec<usize>],
    pub(crate) property_index: usize,
    pub(crate) rules: &'data RuleSet,
    pub(crate) context: &'data str,
    pub(crate) parent_path: &'data [String],
    pub(crate) transparent: bool,
}

#[cfg(test)]
pub(crate) fn statically_selected_transition<'rule>(
    input: StaticTransitionInput<'rule, 'rule>,
) -> Option<&'rule SemanticRule> {
    let mut child_matches = ChildMatchMemo::new();
    statically_selected_transition_with_memo(
        input.matching,
        input.properties,
        input.property_children,
        input.property_index,
        input.rules,
        input.context,
        input.parent_path,
        input.transparent,
        &mut child_matches,
    )
}

/// Memoized form of [`statically_selected_transition`]: `child_matches` is the
/// caller's per-file memo, so repeated transition destinations stop rescanning
/// their context's rule list. Passing the memo as a direct parameter keeps its
/// invariant `'rule` from infecting the independent `context`/`parent_path`
/// lifetimes (which a shared struct field would unify).
#[allow(clippy::too_many_arguments)] // flat params instead of an input struct: see doc comment
fn statically_selected_transition_with_memo<'rule>(
    matching: &[&'rule SemanticRule],
    properties: &[HirProperty],
    property_children: &[Vec<usize>],
    property_index: usize,
    rules: &'rule RuleSet,
    context: &str,
    parent_path: &[String],
    transparent: bool,
    child_matches: &mut ChildMatchMemo<'rule>,
) -> Option<&'rule SemanticRule> {
    if let Some(rule) = equivalent_transition(matching) {
        return Some(rule);
    }
    let children = &property_children[property_index];
    if children.is_empty() {
        return None;
    }
    let property = &properties[property_index];
    let mut possible = Vec::new();
    'candidates: for candidate in matching.iter().copied() {
        let (child_context, child_path) =
            transition_destination(candidate, context, parent_path, &property.key, transparent);
        let buckets = child_match_buckets_for(child_matches, rules, &child_context, &child_path);
        for &child_index in children {
            let key = &properties[child_index].key;
            if !buckets.may_match(key, &key.to_ascii_lowercase()) {
                continue 'candidates;
            }
        }
        possible.push(candidate);
    }
    equivalent_transition(&possible)
}

fn transition_destination(
    rule: &SemanticRule,
    context: &str,
    parent_path: &[String],
    property_key: &str,
    transparent: bool,
) -> (String, Vec<String>) {
    rule.child_context.as_deref().map_or_else(
        || {
            let mut path = parent_path.to_vec();
            if !transparent {
                path.push(property_key.to_owned());
            }
            (context.to_owned(), path)
        },
        |child| (child.to_owned(), Vec::new()),
    )
}

#[cfg(test)]
pub(crate) fn child_key_may_match(
    rules: &RuleSet,
    context: &str,
    parent_path: &[String],
    key: &str,
) -> bool {
    let mut dynamic_matcher = false;
    let root_context = context
        .strip_prefix("type:")
        .map(|type_name| format!("root:{type_name}"));
    for lookup_context in std::iter::once(context).chain(root_context.as_deref()) {
        for rule in rules
            .semantic_rules_for_context(lookup_context)
            .filter(|rule| {
                paths_equal(&rule.parent_path, parent_path)
                    && !matches!(rule.shape, RuleShape::LeafValue)
            })
        {
            match &rule.key {
                KeyMatcher::Exact(expected) if expected.eq_ignore_ascii_case(key) => return true,
                KeyMatcher::Enum(enum_name) => {
                    let Some(values) =
                        rules
                            .model()
                            .semantic
                            .enum_values
                            .iter()
                            .find_map(|(name, values)| {
                                name.eq_ignore_ascii_case(enum_name).then_some(values)
                            })
                    else {
                        dynamic_matcher = true;
                        continue;
                    };
                    if values.iter().any(|value| value.eq_ignore_ascii_case(key)) {
                        return true;
                    }
                }
                KeyMatcher::AnyScalar => return !key.is_empty(),
                KeyMatcher::Date if rule.key.matches(key, |_, _| false, |_, _| false) => {
                    return true;
                }
                KeyMatcher::Type(_) | KeyMatcher::Dynamic(_) => dynamic_matcher = true,
                KeyMatcher::Exact(_) | KeyMatcher::Date => {}
            }
        }
    }
    dynamic_matcher
}

fn equivalent_transition<'rule>(matching: &[&'rule SemanticRule]) -> Option<&'rule SemanticRule> {
    let first = *matching.first()?;
    matching
        .iter()
        .all(|candidate| transition_signature_matches(first, candidate))
        .then_some(first)
}

fn transition_signature_matches(left: &SemanticRule, right: &SemanticRule) -> bool {
    optional_text_matches(
        left.child_context.as_deref(),
        right.child_context.as_deref(),
    ) && optional_text_matches(left.push_scope.as_deref(), right.push_scope.as_deref())
        && left.replace_scope.len() == right.replace_scope.len()
        && left
            .replace_scope
            .iter()
            .all(|(left_register, left_scope)| {
                right
                    .replace_scope
                    .iter()
                    .any(|(right_register, right_scope)| {
                        left_register.eq_ignore_ascii_case(right_register)
                            && left_scope.eq_ignore_ascii_case(right_scope)
                    })
            })
        && right
            .replace_scope
            .iter()
            .all(|(right_register, right_scope)| {
                left.replace_scope
                    .iter()
                    .any(|(left_register, left_scope)| {
                        left_register.eq_ignore_ascii_case(right_register)
                            && left_scope.eq_ignore_ascii_case(right_scope)
                    })
            })
}

fn optional_text_matches(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left.eq_ignore_ascii_case(right),
        (None, None) => true,
        _ => false,
    }
}

fn paths_equal(left: &[String], right: &[String]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            if left.starts_with('<') && left.ends_with('>') {
                !right.is_empty()
            } else {
                left.eq_ignore_ascii_case(right)
            }
        })
}

fn scope_allows(profile: &GameProfile, state: &ScopeState, rule: &SemanticRule) -> bool {
    rule.allowed_scopes.is_empty()
        || state.current.first().is_none_or(|current| match current {
            ScopeValue::Known(scopes) => rule.allowed_scopes.iter().any(|expected| {
                scopes
                    .iter()
                    .any(|actual| profile.scopes_compatible(actual, expected))
            }),
            ScopeValue::Unknown => true,
            ScopeValue::Invalid => false,
        })
}

pub(crate) fn child_scope_state(
    parent: &ScopeState,
    rule: &SemanticRule,
    rules: &RuleSet,
    profile: &GameProfile,
) -> ScopeState {
    let mut child = parent.clone();
    if let Some(push_scope) = rule.push_scope.as_deref() {
        if let Some(current) = child.current.first().cloned() {
            child.previous.insert(0, current);
        }
        let next = if push_scope.eq_ignore_ascii_case("any") {
            ScopeValue::Unknown
        } else {
            ScopeValue::known_single(push_scope)
        };
        child.current.insert(0, next);
    }
    for (register, value) in &rule.replace_scope {
        let value = resolve_scope_expression(&child, value, rules, profile);
        let register = register.to_ascii_lowercase().replace('_', "");
        match register.as_str() {
            "root" => child.root = value,
            "this" => {
                if let Some(current) = child.current.first().cloned() {
                    child.previous.insert(0, current);
                }
                set_scope_register(&mut child.current, 0, value);
            }
            _ => {
                if let Some(depth) = repeated_scope_register_depth(&register, "from") {
                    set_scope_register(&mut child.from, depth, value);
                } else if let Some(depth) = repeated_scope_register_depth(&register, "previous")
                    .or_else(|| repeated_scope_register_depth(&register, "prev"))
                {
                    set_scope_register(&mut child.previous, depth, value);
                }
            }
        }
    }
    child
}

pub(crate) fn resolve_scope_expression(
    state: &ScopeState,
    expression: &str,
    rules: &RuleSet,
    profile: &GameProfile,
) -> ScopeValue {
    if expression.contains('.') {
        let mut segments = expression.split('.');
        let Some(first) = segments.next() else {
            return ScopeValue::Unknown;
        };
        let mut value = resolve_scope_expression(state, first, rules, profile);
        for segment in segments {
            value = resolve_scope_link(&value, segment, rules, profile).1;
            if !matches!(value, ScopeValue::Known(_)) {
                break;
            }
        }
        return value;
    }

    let lowered = expression.to_ascii_lowercase().replace('_', "");
    if lowered == "root" {
        return state.root.clone();
    }
    if lowered == "this" {
        return state
            .current
            .first()
            .cloned()
            .unwrap_or(ScopeValue::Unknown);
    }
    if let Some(depth) = repeated_scope_register_depth(&lowered, "from") {
        return state
            .from
            .get(depth)
            .cloned()
            .unwrap_or(ScopeValue::Unknown);
    }
    if let Some(depth) = repeated_scope_register_depth(&lowered, "previous")
        .or_else(|| repeated_scope_register_depth(&lowered, "prev"))
    {
        return state
            .previous
            .get(depth)
            .cloned()
            .unwrap_or(ScopeValue::Unknown);
    }

    let current = state.current.first().unwrap_or(&ScopeValue::Unknown);
    let (link_expression, resolved) = resolve_scope_link(current, expression, rules, profile);
    if !matches!(resolved, ScopeValue::Unknown) {
        return resolved;
    }
    if expression.eq_ignore_ascii_case("any") || link_expression {
        ScopeValue::Unknown
    } else if profile.is_scope(expression) {
        ScopeValue::known_single(expression)
    } else {
        ScopeValue::Unknown
    }
}

fn resolve_scope_link(
    current: &ScopeValue,
    expression: &str,
    rules: &RuleSet,
    profile: &GameProfile,
) -> (bool, ScopeValue) {
    let mut targets = Vec::new();
    let mut link_expression = false;
    if let ScopeValue::Known(actual_scopes) = current {
        for rule in rules.exact_semantic_rules(expression) {
            if !matches!(
                rule.context.to_ascii_lowercase().as_str(),
                "effect" | "trigger"
            ) {
                continue;
            }
            link_expression = true;
            let allowed = rule.allowed_scopes.is_empty()
                || rule.allowed_scopes.iter().any(|expected| {
                    actual_scopes
                        .iter()
                        .any(|actual| profile.scopes_compatible(actual, expected))
                });
            if !allowed {
                continue;
            }
            if let Some(target) = rule
                .push_scope
                .as_deref()
                .filter(|target| !target.eq_ignore_ascii_case("any"))
                && !targets
                    .iter()
                    .any(|known: &String| known.eq_ignore_ascii_case(target))
            {
                targets.push(target.to_owned());
            }
        }
    } else {
        link_expression = rules.exact_semantic_rules(expression).any(|rule| {
            matches!(
                rule.context.to_ascii_lowercase().as_str(),
                "effect" | "trigger"
            ) && rule.push_scope.is_some()
        });
    }
    if !targets.is_empty() {
        targets.sort_by_key(|target| target.to_ascii_lowercase());
        return (link_expression, ScopeValue::known(targets));
    }
    (link_expression, ScopeValue::Unknown)
}

pub(crate) fn repeated_scope_register_depth(value: &str, token: &str) -> Option<usize> {
    let count = value.len().checked_div(token.len())?;
    if count > 0 && token.repeat(count) == value {
        Some(count - 1)
    } else {
        None
    }
}

fn set_scope_register(registers: &mut Vec<ScopeValue>, depth: usize, value: ScopeValue) {
    if registers.len() <= depth {
        registers.resize(depth + 1, ScopeValue::Unknown);
    }
    registers[depth] = value;
}

fn initial_scope_state(
    rules: &RuleSet,
    profile: &GameProfile,
    context: &str,
    root_key: &str,
) -> ScopeState {
    if let Some(type_name) = context.strip_prefix("type:") {
        if let Some(registers) = rules.type_root_scope_registers(type_name, root_key) {
            let root = initial_scope_value(&registers.root, None, None);
            let current = initial_scope_value(&registers.this, Some(&root), None);
            let from = initial_scope_value(&registers.from, Some(&root), Some(&current));
            return ScopeState::initial_registers(root, current, from);
        }
        // A custom type root has no declared concrete scope, but its register defaults still
        // apply: ROOT/THIS remain unknown and FROM is an unconstrained register.
        return ScopeState::initial_registers(
            ScopeValue::Unknown,
            ScopeValue::Unknown,
            ScopeValue::Unknown,
        );
    }
    let scope = profile
        .root_scope(root_key)
        .map_or(ScopeValue::Unknown, ScopeValue::known_single);
    ScopeState::initial(scope)
}

fn initial_scope_value(
    expression: &str,
    root: Option<&ScopeValue>,
    current: Option<&ScopeValue>,
) -> ScopeValue {
    let expression = expression.trim();
    if expression.is_empty() || expression.eq_ignore_ascii_case("any") {
        return ScopeValue::Unknown;
    }
    if expression.eq_ignore_ascii_case("root") {
        return root.cloned().unwrap_or(ScopeValue::Unknown);
    }
    if expression.eq_ignore_ascii_case("this") {
        return current
            .cloned()
            .or_else(|| root.cloned())
            .unwrap_or(ScopeValue::Unknown);
    }
    ScopeValue::known_single(expression)
}
