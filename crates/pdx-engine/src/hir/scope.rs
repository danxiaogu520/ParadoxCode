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
    };
    for (property_index, property) in properties
        .iter()
        .enumerate()
        .filter(|(_, property)| property.top_level)
    {
        let Some(context) = semantic_root_context(rules, logical_path, &property.key) else {
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
fn scope_transition_rules<'rule>(
    rules: &'rule RuleSet,
    context: &str,
    parent_path: &[String],
    key: &str,
    profile: &GameProfile,
    state: &ScopeState,
) -> Vec<&'rule SemanticRule> {
    let root_context = context
        .strip_prefix("type:")
        .map(|type_name| format!("root:{type_name}"));
    let mut strong = Vec::new();
    let mut weak = Vec::new();
    for lookup_context in std::iter::once(context).chain(root_context.as_deref()) {
        for rule in rules.semantic_rules_for_context(lookup_context) {
            if !paths_equal(&rule.parent_path, parent_path)
                || !matches!(rule.shape, RuleShape::Node | RuleShape::ValueClause)
                || !scope_allows(profile, state, rule)
            {
                continue;
            }
            match &rule.key {
                KeyMatcher::Exact(expected) if expected.eq_ignore_ascii_case(key) => {
                    strong.push(rule);
                }
                KeyMatcher::Enum(enum_name) => {
                    let members =
                        rules
                            .model()
                            .semantic
                            .enum_values
                            .iter()
                            .find_map(|(name, values)| {
                                name.eq_ignore_ascii_case(enum_name).then_some(values)
                            });
                    if members.is_some_and(|values| {
                        values.iter().any(|value| value.eq_ignore_ascii_case(key))
                    }) {
                        strong.push(rule);
                    }
                }
                KeyMatcher::AnyScalar => strong.push(rule),
                KeyMatcher::Date if rule.key.matches(key, |_, _| false, |_, _| false) => {
                    strong.push(rule);
                }
                KeyMatcher::Type(_) | KeyMatcher::Dynamic(_) => weak.push(rule),
                KeyMatcher::Exact(_) | KeyMatcher::Date => {}
            }
        }
    }
    if strong.is_empty() { weak } else { strong }
}

struct ScopeFactLowering<'a> {
    properties: &'a [HirProperty],
    property_children: &'a [Vec<usize>],
    rules: &'a RuleSet,
    profile: &'a GameProfile,
    facts: Vec<ScopeFact>,
}

impl ScopeFactLowering<'_> {
    fn lower_nested(
        &mut self,
        parent_index: usize,
        context: &str,
        parent_path: &[String],
        state: &ScopeState,
    ) {
        for &property_index in &self.property_children[parent_index] {
            let property = &self.properties[property_index];
            let matching = scope_transition_rules(
                self.rules,
                context,
                parent_path,
                &property.key,
                self.profile,
                state,
            );
            let fact_index = self.facts.len();
            self.facts.push(ScopeFact {
                range: property.key_range,
                context: context.to_owned(),
                parent_path: parent_path.to_vec(),
                state: state.clone(),
                transition: None,
            });
            let transparent = context.eq_ignore_ascii_case("trigger")
                && self.profile.is_transparent_scope_wrapper(&property.key);
            let Some(rule) = statically_selected_transition(StaticTransitionInput {
                matching: &matching,
                properties: self.properties,
                property_children: self.property_children,
                property_index,
                rules: self.rules,
                context,
                parent_path,
                transparent,
            }) else {
                continue;
            };
            let (next_context, next_path) =
                transition_destination(rule, context, parent_path, &property.key, transparent);
            let next_state = child_scope_state(state, rule, self.rules, self.profile);
            self.facts[fact_index].transition = Some(next_state.clone());
            self.lower_nested(property_index, &next_context, &next_path, &next_state);
        }
    }
}

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

pub(crate) fn statically_selected_transition<'rule>(
    input: StaticTransitionInput<'_, 'rule>,
) -> Option<&'rule SemanticRule> {
    if let Some(rule) = equivalent_transition(input.matching) {
        return Some(rule);
    }
    let children = &input.property_children[input.property_index];
    if children.is_empty() {
        return None;
    }
    let property = &input.properties[input.property_index];
    let possible = input
        .matching
        .iter()
        .copied()
        .filter(|candidate| {
            let (child_context, child_path) = transition_destination(
                candidate,
                input.context,
                input.parent_path,
                &property.key,
                input.transparent,
            );
            children.iter().all(|child_index| {
                child_key_may_match(
                    input.rules,
                    &child_context,
                    &child_path,
                    &input.properties[*child_index].key,
                )
            })
        })
        .collect::<Vec<_>>();
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
            ScopeValue::Known(vec![push_scope.to_owned()])
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
        ScopeValue::Known(vec![expression.to_owned()])
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
        return (link_expression, ScopeValue::Known(targets));
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
        .map_or(ScopeValue::Unknown, |scope| {
            ScopeValue::Known(vec![scope.to_owned()])
        });
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
    ScopeValue::Known(vec![expression.to_owned()])
}
