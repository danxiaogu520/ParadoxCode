//! HIR facts lowered from the compiled Rules IR.
use parser::{ParsedFile, parse_quoted_script};
use rules::ir::{
    DefName, Matcher, MatcherId, RefTarget, RootRule, RulesIr, ScalarFields, SchemaId, Shape,
    SubtypeSet, SymbolFacts, TypeId,
};
use text::{LogicalPath, TextRange};

use crate::{
    FieldFact, HirDefinition, HirProperty, HirReference, HirReferenceOrigin, HirScalar, SchemaFact,
    ScopeFact, ScopeState, ScopeValue,
};

pub(super) struct IrFacts {
    pub schema_facts: Vec<SchemaFact>,
    pub field_facts: Vec<FieldFact>,
    pub scope_facts: Vec<ScopeFact>,
    pub definitions: Vec<HirDefinition>,
    pub references: Vec<HirReference>,
    pub binding_references: Vec<HirReference>,
    pub definition_attributes: Vec<crate::DefinitionAttributes>,
    pub callable_kinds: std::collections::BTreeSet<String>,
    pub runtime_parameter_guards: Vec<(TextRange, Option<TextRange>)>,
}
struct Body<'a> {
    properties: &'a [HirProperty],
    indices: &'a [usize],
}
struct FactsRef<'a>(&'a dyn SymbolFacts);
impl SymbolFacts for FactsRef<'_> {
    fn type_member(&self, ty: TypeId, name: &str) -> bool {
        self.0.type_member(ty, name)
    }
    fn trait_impl_member(&self, trait_id: rules::ir::TraitId, name: &str) -> bool {
        self.0.trait_impl_member(trait_id, name)
    }
    fn type_subtype_member(&self, ty: TypeId, subtype: rules::ir::Symbol, name: &str) -> bool {
        self.0.type_subtype_member(ty, subtype, name)
    }
}
impl ScalarFields for Body<'_> {
    fn scalar(&self, key: &str) -> Option<&str> {
        self.indices.iter().find_map(|i| {
            let p = &self.properties[*i];
            if p.key.eq_ignore_ascii_case(key) {
                p.scalar.as_ref().map(|s| s.value.as_str())
            } else {
                None
            }
        })
    }
}

pub(super) fn lower(
    ir: &RulesIr,
    path: &LogicalPath,
    props: &[HirProperty],
    bare_values: &[HirScalar],
    syntax: &ParsedFile,
    facts: Option<&dyn SymbolFacts>,
) -> IrFacts {
    let children = crate::scope::property_children(props);
    let mut out = IrFacts {
        schema_facts: vec![],
        field_facts: vec![],
        scope_facts: vec![],
        definitions: vec![],
        references: vec![],
        binding_references: vec![],
        definition_attributes: vec![],
        callable_kinds: callable_kinds(ir),
        runtime_parameter_guards: Vec::new(),
    };
    let unknown = ScopeState::initial(ScopeValue::Unknown);
    let Some((_, file)) = ir.file_rule(path) else {
        return out;
    };
    let roots: Vec<(SchemaId, Vec<usize>, SubtypeSet, ScopeState)> = match &file.root {
        RootRule::Schema(schema) => vec![(
            *schema,
            props
                .iter()
                .enumerate()
                .filter_map(|(i, p)| p.top_level.then_some(i))
                .collect(),
            SubtypeSet::default(),
            unknown.clone(),
        )],
        RootRule::Instance { def, body } => {
            let indices = props
                .iter()
                .enumerate()
                .filter_map(|(i, p)| p.top_level.then_some(i))
                .collect::<Vec<_>>();
            let mut body_subtypes = body
                .map(|schema| inferred_subtypes(ir, schema, props, &indices, facts))
                .unwrap_or_default();
            if let Some(def) = def
                && let Some(subtype) = def.subtype
            {
                body_subtypes.insert(def.type_id, subtype);
            }
            if let Some(def) = def
                && let Some(name) = instance_name(ir, def, None, path, props, &indices)
            {
                let selection =
                    definition_selection(ir, def, None, props, &indices, syntax.root().range());
                push_definition(
                    ir,
                    &mut out,
                    def.type_id,
                    name.clone(),
                    syntax.root().range(),
                    selection,
                    indices
                        .iter()
                        .map(|index| props[*index].key.clone())
                        .collect(),
                    subtype_names_from_set(ir, def.type_id, &body_subtypes),
                );
                add_bindings(ir, def.type_id, &body_subtypes, &name, selection, &mut out);
            }
            body.map(|schema| (schema, indices, body_subtypes, unknown.clone()))
                .into_iter()
                .collect()
        }
        RootRule::Opaque => Vec::new(),
    };
    let mut seen_defs = std::collections::BTreeSet::new();
    for (schema, indices, subtypes, state) in roots {
        out.schema_facts.push(SchemaFact {
            range: syntax.root().range(),
            schema,
            subtypes: subtypes.clone(),
            state: state.clone(),
        });
        descend(
            ir,
            path,
            props,
            bare_values,
            &children,
            indices,
            schema,
            subtypes,
            state,
            facts,
            syntax,
            &mut out,
            &mut seen_defs,
            syntax.root().range(),
            None,
        );
    }
    out.schema_facts
        .sort_by_key(|f| (f.range.start(), f.range.end()));
    out.field_facts.sort_by_key(|f| f.range);
    out.scope_facts.sort_by_key(|f| f.range);
    out.definitions.sort_by_key(|d| d.selection_range.start());
    out.references.sort_by_key(|r| r.range.start());
    out.binding_references.sort_by_key(|r| r.range.start());
    out
}

fn callable_kinds(ir: &RulesIr) -> std::collections::BTreeSet<String> {
    let Some(callable) = ir.trait_by_name("Callable") else {
        return Default::default();
    };
    ir.types
        .iter()
        .filter(|ty| {
            ty.trait_impls
                .iter()
                .any(|implementation| implementation.trait_id == callable)
                || ty.subtypes.iter().any(|subtype| {
                    subtype
                        .trait_impls
                        .iter()
                        .any(|implementation| implementation.trait_id == callable)
                })
        })
        .map(|ty| ir.strings().resolve(ty.name).to_ascii_lowercase())
        .collect()
}

fn inferred_subtypes(
    ir: &RulesIr,
    schema: SchemaId,
    props: &[HirProperty],
    indices: &[usize],
    facts: Option<&dyn SymbolFacts>,
) -> SubtypeSet {
    let body = Body {
        properties: props,
        indices,
    };
    if let Some(facts) = facts {
        ir.subtypes_of_with(schema, &body, &FactsRef(facts))
    } else {
        ir.subtypes_of(schema, &body)
    }
}

fn subtype_names_from_set(ir: &RulesIr, ty: TypeId, set: &SubtypeSet) -> Vec<std::sync::Arc<str>> {
    ir.type_info(ty)
        .subtypes
        .iter()
        .filter(|subtype| set.contains(ty, subtype.name))
        .map(|subtype| ir.strings().resolve(subtype.name).into())
        .collect()
}

pub(super) fn lower_schema_fragment<F: SymbolFacts>(
    ir: &RulesIr,
    syntax: &ParsedFile,
    schema: SchemaId,
    subtypes: SubtypeSet,
    state: ScopeState,
    facts: &F,
) -> IrFacts {
    let collected = crate::collector::collect(syntax);
    let props = collected.properties;
    let bare_values = collected.bare_values;
    let children = crate::scope::property_children(&props);
    let mut out = IrFacts {
        schema_facts: vec![SchemaFact {
            range: syntax.root().range(),
            schema,
            subtypes: subtypes.clone(),
            state: state.clone(),
        }],
        field_facts: vec![],
        scope_facts: vec![],
        definitions: vec![],
        references: vec![],
        binding_references: vec![],
        definition_attributes: vec![],
        callable_kinds: callable_kinds(ir),
        runtime_parameter_guards: Vec::new(),
    };
    let path = LogicalPath::parse("").expect("empty fragment path");
    descend(
        ir,
        &path,
        &props,
        &bare_values,
        &children,
        props
            .iter()
            .enumerate()
            .filter_map(|(i, p)| p.top_level.then_some(i))
            .collect(),
        schema,
        subtypes,
        state,
        Some(facts),
        syntax,
        &mut out,
        &mut std::collections::BTreeSet::new(),
        syntax.root().range(),
        None,
    );
    out.schema_facts
        .sort_by_key(|fact| (fact.range.start(), fact.range.end()));
    out.field_facts.sort_by_key(|fact| fact.range);
    out.scope_facts.sort_by_key(|fact| fact.range);
    out.definitions
        .sort_by_key(|def| def.selection_range.start());
    out
}
#[allow(clippy::too_many_arguments)]
fn descend(
    ir: &RulesIr,
    path: &LogicalPath,
    props: &[HirProperty],
    bare_values: &[HirScalar],
    children: &[Vec<usize>],
    indices: Vec<usize>,
    schema: SchemaId,
    subtypes: SubtypeSet,
    state: ScopeState,
    facts: Option<&dyn SymbolFacts>,
    source: &ParsedFile,
    out: &mut IrFacts,
    seen: &mut std::collections::BTreeSet<(u32, u32)>,
    container_range: TextRange,
    branch_self_schema: Option<SchemaId>,
) {
    if let Some(matcher) = ir.schema(schema).items {
        for item in bare_values.iter().filter(|item| {
            item.range.start() >= container_range.start()
                && item.range.end() <= container_range.end()
        }) {
            collect_matcher_refs(ir, matcher, &item.value, item.range, facts, out);
        }
    }
    for index in indices {
        let p = &props[index];
        let shape = if p.scalar.as_ref().is_some_and(|scalar| scalar.quoted) {
            Shape::Quoted
        } else if p.scalar.is_some() {
            Shape::Scalar
        } else {
            Shape::Block
        };
        let mut candidates = ir.lookup(schema, &p.key, shape).collect::<Vec<_>>();
        if shape == Shape::Quoted {
            candidates.extend(ir.lookup(schema, &p.key, Shape::Scalar));
        }
        let candidates = candidates
            .into_iter()
            .filter(|id| ir.gate_holds(ir.field(*id).gate, &subtypes))
            .filter(|id| matcher_accepts_key(ir, ir.field(*id).key, &p.key, facts, out))
            .collect::<Vec<_>>();
        let exact_candidates = candidates
            .iter()
            .copied()
            .filter(|id| is_exact_key_match(ir, schema, *id, &p.key))
            .collect::<Vec<_>>();
        let candidates = if exact_candidates.is_empty() {
            candidates
        } else {
            exact_candidates
        };
        let scoped = candidates
            .iter()
            .copied()
            .filter(|id| {
                let field = ir.field(*id);
                field.scope.as_ref().is_none_or(|effect| {
                    effect.scopes_in.is_empty()
                        || state.current.first().is_none_or(|current| {
                            scope_value_allows(ir, current, &effect.scopes_in)
                        })
                })
            })
            .collect::<Vec<_>>();
        let mut candidates = if scoped.is_empty() {
            candidates
        } else {
            scoped
        };
        candidates.sort_by_key(|id| std::cmp::Reverse(field_value_priority(ir, *id)));
        let selected = p
            .scalar
            .as_ref()
            .and_then(|scalar| {
                candidates.iter().copied().find(|id| {
                    let rules::ir::FieldValue::Scalar(matcher) = ir.field(*id).value else {
                        return false;
                    };
                    // Untyped alternatives must not hide a malformed typed value.
                    field_value_priority(ir, *id) > 0
                        && (reference_matches(ir, matcher, &scalar.value, facts, out)
                            || facts.map_or_else(
                                || {
                                    ir.scalar_matches(
                                        matcher,
                                        &scalar.value,
                                        &rules::ir::NoSymbolFacts,
                                    )
                                },
                                |facts| ir.scalar_matches(matcher, &scalar.value, &FactsRef(facts)),
                            ))
                })
            })
            .or_else(|| candidates.first().copied());
        let candidates = selected.into_iter().collect::<Vec<_>>();
        out.field_facts.push(FieldFact {
            range: p.key_range,
            schema,
            fields: candidates.clone(),
            subtypes: subtypes.clone(),
        });
        out.scope_facts.push(ScopeFact {
            range: p.key_range,
            context: ir.strings().resolve(ir.schema(schema).name).to_owned(),
            parent_path: Vec::new(),
            state: state.clone(),
            transition: candidates.first().map(|id| {
                transition_state(ir, state.clone(), ir.field(*id).scope.as_ref(), &p.key)
            }),
        });
        for id in candidates {
            let field = ir.field(id);
            if let Some(control) = &field.control
                && matches!(
                    control.kind,
                    rules::source::ControlKind::Branch | rules::source::ControlKind::BranchContinue
                )
            {
                let guard = control.guard.and_then(|guard| {
                    children[index].iter().find_map(|child| {
                        let child = &props[*child];
                        child
                            .key
                            .eq_ignore_ascii_case(ir.strings().resolve(guard))
                            .then_some(child.range)
                    })
                });
                out.runtime_parameter_guards.push((p.range, guard));
            }
            let key_def_is_field_def = matches!(
                (&field.def, ir.matcher(field.key)),
                (
                    Some(def),
                    Matcher::Def { type_id, subtype }
                ) if def.type_id == *type_id && def.subtype == *subtype
            );
            if !key_def_is_field_def {
                collect_matcher_refs(ir, field.key, &p.key, p.key_range, facts, out);
            }
            let mut candidate_subtypes = ir
                .child(id, schema)
                .map(|child_schema| {
                    inferred_subtypes(ir, child_schema, props, &children[index], facts)
                })
                .unwrap_or_default();
            if let Matcher::Def { type_id, subtype } = ir.matcher(field.key) {
                if let Some(subtype) = subtype {
                    candidate_subtypes.insert(*type_id, *subtype);
                }
                let name = p.key.clone();
                push_definition(
                    ir,
                    out,
                    *type_id,
                    name.clone(),
                    p.range,
                    p.key_range,
                    children[index]
                        .iter()
                        .map(|child| props[*child].key.clone())
                        .collect(),
                    subtype_names_from_set(ir, *type_id, &candidate_subtypes),
                );
                add_bindings(ir, *type_id, &candidate_subtypes, &name, p.key_range, out);
            }
            if let rules::ir::FieldValue::Scalar(matcher) = field.value
                && let Some(scalar) = &p.scalar
                && let Matcher::Def { type_id, subtype } = ir.matcher(matcher)
            {
                let mut value_subtypes = candidate_subtypes.clone();
                if let Some(subtype) = subtype {
                    value_subtypes.insert(*type_id, *subtype);
                }
                push_definition(
                    ir,
                    out,
                    *type_id,
                    scalar.value.clone(),
                    p.range,
                    scalar.range,
                    children[index]
                        .iter()
                        .map(|child| props[*child].key.clone())
                        .collect(),
                    subtype_names_from_set(ir, *type_id, &value_subtypes),
                );
                add_bindings(
                    ir,
                    *type_id,
                    &value_subtypes,
                    &scalar.value,
                    scalar.range,
                    out,
                );
            }
            if let Some(def) = &field.def {
                if let Some(subtype) = def.subtype {
                    candidate_subtypes.insert(def.type_id, subtype);
                }
                if let Some(name) = instance_name(ir, def, Some(p), path, props, &children[index]) {
                    let selection = definition_selection(
                        ir,
                        def,
                        Some(p),
                        props,
                        &children[index],
                        p.key_range,
                    );
                    if seen.insert((p.key_range.start(), id.index() as u32)) {
                        push_definition(
                            ir,
                            out,
                            def.type_id,
                            name.clone(),
                            p.range,
                            selection,
                            children[index]
                                .iter()
                                .map(|child| props[*child].key.clone())
                                .collect(),
                            subtype_names_from_set(ir, def.type_id, &candidate_subtypes),
                        );
                    }
                    add_bindings(ir, def.type_id, &candidate_subtypes, &name, selection, out);
                }
            }
            if let rules::ir::FieldValue::Scalar(matcher) = field.value
                && let Some(s) = &p.scalar
            {
                collect_matcher_refs(ir, matcher, &s.value, s.range, facts, out);
                if s.quoted {
                    let instance_def = field
                        .def
                        .as_ref()
                        .map(|def| (def.type_id, def.subtype))
                        .or_else(|| match ir.matcher(field.key) {
                            Matcher::Def { type_id, subtype } => Some((*type_id, *subtype)),
                            _ => None,
                        });
                    let quoted_state =
                        transition_state(ir, state.clone(), field.scope.as_ref(), &p.key);
                    lower_quoted(
                        ir,
                        source,
                        s.range,
                        matcher,
                        path,
                        &quoted_state,
                        &subtypes,
                        instance_def,
                        facts,
                        out,
                        seen,
                    );
                }
            }
            if let (Some(scalar), rules::ir::FieldValue::Quoted(quoted_schema)) =
                (&p.scalar, field.value)
                && scalar.quoted
            {
                let instance_def = field
                    .def
                    .as_ref()
                    .map(|def| (def.type_id, def.subtype))
                    .or_else(|| match ir.matcher(field.key) {
                        Matcher::Def { type_id, subtype } => Some((*type_id, *subtype)),
                        _ => None,
                    });
                let quoted_state =
                    transition_state(ir, state.clone(), field.scope.as_ref(), &p.key);
                lower_quoted_schema(
                    ir,
                    source,
                    scalar.range,
                    quoted_schema,
                    path,
                    &quoted_state,
                    &subtypes,
                    instance_def,
                    facts,
                    out,
                    seen,
                );
            }
            if let Some(child_schema) = if matches!(field.value, rules::ir::FieldValue::SelfBlock) {
                Some(branch_self_schema.unwrap_or(schema))
            } else {
                ir.child(id, schema)
                    .or_else(|| quoted_schema(ir, field.value))
            } {
                if p.scalar.as_ref().is_some_and(|scalar| scalar.quoted) {
                    continue;
                }
                let body = Body {
                    properties: props,
                    indices: &children[index],
                };
                let key_def = match ir.matcher(field.key) {
                    Matcher::Def { type_id, subtype } => Some((*type_id, *subtype)),
                    _ => None,
                };
                let value_def = match field.value {
                    rules::ir::FieldValue::Scalar(matcher) => match ir.matcher(matcher) {
                        Matcher::Def { type_id, subtype } => Some((*type_id, *subtype)),
                        _ => None,
                    },
                    _ => None,
                };
                let instance_def = field
                    .def
                    .as_ref()
                    .map(|def| (def.type_id, def.subtype))
                    .or(key_def)
                    .or(value_def);
                let mut child_subtypes = if instance_def.is_some() {
                    if let Some(facts) = facts {
                        ir.subtypes_of_with(child_schema, &body, &FactsRef(facts))
                    } else {
                        ir.subtypes_of(child_schema, &body)
                    }
                } else {
                    subtypes.clone()
                };
                if let Some((type_id, Some(subtype))) = instance_def {
                    child_subtypes.insert(type_id, subtype);
                }
                let child_state = transition_state(ir, state.clone(), field.scope.as_ref(), &p.key);
                if let Some(range) = p.value_range {
                    out.schema_facts.push(SchemaFact {
                        range,
                        schema: child_schema,
                        subtypes: child_subtypes.clone(),
                        state: child_state.clone(),
                    });
                }
                if !children[index].is_empty() {
                    descend(
                        ir,
                        path,
                        props,
                        bare_values,
                        children,
                        children[index].clone(),
                        child_schema,
                        child_subtypes,
                        child_state,
                        facts,
                        source,
                        out,
                        seen,
                        p.value_range.unwrap_or(p.range),
                        field
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
                            .map(|_| schema),
                    );
                }
            }
        }
    }
}

fn quoted_schema(ir: &RulesIr, value: rules::ir::FieldValue) -> Option<SchemaId> {
    match value {
        rules::ir::FieldValue::Quoted(schema) => Some(schema),
        rules::ir::FieldValue::Scalar(matcher) => quoted_matcher_schema(ir, matcher),
        _ => None,
    }
}
fn quoted_matcher_schema(ir: &RulesIr, id: MatcherId) -> Option<SchemaId> {
    match ir.matcher(id) {
        Matcher::Quoted(schema) => Some(*schema),
        Matcher::Union(items) => items
            .iter()
            .find_map(|item| quoted_matcher_schema(ir, *item)),
        _ => None,
    }
}
#[allow(clippy::too_many_arguments)]
fn lower_quoted(
    ir: &RulesIr,
    source: &ParsedFile,
    range: TextRange,
    matcher: MatcherId,
    path: &LogicalPath,
    state: &ScopeState,
    subtypes: &SubtypeSet,
    instance_def: Option<(TypeId, Option<rules::ir::Symbol>)>,
    facts: Option<&dyn SymbolFacts>,
    out: &mut IrFacts,
    seen: &mut std::collections::BTreeSet<(u32, u32)>,
) {
    if let Some(schema) = quoted_matcher_schema(ir, matcher) {
        lower_quoted_schema(
            ir,
            source,
            range,
            schema,
            path,
            state,
            subtypes,
            instance_def,
            facts,
            out,
            seen,
        );
    }
}
#[allow(clippy::too_many_arguments)]
fn lower_quoted_schema(
    ir: &RulesIr,
    source: &ParsedFile,
    range: TextRange,
    schema: SchemaId,
    path: &LogicalPath,
    state: &ScopeState,
    subtypes: &SubtypeSet,
    instance_def: Option<(TypeId, Option<rules::ir::Symbol>)>,
    facts: Option<&dyn SymbolFacts>,
    out: &mut IrFacts,
    seen: &mut std::collections::BTreeSet<(u32, u32)>,
) {
    let Some(raw) = source.text(range) else {
        return;
    };
    let Some(quoted) = parse_quoted_script(raw) else {
        return;
    };
    let collected = crate::collector::collect(quoted.parsed());
    let properties = collected.properties;
    let bare_values = collected.bare_values;
    let children = crate::scope::property_children(&properties);
    let indices = properties
        .iter()
        .enumerate()
        .filter_map(|(i, p)| p.top_level.then_some(i))
        .collect::<Vec<_>>();
    let mut effective_subtypes = if instance_def.is_some() {
        inferred_subtypes(ir, schema, &properties, &indices, facts)
    } else {
        subtypes.clone()
    };
    if let Some((type_id, Some(subtype))) = instance_def {
        effective_subtypes.insert(type_id, subtype);
    }
    let map_range = |inner: TextRange| {
        quoted
            .source_map()
            .decoded_range(inner)
            .and_then(|relative| {
                TextRange::new(
                    range.start().checked_add(relative.start())?,
                    range.start().checked_add(relative.end())?,
                )
            })
    };
    out.schema_facts.push(SchemaFact {
        range,
        schema,
        subtypes: effective_subtypes.clone(),
        state: state.clone(),
    });
    let schema_start = out.schema_facts.len();
    let field_start = out.field_facts.len();
    let scope_start = out.scope_facts.len();
    let definition_start = out.definitions.len();
    let reference_start = out.references.len();
    let binding_start = out.binding_references.len();
    let attributes_start = out.definition_attributes.len();
    let guards_start = out.runtime_parameter_guards.len();
    descend(
        ir,
        path,
        &properties,
        &bare_values,
        &children,
        indices,
        schema,
        effective_subtypes,
        state.clone(),
        facts,
        quoted.parsed(),
        out,
        seen,
        quoted.parsed().root().range(),
        None,
    );
    for (branch, guard) in &mut out.runtime_parameter_guards[guards_start..] {
        *branch = map_range(*branch).unwrap_or(range);
        *guard = guard.and_then(map_range);
    }
    for fact in &mut out.schema_facts[schema_start..] {
        fact.range = map_range(fact.range).unwrap_or(range);
    }
    for fact in &mut out.field_facts[field_start..] {
        fact.range = map_range(fact.range).unwrap_or(range);
    }
    for fact in &mut out.scope_facts[scope_start..] {
        fact.range = map_range(fact.range).unwrap_or(range);
        fact.transition = None;
    }
    for definition in &mut out.definitions[definition_start..] {
        definition.range = map_range(definition.range).unwrap_or(range);
        definition.selection_range = map_range(definition.selection_range).unwrap_or(range);
    }
    for reference in &mut out.references[reference_start..] {
        reference.range = map_range(reference.range).unwrap_or(range);
    }
    for reference in &mut out.binding_references[binding_start..] {
        reference.range = map_range(reference.range).unwrap_or(range);
    }
    for attributes in &mut out.definition_attributes[attributes_start..] {
        attributes.definition_range = map_range(attributes.definition_range).unwrap_or(range);
    }
}
pub(super) fn transition_state(
    ir: &RulesIr,
    mut state: ScopeState,
    effect: Option<&rules::ir::ScopeEffect>,
    key: &str,
) -> ScopeState {
    if let Some(link) = ir.scopes.links.iter().find(|link| {
        template_key_matches(ir, &link.pattern, key)
            && link_from_matches(ir, link, state.current.first())
    }) {
        state.previous.insert(
            0,
            state
                .current
                .first()
                .cloned()
                .unwrap_or(ScopeValue::Unknown),
        );
        state.current.insert(
            0,
            match link.to {
                rules::ir::ScopeRef::Any => ScopeValue::Unknown,
                rules::ir::ScopeRef::Type(scope) => {
                    ScopeValue::known_single(ir.strings().resolve(scope))
                }
            },
        );
    }
    if let Some(e) = effect {
        if let Some(push) = e.push {
            state.previous.insert(
                0,
                state
                    .current
                    .first()
                    .cloned()
                    .unwrap_or(ScopeValue::Unknown),
            );
            state
                .current
                .insert(0, ScopeValue::known_single(ir.strings().resolve(push)));
        }
        for (reg, scope) in e.set.iter() {
            let name = ir.strings().resolve(*reg);
            let value = ScopeValue::known_single(ir.strings().resolve(*scope));
            if name.eq_ignore_ascii_case("root") {
                state.root = value;
            } else if name.eq_ignore_ascii_case("this") {
                if state.current.is_empty() {
                    state.current.push(value);
                } else {
                    state.current[0] = value;
                }
            } else if let Some(index) = chained_register_index(name, "from") {
                set_register(&mut state.from, index, value);
            } else if let Some(index) = chained_register_index(name, "prev") {
                set_register(&mut state.previous, index, value);
            }
        }
    }
    state
}

fn scope_value_allows(ir: &RulesIr, current: &ScopeValue, expected: &[rules::ir::Symbol]) -> bool {
    match current {
        ScopeValue::Unknown => true,
        ScopeValue::Invalid => false,
        ScopeValue::Known(names) => names.iter().any(|name| {
            ir.strings().lookup_folded(name).is_some_and(|actual| {
                expected
                    .iter()
                    .any(|expected| ir.scopes_compatible(actual, *expected))
            })
        }),
    }
}

fn is_exact_key_match(
    ir: &RulesIr,
    schema: SchemaId,
    field: rules::ir::FieldId,
    key: &str,
) -> bool {
    ir.strings().lookup_folded(key).is_some_and(|symbol| {
        ir.schema(schema)
            .exact
            .get(&symbol)
            .is_some_and(|fields| fields.contains(&field))
    })
}

fn matcher_accepts_key(
    ir: &RulesIr,
    matcher_id: MatcherId,
    key: &str,
    facts: Option<&dyn SymbolFacts>,
    out: &IrFacts,
) -> bool {
    match ir.matcher(matcher_id) {
        Matcher::Scalar | Matcher::Def { .. } => true,
        Matcher::Literal(value) => ir.strings().resolve(*value).eq_ignore_ascii_case(key),
        Matcher::Enum { id, rows } => {
            ir.enum_info(*id)
                .rows
                .iter()
                .enumerate()
                .any(|(index, row)| {
                    ir.strings().resolve(row.name).eq_ignore_ascii_case(key)
                        && rows.as_ref().is_none_or(|set| set.contains(index))
                })
        }
        Matcher::Int { min, max } => key.parse::<i64>().is_ok_and(|value| {
            min.is_none_or(|min| value >= min) && max.is_none_or(|max| value <= max)
        }),
        Matcher::Date => {
            key.split('.').count() == 3 && key.split('.').all(|part| part.parse::<u32>().is_ok())
        }
        Matcher::Template(parts) => template_value_holes(ir, parts, key).is_some_and(|holes| {
            holes.into_iter().all(|(matcher, start, end)| {
                matcher_accepts_key(ir, matcher, &key[start..end], facts, out)
            })
        }),
        Matcher::Union(items) => items
            .iter()
            .any(|item| matcher_accepts_key(ir, *item, key, facts, out)),
        Matcher::Ref(RefTarget::Type {
            type_id,
            subtype,
            strip_prefix,
        }) => {
            let key = strip_prefix.map_or_else(
                || key.to_owned(),
                |prefix| strip_symbol_prefix(ir, key, prefix),
            );
            let local = out.definitions.iter().any(|definition| {
                definition
                    .kind
                    .eq_ignore_ascii_case(ir.strings().resolve(ir.type_info(*type_id).name))
                    && definition.name.eq_ignore_ascii_case(&key)
            });
            if local {
                return subtype.is_none_or(|required| {
                    out.definition_attributes.iter().any(|attrs| {
                        attrs.name.eq_ignore_ascii_case(&key)
                            && attrs.subtypes.iter().any(|name| {
                                name.eq_ignore_ascii_case(ir.strings().resolve(required))
                            })
                    })
                });
            }
            facts.is_some_and(|facts| match subtype {
                Some(subtype) => facts.type_subtype_member(*type_id, *subtype, &key),
                None => facts.type_member(*type_id, &key),
            })
        }
        Matcher::Ref(RefTarget::Trait(trait_id)) => {
            out.definitions
                .iter()
                .any(|definition| definition.name.eq_ignore_ascii_case(key))
                || facts.is_some_and(|facts| facts.trait_impl_member(*trait_id, key))
        }
        Matcher::Scope(scope) => ir.scope_matches(*scope, key),
        Matcher::Link => link_or_register_matches(ir, key),
        Matcher::Quoted(_)
        | Matcher::Opaque
        | Matcher::Loc
        | Matcher::Path(_)
        | Matcher::Bool
        | Matcher::Float { .. } => true,
    }
}

fn field_value_priority(ir: &RulesIr, field: rules::ir::FieldId) -> u8 {
    let rules::ir::FieldValue::Scalar(matcher) = ir.field(field).value else {
        return 0;
    };
    matcher_value_priority(ir, matcher)
}

fn matcher_value_priority(ir: &RulesIr, matcher: MatcherId) -> u8 {
    match ir.matcher(matcher) {
        Matcher::Loc | Matcher::Ref(_) | Matcher::Enum { .. } => 4,
        Matcher::Literal(_) | Matcher::Bool | Matcher::Int { .. } | Matcher::Date => 3,
        Matcher::Union(items) => items
            .iter()
            .map(|item| matcher_value_priority(ir, *item))
            .max()
            .unwrap_or(0),
        Matcher::Scalar | Matcher::Opaque | Matcher::Path(_) => 0,
        _ => 1,
    }
}
fn template_key_matches(ir: &RulesIr, parts: &[rules::ir::TemplatePart], key: &str) -> bool {
    let mut cursor = 0;
    for (index, part) in parts.iter().enumerate() {
        match part {
            rules::ir::TemplatePart::Text(text) => {
                let literal = ir.strings().resolve(*text);
                let Some(offset) = key[cursor..]
                    .to_ascii_lowercase()
                    .find(&literal.to_ascii_lowercase())
                else {
                    return false;
                };
                if index == 0 && offset != 0 {
                    return false;
                }
                cursor += offset + literal.len();
            }
            rules::ir::TemplatePart::Hole(_) => {
                if index + 1 == parts.len() {
                    cursor = key.len();
                } else if let rules::ir::TemplatePart::Text(next) = parts[index + 1] {
                    let needle = ir.strings().resolve(next);
                    let Some(offset) = key[cursor..]
                        .to_ascii_lowercase()
                        .find(&needle.to_ascii_lowercase())
                    else {
                        return false;
                    };
                    cursor += offset;
                }
            }
        }
    }
    cursor == key.len()
}
pub(super) fn link_or_register_matches(ir: &RulesIr, key: &str) -> bool {
    if ir.scopes.registers.iter().any(|register| {
        let name = ir.strings().resolve(register.name);
        key.eq_ignore_ascii_case(name)
            || (register.chain
                && key.len() > name.len()
                && key.len().is_multiple_of(name.len())
                && key
                    .as_bytes()
                    .chunks(name.len())
                    .all(|chunk| chunk.eq_ignore_ascii_case(name.as_bytes())))
    }) {
        return true;
    }
    ir.scopes
        .links
        .iter()
        .any(|link| template_key_matches(ir, &link.pattern, key))
}

fn chained_register_index(name: &str, base: &str) -> Option<usize> {
    if name.len() < base.len() || !name.len().is_multiple_of(base.len()) {
        return None;
    }
    name.as_bytes()
        .chunks(base.len())
        .all(|chunk| chunk.eq_ignore_ascii_case(base.as_bytes()))
        .then_some(name.len() / base.len() - 1)
}

fn set_register(registers: &mut Vec<ScopeValue>, index: usize, value: ScopeValue) {
    if registers.len() <= index {
        registers.resize(index + 1, ScopeValue::Unknown);
    }
    registers[index] = value;
}

fn link_from_matches(
    ir: &RulesIr,
    link: &rules::ir::LinkInfo,
    current: Option<&ScopeValue>,
) -> bool {
    let Some(current) = current else { return true };
    if matches!(current, ScopeValue::Unknown) || link.from.is_empty() {
        return true;
    }
    let ScopeValue::Known(current) = current else {
        return false;
    };
    link.from.iter().any(|from| match from {
        rules::ir::ScopeRef::Any => true,
        rules::ir::ScopeRef::Type(expected) => current.iter().any(|known| {
            ir.strings
                .lookup_folded(known)
                .is_some_and(|actual| ir.scopes_compatible(actual, *expected))
        }),
    })
}
fn instance_name(
    ir: &RulesIr,
    def: &rules::ir::DefSpec,
    property: Option<&HirProperty>,
    path: &LogicalPath,
    props: &[HirProperty],
    siblings: &[usize],
) -> Option<String> {
    let raw = match def.name {
        DefName::Key => property.map(|p| p.key.clone()),
        DefName::Field(field) => siblings.iter().find_map(|i| {
            let p = &props[*i];
            if p.key.eq_ignore_ascii_case(ir.strings().resolve(field)) {
                p.scalar.as_ref().map(|s| s.value.clone())
            } else {
                None
            }
        }),
        DefName::File => path
            .as_str()
            .rsplit('/')
            .next()
            .map(|s| s.rsplit_once('.').map_or(s, |(stem, _)| stem).to_owned()),
    }?;
    let mut name = raw;
    if let Some(prefix) = def.strip_prefix {
        let x = ir.strings().resolve(prefix);
        if name.to_ascii_lowercase().starts_with(x) {
            name.drain(..x.len());
        }
    }
    if let Some(suffix) = def.strip_suffix {
        let x = ir.strings().resolve(suffix);
        if name.to_ascii_lowercase().ends_with(x) {
            name.truncate(name.len().saturating_sub(x.len()));
        }
    }
    Some(name)
}

fn definition_selection(
    ir: &RulesIr,
    def: &rules::ir::DefSpec,
    property: Option<&HirProperty>,
    props: &[HirProperty],
    siblings: &[usize],
    fallback: TextRange,
) -> TextRange {
    match def.name {
        DefName::Key => property.map_or(fallback, |property| property.key_range),
        DefName::Field(field) => siblings
            .iter()
            .find_map(|index| {
                let property = &props[*index];
                property
                    .key
                    .eq_ignore_ascii_case(ir.strings().resolve(field))
                    .then(|| property.scalar.as_ref().map(|scalar| scalar.range))
                    .flatten()
            })
            .unwrap_or(fallback),
        DefName::File => TextRange::new(0, 0).unwrap_or(fallback),
    }
}
#[allow(clippy::too_many_arguments)]
fn push_definition(
    ir: &RulesIr,
    out: &mut IrFacts,
    ty: TypeId,
    name: String,
    range: TextRange,
    selection: TextRange,
    attribute_keys: Vec<String>,
    subtypes: Vec<std::sync::Arc<str>>,
) {
    let kind = ir.strings().resolve(ir.type_info(ty).name).to_owned();
    if out.definitions.iter().any(|definition| {
        definition.name.eq_ignore_ascii_case(&name)
            && definition.kind.eq_ignore_ascii_case(&kind)
            && definition.range == range
    }) {
        return;
    }
    out.definitions.push(HirDefinition {
        kind: kind.clone().into(),
        name: name.clone(),
        range,
        selection_range: selection,
    });
    out.definition_attributes.push(crate::DefinitionAttributes {
        kind: kind.into(),
        name,
        definition_range: range,
        attribute_keys: attribute_keys.into_iter().map(Into::into).collect(),
        subtypes,
    });
}
fn subtype_names(
    ir: &RulesIr,
    ty: TypeId,
    subtype: Option<rules::ir::Symbol>,
) -> Vec<std::sync::Arc<str>> {
    subtype
        .into_iter()
        .filter_map(|symbol| {
            ir.type_info(ty)
                .subtypes
                .iter()
                .find(|candidate| candidate.name == symbol)
                .map(|candidate| ir.strings().resolve(candidate.name).into())
        })
        .collect()
}
fn strip_symbol_prefix(ir: &RulesIr, value: &str, prefix: rules::ir::Symbol) -> String {
    let prefix = ir.strings().resolve(prefix);
    value
        .get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map_or_else(|| value.to_owned(), |_| value[prefix.len()..].to_owned())
}
fn collect_matcher_refs(
    ir: &RulesIr,
    id: MatcherId,
    value: &str,
    range: TextRange,
    facts: Option<&dyn SymbolFacts>,
    out: &mut IrFacts,
) {
    match ir.matcher(id) {
        Matcher::Ref(RefTarget::Type {
            type_id,
            subtype,
            strip_prefix,
        }) => {
            let name = strip_prefix.map_or_else(
                || value.to_owned(),
                |prefix| strip_symbol_prefix(ir, value, prefix),
            );
            out.references.push(HirReference {
                kind: ir.strings().resolve(ir.type_info(*type_id).name).into(),
                name,
                range,
                origin: HirReferenceOrigin::SemanticTyped,
                subtype: subtype.map(|subtype| ir.strings().resolve(subtype).into()),
            });
        }
        Matcher::Ref(RefTarget::Trait(t)) => out.references.push(HirReference {
            kind: ir.strings().resolve(ir.trait_info(*t).name).into(),
            name: value.into(),
            range,
            origin: HirReferenceOrigin::SemanticTyped,
            subtype: None,
        }),
        Matcher::Enum { id, .. } => out.references.push(HirReference {
            kind: ir.strings().resolve(ir.enum_info(*id).name).into(),
            name: value.into(),
            range,
            origin: HirReferenceOrigin::SemanticTyped,
            subtype: None,
        }),
        Matcher::Loc => out.references.push(HirReference {
            kind: "localisation".into(),
            name: value.into(),
            range,
            origin: HirReferenceOrigin::SemanticTyped,
            subtype: None,
        }),
        Matcher::Def { type_id, subtype } => push_definition(
            ir,
            out,
            *type_id,
            value.to_owned(),
            range,
            range,
            Vec::new(),
            subtype_names(ir, *type_id, *subtype),
        ),
        Matcher::Union(items) => {
            let alternatives = items.to_vec();
            let definite = alternatives
                .iter()
                .copied()
                .filter(|item| {
                    definitely_non_reference(ir, *item, value) && !matcher_has_reference(ir, *item)
                })
                .collect::<Vec<_>>();
            if !definite.is_empty() {
                if definite.len() == 1 && matches!(ir.matcher(definite[0]), Matcher::Enum { .. }) {
                    collect_matcher_refs(ir, definite[0], value, range, facts, out);
                }
                return;
            }
            let refs = alternatives
                .iter()
                .copied()
                .filter(|item| matcher_has_reference(ir, *item))
                .collect::<Vec<_>>();
            let matches = refs
                .iter()
                .copied()
                .filter(|item| branch_reference_matches(ir, *item, value, facts, out))
                .collect::<Vec<_>>();
            let selected = if matches.len() == 1 {
                Some(matches[0])
            } else if matches.is_empty() && refs.len() == 1 {
                Some(refs[0])
            } else {
                None
            };
            if let Some(selected) = selected {
                collect_branch_references(ir, selected, value, range, facts, out);
            }
        }
        Matcher::Template(parts) => {
            if let Some(holes) = template_value_holes(ir, parts, value) {
                for (matcher, start, end) in holes {
                    collect_matcher_refs(
                        ir,
                        matcher,
                        &value[start..end],
                        subrange(range, value, start, end),
                        facts,
                        out,
                    );
                }
            }
        }
        _ => {}
    }
}

fn definitely_non_reference(ir: &RulesIr, id: MatcherId, value: &str) -> bool {
    match ir.matcher(id) {
        Matcher::Literal(expected)=>ir.strings().resolve(*expected).eq_ignore_ascii_case(value),
        Matcher::Bool=>value.eq_ignore_ascii_case("yes")||value.eq_ignore_ascii_case("no"),
        Matcher::Int{min,max}=>value.parse::<i64>().is_ok_and(|n|min.is_none_or(|min|n>=min)&&max.is_none_or(|max|n<=max)),
        Matcher::Float{min,max}=>value.parse::<f64>().is_ok_and(|n|min.is_none_or(|min|n>=min)&&max.is_none_or(|max|n<=max)),
        Matcher::Date=>value.split('.').count()==3&&value.split('.').all(|part|part.parse::<u32>().is_ok()),
        Matcher::Enum{id,rows}=>ir.enum_info(*id).rows.iter().enumerate().any(|(i,row)|ir.strings().resolve(row.name).eq_ignore_ascii_case(value)&&rows.as_ref().is_none_or(|set|set.contains(i))),
        Matcher::Scalar | Matcher::Loc => false,
        Matcher::Opaque|Matcher::Path(_)|Matcher::Scope(_)|Matcher::Link|Matcher::Def{..}=>true,
        Matcher::Template(parts)=>template_value_holes(ir,parts,value).is_some()&&!parts.iter().any(|part|matches!(part,rules::ir::TemplatePart::Hole(m) if matcher_has_reference(ir,*m))),
        Matcher::Union(items)=>items.iter().any(|item|definitely_non_reference(ir,*item,value)),
        Matcher::Ref(_)|Matcher::Quoted(_)=>false,
    }
}
fn matcher_has_reference(ir: &RulesIr, id: MatcherId) -> bool {
    match ir.matcher(id) {
        Matcher::Ref(_) | Matcher::Loc => true,
        Matcher::Union(items) => items.iter().any(|item| matcher_has_reference(ir, *item)),
        Matcher::Template(parts) => parts.iter().any(
            |part| matches!(part,rules::ir::TemplatePart::Hole(m) if matcher_has_reference(ir,*m)),
        ),
        _ => false,
    }
}
fn branch_reference_matches(
    ir: &RulesIr,
    id: MatcherId,
    value: &str,
    facts: Option<&dyn SymbolFacts>,
    out: &IrFacts,
) -> bool {
    match ir.matcher(id) {
        Matcher::Ref(_) => reference_matches(ir, id, value, facts, out),
        Matcher::Loc => true,
        Matcher::Template(parts) => template_value_holes(ir, parts, value).is_some_and(|holes| {
            holes.iter().any(|(matcher, start, end)| {
                matcher_has_reference(ir, *matcher)
                    && branch_reference_matches(ir, *matcher, &value[*start..*end], facts, out)
            })
        }),
        Matcher::Union(items) => items
            .iter()
            .any(|item| branch_reference_matches(ir, *item, value, facts, out)),
        _ => false,
    }
}
fn collect_branch_references(
    ir: &RulesIr,
    id: MatcherId,
    value: &str,
    range: TextRange,
    facts: Option<&dyn SymbolFacts>,
    out: &mut IrFacts,
) {
    match ir.matcher(id) {
        Matcher::Ref(_) | Matcher::Loc => collect_matcher_refs(ir, id, value, range, facts, out),
        Matcher::Template(parts) => {
            if let Some(holes) = template_value_holes(ir, parts, value) {
                for (matcher, start, end) in holes {
                    if matcher_has_reference(ir, matcher) {
                        collect_branch_references(
                            ir,
                            matcher,
                            &value[start..end],
                            subrange(range, value, start, end),
                            facts,
                            out,
                        );
                    }
                }
            }
        }
        Matcher::Union(items) => {
            for item in items.iter() {
                collect_branch_references(ir, *item, value, range, facts, out)
            }
        }
        _ => {}
    }
}
fn reference_matches(
    ir: &RulesIr,
    id: MatcherId,
    value: &str,
    facts: Option<&dyn SymbolFacts>,
    out: &IrFacts,
) -> bool {
    let Matcher::Ref(target) = ir.matcher(id) else {
        return false;
    };
    match target {
        RefTarget::Type {
            type_id,
            subtype,
            strip_prefix,
        } => {
            let value = strip_prefix.map_or_else(
                || value.to_owned(),
                |prefix| strip_symbol_prefix(ir, value, prefix),
            );
            if out.definitions.iter().any(|def| {
                def.kind
                    .eq_ignore_ascii_case(ir.strings().resolve(ir.type_info(*type_id).name))
                    && def.name.eq_ignore_ascii_case(&value)
            }) {
                return subtype.is_none_or(|required| {
                    out.definition_attributes.iter().any(|attrs| {
                        attrs.name.eq_ignore_ascii_case(&value)
                            && attrs
                                .subtypes
                                .iter()
                                .any(|st| st.eq_ignore_ascii_case(ir.strings().resolve(required)))
                    })
                });
            }
            facts.is_some_and(|facts| match subtype {
                Some(subtype) => facts.type_subtype_member(*type_id, *subtype, &value),
                None => facts.type_member(*type_id, &value),
            })
        }
        RefTarget::Trait(trait_id) => {
            out.definitions
                .iter()
                .any(|def| def.name.eq_ignore_ascii_case(value))
                || facts.is_some_and(|facts| facts.trait_impl_member(*trait_id, value))
        }
    }
}
fn template_value_holes(
    ir: &RulesIr,
    parts: &[rules::ir::TemplatePart],
    value: &str,
) -> Option<Vec<(MatcherId, usize, usize)>> {
    let mut cursor = 0;
    let mut holes = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        match part {
            rules::ir::TemplatePart::Text(text) => {
                let literal = ir.strings().resolve(*text);
                let tail = value.get(cursor..)?;
                let offset = tail
                    .to_ascii_lowercase()
                    .find(&literal.to_ascii_lowercase())?;
                if index == 0 && offset != 0 {
                    return None;
                }
                cursor += offset + literal.len();
            }
            rules::ir::TemplatePart::Hole(matcher) => {
                let start = cursor;
                let end = if let Some(rules::ir::TemplatePart::Text(next)) = parts.get(index + 1) {
                    let needle = ir.strings().resolve(*next);
                    let offset = value
                        .get(cursor..)?
                        .to_ascii_lowercase()
                        .find(&needle.to_ascii_lowercase())?;
                    cursor + offset
                } else {
                    value.len()
                };
                holes.push((*matcher, start, end));
                cursor = end;
            }
        }
    }
    (cursor == value.len()).then_some(holes)
}
fn subrange(range: TextRange, value: &str, from: usize, to: usize) -> TextRange {
    if usize::try_from(range.len()).ok() == Some(value.len()) {
        let Some(start) = u32::try_from(from)
            .ok()
            .and_then(|offset| range.start().checked_add(offset))
        else {
            return range;
        };
        let Some(end) = u32::try_from(to)
            .ok()
            .and_then(|offset| range.start().checked_add(offset))
        else {
            return range;
        };
        TextRange::new(start, end).unwrap_or(range)
    } else {
        range
    }
}
fn add_bindings(
    ir: &RulesIr,
    ty: TypeId,
    subtypes: &SubtypeSet,
    name: &str,
    range: TextRange,
    out: &mut IrFacts,
) {
    let info = ir.type_info(ty);
    let mut traits = info
        .trait_impls
        .iter()
        .map(|x| x.trait_id)
        .collect::<Vec<_>>();
    for subtype in info
        .subtypes
        .iter()
        .filter(|subtype| subtypes.contains(ty, subtype.name))
    {
        traits.extend(subtype.trait_impls.iter().map(|x| x.trait_id));
    }
    for id in traits {
        for (_, b) in ir.trait_info(id).bindings.iter() {
            for (kind, template, required) in [
                ("localisation", b.loc, b.required),
                ("sprite", b.sprite, b.required),
            ] {
                let Some(template) = template else { continue };
                let reference = HirReference {
                    kind: kind.into(),
                    name: ir.strings().resolve(template).replace('$', name),
                    range,
                    origin: if kind == "sprite" {
                        HirReferenceOrigin::DerivedSprite
                    } else {
                        HirReferenceOrigin::DerivedLocalisation
                    },
                    subtype: None,
                };
                out.binding_references.push(reference.clone());
                if required {
                    out.references.push(reference)
                }
            }
        }
    }
    for implementation in info.trait_impls.iter().chain(
        info.subtypes
            .iter()
            .filter(|item| subtypes.contains(ty, item.name))
            .flat_map(|item| item.trait_impls.iter()),
    ) {
        for (_, argument) in &implementation.arguments {
            if let rules::ir::TraitArgument::Binding(binding) = argument {
                for (kind, template, required) in [
                    ("localisation", binding.loc, binding.required),
                    ("sprite", binding.sprite, binding.required),
                ] {
                    let Some(template) = template else { continue };
                    let reference = HirReference {
                        kind: kind.into(),
                        name: ir.strings().resolve(template).replace('$', name),
                        range,
                        origin: if kind == "sprite" {
                            HirReferenceOrigin::DerivedSprite
                        } else {
                            HirReferenceOrigin::DerivedLocalisation
                        },
                        subtype: None,
                    };
                    out.binding_references.push(reference.clone());
                    if required {
                        out.references.push(reference);
                    }
                }
            }
        }
    }
}
