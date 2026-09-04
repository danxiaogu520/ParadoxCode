//! Query-local inference of dynamic-definition argument value constraints.

use std::collections::BTreeMap;

use pdx_engine::AnalysisSnapshot;
use pdx_engine::hir::{
    TemplateFragment, TemplateItem, TemplateProperty, TemplateToken, TemplateValue,
};
use pdx_rules::{KeyMatcher, RuleShape, ValueMatcher};

use crate::semantic::{
    DynamicDefinitionIdentity, ResolvedDynamicDefinition, dynamic_definition_type,
    resolve_dynamic_definition, semantic_child_scope, semantic_rule_key_matches,
    semantic_scope_allows, semantic_transition_destination,
};
use crate::support::{ScopeContext, ScriptProperty};
use crate::types::{CancellationToken, Cancelled};

use super::{
    SemanticCompletionContext, semantic_rules_for_completion, semantic_rules_for_container,
};

#[derive(Clone, Debug)]
pub(crate) struct DynamicValueConstraintSite {
    pub(crate) matchers: Vec<ValueMatcher>,
    pub(crate) scope: ScopeContext,
}

#[derive(Clone, Debug)]
pub(crate) struct DynamicQuotedScriptConstraintSite {
    pub(crate) context: String,
    pub(crate) parent_path: Vec<std::sync::Arc<str>>,
    pub(crate) scope: ScopeContext,
}

#[derive(Clone, Debug, Default)]
struct DynamicArgumentConstraints {
    values: Vec<DynamicValueConstraintSite>,
    quoted_scripts: Vec<DynamicQuotedScriptConstraintSite>,
    /// True when at least one usage site constrains the parameter value to a
    /// non-enumerable shape (numbers, opaque strings, dynamic sets): no item
    /// can be offered, and callers must treat the value as constrained.
    unenumerable: bool,
}

#[derive(Clone, Debug)]
enum SymbolicToken {
    Concrete(String),
    Target,
    Unknown,
}

#[derive(Clone, Debug)]
enum SymbolicValue {
    Scalar(SymbolicToken),
    Block(SymbolicContainer),
}

#[derive(Clone, Debug, Default)]
struct SymbolicContainer {
    properties: Vec<SymbolicProperty>,
    bare_values: Vec<SymbolicToken>,
}

#[derive(Clone, Debug)]
struct SymbolicProperty {
    key: SymbolicToken,
    operator: Option<String>,
    value: SymbolicValue,
}

/// The inferred completion story for a dynamic parameter's value.
#[derive(Clone, Debug, Default)]
pub(crate) struct DynamicValueConstraints {
    pub(crate) sites: Vec<DynamicValueConstraintSite>,
    /// At least one usage site constrains the value to a non-enumerable
    /// shape, so nothing can be offered yet the value is not free-form.
    pub(crate) unenumerable: bool,
}

pub(crate) fn infer_dynamic_value_constraints(
    snapshot: &AnalysisSnapshot,
    context: &SemanticCompletionContext,
    target: &ScriptProperty,
    cancellation: &CancellationToken,
) -> Result<DynamicValueConstraints, Cancelled> {
    infer_dynamic_argument_constraints(snapshot, context, target, cancellation).map(|constraints| {
        DynamicValueConstraints {
            sites: constraints.values,
            unenumerable: constraints.unenumerable,
        }
    })
}

pub(crate) fn infer_dynamic_quoted_script_constraints(
    snapshot: &AnalysisSnapshot,
    context: &SemanticCompletionContext,
    target: &ScriptProperty,
    cancellation: &CancellationToken,
) -> Result<Vec<DynamicQuotedScriptConstraintSite>, Cancelled> {
    infer_dynamic_argument_constraints(snapshot, context, target, cancellation)
        .map(|constraints| constraints.quoted_scripts)
}

fn infer_dynamic_argument_constraints(
    snapshot: &AnalysisSnapshot,
    context: &SemanticCompletionContext,
    target: &ScriptProperty,
    cancellation: &CancellationToken,
) -> Result<DynamicArgumentConstraints, Cancelled> {
    let Some(invocation) = context.container_property.as_ref() else {
        return Ok(DynamicArgumentConstraints::default());
    };
    let Some((owner_kind, owner_name, caller_scope)) =
        dynamic_parameter_owner(snapshot, context, target, invocation)
    else {
        return Ok(DynamicArgumentConstraints::default());
    };
    let Some(resolved) = resolve_dynamic_definition(snapshot, &owner_kind, &owner_name) else {
        return Ok(DynamicArgumentConstraints::default());
    };
    let Some(template) = resolved.summary.template.clone() else {
        return Ok(DynamicArgumentConstraints::default());
    };
    let bindings = invocation_bindings(invocation, Some(target));
    let mut collector = ConstraintCollector::new(snapshot, cancellation);
    if !collector.budget.enter(&resolved) {
        return Ok(DynamicArgumentConstraints::default());
    }
    let result = (|| {
        let container = collector.instantiate_items(&template.items, &bindings)?;
        collector.collect_container(&container, &resolved.body_context, &[], &caller_scope)?;
        Ok(if collector.exhausted {
            DynamicArgumentConstraints::default()
        } else {
            DynamicArgumentConstraints {
                values: collector.value_sites.clone(),
                quoted_scripts: collector.quoted_script_sites.clone(),
                unenumerable: collector.unenumerable_value_site,
            }
        })
    })();
    collector.budget.leave();
    result
}

pub(crate) fn dynamic_parameter_owner(
    snapshot: &AnalysisSnapshot,
    context: &SemanticCompletionContext,
    target: &ScriptProperty,
    invocation: &ScriptProperty,
) -> Option<(String, String, ScopeContext)> {
    semantic_rules_for_completion(snapshot, context)
        .into_iter()
        .find_map(|candidate| {
            let KeyMatcher::Enum(enum_name) = &candidate.rule.key else {
                return None;
            };
            if !enum_name.eq_ignore_ascii_case("scripted_effect_params")
                || !semantic_rule_key_matches(
                    snapshot,
                    candidate.rule,
                    candidate.parent_path,
                    &target.key,
                )
                || !semantic_scope_allows(candidate.rule, candidate.scope)
            {
                return None;
            }
            let owner_kind = candidate
                .rule
                .parent_path
                .last()?
                .strip_prefix('<')?
                .strip_suffix('>')?;
            if !dynamic_definition_type(snapshot, owner_kind) {
                return None;
            }
            let owner_name = candidate.parent_path.last()?;
            if !invocation.key.eq_ignore_ascii_case(owner_name) {
                return None;
            }
            Some((
                owner_kind.to_owned(),
                owner_name.to_string(),
                candidate.scope.clone(),
            ))
        })
}

fn invocation_bindings(
    invocation: &ScriptProperty,
    target: Option<&ScriptProperty>,
) -> BTreeMap<String, SymbolicToken> {
    let mut bindings = BTreeMap::new();
    for argument in &invocation.block {
        let value = if target.is_some_and(|target| target.range == argument.range) {
            SymbolicToken::Target
        } else if let Some((value, _)) = &argument.scalar {
            SymbolicToken::Concrete(value.to_string())
        } else {
            SymbolicToken::Unknown
        };
        bindings.insert(argument.key.to_ascii_lowercase(), value);
    }
    bindings
}

struct ConstraintCollector<'a> {
    snapshot: &'a AnalysisSnapshot,
    cancellation: &'a CancellationToken,
    budget: SymbolicBudget,
    exhausted: bool,
    value_sites: Vec<DynamicValueConstraintSite>,
    quoted_script_sites: Vec<DynamicQuotedScriptConstraintSite>,
    unenumerable_value_site: bool,
}

impl<'a> ConstraintCollector<'a> {
    fn new(snapshot: &'a AnalysisSnapshot, cancellation: &'a CancellationToken) -> Self {
        Self {
            snapshot,
            cancellation,
            budget: SymbolicBudget::default(),
            exhausted: false,
            value_sites: Vec::new(),
            quoted_script_sites: Vec::new(),
            unenumerable_value_site: false,
        }
    }

    fn instantiate_items(
        &mut self,
        items: &[TemplateItem],
        bindings: &BTreeMap<String, SymbolicToken>,
    ) -> Result<SymbolicContainer, Cancelled> {
        let mut container = SymbolicContainer::default();
        for item in items {
            self.cancellation.checkpoint()?;
            if !self.charge_node() {
                break;
            }
            match item {
                TemplateItem::Property(property) => {
                    container
                        .properties
                        .push(self.instantiate_property(property, bindings)?);
                }
                TemplateItem::BareValue(token) => {
                    container
                        .bare_values
                        .push(self.render_token(token, bindings));
                }
                TemplateItem::Conditional(conditional) => {
                    let supplied = bindings.contains_key(&conditional.name.to_ascii_lowercase());
                    if supplied != conditional.negated {
                        let nested = self.instantiate_items(&conditional.items, bindings)?;
                        container.properties.extend(nested.properties);
                        container.bare_values.extend(nested.bare_values);
                    }
                }
            }
        }
        Ok(container)
    }

    fn instantiate_property(
        &mut self,
        property: &TemplateProperty,
        bindings: &BTreeMap<String, SymbolicToken>,
    ) -> Result<SymbolicProperty, Cancelled> {
        let key = self.render_token(&property.key, bindings);
        let value = match &property.value {
            TemplateValue::Scalar(token) => {
                SymbolicValue::Scalar(self.render_token(token, bindings))
            }
            TemplateValue::Block { items, .. } => {
                SymbolicValue::Block(self.instantiate_items(items, bindings)?)
            }
        };
        Ok(SymbolicProperty {
            key,
            operator: property.operator.clone(),
            value,
        })
    }

    fn render_token(
        &mut self,
        token: &TemplateToken,
        bindings: &BTreeMap<String, SymbolicToken>,
    ) -> SymbolicToken {
        if let [TemplateFragment::Parameter { name, .. }] = token.fragments.as_slice() {
            let rendered = bindings
                .get(&name.to_ascii_lowercase())
                .cloned()
                .unwrap_or(SymbolicToken::Unknown);
            if let SymbolicToken::Concrete(value) = &rendered
                && !self.budget.charge_token_bytes(value.len())
            {
                self.exhausted = true;
                return SymbolicToken::Unknown;
            }
            return rendered;
        }
        let mut value = String::new();
        for fragment in &token.fragments {
            match fragment {
                TemplateFragment::Literal(literal) => value.push_str(literal),
                TemplateFragment::Parameter { name, .. } => {
                    let Some(SymbolicToken::Concrete(argument)) =
                        bindings.get(&name.to_ascii_lowercase())
                    else {
                        return SymbolicToken::Unknown;
                    };
                    value.push_str(argument);
                }
            }
        }
        if !self.budget.charge_token_bytes(value.len()) {
            self.exhausted = true;
            SymbolicToken::Unknown
        } else {
            SymbolicToken::Concrete(value)
        }
    }

    fn collect_container(
        &mut self,
        container: &SymbolicContainer,
        context: &str,
        parent_path: &[std::sync::Arc<str>],
        scope: &ScopeContext,
    ) -> Result<(), Cancelled> {
        if self.exhausted {
            return Ok(());
        }
        if container
            .bare_values
            .iter()
            .any(|value| matches!(value, SymbolicToken::Target))
        {
            let site = DynamicQuotedScriptConstraintSite {
                context: context.to_owned(),
                parent_path: parent_path.to_vec(),
                scope: scope.clone(),
            };
            if !self.quoted_script_sites.iter().any(|known| {
                known.context.eq_ignore_ascii_case(&site.context)
                    && known.parent_path == site.parent_path
                    && known.scope == site.scope
            }) {
                self.quoted_script_sites.push(site);
            }
        }
        let rules = semantic_rules_for_container(self.snapshot, context, parent_path, scope);
        for property in &container.properties {
            self.cancellation.checkpoint()?;
            if self.exhausted {
                break;
            }
            let SymbolicToken::Concrete(key) = &property.key else {
                continue;
            };
            let matching = rules
                .iter()
                .copied()
                .filter(|rule| {
                    // Leaf-value rules participate so a dynamic parameter inside a leaf-value
                    // container (for example `required_missions = { $MISSION$ }`) inherits the
                    // container's value-type constraint.
                    semantic_rule_key_matches(self.snapshot, rule, parent_path, key)
                        && semantic_scope_allows(rule, scope)
                        && operator_matches(rule, property)
                })
                .collect::<Vec<_>>();
            match &property.value {
                SymbolicValue::Scalar(SymbolicToken::Target) => {
                    let mut raw = matching
                        .iter()
                        .filter(|rule| matches!(rule.shape, RuleShape::Leaf | RuleShape::LeafValue))
                        .map(|rule| rule.value.clone())
                        .collect::<Vec<_>>();
                    raw.sort_by_key(|matcher| format!("{matcher:?}"));
                    raw.dedup();
                    let mut matchers = raw
                        .iter()
                        .filter(|matcher| is_completion_constraint(matcher))
                        .cloned()
                        .collect::<Vec<_>>();
                    matchers.sort_by_key(|matcher| format!("{matcher:?}"));
                    matchers.dedup();
                    if !matchers.is_empty() {
                        self.value_sites.push(DynamicValueConstraintSite {
                            matchers,
                            scope: scope.clone(),
                        });
                    } else if !raw.is_empty() {
                        // Every constraint at this site is a non-enumerable
                        // shape (numbers, opaque strings): the parameter is
                        // not free-form, but no item can be offered.
                        self.unenumerable_value_site = true;
                    }
                }
                SymbolicValue::Block(children) => {
                    if self.follow_nested_dynamic(key, children, &matching, scope)? {
                        continue;
                    }
                    let transparent_wrapper = context.eq_ignore_ascii_case("trigger")
                        && self
                            .snapshot
                            .game_profile()
                            .is_transparent_scope_wrapper(key);
                    let mut destinations = Vec::new();
                    for rule in matching
                        .iter()
                        .copied()
                        .filter(|rule| matches!(rule.shape, RuleShape::Node))
                    {
                        let (next_context, next_path) = semantic_transition_destination(
                            rule,
                            context,
                            parent_path,
                            key,
                            transparent_wrapper,
                        );
                        let next_scope = semantic_child_scope(self.snapshot, scope, rule);
                        if !destinations.iter().any(
                            |(known_context, known_path, known_scope): &(
                                std::sync::Arc<str>,
                                Vec<std::sync::Arc<str>>,
                                ScopeContext,
                            )| {
                                known_context.eq_ignore_ascii_case(next_context.as_ref())
                                    && known_path == &next_path
                                    && known_scope == &next_scope
                            },
                        ) {
                            destinations.push((next_context, next_path, next_scope));
                        }
                    }
                    if destinations.is_empty() {
                        let mut structural_path = parent_path.to_vec();
                        if !transparent_wrapper {
                            structural_path.push(pdx_engine::intern_shard_string(key));
                        }
                        self.collect_container(children, context, &structural_path, scope)?;
                    } else {
                        for (next_context, next_path, next_scope) in destinations {
                            self.collect_container(
                                children,
                                &next_context,
                                &next_path,
                                &next_scope,
                            )?;
                        }
                    }
                }
                SymbolicValue::Scalar(SymbolicToken::Concrete(_) | SymbolicToken::Unknown) => {}
            }
        }
        Ok(())
    }

    fn follow_nested_dynamic(
        &mut self,
        key: &str,
        arguments: &SymbolicContainer,
        matching: &[&pdx_rules::SemanticRule],
        scope: &ScopeContext,
    ) -> Result<bool, Cancelled> {
        let Some(type_name) = matching.iter().find_map(|rule| match &rule.key {
            KeyMatcher::Type(type_name) | KeyMatcher::Dynamic(type_name)
                if dynamic_definition_type(self.snapshot, type_name) =>
            {
                Some(type_name.as_str())
            }
            _ => None,
        }) else {
            return Ok(false);
        };
        let Some(resolved) = resolve_dynamic_definition(self.snapshot, type_name, key) else {
            if symbolic_container_contains_target(arguments) {
                self.exhausted = true;
            }
            return Ok(true);
        };
        let Some(template) = resolved.summary.template.clone() else {
            if symbolic_container_contains_target(arguments) {
                self.exhausted = true;
            }
            return Ok(true);
        };
        if !self.budget.enter(&resolved) {
            if symbolic_container_contains_target(arguments) {
                self.exhausted = true;
            }
            return Ok(true);
        }
        let bindings = symbolic_bindings(&arguments.properties);
        let result = (|| {
            let container = self.instantiate_items(&template.items, &bindings)?;
            self.collect_container(&container, &resolved.body_context, &[], scope)
        })();
        self.budget.leave();
        result?;
        Ok(true)
    }

    fn charge_node(&mut self) -> bool {
        if !self.budget.charge_node() {
            self.exhausted = true;
            false
        } else {
            true
        }
    }
}

/// Cycle guard and instantiation budgets for the symbolic walker. Genuine
/// recursion is rejected earlier by definition-site cycle analysis; these
/// bounds keep a deep acyclic fan-out from starving completion. Each budget
/// can be raised or lowered through an environment variable so a pathological
/// workspace can be diagnosed without a new build.
#[derive(Debug, Default)]
struct SymbolicBudget {
    stack: Vec<DynamicDefinitionIdentity>,
    nodes: usize,
    token_bytes: usize,
}

impl SymbolicBudget {
    fn enter(&mut self, resolved: &ResolvedDynamicDefinition) -> bool {
        if self.stack.contains(&resolved.identity) || self.stack.len() >= max_symbolic_depth() {
            return false;
        }
        self.stack.push(resolved.identity.clone());
        true
    }

    fn leave(&mut self) {
        let _ = self.stack.pop();
    }

    fn charge_node(&mut self) -> bool {
        self.nodes = self.nodes.saturating_add(1);
        self.nodes <= max_symbolic_nodes()
    }

    fn charge_token_bytes(&mut self, bytes: usize) -> bool {
        self.token_bytes = self.token_bytes.saturating_add(bytes);
        self.token_bytes <= max_symbolic_token_bytes()
    }
}

fn max_symbolic_depth() -> usize {
    static VALUE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *VALUE.get_or_init(|| env_budget("PDX_DYNAMIC_DEPTH", 32, 1, 1024))
}

fn max_symbolic_nodes() -> usize {
    static VALUE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *VALUE.get_or_init(|| env_budget("PDX_DYNAMIC_NODES", 200_000, 1, 100_000_000))
}

fn max_symbolic_token_bytes() -> usize {
    static VALUE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *VALUE.get_or_init(|| {
        env_budget(
            "PDX_DYNAMIC_TOKEN_BYTES",
            4 * 1024 * 1024,
            1,
            u64::MAX as usize,
        )
    })
}

/// Reads one budget override; unparseable values fall back to the default so a
/// typo can never silently disable the safety net.
fn env_budget(name: &str, default: usize, min: usize, max: usize) -> usize {
    let Some(raw) = std::env::var(name).ok() else {
        return default;
    };
    raw.trim()
        .parse::<usize>()
        .map_or(default, |parsed| parsed.clamp(min, max))
}

fn symbolic_bindings(arguments: &[SymbolicProperty]) -> BTreeMap<String, SymbolicToken> {
    let mut bindings = BTreeMap::new();
    for argument in arguments {
        let SymbolicToken::Concrete(key) = &argument.key else {
            continue;
        };
        let value = match &argument.value {
            SymbolicValue::Scalar(value) => value.clone(),
            SymbolicValue::Block(_) => SymbolicToken::Unknown,
        };
        bindings.insert(key.to_ascii_lowercase(), value);
    }
    bindings
}

fn symbolic_container_contains_target(container: &SymbolicContainer) -> bool {
    container
        .bare_values
        .iter()
        .any(|value| matches!(value, SymbolicToken::Target))
        || container.properties.iter().any(|property| {
            matches!(property.key, SymbolicToken::Target)
                || match &property.value {
                    SymbolicValue::Scalar(value) => matches!(value, SymbolicToken::Target),
                    SymbolicValue::Block(children) => symbolic_container_contains_target(children),
                }
        })
}

fn operator_matches(rule: &pdx_rules::SemanticRule, property: &SymbolicProperty) -> bool {
    rule.operator
        .as_deref()
        .is_none_or(|operator| property.operator.as_deref() == Some(operator))
}

fn is_completion_constraint(matcher: &ValueMatcher) -> bool {
    !matches!(
        matcher,
        ValueMatcher::AnyScalar
            | ValueMatcher::Int { .. }
            | ValueMatcher::Float { .. }
            | ValueMatcher::Date
            | ValueMatcher::DynamicSet(_)
            | ValueMatcher::Filepath
            | ValueMatcher::Opaque(_)
    )
}
