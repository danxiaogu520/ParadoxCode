//! Query-local inference of scripted-macro argument value constraints.

use std::collections::BTreeMap;

use pdx_engine::AnalysisSnapshot;
use pdx_engine::hir::{
    MacroTemplateFragment, MacroTemplateItem, MacroTemplateProperty, MacroTemplateToken,
    MacroTemplateValue,
};
use pdx_rules::{KeyMatcher, RuleShape, ValueMatcher};

use crate::macro_expansion::MacroExpansionSession;
use crate::semantic::{
    resolve_macro_definition, scripted_macro_type, semantic_child_scope, semantic_rule_key_matches,
    semantic_scope_allows, semantic_transition_destination,
};
use crate::support::{ScopeContext, ScriptProperty};
use crate::types::{CancellationToken, Cancelled};

use super::{
    SemanticCompletionContext, semantic_rules_for_completion, semantic_rules_for_container,
};

#[derive(Clone, Debug)]
pub(crate) struct MacroValueConstraintSite {
    pub(crate) matchers: Vec<ValueMatcher>,
    pub(crate) scope: ScopeContext,
}

#[derive(Clone, Debug)]
pub(crate) struct MacroQuotedScriptConstraintSite {
    pub(crate) context: String,
    pub(crate) parent_path: Vec<String>,
    pub(crate) scope: ScopeContext,
}

#[derive(Clone, Debug, Default)]
struct MacroArgumentConstraints {
    values: Vec<MacroValueConstraintSite>,
    quoted_scripts: Vec<MacroQuotedScriptConstraintSite>,
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

pub(crate) fn infer_macro_value_constraints(
    snapshot: &AnalysisSnapshot,
    context: &SemanticCompletionContext,
    target: &ScriptProperty,
    cancellation: &CancellationToken,
) -> Result<Vec<MacroValueConstraintSite>, Cancelled> {
    infer_macro_argument_constraints(snapshot, context, target, cancellation)
        .map(|constraints| constraints.values)
}

pub(crate) fn infer_macro_quoted_script_constraints(
    snapshot: &AnalysisSnapshot,
    context: &SemanticCompletionContext,
    target: &ScriptProperty,
    cancellation: &CancellationToken,
) -> Result<Vec<MacroQuotedScriptConstraintSite>, Cancelled> {
    infer_macro_argument_constraints(snapshot, context, target, cancellation)
        .map(|constraints| constraints.quoted_scripts)
}

fn infer_macro_argument_constraints(
    snapshot: &AnalysisSnapshot,
    context: &SemanticCompletionContext,
    target: &ScriptProperty,
    cancellation: &CancellationToken,
) -> Result<MacroArgumentConstraints, Cancelled> {
    let Some(invocation) = context.container_property.as_ref() else {
        return Ok(MacroArgumentConstraints::default());
    };
    let Some((owner_kind, owner_name, caller_scope)) =
        macro_parameter_owner(snapshot, context, target, invocation)
    else {
        return Ok(MacroArgumentConstraints::default());
    };
    let Some(resolved) = resolve_macro_definition(snapshot, &owner_kind, &owner_name) else {
        return Ok(MacroArgumentConstraints::default());
    };
    let Some(template) = resolved.summary.template.clone() else {
        return Ok(MacroArgumentConstraints::default());
    };
    let bindings = invocation_bindings(invocation, Some(target));
    let mut collector = ConstraintCollector::new(snapshot, cancellation);
    if collector.expansion.enter(&resolved).is_err() {
        return Ok(MacroArgumentConstraints::default());
    }
    let result = (|| {
        let container = collector.instantiate_items(&template.items, &bindings)?;
        collector.collect_container(&container, &resolved.body_context, &[], &caller_scope)?;
        Ok(if collector.exhausted {
            MacroArgumentConstraints::default()
        } else {
            MacroArgumentConstraints {
                values: collector.value_sites.clone(),
                quoted_scripts: collector.quoted_script_sites.clone(),
            }
        })
    })();
    collector.expansion.leave();
    result
}

fn macro_parameter_owner(
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
            if !scripted_macro_type(snapshot, owner_kind) {
                return None;
            }
            let owner_name = candidate.parent_path.last()?;
            if !invocation.key.eq_ignore_ascii_case(owner_name) {
                return None;
            }
            Some((
                owner_kind.to_owned(),
                owner_name.clone(),
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
            SymbolicToken::Concrete(value.clone())
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
    expansion: MacroExpansionSession,
    exhausted: bool,
    value_sites: Vec<MacroValueConstraintSite>,
    quoted_script_sites: Vec<MacroQuotedScriptConstraintSite>,
}

impl<'a> ConstraintCollector<'a> {
    fn new(snapshot: &'a AnalysisSnapshot, cancellation: &'a CancellationToken) -> Self {
        Self {
            snapshot,
            cancellation,
            expansion: MacroExpansionSession::default(),
            exhausted: false,
            value_sites: Vec::new(),
            quoted_script_sites: Vec::new(),
        }
    }

    fn instantiate_items(
        &mut self,
        items: &[MacroTemplateItem],
        bindings: &BTreeMap<String, SymbolicToken>,
    ) -> Result<SymbolicContainer, Cancelled> {
        let mut container = SymbolicContainer::default();
        for item in items {
            self.cancellation.checkpoint()?;
            if !self.charge_node() {
                break;
            }
            match item {
                MacroTemplateItem::Property(property) => {
                    container
                        .properties
                        .push(self.instantiate_property(property, bindings)?);
                }
                MacroTemplateItem::BareValue(token) => {
                    container
                        .bare_values
                        .push(self.render_token(token, bindings));
                }
                MacroTemplateItem::Conditional(conditional) => {
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
        property: &MacroTemplateProperty,
        bindings: &BTreeMap<String, SymbolicToken>,
    ) -> Result<SymbolicProperty, Cancelled> {
        let key = self.render_token(&property.key, bindings);
        let value = match &property.value {
            MacroTemplateValue::Scalar(token) => {
                SymbolicValue::Scalar(self.render_token(token, bindings))
            }
            MacroTemplateValue::Block { items, .. } => {
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
        token: &MacroTemplateToken,
        bindings: &BTreeMap<String, SymbolicToken>,
    ) -> SymbolicToken {
        if let [MacroTemplateFragment::Parameter { name, .. }] = token.fragments.as_slice() {
            let rendered = bindings
                .get(&name.to_ascii_lowercase())
                .cloned()
                .unwrap_or(SymbolicToken::Unknown);
            if let SymbolicToken::Concrete(value) = &rendered
                && self.expansion.charge_token_bytes(value.len()).is_err()
            {
                self.exhausted = true;
                return SymbolicToken::Unknown;
            }
            return rendered;
        }
        let mut value = String::new();
        for fragment in &token.fragments {
            match fragment {
                MacroTemplateFragment::Literal(literal) => value.push_str(literal),
                MacroTemplateFragment::Parameter { name, .. } => {
                    let Some(SymbolicToken::Concrete(argument)) =
                        bindings.get(&name.to_ascii_lowercase())
                    else {
                        return SymbolicToken::Unknown;
                    };
                    value.push_str(argument);
                }
            }
        }
        if self.expansion.charge_token_bytes(value.len()).is_err() {
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
        parent_path: &[String],
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
            let site = MacroQuotedScriptConstraintSite {
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
                    // Leaf-value rules participate so a macro parameter inside a leaf-value
                    // container (for example `required_missions = { $MISSION$ }`) inherits the
                    // container's value-type constraint.
                    semantic_rule_key_matches(self.snapshot, rule, parent_path, key)
                        && semantic_scope_allows(rule, scope)
                        && operator_matches(rule, property)
                })
                .collect::<Vec<_>>();
            match &property.value {
                SymbolicValue::Scalar(SymbolicToken::Target) => {
                    let mut matchers = matching
                        .iter()
                        .filter(|rule| matches!(rule.shape, RuleShape::Leaf | RuleShape::LeafValue))
                        .map(|rule| rule.value.clone())
                        .filter(is_completion_constraint)
                        .collect::<Vec<_>>();
                    matchers.sort_by_key(|matcher| format!("{matcher:?}"));
                    matchers.dedup();
                    if !matchers.is_empty() {
                        self.value_sites.push(MacroValueConstraintSite {
                            matchers,
                            scope: scope.clone(),
                        });
                    }
                }
                SymbolicValue::Block(children) => {
                    if self.follow_nested_macro(key, children, &matching, scope)? {
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
                                String,
                                Vec<String>,
                                ScopeContext,
                            )| {
                                known_context.eq_ignore_ascii_case(&next_context)
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
                            structural_path.push(key.clone());
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

    fn follow_nested_macro(
        &mut self,
        key: &str,
        arguments: &SymbolicContainer,
        matching: &[&pdx_rules::SemanticRule],
        scope: &ScopeContext,
    ) -> Result<bool, Cancelled> {
        let Some(type_name) = matching.iter().find_map(|rule| match &rule.key {
            KeyMatcher::Type(type_name) | KeyMatcher::Dynamic(type_name)
                if scripted_macro_type(self.snapshot, type_name) =>
            {
                Some(type_name.as_str())
            }
            _ => None,
        }) else {
            return Ok(false);
        };
        let Some(resolved) = resolve_macro_definition(self.snapshot, type_name, key) else {
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
        if self.expansion.enter(&resolved).is_err() {
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
        self.expansion.leave();
        result?;
        Ok(true)
    }

    fn charge_node(&mut self) -> bool {
        if self.expansion.charge_node().is_err() {
            self.exhausted = true;
            false
        } else {
            true
        }
    }
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
