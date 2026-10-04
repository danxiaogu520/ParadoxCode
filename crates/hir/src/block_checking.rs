//! Whole-container overload selection, shared by ordinary lowering and Template instances.
use crate::analysis::{Analysis, AnalysisCoverage, AnalysisLimit, ResidualReason, Validation};
use crate::{HirProperty, ScopeState};
use rules::ir::{FieldId, FieldValue, Matcher, RulesIr, SchemaId, Shape, SymbolFacts};
use std::collections::BTreeMap;
use text::TextRange;

/// Possible interpretations of one physical block. Valid selection follows field priority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OverloadFact {
    /// Owning property key.
    pub range: TextRange,
    /// Complete container that was checked.
    pub container: TextRange,
    /// Parent rule context.
    pub schema: SchemaId,
    /// Remaining field interpretations; a proved selection contains one field.
    pub fields: Vec<FieldId>,
    /// Complete, rejected, or unresolved selection.
    pub validation: Validation,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
struct Context {
    schema: SchemaId,
    range: TextRange,
    state: ScopeState,
    indices: Vec<usize>,
    branch_self: Option<SchemaId>,
}
enum Node {
    All(Vec<usize>),
    Any(Vec<usize>),
    Value(Validation),
}
enum Task {
    Enter(Context, usize),
    Finish(Context, usize),
}
fn add(nodes: &mut Vec<Node>, parent: usize, node: Node) -> usize {
    let id = nodes.len();
    nodes.push(node);
    match &mut nodes[parent] {
        Node::All(children) | Node::Any(children) => children.push(id),
        _ => {}
    };
    id
}
fn combined(nodes: &[Node], id: usize, values: &BTreeMap<usize, Validation>) -> Validation {
    let (children, any) = match &nodes[id] {
        Node::Value(value) => return *value,
        Node::All(children) => (children, false),
        Node::Any(children) => (children, true),
    };
    let outcomes = children
        .iter()
        .map(|id| values.get(id).copied().unwrap_or(Validation::Unknown))
        .collect::<Vec<_>>();
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
    }
}
fn fold(nodes: &[Node], root: usize) -> Validation {
    let mut values = BTreeMap::new();
    let mut pending = vec![(root, false)];
    while let Some((id, finish)) = pending.pop() {
        if values.contains_key(&id) {
            continue;
        }
        match &nodes[id] {
            Node::Value(value) => {
                values.insert(id, *value);
            }
            Node::All(children) | Node::Any(children) => {
                if !finish {
                    pending.push((id, true));
                    pending.extend(children.iter().map(|id| (*id, false)));
                } else {
                    values.insert(id, combined(nodes, id, &values));
                }
            }
        }
    }
    values.get(&root).copied().unwrap_or(Validation::Unknown)
}

/// Checks every alternative against the same full block, rather than mixing their children.
#[allow(clippy::too_many_arguments)]
pub fn select_overloads<E>(
    ir: &RulesIr,
    props: &[HirProperty],
    children: &[Vec<usize>],
    index: usize,
    schema: SchemaId,
    state: &ScopeState,
    fields: &[FieldId],
    facts: &dyn SymbolFacts,
    unknown_ranges: &[TextRange],
    template_text: bool,
    checkpoint: &mut dyn FnMut() -> Result<(), E>,
) -> Result<Analysis<OverloadFact>, E> {
    let property = &props[index];
    let range = property.value_range.unwrap_or(property.range);
    let mut coverage = AnalysisCoverage::default();
    let mut candidates = Vec::new();
    let mut remaining = 262_144usize;
    for field in fields {
        checkpoint()?;
        let Some(child) = ir.child(*field, schema) else {
            continue;
        };
        let next = crate::transition_ir_scope(
            ir,
            state.clone(),
            ir.field(*field).scope.as_ref(),
            &property.key,
        );
        let context = Context {
            schema: child,
            range,
            state: next,
            indices: children[index].clone(),
            branch_self: None,
        };
        let check = check(
            ir,
            props,
            children,
            facts,
            context,
            unknown_ranges,
            template_text,
            &mut remaining,
            checkpoint,
        )?;
        coverage.merge(&check.coverage);
        candidates.push((*field, check.value));
    }
    let (validation, possible) = if let Some((field, _)) = candidates
        .iter()
        .find(|(_, result)| *result == Validation::Valid)
    {
        (Validation::Valid, vec![*field])
    } else {
        let unknown = candidates
            .iter()
            .filter(|(_, result)| *result == Validation::Unknown)
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        if !unknown.is_empty() {
            (Validation::Unknown, unknown)
        } else {
            (Validation::Invalid, fields.to_vec())
        }
    };
    if validation == Validation::Unknown {
        coverage.residuals.insert(ResidualReason::Interpretation);
    }
    Ok(Analysis {
        value: OverloadFact {
            range: property.key_range,
            container: range,
            schema,
            fields: possible,
            validation,
        },
        coverage,
    })
}

#[allow(clippy::too_many_arguments)]
fn check<E>(
    ir: &RulesIr,
    props: &[HirProperty],
    children: &[Vec<usize>],
    facts: &dyn SymbolFacts,
    root: Context,
    unknown_ranges: &[TextRange],
    template_text: bool,
    remaining: &mut usize,
    checkpoint: &mut dyn FnMut() -> Result<(), E>,
) -> Result<Analysis<Validation>, E> {
    let mut coverage = AnalysisCoverage::default();
    let mut nodes = vec![Node::All(Vec::new())];
    let mut memo = std::collections::HashMap::new();
    let mut work = vec![Task::Enter(root, 0)];
    let hole = |range: TextRange| {
        unknown_ranges
            .iter()
            .any(|hole| hole.start() < range.end() && range.start() < hole.end())
    };
    while let Some(task) = work.pop() {
        checkpoint()?;
        if *remaining == 0 {
            coverage.limits.insert(AnalysisLimit::Nodes);
            break;
        }
        *remaining -= 1;
        match task {
            Task::Finish(context, target) => {
                let value = fold(&nodes, target);
                nodes[target] = Node::Value(value);
                memo.insert(context, value);
            }
            Task::Enter(context, target) => {
                if let Some(value) = memo.get(&context) {
                    add(&mut nodes, target, Node::Value(*value));
                    continue;
                }
                work.push(Task::Finish(context.clone(), target));
                let mut counts = BTreeMap::<(usize, String), u32>::new();
                let mut partial = hole(context.range);
                for index in &context.indices {
                    checkpoint()?;
                    if *remaining == 0 {
                        coverage.limits.insert(AnalysisLimit::Nodes);
                        break;
                    }
                    *remaining -= 1;
                    let property = &props[*index];
                    if hole(property.key_range) || template_text && property.key.contains('$') {
                        partial = true;
                        add(&mut nodes, target, Node::Value(Validation::Unknown));
                        continue;
                    }
                    let shape = if property.scalar.is_some() {
                        Shape::Scalar
                    } else {
                        Shape::Block
                    };
                    let fields = crate::checking::field_candidates(
                        ir,
                        context.schema,
                        &property.key,
                        shape,
                        facts,
                    );
                    if fields.is_empty() {
                        let unknown = property.value_range.is_none() || property.operator.is_none();
                        let result = if unknown {
                            partial = true;
                            Validation::Unknown
                        } else if ir.schema(context.schema).open {
                            Validation::Valid
                        } else {
                            Validation::Invalid
                        };
                        add(&mut nodes, target, Node::Value(result));
                        continue;
                    }
                    let alternatives = add(&mut nodes, target, Node::Any(Vec::new()));
                    let mut selected = None;
                    for field in fields {
                        let row = add(&mut nodes, alternatives, Node::All(Vec::new()));
                        let spec = ir.field(field);
                        let (key, limit) =
                            crate::checking::scalar_outcome(ir, spec.key, &property.key, facts);
                        if limit.is_some() {
                            coverage.limits.insert(AnalysisLimit::PatternSearch);
                        }
                        let key = if key.is_none() {
                            partial = true;
                            Validation::Unknown
                        } else {
                            Validation::Valid
                        };
                        add(&mut nodes, row, Node::Value(key));
                        let scope =
                            crate::checking::field_scope_validation(ir, field, &context.state);
                        add(&mut nodes, row, Node::Value(scope));
                        if scope == Validation::Invalid {
                            continue;
                        }
                        let result = match (spec.value, property.scalar.as_ref()) {
                            (FieldValue::Scalar(_), Some(value))
                                if hole(value.range)
                                    || template_text && value.value.contains('$') =>
                            {
                                Validation::Unknown
                            }
                            (FieldValue::Scalar(matcher), Some(value)) => {
                                let checked = crate::checking::scalar_validation_cancellable(
                                    ir,
                                    matcher,
                                    &value.value,
                                    &context.state,
                                    facts,
                                    checkpoint,
                                )?;
                                coverage.merge(&checked.coverage);
                                checked.value
                            }
                            (FieldValue::Scalar(_), None) => Validation::Invalid,
                            (_, Some(_)) => Validation::Invalid,
                            _ => Validation::Valid,
                        };
                        add(&mut nodes, row, Node::Value(result));
                        if result == Validation::Invalid {
                            continue;
                        }
                        if selected.is_none() {
                            selected = Some(field);
                        }
                        if scope == Validation::Unknown || result == Validation::Unknown {
                            coverage.residuals.insert(ResidualReason::Binding);
                        }
                        if property.scalar.is_none()
                            && !spec.control.as_ref().is_some_and(|control| {
                                control.kind == rules::source::ControlKind::DisplayOnly
                            })
                            && let Some(child) = if matches!(spec.value, FieldValue::SelfBlock) {
                                Some(context.branch_self.unwrap_or(context.schema))
                            } else {
                                ir.child(field, context.schema)
                            }
                        {
                            let state = crate::transition_ir_scope(
                                ir,
                                context.state.clone(),
                                spec.scope.as_ref(),
                                &property.key,
                            );
                            let branch_self = spec
                                .control
                                .as_ref()
                                .filter(|control| {
                                    matches!(
                                        control.kind,
                                        rules::source::ControlKind::Weighted
                                            | rules::source::ControlKind::Chance
                                            | rules::source::ControlKind::Switch
                                    )
                                })
                                .map(|_| context.schema);
                            work.push(Task::Enter(
                                Context {
                                    schema: child,
                                    range: property.value_range.unwrap_or(property.range),
                                    state,
                                    indices: children[*index].clone(),
                                    branch_self,
                                },
                                row,
                            ));
                        }
                    }
                    if let Some(field) = selected {
                        *counts
                            .entry((field.index(), property.key.to_ascii_lowercase()))
                            .or_default() += 1;
                    }
                }
                let schema = ir.schema(context.schema);
                if !partial
                    && !schema.forms.is_empty()
                    && !schema.forms.iter().any(|form| {
                        form.counts.iter().all(|(ids, card)| {
                            let count = ids
                                .iter()
                                .map(|id| {
                                    counts
                                        .iter()
                                        .filter(|((field, _), _)| *field == id.index())
                                        .map(|(_, count)| *count)
                                        .sum::<u32>()
                                })
                                .sum::<u32>();
                            count >= card.min && card.max.is_none_or(|max| count <= max)
                        })
                    })
                {
                    add(&mut nodes, target, Node::Value(Validation::Invalid));
                }
                let mut families = BTreeMap::<usize, Vec<FieldId>>::new();
                for field in ir.fields(context.schema) {
                    families
                        .entry(ir.field(field).key.index())
                        .or_default()
                        .push(field);
                }
                for family in families.values() {
                    let applicable = family
                        .iter()
                        .copied()
                        .filter(|id| {
                            crate::checking::field_scope_validation(ir, *id, &context.state)
                                == Validation::Valid
                        })
                        .collect::<Vec<_>>();
                    if applicable.is_empty() {
                        continue;
                    }
                    let minimum = applicable
                        .iter()
                        .map(|id| ir.field(*id).card.min)
                        .max()
                        .unwrap_or(0);
                    let maximum = if applicable.iter().any(|id| ir.field(*id).card.max.is_none()) {
                        None
                    } else {
                        applicable
                            .iter()
                            .filter_map(|id| ir.field(*id).card.max)
                            .max()
                    };
                    let key = ir.field(family[0]).key;
                    let exact = matches!(ir.matcher(key), Matcher::Literal(_));
                    let names = counts
                        .keys()
                        .filter(|(id, _)| family.iter().any(|field| field.index() == *id))
                        .map(|(_, name)| name.clone())
                        .collect::<std::collections::BTreeSet<_>>();
                    let quota = |name: Option<&str>| {
                        let count = counts
                            .iter()
                            .filter(|((id, key), _)| {
                                family.iter().any(|field| field.index() == *id)
                                    && name.is_none_or(|name| key == name)
                            })
                            .map(|(_, count)| *count)
                            .sum::<u32>();
                        (count < minimum && !partial) || maximum.is_some_and(|max| count > max)
                    };
                    if exact {
                        if quota(None) {
                            add(&mut nodes, target, Node::Value(Validation::Invalid));
                        }
                    } else if names.iter().any(|name| quota(Some(name)))
                        || names.is_empty() && minimum > 0 && !partial
                    {
                        add(&mut nodes, target, Node::Value(Validation::Invalid));
                    }
                }
            }
        }
    }
    if !coverage.is_complete() {
        add(&mut nodes, 0, Node::Value(Validation::Unknown));
    }
    Ok(Analysis {
        value: fold(&nodes, 0),
        coverage,
    })
}
