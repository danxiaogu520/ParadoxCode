//! Binding-aware Template interpreter shared by indexing and IDE queries.
use crate::analysis::{Analysis, AnalysisCoverage, AnalysisLimit, ResidualReason};
use crate::{ScopeState, TemplateFragment, TemplateToken};
use rules::ir::{FieldValue, Matcher, MatcherId, SchemaId, Shape};
use rules::template::{TemplateInstruction, TemplateOperand, TemplateProgram};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;

/// Existence is independent of text knowledge: a present editing hole has no known value.
#[derive(Clone, Debug, Default)]
pub struct BindingInputs {
    /// Concrete logical text, including known empty strings.
    pub values: BTreeMap<String, String>,
    /// Supplied keys, including editing holes.
    pub present: BTreeSet<String>,
}
impl BindingInputs {
    /// Concrete bindings have known text and are all present.
    pub fn concrete(values: &BTreeMap<String, String>) -> Self {
        Self {
            values: values.clone(),
            present: values
                .keys()
                .map(|name| name.to_ascii_lowercase())
                .collect(),
        }
    }
}

/// Logical Template binding view. Ordinary scalar HIR retains its original spelling.
pub fn binding_value(syntax: &parser::ParsedFile, scalar: &crate::HirScalar) -> String {
    syntax
        .text(scalar.range)
        .filter(|_| scalar.quoted)
        .and_then(parser::decode_quoted_script)
        .map_or_else(|| scalar.value.clone(), |(value, _)| value)
}

#[derive(Clone, Debug)]
/// Interpretation of one substituted token at its usage site.
pub enum Domain {
    /// Scalar matcher alternatives valid at one site.
    Value(Vec<MatcherId>),
    /// A key in a specific schema and value shape.
    Key { schema: SchemaId, shape: Shape },
    /// Template script consumption as a complete block or a spliced fragment.
    Template { schema: SchemaId, complete: bool },
    /// A recursive or unresolved usage grants no concrete constraint.
    Unresolved,
}

#[derive(Clone, Debug)]
/// One source-ranged parameter usage with its invocation scope.
pub struct ParameterSite {
    /// Template containing this usage; forwarded definitions can share offsets.
    pub origin: (String, String),
    /// Scalar matcher, key or Template consumption.
    pub domain: Domain,
    /// Definition-side token with forwarded substitutions.
    pub token: TemplateToken,
    /// Scope registers at the usage site.
    pub state: ScopeState,
}

#[allow(clippy::too_many_arguments)]
/// Interpreters active branches, leaving the selected parameter unbound.
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
        ir, facts, kind, name, parameter, bindings, state, false, 262_144, None, checkpoint,
    )
}

#[allow(clippy::too_many_arguments)]
/// Same semantic goal with an explicit query-local work budget for audits and interactive policy.
pub fn parameter_sites_with_budget<E>(
    ir: &rules::ir::RulesIr,
    facts: &dyn rules::ir::SymbolFacts,
    kind: &str,
    name: &str,
    parameter: &str,
    bindings: &BTreeMap<String, String>,
    state: ScopeState,
    nodes: usize,
    checkpoint: &mut dyn FnMut() -> Result<(), E>,
) -> Result<Analysis<Vec<ParameterSite>>, E> {
    parameter_sites_with(
        ir, facts, kind, name, parameter, bindings, state, false, nodes, None, checkpoint,
    )
}

#[allow(clippy::too_many_arguments)]
/// Interpreters active branches for syntactic symbol indexing, including preview blocks.
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
        ir, facts, kind, name, parameter, bindings, state, true, 262_144, None, checkpoint,
    )
}

/// Binding-aware query shared by editing and syntactic symbol consumers.
#[allow(clippy::too_many_arguments)]
pub fn parameter_sites_with_inputs<E>(
    ir: &rules::ir::RulesIr,
    facts: &dyn rules::ir::SymbolFacts,
    kind: &str,
    name: &str,
    parameter: &str,
    inputs: &BindingInputs,
    state: ScopeState,
    include_display_only: bool,
    checkpoint: &mut dyn FnMut() -> Result<(), E>,
) -> Result<Analysis<Vec<ParameterSite>>, E> {
    parameter_sites_with(
        ir,
        facts,
        kind,
        name,
        parameter,
        &inputs.values,
        state,
        include_display_only,
        262_144,
        Some(&inputs.present),
        checkpoint,
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
    nodes: usize,
    present: Option<&BTreeSet<String>>,
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
    let mut interpreter = Interpreter {
        ir,
        facts,
        parameter,
        checkpoint,
        coverage: AnalysisCoverage::default(),
        sites: Vec::new(),
        missing: BTreeSet::new(),
        completed: HashSet::new(),
        visiting: HashSet::new(),
        budget: nodes,
        all_branches: false,
        include_display_only,
        origin: (String::new(), String::new()),
    };
    interpreter.call(
        kind,
        name,
        &environment,
        &present.cloned().unwrap_or_else(|| {
            bindings
                .keys()
                .map(|key| key.to_ascii_lowercase())
                .collect()
        }),
        state,
    )?;
    if bindings.values().any(|value| value.contains('$')) {
        interpreter
            .coverage
            .residuals
            .insert(crate::analysis::ResidualReason::TextInterpretation);
    }
    Ok(Analysis {
        value: interpreter.sites,
        coverage: interpreter.coverage,
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
    let mut interpreter = Interpreter {
        ir,
        facts,
        parameter,
        checkpoint,
        coverage: AnalysisCoverage::default(),
        sites: Vec::new(),
        missing: BTreeSet::new(),
        completed: HashSet::new(),
        visiting: HashSet::new(),
        budget: 262_144,
        all_branches: true,
        include_display_only: false,
        origin: (String::new(), String::new()),
    };
    interpreter.call(
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
        value: interpreter.sites,
        coverage: interpreter.coverage,
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
    missing_parameters_with_inputs(
        ir,
        facts,
        kind,
        name,
        &BindingInputs::concrete(bindings),
        state,
        checkpoint,
    )
}

/// Missing text reads in active branches; present holes activate guards but are not missing keys.
#[allow(clippy::too_many_arguments)]
pub fn missing_parameters_with_inputs<E>(
    ir: &rules::ir::RulesIr,
    facts: &dyn rules::ir::SymbolFacts,
    kind: &str,
    name: &str,
    inputs: &BindingInputs,
    state: ScopeState,
    checkpoint: &mut dyn FnMut() -> Result<(), E>,
) -> Result<Analysis<BTreeSet<String>>, E> {
    let bindings = &inputs.values;
    let env = bindings
        .iter()
        .map(|(key, value)| {
            (
                key.to_ascii_lowercase(),
                vec![TemplateFragment::Literal(value.clone())],
            )
        })
        .collect();
    let mut interpreter = Interpreter {
        ir,
        facts,
        parameter: "",
        checkpoint,
        coverage: AnalysisCoverage::default(),
        sites: Vec::new(),
        missing: BTreeSet::new(),
        completed: HashSet::new(),
        visiting: HashSet::new(),
        budget: 262_144,
        all_branches: false,
        include_display_only: false,
        origin: (String::new(), String::new()),
    };
    interpreter.call(kind, name, &env, &inputs.present, state)?;
    Ok(Analysis {
        value: interpreter.missing,
        coverage: interpreter.coverage,
    })
}

type Environment = BTreeMap<String, Vec<TemplateFragment>>;

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
struct StateKey {
    kind: String,
    name: String,
    env: Environment,
    active: BTreeSet<String>,
    state: ScopeState,
}

#[derive(Clone)]
struct MemoSites {
    sites: Vec<ParameterSite>,
    coverage: AnalysisCoverage,
}

fn memo_key(state: &StateKey, parameter: &str, all_branches: bool, display: bool) -> String {
    // Source ranges remain in the key until all cached projections are fully relative.
    format!("{parameter}:{all_branches}:{display}:{state:?}")
}

enum Task {
    Call {
        kind: String,
        name: String,
        env: Arc<Environment>,
        active: Arc<BTreeSet<String>>,
        state: ScopeState,
    },
    Exit(StateKey, usize, Option<String>),
    Walk {
        schema: SchemaId,
        program: Arc<TemplateProgram>,
        block: usize,
        index: usize,
        env: Arc<Environment>,
        active: Arc<BTreeSet<String>>,
        state: ScopeState,
        cases: Option<Vec<MatcherId>>,
        origin: (String, String),
    },
}

struct Interpreter<'a, E> {
    ir: &'a rules::ir::RulesIr,
    facts: &'a dyn rules::ir::SymbolFacts,
    parameter: &'a str,
    checkpoint: &'a mut dyn FnMut() -> Result<(), E>,
    coverage: AnalysisCoverage,
    sites: Vec<ParameterSite>,
    missing: BTreeSet<String>,
    visiting: HashSet<StateKey>,
    completed: HashSet<StateKey>,
    budget: usize,
    all_branches: bool,
    include_display_only: bool,
    origin: (String, String),
}

impl<E> Interpreter<'_, E> {
    fn call(
        &mut self,
        kind: &str,
        name: &str,
        env: &Environment,
        active: &BTreeSet<String>,
        state: ScopeState,
    ) -> Result<(), E> {
        let memo = self.facts.template_memo();
        let mut work = vec![Task::Call {
            kind: kind.to_ascii_lowercase(),
            name: name.to_ascii_lowercase(),
            env: Arc::new(env.clone()),
            active: Arc::new(active.clone()),
            state,
        }];
        while let Some(task) = work.pop() {
            (self.checkpoint)()?;
            if self.budget == 0 {
                self.coverage.limits.insert(AnalysisLimit::Nodes);
                break;
            }
            match task {
                Task::Exit(key, start, cache_key) => {
                    if self.coverage.is_complete()
                        && let (Some(memo), Some(cache_key)) = (&memo, cache_key)
                    {
                        let sites = self.sites[start..].to_vec();
                        let bytes = sites
                            .iter()
                            .map(|site| {
                                256 + site
                                    .token
                                    .fragments
                                    .iter()
                                    .map(|fragment| match fragment {
                                        TemplateFragment::Literal(value) => value.len() + 32,
                                        TemplateFragment::Parameter { name, .. } => name.len() + 32,
                                    })
                                    .sum::<usize>()
                            })
                            .sum();
                        memo.insert(
                            cache_key,
                            Arc::new(MemoSites {
                                sites,
                                coverage: self.coverage.clone(),
                            }),
                            bytes,
                        );
                    }
                    self.visiting.remove(&key);
                    self.completed.insert(key);
                }
                Task::Call {
                    kind,
                    name,
                    env,
                    active,
                    state,
                } => {
                    let key = StateKey {
                        kind: kind.clone(),
                        name: name.clone(),
                        env: (*env).clone(),
                        active: (*active).clone(),
                        state: state.clone(),
                    };
                    if self.completed.contains(&key) {
                        continue;
                    }
                    let cache_key = (memo.is_some() && !self.parameter.is_empty()).then(|| {
                        memo_key(
                            &key,
                            self.parameter,
                            self.all_branches,
                            self.include_display_only,
                        )
                    });
                    if let (Some(memo), Some(cache_key)) = (&memo, &cache_key)
                        && let Some(cached) = memo.get::<MemoSites>(cache_key)
                    {
                        self.coverage.merge(&cached.coverage);
                        self.sites.extend(cached.sites.iter().cloned());
                        self.completed.insert(key);
                        continue;
                    }
                    let site_start = self.sites.len();
                    if !self.visiting.insert(key.clone()) {
                        self.coverage.limits.insert(AnalysisLimit::RecursiveState);
                        continue;
                    }
                    if let Some(type_id) = self.ir.type_by_name(&kind)
                        && let Some(template) = self.facts.template(type_id, &name)
                        && let Some(schema) = template_body(self.ir, type_id)
                    {
                        if self.parameter.is_empty() {
                            self.require_program(&template.program, env.as_ref(), active.as_ref())?;
                        }
                        work.push(Task::Exit(key, site_start, cache_key));
                        work.push(Task::Walk {
                            schema,
                            program: template.program.clone(),
                            block: 0,
                            index: 0,
                            env,
                            active,
                            state,
                            cases: None,
                            origin: (kind, name),
                        });
                    } else {
                        self.coverage
                            .limits
                            .insert(AnalysisLimit::UnavailableTemplate);
                        self.visiting.remove(&key);
                    }
                }
                Task::Walk {
                    schema,
                    program,
                    block,
                    index,
                    env,
                    active,
                    state,
                    cases,
                    origin,
                } => {
                    let Some(instruction) =
                        program.blocks.get(block).and_then(|items| items.get(index))
                    else {
                        continue;
                    };
                    self.budget -= 1;
                    self.origin = origin.clone();
                    work.push(Task::Walk {
                        schema,
                        program: program.clone(),
                        block,
                        index: index + 1,
                        env: env.clone(),
                        active: active.clone(),
                        state: state.clone(),
                        cases: cases.clone(),
                        origin: origin.clone(),
                    });
                    self.instruction(
                        schema,
                        program.clone(),
                        instruction,
                        env.as_ref(),
                        active.as_ref(),
                        state,
                        cases.as_deref(),
                        origin,
                        &mut work,
                    )?;
                }
            }
        }
        Ok(())
    }

    fn require_program(
        &mut self,
        program: &TemplateProgram,
        env: &Environment,
        active: &BTreeSet<String>,
    ) -> Result<(), E> {
        let mut pending = vec![0];
        while let Some(block) = pending.pop() {
            for instruction in program.blocks[block].iter() {
                (self.checkpoint)()?;
                if self.budget == 0 {
                    self.coverage.limits.insert(AnalysisLimit::Nodes);
                    return Ok(());
                }
                self.budget -= 1;
                match instruction {
                    TemplateInstruction::Recover(_) => {
                        self.coverage
                            .limits
                            .insert(AnalysisLimit::StructuralRecovery);
                    }
                    TemplateInstruction::Consume(token) => self.required_token(token, env, active),
                    TemplateInstruction::When {
                        name,
                        negated,
                        block,
                    } => {
                        if self.all_branches
                            || active.contains(&name.to_ascii_lowercase()) != *negated
                        {
                            pending.push(*block);
                        }
                    }
                    TemplateInstruction::Dispatch(property) => {
                        self.required_token(&property.key, env, active);
                        match &property.value {
                            TemplateOperand::Scalar(token) => {
                                self.required_token(token, env, active)
                            }
                            TemplateOperand::Block(block) => pending.push(*block),
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn required_token(
        &mut self,
        token: &TemplateToken,
        env: &Environment,
        active: &BTreeSet<String>,
    ) {
        for part in &token.fragments {
            let TemplateFragment::Parameter { name, range } = part else {
                continue;
            };
            if active.contains(&name.to_ascii_lowercase()) {
                continue;
            }
            let substituted = substitute(
                &TemplateToken {
                    range: *range,
                    quoted: false,
                    fragments: vec![part.clone()],
                },
                env,
            );
            self.missing.extend(
                substituted
                    .fragments
                    .into_iter()
                    .filter_map(|part| match part {
                        TemplateFragment::Parameter { name, .. } => Some(name.to_string()),
                        _ => None,
                    }),
            );
        }
    }
    fn add(&mut self, domain: Domain, token: TemplateToken, state: &ScopeState) {
        if token.fragments.iter().any(|part|matches!(part,TemplateFragment::Parameter{name,..} if name.eq_ignore_ascii_case(self.parameter))) {
            self.sites.push(ParameterSite{origin:self.origin.clone(),domain,token,state:state.clone()});
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn instruction(
        &mut self,
        schema: SchemaId,
        program: Arc<TemplateProgram>,
        instruction: &TemplateInstruction,
        env: &Environment,
        active: &BTreeSet<String>,
        state: ScopeState,
        switch_cases: Option<&[MatcherId]>,
        origin: (String, String),
        work: &mut Vec<Task>,
    ) -> Result<(), E> {
        let ir = self.ir;
        match instruction {
            TemplateInstruction::Recover(_) => {
                self.coverage
                    .limits
                    .insert(AnalysisLimit::StructuralRecovery);
            }
            TemplateInstruction::When {
                name,
                negated,
                block,
            } => {
                if self.all_branches || active.contains(&name.to_ascii_lowercase()) != *negated {
                    work.push(Task::Walk {
                        schema,
                        program: program.clone(),
                        block: *block,
                        index: 0,
                        env: Arc::new(env.clone()),
                        active: Arc::new(active.clone()),
                        state: state.clone(),
                        cases: switch_cases.map(|cases| cases.to_vec()),
                        origin: origin.clone(),
                    });
                }
            }
            TemplateInstruction::Consume(token) => {
                let domain = Domain::Template {
                    schema,
                    complete: false,
                };
                self.add(domain, substitute(token, env), &state);
            }
            TemplateInstruction::Dispatch(property) => {
                let token = substitute(&property.key, env);
                let shape = match &property.value {
                    TemplateOperand::Block(_) => Shape::Block,
                    _ => Shape::Scalar,
                };
                let Some(key) = literal(&token) else {
                    self.coverage.residuals.insert(ResidualReason::DynamicKey);
                    let domain = if shape == Shape::Block
                        && let Some(matchers) = switch_cases
                    {
                        Domain::Value(matchers.to_vec())
                    } else {
                        Domain::Key { schema, shape }
                    };
                    self.add(domain, token, &state);
                    return Ok(());
                };
                let mut fields =
                    crate::checking::field_candidates(ir, schema, &key, shape, self.facts);
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
                    template_kind(ir, ir.field(*id).key).filter(|kind| {
                        ir.type_by_name(kind)
                            .is_some_and(|type_id| self.facts.template(type_id, &key).is_some())
                    })
                });
                if let Some(kind) = callee {
                    let mut child_env = Environment::new();
                    let mut child_active = BTreeSet::new();
                    if let TemplateOperand::Block(block) = &property.value {
                        forward_program(
                            &program,
                            *block,
                            env,
                            active,
                            &mut child_env,
                            &mut child_active,
                        );
                    }
                    work.push(Task::Call {
                        kind,
                        name: key.clone(),
                        env: Arc::new(child_env),
                        active: Arc::new(child_active),
                        state: state.clone(),
                    });
                    return Ok(());
                }
                match &property.value {
                    TemplateOperand::Scalar(value) => {
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
                    }
                    TemplateOperand::Block(block) => {
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
                                    let selector =
                                        active_program_scalar(&program, *block, on, env, active)?;
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
                                work.push(Task::Walk {
                                    schema: child,
                                    program: program.clone(),
                                    block: *block,
                                    index: 0,
                                    env: Arc::new(env.clone()),
                                    active: Arc::new(active.clone()),
                                    state: next,
                                    cases,
                                    origin: origin.clone(),
                                });
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

fn active_program_scalar(
    program: &TemplateProgram,
    block: usize,
    key: &str,
    env: &Environment,
    active: &BTreeSet<String>,
) -> Option<String> {
    let mut pending = vec![(block, program.blocks[block].len())];
    while let Some((block, index)) = pending.pop() {
        if index == 0 {
            continue;
        }
        pending.push((block, index - 1));
        match &program.blocks[block][index - 1] {
            TemplateInstruction::Dispatch(property)
                if literal(&substitute(&property.key, env))
                    .is_some_and(|name| name.eq_ignore_ascii_case(key)) =>
            {
                if let TemplateOperand::Scalar(value) = &property.value {
                    return literal(&substitute(value, env));
                }
            }
            TemplateInstruction::When {
                name,
                negated,
                block,
            } if active.contains(&name.to_ascii_lowercase()) != *negated => {
                pending.push((*block, program.blocks[*block].len()))
            }
            _ => {}
        }
    }
    None
}

/// Type namespace of a matcher implementing the declared Template trait.
pub fn template_kind(ir: &rules::ir::RulesIr, matcher: MatcherId) -> Option<String> {
    match ir.matcher(matcher) {
        Matcher::Ref(rules::ir::RefTarget::Type { type_id, .. }) => {
            let template = ir.trait_by_name("Template")?;
            let info = ir.type_info(*type_id);
            info.trait_impls
                .iter()
                .any(|implementation| implementation.trait_id == template)
                .then(|| ir.strings().resolve(info.name).to_owned())
        }
        Matcher::Union(items) => items.iter().find_map(|id| template_kind(ir, *id)),
        _ => None,
    }
}

fn forward_program(
    program: &TemplateProgram,
    block: usize,
    env: &Environment,
    active: &BTreeSet<String>,
    out: &mut Environment,
    child_active: &mut BTreeSet<String>,
) {
    let mut pending = vec![(block, 0)];
    while let Some((block, index)) = pending.pop() {
        let Some(instruction) = program.blocks[block].get(index) else {
            continue;
        };
        pending.push((block, index + 1));
        match instruction {
            TemplateInstruction::When {
                name,
                negated,
                block,
            } if active.contains(&name.to_ascii_lowercase()) != *negated => {
                pending.push((*block, 0))
            }
            TemplateInstruction::Dispatch(property) => {
                if let Some(key) = literal(&substitute(&property.key, env))
                    && let TemplateOperand::Scalar(value) = &property.value
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

/// Body schema declared by the Template implementation of a type.
pub fn template_body(ir: &rules::ir::RulesIr, type_id: rules::ir::TypeId) -> Option<SchemaId> {
    ir.type_info(type_id)
        .trait_impls
        .iter()
        .filter(|implementation| Some(implementation.trait_id) == ir.trait_by_name("Template"))
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
    crate::checking::scalar_matches(ir, matcher, value, facts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rules::template::{Template, TemplateItem, TemplateProperty, TemplateValue};

    struct Definitions(BTreeMap<String, Arc<Template>>, rules::ir::TypeId);
    impl rules::ir::SymbolFacts for Definitions {
        fn type_member(&self, ty: rules::ir::TypeId, name: &str) -> bool {
            ty == self.1 && self.0.contains_key(&name.to_ascii_lowercase())
        }
        fn template(&self, _: rules::ir::TypeId, name: &str) -> Option<Arc<Template>> {
            self.0.get(&name.to_ascii_lowercase()).cloned()
        }
    }
    fn token(text: &str) -> TemplateToken {
        TemplateToken {
            range: text::TextRange::empty(0),
            quoted: false,
            fragments: vec![TemplateFragment::Literal(text.into())],
        }
    }
    fn variable() -> TemplateToken {
        TemplateToken {
            range: text::TextRange::empty(0),
            quoted: false,
            fragments: vec![TemplateFragment::Parameter {
                name: Arc::from("N"),
                range: text::TextRange::empty(0),
            }],
        }
    }
    fn definition(name: &str, key: &str, call: bool) -> Arc<Template> {
        let value = if call {
            TemplateValue::Block {
                range: text::TextRange::empty(0),
                items: vec![TemplateItem::Property(TemplateProperty {
                    key: token("N"),
                    range: text::TextRange::empty(0),
                    operator: Some(Arc::from("=")),
                    value: TemplateValue::Scalar(variable()),
                })],
            }
        } else {
            TemplateValue::Scalar(variable())
        };
        let items = vec![TemplateItem::Property(TemplateProperty {
            key: token(key),
            range: text::TextRange::empty(0),
            operator: Some(Arc::from("=")),
            value,
        })];
        Arc::new(Template {
            source: Arc::from(""),
            kind: Arc::from("scripted_effect"),
            name: name.into(),
            definition_range: text::TextRange::empty(0),
            body_range: text::TextRange::empty(0),
            program: Arc::new(TemplateProgram::compile(&items)),
            items: Arc::from(items),
        })
    }

    #[test]
    fn deep_calls_reach_the_terminal_constraint() {
        let ir = game::eu4::first_party_ir().unwrap();
        let mut definitions = BTreeMap::new();
        // Keep a deep representative chain without a 10,000-call stress fixture.
        const DEPTH: usize = 1024;
        for depth in 0..DEPTH {
            definitions.insert(
                format!("chain{depth}"),
                definition(
                    &format!("chain{depth}"),
                    &format!("chain{}", depth + 1),
                    true,
                ),
            );
        }
        definitions.insert(
            format!("chain{DEPTH}"),
            definition(&format!("chain{DEPTH}"), "add_prestige", false),
        );
        let facts = Definitions(definitions, ir.type_by_name("scripted_effect").unwrap());
        let sites = parameter_sites::<std::convert::Infallible>(
            &ir,
            &facts,
            "scripted_effect",
            "chain0",
            "N",
            &BTreeMap::new(),
            ScopeState::initial(crate::ScopeValue::known_single("country")),
            &mut || Ok(()),
        )
        .unwrap();
        assert!(sites.coverage.is_complete(), "{:?}", sites.coverage);
        assert_eq!(sites.len(), 1);
        let scopes = crate::template_scope::entry_scopes::<std::convert::Infallible>(
            &ir,
            &facts,
            "scripted_effect",
            "chain0",
            &mut || Ok(()),
        )
        .unwrap();
        assert!(scopes.coverage.is_complete(), "{:?}", scopes.coverage);
        assert_eq!(scopes.value.possible, vec!["country"]);
        let Domain::Value(matchers) = &sites[0].domain else {
            panic!("terminal scalar check missing");
        };
        assert!(
            !matchers
                .iter()
                .any(|id| ir.scalar_matches(*id, "wrong", &facts))
        );
    }
}
