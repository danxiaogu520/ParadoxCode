//! Binding-aware replacement replay shared by indexing and IDE queries.
use crate::analysis::{Analysis, AnalysisCoverage, AnalysisLimit, ResidualReason};
use crate::{ScopeState, TemplateFragment, TemplateItem, TemplateToken, TemplateValue};
use rules::ir::{FieldValue, Matcher, MatcherId, SchemaId, Shape};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug)]
/// Interpretation of one substituted token at its usage site.
pub enum Domain {
    /// Scalar matcher alternatives valid at one site.
    Value(Vec<MatcherId>),
    /// A key in a specific schema and value shape.
    Key { schema: SchemaId, shape: Shape },
    /// A complete quoted block or a spliced block fragment.
    Payload { schema: SchemaId, complete: bool },
    /// A recursive or unresolved usage grants no concrete constraint.
    Unresolved,
}

#[derive(Clone, Debug)]
/// One source-ranged parameter usage with its invocation scope.
pub struct ParameterSite {
    /// Callable containing this usage; forwarded definitions can share offsets.
    pub origin: (String, String),
    /// Matcher, key or payload interpretation.
    pub domain: Domain,
    /// Definition-side token with forwarded substitutions.
    pub token: TemplateToken,
    /// Scope registers at the usage site.
    pub state: ScopeState,
}

#[allow(clippy::too_many_arguments)]
/// Replays active branches, leaving the selected parameter unbound.
/// Returns a checkpoint error immediately if cancellation is requested.
pub fn parameter_sites<E>(
    ir: &rules::ir::RulesIr,
    facts: &dyn rules::ir::SymbolFacts,
    kind: &str,
    name: &str,
    parameter: &str,
    bindings: &BTreeMap<String, String>,
    state: ScopeState,
    checkpoint: &mut dyn FnMut() -> Result<(), E>,
) -> Result<Analysis<Vec<ParameterSite>>, E> {
    parameter_sites_with(
        ir, facts, kind, name, parameter, bindings, state, false, checkpoint,
    )
}

#[allow(clippy::too_many_arguments)]
/// Replays active branches for syntactic symbol indexing, including preview blocks.
/// Preview writes are declarations for navigation even though they do not execute.
/// Returns a checkpoint error immediately if cancellation is requested.
pub fn parameter_symbol_sites<E>(
    ir: &rules::ir::RulesIr,
    facts: &dyn rules::ir::SymbolFacts,
    kind: &str,
    name: &str,
    parameter: &str,
    bindings: &BTreeMap<String, String>,
    state: ScopeState,
    checkpoint: &mut dyn FnMut() -> Result<(), E>,
) -> Result<Analysis<Vec<ParameterSite>>, E> {
    parameter_sites_with(
        ir, facts, kind, name, parameter, bindings, state, true, checkpoint,
    )
}

#[allow(clippy::too_many_arguments)]
fn parameter_sites_with<E>(
    ir: &rules::ir::RulesIr,
    facts: &dyn rules::ir::SymbolFacts,
    kind: &str,
    name: &str,
    parameter: &str,
    bindings: &BTreeMap<String, String>,
    state: ScopeState,
    include_display_only: bool,
    checkpoint: &mut dyn FnMut() -> Result<(), E>,
) -> Result<Analysis<Vec<ParameterSite>>, E> {
    let environment = bindings
        .iter()
        .filter(|(key, _)| !key.eq_ignore_ascii_case(parameter))
        .map(|(key, value)| {
            (
                key.to_ascii_lowercase(),
                vec![TemplateFragment::Literal(value.clone())],
            )
        })
        .collect();
    let mut replay = Replay {
        ir,
        facts,
        parameter,
        checkpoint,
        coverage: AnalysisCoverage::default(),
        sites: Vec::new(),
        missing: BTreeSet::new(),
        visiting: BTreeSet::new(),
        budget: 16_384,
        all_branches: false,
        include_display_only,
        origin: (String::new(), String::new()),
    };
    replay.call(
        kind,
        name,
        &environment,
        &bindings
            .keys()
            .map(|key| key.to_ascii_lowercase())
            .collect(),
        state,
    )?;
    Ok(Analysis {
        value: replay.sites,
        coverage: replay.coverage,
    })
}

/// Inspects every definition-side branch before invocation bindings are known.
/// Returns a checkpoint error immediately if cancellation is requested.
pub fn definition_parameter_sites<E>(
    ir: &rules::ir::RulesIr,
    facts: &dyn rules::ir::SymbolFacts,
    kind: &str,
    name: &str,
    parameter: &str,
    checkpoint: &mut dyn FnMut() -> Result<(), E>,
) -> Result<Analysis<Vec<ParameterSite>>, E> {
    let mut replay = Replay {
        ir,
        facts,
        parameter,
        checkpoint,
        coverage: AnalysisCoverage::default(),
        sites: Vec::new(),
        missing: BTreeSet::new(),
        visiting: BTreeSet::new(),
        budget: 16_384,
        all_branches: true,
        include_display_only: false,
        origin: (String::new(), String::new()),
    };
    replay.call(
        kind,
        name,
        &Environment::new(),
        &BTreeSet::new(),
        ScopeState {
            root: crate::ScopeValue::Unknown,
            current: vec![crate::ScopeValue::Unknown],
            from: Vec::new(),
            previous: Vec::new(),
        },
    )?;
    Ok(Analysis {
        value: replay.sites,
        coverage: replay.coverage,
    })
}

/// Collects substitutions that remain unbound in active invocation branches.
/// Returns a checkpoint error immediately if cancellation is requested.
pub fn missing_parameters<E>(
    ir: &rules::ir::RulesIr,
    facts: &dyn rules::ir::SymbolFacts,
    kind: &str,
    name: &str,
    bindings: &BTreeMap<String, String>,
    state: ScopeState,
    checkpoint: &mut dyn FnMut() -> Result<(), E>,
) -> Result<Analysis<BTreeSet<String>>, E> {
    let env = bindings
        .iter()
        .map(|(key, value)| {
            (
                key.to_ascii_lowercase(),
                vec![TemplateFragment::Literal(value.clone())],
            )
        })
        .collect();
    let mut replay = Replay {
        ir,
        facts,
        parameter: "",
        checkpoint,
        coverage: AnalysisCoverage::default(),
        sites: Vec::new(),
        missing: BTreeSet::new(),
        visiting: BTreeSet::new(),
        budget: 16_384,
        all_branches: false,
        include_display_only: false,
        origin: (String::new(), String::new()),
    };
    replay.call(kind, name, &env, &bindings.keys().cloned().collect(), state)?;
    Ok(Analysis {
        value: replay.missing,
        coverage: replay.coverage,
    })
}

type Environment = BTreeMap<String, Vec<TemplateFragment>>;

struct Replay<'a, E> {
    ir: &'a rules::ir::RulesIr,
    facts: &'a dyn rules::ir::SymbolFacts,
    parameter: &'a str,
    checkpoint: &'a mut dyn FnMut() -> Result<(), E>,
    coverage: AnalysisCoverage,
    sites: Vec<ParameterSite>,
    missing: BTreeSet<String>,
    visiting: BTreeSet<(String, String)>,
    budget: usize,
    all_branches: bool,
    include_display_only: bool,
    origin: (String, String),
}

impl<E> Replay<'_, E> {
    fn call(
        &mut self,
        kind: &str,
        name: &str,
        env: &Environment,
        active: &BTreeSet<String>,
        state: ScopeState,
    ) -> Result<(), E> {
        (self.checkpoint)()?;
        let key = (kind.to_ascii_lowercase(), name.to_ascii_lowercase());
        if self.visiting.len() >= 32 || !self.visiting.insert(key.clone()) {
            self.coverage.limits.insert(if self.visiting.len() >= 32 {
                AnalysisLimit::CallDepth
            } else {
                AnalysisLimit::RecursiveState
            });
            for fragments in env.values() {
                self.add(
                    Domain::Unresolved,
                    TemplateToken {
                        range: text::TextRange::new(0, 0).expect("empty range"),
                        quoted: false,
                        fragments: fragments.clone(),
                    },
                    &state,
                );
            }
            return Ok(());
        }
        let previous_origin = std::mem::replace(&mut self.origin, key.clone());
        if let Some(type_id) = self.ir.type_by_name(kind)
            && let Some(template) = self.facts.replacement_template(type_id, name)
            && let Some(schema) = replacement_body(self.ir, type_id)
        {
            if self.parameter.is_empty() {
                let mut budget = 16_384;
                self.required_substitutions(&template.items, env, active, &mut budget)?;
            }
            self.walk(schema, &template.items, env, active, state, None)?;
        } else {
            self.coverage
                .limits
                .insert(AnalysisLimit::UnavailableTemplate);
        }
        self.origin = previous_origin;
        self.visiting.remove(&key);
        Ok(())
    }

    // Text replacement runs before the resulting script is interpreted. Even an
    // unknown field or a presentation-only block must bind its active substitutions.
    fn required_substitutions(
        &mut self,
        items: &[TemplateItem],
        env: &Environment,
        active: &BTreeSet<String>,
        budget: &mut usize,
    ) -> Result<(), E> {
        for item in items {
            (self.checkpoint)()?;
            if *budget == 0 {
                self.coverage.limits.insert(AnalysisLimit::Nodes);
                break;
            }
            *budget -= 1;
            match item {
                TemplateItem::Conditional(conditional) => {
                    if self.all_branches
                        || active.contains(&conditional.name.to_ascii_lowercase())
                            != conditional.negated
                    {
                        self.required_substitutions(&conditional.items, env, active, budget)?;
                    }
                }
                TemplateItem::Property(property) => {
                    self.required_token(&property.key, env);
                    match &property.value {
                        TemplateValue::Scalar(token) => self.required_token(token, env),
                        TemplateValue::Block { items, .. } => {
                            self.required_substitutions(items, env, active, budget)?
                        }
                    }
                }
                TemplateItem::BareValue(token) => self.required_token(token, env),
            }
        }
        Ok(())
    }

    fn required_token(&mut self, token: &TemplateToken, env: &Environment) {
        self.missing.extend(
            substitute(token, env)
                .fragments
                .into_iter()
                .filter_map(|fragment| match fragment {
                    TemplateFragment::Parameter { name, .. } => Some(name.to_string()),
                    _ => None,
                }),
        );
    }

    fn add(&mut self, domain: Domain, token: TemplateToken, state: &ScopeState) {
        if !matches!(domain, Domain::Unresolved) {
            self.missing.extend(
                token
                    .fragments
                    .iter()
                    .filter_map(|fragment| match fragment {
                        TemplateFragment::Parameter { name, .. } => Some(name.to_string()),
                        _ => None,
                    }),
            );
        }
        if token.fragments.iter().any(|fragment| matches!(fragment, TemplateFragment::Parameter { name, .. } if name.eq_ignore_ascii_case(self.parameter))) {
            self.sites.push(ParameterSite { origin: self.origin.clone(), domain, token, state: state.clone() });
        }
    }

    fn walk(
        &mut self,
        schema: SchemaId,
        items: &[TemplateItem],
        env: &Environment,
        active: &BTreeSet<String>,
        state: ScopeState,
        switch_cases: Option<&[MatcherId]>,
    ) -> Result<(), E> {
        let ir = self.ir;
        for item in items {
            (self.checkpoint)()?;
            if self.budget == 0 {
                self.coverage.limits.insert(AnalysisLimit::Nodes);
                break;
            }
            self.budget -= 1;
            match item {
                TemplateItem::Conditional(conditional) => {
                    if self.all_branches
                        || active.contains(&conditional.name.to_ascii_lowercase())
                            != conditional.negated
                    {
                        self.walk(
                            schema,
                            &conditional.items,
                            env,
                            active,
                            state.clone(),
                            switch_cases,
                        )?;
                    }
                }
                TemplateItem::BareValue(token) => {
                    let domain = ir.schema(schema).items.map_or(
                        Domain::Payload {
                            schema,
                            complete: false,
                        },
                        |matcher| Domain::Value(vec![matcher]),
                    );
                    self.add(domain, substitute(token, env), &state);
                }
                TemplateItem::Property(property) => {
                    let token = substitute(&property.key, env);
                    let shape = match &property.value {
                        TemplateValue::Block { .. } => Shape::Block,
                        TemplateValue::Scalar(token) if token.quoted => Shape::Quoted,
                        _ => Shape::Scalar,
                    };
                    let Some(key) = literal(&token) else {
                        self.coverage.residuals.insert(ResidualReason::DynamicKey);
                        let domain = if shape == Shape::Block
                            && let Some(matchers) = switch_cases
                        {
                            Domain::Value(matchers.to_vec())
                        } else {
                            Domain::Key {
                                schema,
                                shape: if shape == Shape::Quoted {
                                    Shape::Scalar
                                } else {
                                    shape
                                },
                            }
                        };
                        self.add(domain, token, &state);
                        continue;
                    };
                    let mut fields = ir.lookup(schema, &key, shape).collect::<Vec<_>>();
                    if shape == Shape::Quoted {
                        fields.extend(ir.lookup(schema, &key, Shape::Scalar));
                    }
                    let exact = fields.iter().copied().filter(|id| matches!(ir.matcher(ir.field(*id).key), Matcher::Literal(symbol) if ir.strings().resolve(*symbol).eq_ignore_ascii_case(&key))).collect::<Vec<_>>();
                    if !exact.is_empty() {
                        fields = exact;
                    }
                    fields.retain(|id| key_matches(ir, ir.field(*id).key, &key, self.facts));
                    let scoped = fields
                        .iter()
                        .copied()
                        .filter(|id| {
                            ir.field(*id).scope.as_ref().is_none_or(|scope| {
                                scope.scopes_in.is_empty()
                                    || state.current.first().is_none_or(|current| {
                                        crate::ir_lowering::scope_value_allows(
                                            ir,
                                            current,
                                            &scope.scopes_in,
                                        )
                                    })
                            })
                        })
                        .collect::<Vec<_>>();
                    if !scoped.is_empty() {
                        fields = scoped;
                    }
                    // Forward argument tokens through the callee's own schema rather than treating
                    // its parameter block as effect statements.
                    let callee = fields.iter().find_map(|id| {
                        callable_kind(ir, ir.field(*id).key).filter(|kind| {
                            ir.type_by_name(kind).is_some_and(|type_id| {
                                self.facts.replacement_template(type_id, &key).is_some()
                            })
                        })
                    });
                    if let Some(kind) = callee {
                        let mut child_env = Environment::new();
                        let mut child_active = BTreeSet::new();
                        if let TemplateValue::Block { items, .. } = &property.value {
                            forward_bindings(items, env, active, &mut child_env, &mut child_active);
                        }
                        self.call(&kind, &key, &child_env, &child_active, state.clone())?;
                        continue;
                    }
                    match &property.value {
                        TemplateValue::Scalar(value) => {
                            let token = substitute(value, env);
                            let matchers = fields
                                .iter()
                                .filter_map(|id| match ir.field(*id).value {
                                    FieldValue::Scalar(matcher) => Some(matcher),
                                    _ => None,
                                })
                                .collect::<Vec<_>>();
                            if !matchers.is_empty() {
                                self.add(Domain::Value(matchers), token.clone(), &state);
                            }
                            for id in fields {
                                if let FieldValue::Quoted(child) = ir.field(id).value {
                                    self.add(
                                        Domain::Payload {
                                            schema: child,
                                            complete: true,
                                        },
                                        token.clone(),
                                        &state,
                                    );
                                }
                            }
                        }
                        TemplateValue::Block { items, .. } => {
                            for id in fields {
                                if !self.include_display_only
                                    && ir.field(id).control.as_ref().is_some_and(|control| {
                                        control.kind == rules::source::ControlKind::DisplayOnly
                                    })
                                {
                                    continue;
                                }
                                if let Some(child) = ir.child(id, schema) {
                                    let next = crate::transition_ir_scope(
                                        ir,
                                        state.clone(),
                                        ir.field(id).scope.as_ref(),
                                        &key,
                                    );
                                    let cases = ir.field(id).control.as_ref().and_then(|control| {
                                        let selector_schema = control.selector_schema?;
                                        let on = ir.strings().resolve(control.on?);
                                        let selector = active_scalar(items, on, env, active)?;
                                        let matchers = ir
                                            .lookup(selector_schema, &selector, Shape::Scalar)
                                            .filter(|id| {
                                                key_matches(
                                                    ir,
                                                    ir.field(*id).key,
                                                    &selector,
                                                    self.facts,
                                                )
                                            })
                                            .filter_map(|id| match ir.field(id).value {
                                                FieldValue::Scalar(matcher) => Some(matcher),
                                                _ => None,
                                            })
                                            .collect::<Vec<_>>();
                                        (!matchers.is_empty()).then_some(matchers)
                                    });
                                    self.walk(child, items, env, active, next, cases.as_deref())?;
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

fn active_scalar(
    items: &[TemplateItem],
    key: &str,
    env: &Environment,
    active: &BTreeSet<String>,
) -> Option<String> {
    items.iter().rev().find_map(|item| match item {
        TemplateItem::Property(property)
            if literal(&substitute(&property.key, env))
                .is_some_and(|name| name.eq_ignore_ascii_case(key)) =>
        {
            match &property.value {
                TemplateValue::Scalar(token) => literal(&substitute(token, env)),
                _ => None,
            }
        }
        TemplateItem::Conditional(conditional)
            if active.contains(&conditional.name.to_ascii_lowercase()) != conditional.negated =>
        {
            active_scalar(&conditional.items, key, env, active)
        }
        _ => None,
    })
}

/// Type namespace of a matcher implementing the declared Callable trait.
pub fn callable_kind(ir: &rules::ir::RulesIr, matcher: MatcherId) -> Option<String> {
    match ir.matcher(matcher) {
        Matcher::Ref(rules::ir::RefTarget::Type { type_id, .. }) => {
            let callable = ir.trait_by_name("Callable")?;
            let info = ir.type_info(*type_id);
            info.trait_impls
                .iter()
                .any(|implementation| implementation.trait_id == callable)
                .then(|| ir.strings().resolve(info.name).to_owned())
        }
        Matcher::Union(items) => items.iter().find_map(|id| callable_kind(ir, *id)),
        _ => None,
    }
}

fn forward_bindings(
    items: &[TemplateItem],
    env: &Environment,
    active: &BTreeSet<String>,
    out: &mut Environment,
    child_active: &mut BTreeSet<String>,
) {
    for item in items {
        match item {
            TemplateItem::Conditional(conditional)
                if active.contains(&conditional.name.to_ascii_lowercase())
                    != conditional.negated =>
            {
                forward_bindings(&conditional.items, env, active, out, child_active)
            }
            TemplateItem::Property(property) => {
                if let Some(key) = literal(&substitute(&property.key, env))
                    && let TemplateValue::Scalar(value) = &property.value
                {
                    let token = substitute(value, env);
                    let present = value.fragments.iter().all(|fragment| match fragment {
                        TemplateFragment::Literal(_) => true,
                        TemplateFragment::Parameter { name, .. } => {
                            active.contains(&name.to_ascii_lowercase())
                        }
                    });
                    if present {
                        child_active.insert(key.to_ascii_lowercase());
                    }
                    out.insert(key.to_ascii_lowercase(), token.fragments);
                }
            }
            _ => {}
        }
    }
}

fn substitute(token: &TemplateToken, env: &Environment) -> TemplateToken {
    let mut token = token.clone();
    token.fragments = token
        .fragments
        .iter()
        .flat_map(|fragment| match fragment {
            TemplateFragment::Parameter { name, .. } => env
                .get(&name.to_ascii_lowercase())
                .cloned()
                .unwrap_or_else(|| vec![fragment.clone()]),
            TemplateFragment::Literal(_) => vec![fragment.clone()],
        })
        .collect();
    token
}

fn literal(token: &TemplateToken) -> Option<String> {
    token
        .fragments
        .iter()
        .map(|fragment| match fragment {
            TemplateFragment::Literal(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()
        .map(|parts| parts.concat())
}

/// Body schema declared by the Callable implementation of a type.
pub fn replacement_body(ir: &rules::ir::RulesIr, type_id: rules::ir::TypeId) -> Option<SchemaId> {
    ir.type_info(type_id)
        .trait_impls
        .iter()
        .filter(|implementation| Some(implementation.trait_id) == ir.trait_by_name("Callable"))
        .find_map(|implementation| {
            implementation
                .arguments
                .iter()
                .find_map(|(name, argument)| {
                    if ir.strings.resolve(*name) != "body" {
                        return None;
                    }
                    match argument {
                        rules::ir::TraitArgument::Text(body) => {
                            ir.schema_by_name(ir.strings.resolve(*body))
                        }
                        _ => None,
                    }
                })
        })
}

fn key_matches(
    ir: &rules::ir::RulesIr,
    matcher: MatcherId,
    value: &str,
    facts: &dyn rules::ir::SymbolFacts,
) -> bool {
    match ir.matcher(matcher) {
        Matcher::Link => crate::is_ir_scope_link(ir, value),
        Matcher::Union(items) => items
            .iter()
            .any(|item| key_matches(ir, *item, value, facts)),
        _ => ir.scalar_matches(matcher, value, &FactsRef(facts)),
    }
}

struct FactsRef<'a>(&'a dyn rules::ir::SymbolFacts);
impl rules::ir::SymbolFacts for FactsRef<'_> {
    fn type_member(&self, id: rules::ir::TypeId, name: &str) -> bool {
        self.0.type_member(id, name)
    }
    fn type_subtype_member(
        &self,
        id: rules::ir::TypeId,
        subtype: rules::ir::Symbol,
        name: &str,
    ) -> bool {
        self.0.type_subtype_member(id, subtype, name)
    }
}
