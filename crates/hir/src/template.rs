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
    /// Bound physical siblings, including query dependencies inside Pattern holes.
    pub query_context: crate::query::OwnedQueryContext,
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

/// Shared definition-side and active-call requiredness, including original spelling.
#[derive(Clone, Debug, Default)]
pub struct RequiredParameters {
    pub unconditional: BTreeSet<String>,
    pub missing: BTreeSet<String>,
}

/// The authoritative required-argument inference for both direct invocations and
/// empty-argument query eligibility. Nested eligibility requests defer rather
/// than starting a recursive query/call interpreter.
#[allow(clippy::too_many_arguments)]
pub fn required_parameters_with_inputs<E>(
    ir: &rules::ir::RulesIr,
    facts: &dyn rules::ir::SymbolFacts,
    kind: &str,
    name: &str,
    inputs: &BindingInputs,
    state: ScopeState,
    checkpoint: &mut dyn FnMut() -> Result<(), E>,
) -> Result<Analysis<RequiredParameters>, E> {
    let suppressed = RequirednessFacts {
        facts,
        dependent: std::cell::Cell::new(false),
    };
    let active =
        missing_parameters_with_inputs(ir, &suppressed, kind, name, inputs, state, checkpoint)?;
    let mut coverage = active.coverage;
    if !facts.facts_complete() {
        coverage.limits.insert(AnalysisLimit::FactStability);
    }
    if suppressed.dependent.get() {
        coverage.limits.insert(AnalysisLimit::DependentQuery);
    }
    let mut unconditional = BTreeSet::new();
    if let Some(template) = ir
        .type_by_name(kind)
        .and_then(|ty| facts.template(ty, name))
    {
        let mut remaining = 262_144usize;
        let mut error = None;
        let reads = template
            .program
            .unconditional_reads_with_checkpoint(&mut || {
                if let Err(failure) = checkpoint() {
                    error = Some(failure);
                    return true;
                }
                if remaining == 0 {
                    return true;
                }
                remaining -= 1;
                false
            });
        if let Some(error) = error {
            return Err(error);
        }
        if let Some(reads) = reads {
            for name in template.program.parameters.iter() {
                checkpoint()?;
                if remaining == 0 {
                    coverage.limits.insert(AnalysisLimit::Nodes);
                    break;
                }
                remaining -= 1;
                if reads.contains(&name.to_ascii_lowercase())
                    && !inputs
                        .present
                        .iter()
                        .any(|supplied| supplied.eq_ignore_ascii_case(name))
                {
                    unconditional.insert(name.to_string());
                }
            }
        } else {
            coverage.limits.insert(AnalysisLimit::Nodes);
        }
    } else {
        coverage.limits.insert(AnalysisLimit::UnavailableTemplate);
    }
    let mut names = BTreeMap::new();
    for name in unconditional.iter().chain(active.value.iter()) {
        if !inputs
            .present
            .iter()
            .any(|supplied| supplied.eq_ignore_ascii_case(name))
        {
            names
                .entry(name.to_ascii_lowercase())
                .or_insert_with(|| name.clone());
        }
    }
    Ok(Analysis {
        value: RequiredParameters {
            unconditional,
            missing: names.into_values().collect(),
        },
        coverage,
    })
}

struct RequirednessFacts<'a> {
    facts: &'a dyn rules::ir::SymbolFacts,
    dependent: std::cell::Cell<bool>,
}
impl rules::ir::SymbolFacts for RequirednessFacts<'_> {
    fn asset_member(&self, category: &str, name: &str) -> Option<bool> {
        self.facts.asset_member(category, name)
    }
    fn facts_complete(&self) -> bool {
        self.facts.facts_complete()
    }
    fn type_member(&self, ty: rules::ir::TypeId, name: &str) -> bool {
        self.facts.type_member(ty, name)
    }
    fn type_subtype_member(
        &self,
        ty: rules::ir::TypeId,
        subtype: rules::ir::Symbol,
        name: &str,
    ) -> bool {
        self.facts.type_subtype_member(ty, subtype, name)
    }
    fn template(
        &self,
        ty: rules::ir::TypeId,
        name: &str,
    ) -> Option<Arc<rules::template::Template>> {
        self.facts.template(ty, name)
    }
    fn template_accepts_no_arguments(
        &self,
        _: rules::ir::TypeId,
        _: &str,
        _: &mut dyn FnMut() -> bool,
    ) -> Option<bool> {
        self.dependent.set(true);
        None
    }
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
        cases: Option<Domain>,
        query_context: Arc<crate::query::OwnedQueryContext>,
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
                                256 + site.query_context.estimated_bytes()
                                    + site
                                        .token
                                        .fragments
                                        .iter()
                                        .map(|fragment| match fragment {
                                            TemplateFragment::Literal(value) => value.len() + 32,
                                            TemplateFragment::Parameter { name, .. } => {
                                                name.len() + 32
                                            }
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
                            if self.budget == 0 {
                                self.coverage.limits.insert(AnalysisLimit::Nodes);
                                continue;
                            }
                        }
                        work.push(Task::Exit(key, site_start, cache_key));
                        let query_context = Arc::new(active_program_context(
                            &template.program,
                            0,
                            &env,
                            &active,
                            self.all_branches,
                            self.parameter,
                        ));
                        work.push(Task::Walk {
                            schema,
                            program: template.program.clone(),
                            block: 0,
                            index: 0,
                            env,
                            active,
                            state,
                            cases: None,
                            query_context,
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
                    query_context,
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
                        query_context: query_context.clone(),
                        origin: origin.clone(),
                    });
                    self.instruction(
                        schema,
                        program.clone(),
                        instruction,
                        env.as_ref(),
                        active.as_ref(),
                        state,
                        cases.as_ref(),
                        query_context,
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
                    TemplateInstruction::Consume(token) => {
                        self.required_token(token, env, active)?
                    }
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
                        self.required_token(&property.key, env, active)?;
                        match &property.value {
                            TemplateOperand::Scalar(token) => {
                                self.required_token(token, env, active)?
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
    ) -> Result<(), E> {
        for part in &token.fragments {
            (self.checkpoint)()?;
            if self.budget == 0 {
                self.coverage.limits.insert(AnalysisLimit::Nodes);
                return Ok(());
            }
            self.budget -= 1;
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
        Ok(())
    }
    fn add(
        &mut self,
        domain: Domain,
        token: TemplateToken,
        state: &ScopeState,
        query_context: &crate::query::OwnedQueryContext,
    ) {
        if matches!(domain, Domain::Unresolved) {
            self.coverage.residuals.insert(ResidualReason::Binding);
        }
        if token.fragments.iter().any(|part|matches!(part,TemplateFragment::Parameter{name,..} if name.eq_ignore_ascii_case(self.parameter))) {
            self.sites.push(ParameterSite{origin:self.origin.clone(),domain,token,state:state.clone(),query_context:query_context.clone()});
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
        switch_cases: Option<&Domain>,
        query_context: Arc<crate::query::OwnedQueryContext>,
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
                        cases: switch_cases.cloned(),
                        query_context: query_context.clone(),
                        origin: origin.clone(),
                    });
                }
            }
            TemplateInstruction::Consume(token) => {
                let domain = Domain::Template {
                    schema,
                    complete: false,
                };
                self.add(
                    domain,
                    substitute(token, env),
                    &state,
                    query_context.as_ref(),
                );
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
                        matchers.clone()
                    } else {
                        let keys = ir
                            .fields(schema)
                            .into_iter()
                            .filter(|field| ir.shape(*field) == Some(shape))
                            .map(|field| ir.field(field).key)
                            .collect::<Vec<_>>();
                        if keys.iter().any(|matcher| matcher_has_query(ir, *matcher)) {
                            query_value_domain(ir, &keys, self.facts, query_context.as_ref())
                        } else {
                            Domain::Key { schema, shape }
                        }
                    };
                    self.add(domain, token, &state, query_context.as_ref());
                    return Ok(());
                };
                let mut fields = crate::checking::field_candidates_with_context(
                    ir,
                    schema,
                    &key,
                    shape,
                    self.facts,
                    query_context.as_ref(),
                );
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
                            let domain = query_value_domain(
                                ir,
                                &matchers,
                                self.facts,
                                query_context.as_ref(),
                            );
                            self.add(domain, token.clone(), &state, query_context.as_ref());
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
                                let child_context = Arc::new(active_program_context(
                                    &program,
                                    *block,
                                    env,
                                    active,
                                    self.all_branches,
                                    self.parameter,
                                ));
                                let cases = ir.field(id).control.as_ref().and_then(|control| {
                                    let selector_schema = control.selector_schema?;
                                    let on = ir.strings().resolve(control.on?);
                                    let mut query = rules::ir::FieldQuery::new(
                                        selector_schema,
                                        rules::ir::QueryProjection::Values,
                                    );
                                    query.shape = Some(Shape::Scalar);
                                    query.call_args_none = true;
                                    let result = match rules::query::QueryContext::sibling(
                                        child_context.as_ref(),
                                        on,
                                    ) {
                                        rules::query::SiblingValue::Scalar(selector) => ir
                                            .query_selected_fields(
                                                &query,
                                                selector,
                                                &Facts(self.facts),
                                                child_context.as_ref(),
                                            ),
                                        rules::query::SiblingValue::Deferred => {
                                            return Some(Domain::Unresolved);
                                        }
                                        _ => return Some(Domain::Value(Vec::new())),
                                    };
                                    if result.state == rules::query::QueryState::Deferred {
                                        return Some(Domain::Unresolved);
                                    }
                                    Some(Domain::Value(
                                        result
                                            .fields
                                            .into_iter()
                                            .filter_map(|field| {
                                                ir.query_projected_matcher(&query, field)
                                            })
                                            .collect(),
                                    ))
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
                                    query_context: child_context,
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

struct Facts<'a>(&'a dyn rules::ir::SymbolFacts);
impl rules::ir::SymbolFacts for Facts<'_> {
    fn template_accepts_no_arguments(
        &self,
        ty: rules::ir::TypeId,
        name: &str,
        checkpoint: &mut dyn FnMut() -> bool,
    ) -> Option<bool> {
        self.0.template_accepts_no_arguments(ty, name, checkpoint)
    }
    fn facts_complete(&self) -> bool {
        self.0.facts_complete()
    }
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

fn active_program_context(
    program: &TemplateProgram,
    block: usize,
    env: &Environment,
    active: &BTreeSet<String>,
    all_branches: bool,
    parameter: &str,
) -> crate::query::OwnedQueryContext {
    let mut context = crate::query::OwnedQueryContext::default();
    let mut pending = vec![block];
    while let Some(block) = pending.pop() {
        for instruction in program.blocks[block].iter() {
            match instruction {
                TemplateInstruction::Dispatch(property) => {
                    let key_token = substitute(&property.key, env);
                    let Some(key) = literal(&key_token) else {
                        if !key_token.fragments.iter().any(|fragment| matches!(fragment, TemplateFragment::Parameter {name,..} if name.eq_ignore_ascii_case(parameter))) {
                            context.unknown_key = true;
                        }
                        continue;
                    };
                    let value = match &property.value {
                        TemplateOperand::Scalar(value) => literal(&substitute(value, env)).map_or(
                            crate::query::OwnedSibling::Deferred,
                            crate::query::OwnedSibling::Scalar,
                        ),
                        TemplateOperand::Block(_) => crate::query::OwnedSibling::Invalid,
                    };
                    context.insert(key, value);
                }
                TemplateInstruction::When { .. } if all_branches => context.unknown_key = true,
                TemplateInstruction::When {
                    name,
                    negated,
                    block,
                } if active.contains(&name.to_ascii_lowercase()) != *negated => {
                    pending.push(*block)
                }
                TemplateInstruction::Consume(_) | TemplateInstruction::Recover(_) => {
                    context.unknown_key = true
                }
                _ => {}
            }
        }
    }
    context
}

fn matcher_has_query(ir: &rules::ir::RulesIr, matcher: MatcherId) -> bool {
    match ir.matcher(matcher) {
        Matcher::Query(_) => true,
        Matcher::Union(items) => items.iter().any(|item| matcher_has_query(ir, *item)),
        Matcher::Pattern(parts) => parts.iter().any(|part| matches!(part, rules::ir::PatternPart::Hole(matcher) if matcher_has_query(ir, *matcher))),
        _ => false,
    }
}

fn query_value_domain(
    ir: &rules::ir::RulesIr,
    matchers: &[MatcherId],
    facts: &dyn rules::ir::SymbolFacts,
    context: &impl rules::query::QueryContext,
) -> Domain {
    let mut pending = matchers.to_vec();
    let mut result = Vec::new();
    let mut visited = BTreeSet::new();
    while let Some(matcher) = pending.pop() {
        if !visited.insert(matcher) {
            continue;
        }
        match ir.matcher(matcher) {
            Matcher::Query(query)
                if query.projection == rules::ir::QueryProjection::Values
                    || query.selector.is_some() =>
            {
                let selection = ir.query_fields(query, &Facts(facts), context);
                if selection.state == rules::query::QueryState::Deferred {
                    return Domain::Unresolved;
                }
                if selection.state == rules::query::QueryState::Resolved {
                    // Retain the original query and source-field provenance. The site
                    // carries its physical sibling context for all later projections.
                    result.push(matcher);
                }
            }
            Matcher::Union(items) => pending.extend(items.iter().rev().copied()),
            _ => result.push(matcher),
        }
    }
    result.sort_unstable();
    result.dedup();
    Domain::Value(result)
}

/// Returns the Template type referenced by a matcher.
pub fn template_kind(ir: &rules::ir::RulesIr, matcher: MatcherId) -> Option<String> {
    rules::template::template_key_type(ir, matcher, &mut || false)
        .map(|type_id| ir.strings().resolve(ir.type_info(type_id).name).to_owned())
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

    fn query_fixture(key_parameter: bool) -> (rules::ir::RulesIr, Definitions, Arc<Template>) {
        let ir = rules::lower::lower(&[("query.json".into(), serde_json::from_str(r#"{
          "files":{"test":{"path":"test","ext":"txt","root":"select"}},
          "traits":{"Template":{}},
          "types":{"scripted_effect":{"impl":{"Template":{"body":"select"}}}},
          "schemas":{
            "select":{"fields":{
              "on_trigger":{"value":"keysof<triggers,shape=scalar>","card":"0..1"},
              "value":{"value":"valuesof<triggers,key=sibling<on_trigger>,shape=scalar>","card":"0..*"}
            },"patterns":[{"key":"valuesof<triggers,key=sibling<on_trigger>,shape=scalar>","body":"empty","card":"0..*"}]},
            "triggers":{"fields":{"flag":{"value":"bool","card":"0..*"},"count":{"value":"int","card":"0..*"}}},
            "empty":{}
          }
        }"#).unwrap())], Default::default()).unwrap();
        let mut selector = variable();
        selector.fragments = vec![TemplateFragment::Parameter {
            name: Arc::from("SELECT"),
            range: text::TextRange::empty(0),
        }];
        let property = |key, value| {
            TemplateItem::Property(TemplateProperty {
                key,
                value,
                operator: Some(Arc::from("=")),
                range: text::TextRange::empty(0),
            })
        };
        let items = vec![
            property(token("on_trigger"), TemplateValue::Scalar(selector)),
            if key_parameter {
                property(
                    variable(),
                    TemplateValue::Block {
                        range: text::TextRange::empty(0),
                        items: Vec::new(),
                    },
                )
            } else {
                property(token("value"), TemplateValue::Scalar(variable()))
            },
        ];
        let template = Arc::new(Template {
            source: Arc::from(if key_parameter {
                "{ on_trigger = $SELECT$ $N$ = {} }"
            } else {
                "{ on_trigger = $SELECT$ value = $N$ }"
            }),
            kind: Arc::from("scripted_effect"),
            name: "query".into(),
            definition_range: text::TextRange::empty(0),
            body_range: text::TextRange::empty(0),
            program: Arc::new(TemplateProgram::compile(&items)),
            items: items.into(),
        });
        let facts = Definitions(
            BTreeMap::from([("query".into(), template.clone())]),
            ir.type_by_name("scripted_effect").unwrap(),
        );
        (ir, facts, template)
    }

    #[test]
    fn query_parameter_domains_follow_bound_siblings_for_values_and_keys() {
        for key_parameter in [false, true] {
            let (ir, facts, _) = query_fixture(key_parameter);
            for (selector, valid, invalid) in [("flag", "yes", "5"), ("count", "5", "yes")] {
                let sites = parameter_sites::<std::convert::Infallible>(
                    &ir,
                    &facts,
                    "scripted_effect",
                    "query",
                    "N",
                    &BTreeMap::from([("SELECT".into(), selector.into())]),
                    ScopeState::initial(crate::ScopeValue::Unknown),
                    &mut || Ok(()),
                )
                .unwrap();
                assert_eq!(sites.value.len(), 1);
                let Domain::Value(matchers) = &sites.value[0].domain else {
                    panic!("missing selected query domain: {:?}", sites.value[0].domain);
                };
                assert!(
                    matchers
                        .iter()
                        .any(|matcher| ir.scalar_outcome_with_context(
                            *matcher,
                            valid,
                            &facts,
                            &sites.value[0].query_context
                        ) == Some(true))
                );
                assert!(
                    !matchers
                        .iter()
                        .any(|matcher| ir.scalar_outcome_with_context(
                            *matcher,
                            invalid,
                            &facts,
                            &sites.value[0].query_context
                        ) == Some(true))
                );
            }
            let sites = parameter_sites::<std::convert::Infallible>(
                &ir,
                &facts,
                "scripted_effect",
                "query",
                "N",
                &BTreeMap::new(),
                ScopeState::initial(crate::ScopeValue::Unknown),
                &mut || Ok(()),
            )
            .unwrap();
            assert!(matches!(sites.value[0].domain, Domain::Unresolved));
        }
    }

    #[test]
    fn query_template_instantiation_revalidates_after_selector_changes() {
        let (ir, facts, template) = query_fixture(false);
        for (selector, invalid) in [("flag", false), ("count", true)] {
            let bindings = BTreeMap::from([
                ("select".into(), selector.into()),
                ("n".into(), "yes".into()),
            ]);
            let instance = crate::template_instance::instantiate::<std::convert::Infallible>(
                &ir,
                &facts,
                template.clone(),
                &bindings,
                &bindings,
                &bindings
                    .keys()
                    .map(|key| key.to_ascii_lowercase())
                    .collect(),
                ir.schema_by_name("select").unwrap(),
                ScopeState::initial(crate::ScopeValue::Unknown),
                &[],
                crate::template_instance::InstanceGoal::Validation,
                &mut || Ok(()),
            )
            .unwrap();
            assert_eq!(
                instance
                    .evidence
                    .iter()
                    .any(|issue| issue.kind == crate::checking::IssueKind::Value),
                invalid,
                "{}: {:?}",
                instance.rendered.text,
                instance.evidence
            );
        }
    }

    fn requiredness_fixture() -> (rules::ir::RulesIr, Definitions) {
        let ir = rules::lower::lower(&[("calls.json".into(), serde_json::from_str(r#"{
          "traits":{"Template":{}},
          "types":{"scripted_effect":{"impl":{"Template":{"body":"commands"}}}},
          "files":{"test":{"path":"test","root":"commands"}},
          "schemas":{
            "commands":{"fields":{"integer":{"value":"int","card":"0..*"},"select":{"body":"selection","card":"0..*"}},"patterns":[
              {"key":"ref<scripted_effect>","value":"bool","card":"0..*"},
              {"key":"ref<scripted_effect>","body":"arguments","card":"0..*"}
            ]},
            "selection":{"patterns":[{"key":"keysof<commands,shape=scalar,call_args=none>","value":"bool","card":"0..*"}]},
            "arguments":{"open":true}
          }
        }"#).unwrap())], Default::default()).unwrap();
        let required = definition("required", "integer", false);
        let make = |name: &str, items: Vec<TemplateItem>| {
            Arc::new(Template {
                source: Arc::from(""),
                kind: Arc::from("scripted_effect"),
                name: name.into(),
                definition_range: text::TextRange::empty(0),
                body_range: text::TextRange::empty(0),
                program: Arc::new(TemplateProgram::compile(&items)),
                items: items.into(),
            })
        };
        let property = |key: &str, value| {
            TemplateItem::Property(TemplateProperty {
                key: token(key),
                range: text::TextRange::empty(0),
                operator: Some("=".into()),
                value,
            })
        };
        let when = |negated: bool, items| {
            TemplateItem::Conditional(rules::template::TemplateConditional {
                name: Arc::from("N"),
                negated,
                range: text::TextRange::empty(0),
                items,
            })
        };
        let optional = make("optional", vec![when(false, required.items.to_vec())]);
        let defaulted = make(
            "defaulted",
            vec![
                when(false, required.items.to_vec()),
                when(
                    true,
                    vec![property("integer", TemplateValue::Scalar(token("5")))],
                ),
            ],
        );
        let active = make("active", vec![when(true, required.items.to_vec())]);
        let forwarded = make(
            "forwarded",
            vec![property("required", TemplateValue::Scalar(token("yes")))],
        );
        let recursive = make(
            "recursive",
            vec![property("recursive", TemplateValue::Scalar(token("yes")))],
        );
        let query = make(
            "query",
            vec![property(
                "select",
                TemplateValue::Block {
                    range: text::TextRange::empty(0),
                    items: vec![property("optional", TemplateValue::Scalar(token("yes")))],
                },
            )],
        );
        let definitions = [
            required, optional, defaulted, active, forwarded, recursive, query,
        ]
        .into_iter()
        .map(|template| (template.name.clone(), template))
        .collect();
        let ty = ir.type_by_name("scripted_effect").unwrap();
        (ir, Definitions(definitions, ty))
    }

    #[test]
    fn required_argument_authority_covers_optional_defaulted_active_and_forwarded_reads() {
        let (ir, facts) = requiredness_fixture();
        for (name, missing, unconditional) in [
            ("required", true, true),
            ("optional", false, false),
            ("defaulted", false, false),
            ("active", true, false),
            ("forwarded", true, false),
        ] {
            let result = required_parameters_with_inputs::<std::convert::Infallible>(
                &ir,
                &facts,
                "scripted_effect",
                name,
                &BindingInputs::default(),
                ScopeState::initial(crate::ScopeValue::Unknown),
                &mut || Ok(()),
            )
            .unwrap();
            assert!(result.coverage.is_known(), "{name}: {:?}", result.coverage);
            assert_eq!(result.missing.contains("N"), missing, "{name}");
            assert_eq!(result.unconditional.contains("N"), unconditional, "{name}");
        }
        // A present editing hole activates its guard without inventing a missing argument.
        let result = required_parameters_with_inputs::<std::convert::Infallible>(
            &ir,
            &facts,
            "scripted_effect",
            "optional",
            &BindingInputs {
                present: BTreeSet::from(["n".into()]),
                ..Default::default()
            },
            ScopeState::initial(crate::ScopeValue::Unknown),
            &mut || Ok(()),
        )
        .unwrap();
        assert!(result.missing.is_empty());
    }

    #[test]
    fn required_argument_authority_defers_reentrant_queries_and_recursive_definitions() {
        let (ir, facts) = requiredness_fixture();
        for (name, limit) in [
            ("recursive", AnalysisLimit::RecursiveState),
            ("query", AnalysisLimit::DependentQuery),
        ] {
            let result = required_parameters_with_inputs::<std::convert::Infallible>(
                &ir,
                &facts,
                "scripted_effect",
                name,
                &BindingInputs::default(),
                ScopeState::initial(crate::ScopeValue::Unknown),
                &mut || Ok(()),
            )
            .unwrap();
            assert!(result.missing.is_empty());
            assert!(
                result.coverage.limits.contains(&limit),
                "{name}: {:?}",
                result.coverage
            );
        }
        let result = required_parameters_with_inputs(
            &ir,
            &facts,
            "scripted_effect",
            "required",
            &BindingInputs::default(),
            ScopeState::initial(crate::ScopeValue::Unknown),
            &mut || Err("cancelled"),
        );
        assert_eq!(result.unwrap_err(), "cancelled");
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
