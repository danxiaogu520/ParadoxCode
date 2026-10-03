//! Entry-scope projections over the shared Template arena, with explicit Any/All continuations.
use crate::analysis::{Analysis, AnalysisCoverage, AnalysisLimit, ResidualReason, Validation};
use crate::{ScopeState, ScopeValue};
use rules::ir::{Matcher, RulesIr, SchemaId, Shape, SymbolFacts};
use rules::replacement::{
    TemplateFragment, TemplateInstruction, TemplateOperand, TemplateProgram, TemplateToken,
};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::Arc,
};

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
struct StateKey {
    kind: String,
    name: String,
    values: BTreeMap<String, String>,
    present: Option<BTreeSet<String>>,
    state: ScopeState,
}
#[derive(Clone)]
struct ScopeMemo {
    value: Validation,
    coverage: AnalysisCoverage,
    conditions: BTreeSet<String>,
    dynamic: bool,
}
fn memo_key(key: &StateKey) -> String {
    format!("template:scope:{key:?}")
}
enum Constraint {
    All(Vec<usize>),
    Any(Vec<usize>),
    Value(Validation),
}
enum Task {
    Call {
        key: StateKey,
        target: usize,
    },
    Exit(StateKey),
    Block {
        program: Arc<TemplateProgram>,
        block: usize,
        schema: SchemaId,
        key: StateKey,
        target: usize,
    },
}

/// Scope-only result: value validity belongs to its own goal, while unknown presence is retained.
#[derive(Clone, Debug, Default)]
pub struct EntryScopeSummary {
    /// Scopes not independently rejected by unconditional statements.
    pub possible: Vec<String>,
    /// Guards whose presence is unknown in the definition-side projection.
    pub conditions: BTreeSet<String>,
    /// Whether an unbound key can select another command or scope.
    pub dynamic: bool,
}

/// Checks the actual call's scope. Supplied holes activate guards without supplying text.
#[allow(clippy::too_many_arguments)]
pub fn check_scope<E>(
    ir: &RulesIr,
    facts: &dyn SymbolFacts,
    kind: &str,
    name: &str,
    inputs: &crate::template::BindingInputs,
    state: ScopeState,
    checkpoint: &mut dyn FnMut() -> Result<(), E>,
) -> Result<Analysis<Validation>, E> {
    let (_, result) = solve(
        ir,
        facts,
        StateKey {
            kind: kind.to_ascii_lowercase(),
            name: name.to_ascii_lowercase(),
            values: inputs.values.clone(),
            present: Some(inputs.present.clone()),
            state,
        },
        checkpoint,
    )?;
    Ok(result)
}

/// Projects guaranteed entry requirements without treating unknown guards as unconditional.
/// Concrete argument forwarding still activates guards in callees when the argument is supplied.
pub fn entry_scopes<E>(
    ir: &RulesIr,
    facts: &dyn SymbolFacts,
    kind: &str,
    name: &str,
    checkpoint: &mut dyn FnMut() -> Result<(), E>,
) -> Result<Analysis<EntryScopeSummary>, E> {
    let mut summary = EntryScopeSummary::default();
    let mut coverage = AnalysisCoverage::default();
    for scope in &ir.scopes.types {
        checkpoint()?;
        let scope = ir.strings().resolve(*scope);
        let state = ScopeState {
            root: ScopeValue::Unknown,
            current: vec![ScopeValue::known_single(scope)],
            previous: Vec::new(),
            from: Vec::new(),
        };
        let (part, result) = solve(
            ir,
            facts,
            StateKey {
                kind: kind.to_ascii_lowercase(),
                name: name.to_ascii_lowercase(),
                values: BTreeMap::new(),
                present: None,
                state,
            },
            checkpoint,
        )?;
        coverage.merge(&result.coverage);
        summary.conditions.extend(part.conditions);
        summary.dynamic |= part.dynamic;
        if result.value != Validation::Invalid {
            summary.possible.push(scope.to_owned());
        }
    }
    Ok(Analysis {
        value: summary,
        coverage,
    })
}

fn value(token: &TemplateToken, values: &BTreeMap<String, String>) -> Option<String> {
    token
        .fragments
        .iter()
        .map(|part| match part {
            TemplateFragment::Literal(text) => Some(text.as_str()),
            TemplateFragment::Parameter { name, .. } => {
                values.get(&name.to_ascii_lowercase()).map(String::as_str)
            }
        })
        .collect()
}
fn append(nodes: &mut Vec<Constraint>, parent: usize, node: Constraint) -> usize {
    let id = nodes.len();
    nodes.push(node);
    match &mut nodes[parent] {
        Constraint::All(children) | Constraint::Any(children) => children.push(id),
        Constraint::Value(_) => {}
    }
    id
}

fn solve<E>(
    ir: &RulesIr,
    facts: &dyn SymbolFacts,
    root: StateKey,
    checkpoint: &mut dyn FnMut() -> Result<(), E>,
) -> Result<(EntryScopeSummary, Analysis<Validation>), E> {
    let mut coverage = AnalysisCoverage::default();
    let mut summary = EntryScopeSummary::default();
    let shared_memo = facts.template_memo();
    let mut nodes = vec![Constraint::All(Vec::new())];
    let mut memo = HashMap::new();
    let mut visiting = HashSet::new();
    let mut work = vec![Task::Call {
        key: root,
        target: 0,
    }];
    let mut remaining = 262_144usize;
    while let Some(task) = work.pop() {
        checkpoint()?;
        if remaining == 0 {
            coverage.limits.insert(AnalysisLimit::Nodes);
            break;
        }
        remaining -= 1;
        match task {
            Task::Exit(key) => {
                if let Some(id) = memo.get(&key).copied() {
                    let verdict = fold(&nodes, id, checkpoint)?;
                    nodes[id] = Constraint::Value(verdict);
                    if coverage.is_complete()
                        && let Some(shared) = &shared_memo
                    {
                        let cached = ScopeMemo {
                            value: verdict,
                            coverage: coverage.clone(),
                            conditions: summary.conditions.clone(),
                            dynamic: summary.dynamic,
                        };
                        let bytes = 256
                            + cached
                                .conditions
                                .iter()
                                .map(|value| value.len() + 24)
                                .sum::<usize>();
                        shared.insert(memo_key(&key), Arc::new(cached), bytes);
                    }
                }
                visiting.remove(&key);
            }
            Task::Call { key, target } => {
                if visiting.contains(&key) {
                    coverage.limits.insert(AnalysisLimit::RecursiveState);
                    append(&mut nodes, target, Constraint::Value(Validation::Unknown));
                    continue;
                }
                if let Some(existing) = memo.get(&key).copied() {
                    if let Constraint::All(children) = &mut nodes[target] {
                        children.push(existing);
                    }
                    continue;
                }
                if let Some(shared) = &shared_memo
                    && let Some(cached) = shared.get::<ScopeMemo>(&memo_key(&key))
                {
                    append(&mut nodes, target, Constraint::Value(cached.value));
                    coverage.merge(&cached.coverage);
                    summary.conditions.extend(cached.conditions.iter().cloned());
                    summary.dynamic |= cached.dynamic;
                    continue;
                }
                let template = ir
                    .type_by_name(&key.kind)
                    .and_then(|id| facts.replacement_template(id, &key.name));
                let schema = ir
                    .type_by_name(&key.kind)
                    .and_then(|id| crate::template::template_body(ir, id));
                let (Some(template), Some(schema)) = (template, schema) else {
                    coverage.limits.insert(AnalysisLimit::UnavailableTemplate);
                    append(&mut nodes, target, Constraint::Value(Validation::Unknown));
                    continue;
                };
                let body = append(&mut nodes, target, Constraint::All(Vec::new()));
                memo.insert(key.clone(), body);
                visiting.insert(key.clone());
                work.push(Task::Exit(key.clone()));
                work.push(Task::Block {
                    program: template.program.clone(),
                    block: 0,
                    schema,
                    key,
                    target: body,
                });
            }
            Task::Block {
                program,
                block,
                schema,
                key,
                target,
            } => {
                let mut uncertain_structure = false;
                for instruction in program.blocks[block].iter() {
                    checkpoint()?;
                    if remaining == 0 {
                        coverage.limits.insert(AnalysisLimit::Nodes);
                        break;
                    }
                    remaining -= 1;
                    match instruction {
                        TemplateInstruction::Recover(_) => {
                            uncertain_structure = true;
                            coverage.limits.insert(AnalysisLimit::StructuralRecovery);
                            append(&mut nodes, target, Constraint::Value(Validation::Unknown));
                        }
                        TemplateInstruction::Consume(token) => {
                            if ir.schema(schema).items.is_none() {
                                uncertain_structure = true;
                            }
                            if value(token, &key.values).is_none() {
                                summary.dynamic = true;
                                coverage.residuals.insert(ResidualReason::Binding);
                                append(&mut nodes, target, Constraint::Value(Validation::Unknown));
                            }
                        }
                        TemplateInstruction::When {
                            name,
                            negated,
                            block,
                        } => {
                            if let Some(present) = &key.present {
                                if present.contains(&name.to_ascii_lowercase()) != *negated {
                                    work.push(Task::Block {
                                        program: program.clone(),
                                        block: *block,
                                        schema,
                                        key: key.clone(),
                                        target,
                                    });
                                }
                            } else {
                                if program.blocks.iter().any(|block| {
                                    block.iter().any(|node| {
                                        matches!(
                                            node,
                                            TemplateInstruction::Consume(_)
                                                | TemplateInstruction::Recover(_)
                                        )
                                    })
                                }) {
                                    uncertain_structure = true;
                                }
                                summary.conditions.insert(name.to_string());
                                coverage.residuals.insert(ResidualReason::Binding);
                            }
                        }
                        TemplateInstruction::Dispatch(property) => {
                            if uncertain_structure {
                                append(&mut nodes, target, Constraint::Value(Validation::Unknown));
                                coverage.residuals.insert(ResidualReason::Interpretation);
                                continue;
                            }
                            let Some(spelling) = value(&property.key, &key.values) else {
                                uncertain_structure = true;
                                summary.dynamic = true;
                                coverage.residuals.insert(ResidualReason::DynamicKey);
                                append(&mut nodes, target, Constraint::Value(Validation::Unknown));
                                continue;
                            };
                            let shape = if matches!(property.value, TemplateOperand::Block(_)) {
                                Shape::Block
                            } else {
                                Shape::Scalar
                            };
                            let fields = crate::checking::field_candidates(
                                ir, schema, &spelling, shape, facts,
                            );
                            if fields.is_empty() {
                                continue;
                            } // Unknown command is a key goal, not a scope proof.
                            let alternatives =
                                append(&mut nodes, target, Constraint::Any(Vec::new()));
                            for id in fields {
                                let field = ir.field(id);
                                let row =
                                    append(&mut nodes, alternatives, Constraint::All(Vec::new()));
                                let mut allowed =
                                    crate::checking::field_scope_validation(ir, id, &key.state);
                                if matches!(
                                    ir.matcher(field.key),
                                    Matcher::Link | Matcher::Scope(_)
                                ) {
                                    let scope = crate::checking::scalar_validation(
                                        ir, field.key, &spelling, &key.state, facts,
                                    );
                                    if scope == Validation::Invalid
                                        || allowed == Validation::Invalid
                                    {
                                        allowed = Validation::Invalid;
                                    } else if scope == Validation::Unknown {
                                        allowed = Validation::Unknown;
                                    }
                                }
                                if allowed == Validation::Unknown {
                                    coverage.residuals.insert(ResidualReason::Scope);
                                }
                                append(&mut nodes, row, Constraint::Value(allowed));
                                if allowed == Validation::Invalid {
                                    continue;
                                }
                                if let Some(kind) = crate::template::template_kind(ir, field.key) {
                                    let mut values = BTreeMap::new();
                                    let mut present = BTreeSet::new();
                                    if let TemplateOperand::Block(block) = &property.value {
                                        let mut pending = vec![*block];
                                        while let Some(block) = pending.pop() {
                                            for argument in program.blocks[block].iter() {
                                                checkpoint()?;
                                                if remaining == 0 {
                                                    coverage.limits.insert(AnalysisLimit::Nodes);
                                                    break;
                                                }
                                                remaining -= 1;
                                                match argument {
                                                    TemplateInstruction::Dispatch(argument) => {
                                                        if let Some(name) =
                                                            value(&argument.key, &key.values)
                                                        {
                                                            present
                                                                .insert(name.to_ascii_lowercase());
                                                            if let TemplateOperand::Scalar(token) =
                                                                &argument.value
                                                                && let Some(text) =
                                                                    value(token, &key.values)
                                                            {
                                                                values.insert(
                                                                    name.to_ascii_lowercase(),
                                                                    text,
                                                                );
                                                            }
                                                        }
                                                    }
                                                    TemplateInstruction::When {
                                                        name,
                                                        negated,
                                                        block,
                                                    } if key.present.as_ref().is_some_and(
                                                        |present| {
                                                            present.contains(
                                                                &name.to_ascii_lowercase(),
                                                            ) != *negated
                                                        },
                                                    ) =>
                                                    {
                                                        pending.push(*block)
                                                    }
                                                    _ => {}
                                                }
                                            }
                                        }
                                    }
                                    work.push(Task::Call {
                                        key: StateKey {
                                            kind,
                                            name: spelling.to_ascii_lowercase(),
                                            values,
                                            present: Some(present),
                                            state: key.state.clone(),
                                        },
                                        target: row,
                                    });
                                } else if let TemplateOperand::Block(block) = &property.value
                                    && let Some(child) = ir.child(id, schema)
                                {
                                    if field.control.as_ref().is_some_and(|control| {
                                        control.kind == rules::source::ControlKind::DisplayOnly
                                    }) {
                                        continue;
                                    }
                                    let mut next = key.clone();
                                    next.state = crate::transition_ir_scope(
                                        ir,
                                        next.state,
                                        field.scope.as_ref(),
                                        &spelling,
                                    );
                                    work.push(Task::Block {
                                        program: program.clone(),
                                        block: *block,
                                        schema: child,
                                        key: next,
                                        target: row,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    if !coverage.is_complete() {
        append(&mut nodes, 0, Constraint::Value(Validation::Unknown));
    }
    let result = fold(&nodes, 0, checkpoint)?;
    Ok((
        summary,
        Analysis {
            value: result,
            coverage,
        },
    ))
}

fn fold<E>(
    nodes: &[Constraint],
    root: usize,
    checkpoint: &mut dyn FnMut() -> Result<(), E>,
) -> Result<Validation, E> {
    let mut values = HashMap::new();
    let mut pending = vec![(root, false)];
    while let Some((id, finish)) = pending.pop() {
        checkpoint()?;
        if values.contains_key(&id) {
            continue;
        }
        match &nodes[id] {
            Constraint::Value(value) => {
                values.insert(id, *value);
            }
            Constraint::All(children) | Constraint::Any(children) => {
                if !finish {
                    pending.push((id, true));
                    pending.extend(children.iter().map(|id| (*id, false)));
                    continue;
                }
                let any = matches!(nodes[id], Constraint::Any(_));
                let outcomes = children
                    .iter()
                    .map(|id| values.get(id).copied().unwrap_or(Validation::Unknown))
                    .collect::<Vec<_>>();
                values.insert(
                    id,
                    if any {
                        if outcomes.contains(&Validation::Valid) {
                            Validation::Valid
                        } else if outcomes.contains(&Validation::Unknown) {
                            Validation::Unknown
                        } else {
                            Validation::Invalid
                        }
                    } else if outcomes.contains(&Validation::Invalid) {
                        Validation::Invalid
                    } else if outcomes.contains(&Validation::Unknown) {
                        Validation::Unknown
                    } else {
                        Validation::Valid
                    },
                );
            }
        }
    }
    Ok(values.get(&root).copied().unwrap_or(Validation::Unknown))
}
