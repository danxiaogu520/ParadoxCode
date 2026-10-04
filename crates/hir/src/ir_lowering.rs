//! HIR facts lowered from the compiled Rules IR.
use parser::ParsedFile;
use rules::ir::{
    DefName, FieldId, Matcher, MatcherId, RefTarget, RootRule, RulesIr, SchemaId, Shape,
    SubtypeSet, SymbolFacts, TypeId,
};
use text::{LogicalPath, TextRange};

use crate::{
    FieldFact, HirDefinition, HirProperty, HirReference, HirReferenceOrigin, HirScalar, SchemaFact,
    ScopeFact, ScopeState, ScopeValue,
};

#[derive(Clone)]
pub(super) struct IrFacts {
    pub analysis_coverage: crate::analysis::AnalysisCoverage,
    pub pattern_ambiguous: std::cell::Cell<bool>,
    pub pattern_search_incomplete: std::cell::Cell<bool>,
    pub overload_facts: Vec<crate::block_checking::OverloadFact>,
    pub unknown_ranges: Vec<TextRange>,
    pub retain_validation_facts: bool,
    pub symbol_facts_dependency: std::cell::Cell<bool>,
    pub schema_facts: Vec<SchemaFact>,
    pub field_facts: Vec<FieldFact>,
    pub scope_facts: Vec<ScopeFact>,
    pub definitions: Vec<HirDefinition>,
    pub references: Vec<HirReference>,
    pub binding_references: Vec<HirReference>,
    pub definition_attributes: Vec<crate::DefinitionAttributes>,
    pub template_kinds: std::collections::BTreeSet<String>,
    pub runtime_parameter_guards: Vec<(TextRange, Option<TextRange>)>,
}
struct FactsRef<'a>(Option<&'a dyn SymbolFacts>, &'a std::cell::Cell<bool>);
impl SymbolFacts for FactsRef<'_> {
    fn facts_complete(&self) -> bool {
        self.0.is_none_or(SymbolFacts::facts_complete)
    }
    fn type_member(&self, ty: TypeId, name: &str) -> bool {
        self.1.set(true);
        self.0.is_some_and(|facts| facts.type_member(ty, name))
    }

    fn type_subtype_member(&self, ty: TypeId, subtype: rules::ir::Symbol, name: &str) -> bool {
        self.1.set(true);
        self.0
            .is_some_and(|facts| facts.type_subtype_member(ty, subtype, name))
    }
}

pub(super) fn lower(
    ir: &RulesIr,
    path: &LogicalPath,
    props: &[HirProperty],
    bare_values: &[HirScalar],
    syntax: &ParsedFile,
    facts: Option<&dyn SymbolFacts>,
    retain_validation_facts: bool,
) -> IrFacts {
    let children = crate::scope::property_children(props);
    let mut out = IrFacts {
        analysis_coverage: Default::default(),
        pattern_search_incomplete: std::cell::Cell::new(false),
        pattern_ambiguous: std::cell::Cell::new(false),
        overload_facts: Vec::new(),
        unknown_ranges: Vec::new(),
        retain_validation_facts,
        symbol_facts_dependency: std::cell::Cell::new(false),
        schema_facts: vec![],
        field_facts: vec![],
        scope_facts: vec![],
        definitions: vec![],
        references: vec![],
        binding_references: vec![],
        definition_attributes: vec![],
        template_kinds: template_kinds(ir),
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
        RootRule::Instance { def, body, scope } => {
            let indices = props
                .iter()
                .enumerate()
                .filter_map(|(i, p)| p.top_level.then_some(i))
                .collect::<Vec<_>>();
            let mut body_subtypes = SubtypeSet::default();
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
                add_bindings(ir, def.type_id, &name, selection, &mut out);
            }
            let state = transition_state(ir, unknown.clone(), scope.as_ref(), "");
            body.map(|schema| (schema, indices, body_subtypes, state))
                .into_iter()
                .collect()
        }
        RootRule::Opaque => Vec::new(),
    };
    let mut seen_defs = std::collections::BTreeSet::new();
    for (schema, indices, subtypes, state) in roots {
        if out.retain_validation_facts {
            out.schema_facts.push(SchemaFact {
                range: syntax.root().range(),
                schema,
                subtypes: subtypes.clone(),
                state: state.clone(),
            });
        }
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
    merge_attribute_summaries(&mut out.definition_attributes);
    out.schema_facts
        .sort_by_key(|f| (f.range.start(), f.range.end()));
    out.field_facts.sort_by_key(|f| f.range);
    out.scope_facts.sort_by_key(|f| f.range);
    out.definitions.sort_by_key(|d| d.selection_range.start());
    out.references.sort_by_key(|r| r.range.start());
    out.binding_references.sort_by_key(|r| r.range.start());
    if out.pattern_ambiguous.get() {
        out.analysis_coverage
            .residuals
            .insert(crate::analysis::ResidualReason::Interpretation);
    }
    if out.pattern_search_incomplete.get() {
        out.analysis_coverage
            .limits
            .insert(crate::analysis::AnalysisLimit::PatternSearch);
    }
    out
}

fn merge_attribute_summaries(attributes: &mut Vec<crate::DefinitionAttributes>) {
    let mut sites = std::collections::BTreeMap::new();
    let mut merged: Vec<crate::DefinitionAttributes> = Vec::new();
    for summary in std::mem::take(attributes) {
        let site = (
            summary.kind.to_ascii_lowercase(),
            summary.name.to_ascii_lowercase(),
            summary.definition_range,
        );
        if let Some(index) = sites.get(&site).copied() {
            let retained: &mut crate::DefinitionAttributes = &mut merged[index];
            for key in summary.attribute_keys {
                if !retained
                    .attribute_keys
                    .iter()
                    .any(|existing| existing.eq_ignore_ascii_case(&key))
                {
                    retained.attribute_keys.push(key);
                }
            }
            for subtype in summary.subtypes {
                if !retained
                    .subtypes
                    .iter()
                    .any(|existing| existing.eq_ignore_ascii_case(&subtype))
                {
                    retained.subtypes.push(subtype);
                }
            }
        } else {
            sites.insert(site, merged.len());
            merged.push(summary);
        }
    }
    *attributes = merged;
}

fn template_kinds(ir: &RulesIr) -> std::collections::BTreeSet<String> {
    let Some(template) = ir.trait_by_name("Template") else {
        return Default::default();
    };
    ir.types
        .iter()
        .filter(|ty| {
            ty.trait_impls
                .iter()
                .any(|implementation| implementation.trait_id == template)
        })
        .map(|ty| ir.strings().resolve(ty.name).to_ascii_lowercase())
        .collect()
}

fn subtype_names_from_set(ir: &RulesIr, ty: TypeId, set: &SubtypeSet) -> Vec<std::sync::Arc<str>> {
    ir.type_info(ty)
        .subtypes
        .iter()
        .filter(|subtype| set.contains(ty, subtype.name))
        .map(|subtype| ir.strings().resolve(subtype.name).into())
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn lower_schema_fragment(
    ir: &RulesIr,
    syntax: &ParsedFile,
    schema: SchemaId,
    subtypes: SubtypeSet,
    state: ScopeState,
    facts: &dyn SymbolFacts,
    root_range: Option<TextRange>,
    unknown_ranges: &[TextRange],
) -> IrFacts {
    let collected = crate::collector::collect(syntax);
    let props = collected.properties;
    let bare_values = collected.bare_values;
    let children = crate::scope::property_children(&props);
    let root_range = root_range.unwrap_or(syntax.root().range());
    let mut out = IrFacts {
        analysis_coverage: Default::default(),
        pattern_search_incomplete: std::cell::Cell::new(false),
        pattern_ambiguous: std::cell::Cell::new(false),
        overload_facts: Vec::new(),
        unknown_ranges: unknown_ranges.to_vec(),
        retain_validation_facts: true,
        symbol_facts_dependency: std::cell::Cell::new(false),
        schema_facts: vec![SchemaFact {
            range: root_range,
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
        template_kinds: template_kinds(ir),
        runtime_parameter_guards: Vec::new(),
    };
    let path = LogicalPath::parse("").expect("empty fragment path");
    let included = props
        .iter()
        .enumerate()
        .filter(|(_, property)| {
            root_range.start() <= property.range.start() && property.range.end() <= root_range.end()
        })
        .map(|(index, _)| index)
        .collect::<std::collections::BTreeSet<_>>();
    let nested = included
        .iter()
        .flat_map(|index| children[*index].iter().copied())
        .collect::<std::collections::BTreeSet<_>>();
    descend(
        ir,
        &path,
        &props,
        &bare_values,
        &children,
        included.difference(&nested).copied().collect(),
        schema,
        subtypes,
        state,
        Some(facts),
        syntax,
        &mut out,
        &mut std::collections::BTreeSet::new(),
        root_range,
        None,
    );
    merge_attribute_summaries(&mut out.definition_attributes);
    out.schema_facts
        .sort_by_key(|fact| (fact.range.start(), fact.range.end()));
    out.field_facts.sort_by_key(|fact| fact.range);
    out.scope_facts.sort_by_key(|fact| fact.range);
    out.definitions
        .sort_by_key(|def| def.selection_range.start());
    if out.pattern_ambiguous.get() {
        out.analysis_coverage
            .residuals
            .insert(crate::analysis::ResidualReason::Interpretation);
    }
    if out.pattern_search_incomplete.get() {
        out.analysis_coverage
            .limits
            .insert(crate::analysis::AnalysisLimit::PatternSearch);
    }
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
        // Only direct list members use this matcher. Nested properties own
        // their values, even when they also contain unkeyed list elements.
        for item in bare_values.iter().filter(|item| {
            item.range.start() >= container_range.start()
                && item.range.end() <= container_range.end()
                && !indices.iter().any(|index| {
                    let child = &props[*index];
                    child.range.start() <= item.range.start()
                        && item.range.end() <= child.range.end()
                })
        }) {
            collect_matcher_refs(ir, matcher, &item.value, item.range, facts, out);
        }
    }
    for index in indices {
        let p = &props[index];
        let shape = if p.scalar.is_some() {
            Shape::Scalar
        } else {
            Shape::Block
        };
        let candidates = ir.lookup(schema, &p.key, shape).collect::<Vec<_>>();
        let exact_candidates = candidates
            .iter()
            .copied()
            .filter(|id| is_exact_key_match(ir, schema, *id, &p.key))
            .collect::<Vec<_>>();
        let candidates = if exact_candidates.is_empty() {
            candidates
                .into_iter()
                .filter(|id| matcher_accepts_key(ir, ir.field(*id).key, &p.key, facts, out))
                .collect()
        } else {
            // An exact field shadows patterns. Avoid asking workspace symbol
            // facts for every pattern when none of them can be selected.
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
        if let Some(scalar) = &p.scalar {
            for id in &candidates {
                if let rules::ir::FieldValue::Scalar(matcher) = ir.field(*id).value
                    && matcher_has_pattern(ir, matcher)
                    && crate::checking::scalar_outcome(
                        ir,
                        matcher,
                        &scalar.value,
                        &FactsRef(facts, &out.symbol_facts_dependency),
                    )
                    .1
                    .is_some()
                {
                    out.pattern_search_incomplete.set(true);
                }
            }
        }
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
                            || definitely_non_reference(ir, matcher, &scalar.value)
                            || ir.scalar_matches(
                                matcher,
                                &scalar.value,
                                &FactsRef(facts, &out.symbol_facts_dependency),
                            ))
                })
            })
            .or_else(|| candidates.first().copied());
        // A runtime target or preview can leave the current scope unknown. Keep
        // scalar overloads until the IDE can also check the value's registers;
        // a stateless ROOT match cannot choose country vs province here.
        let uncertain_scope = state.current.first().is_none_or(|scope| match scope {
            ScopeValue::Unknown => true,
            ScopeValue::Known(names) => names.len() != 1 || names[0].eq_ignore_ascii_case("any"),
            ScopeValue::Invalid => false,
        });
        let mut candidates = if (p.scalar.is_some() && uncertain_scope)
            || (p.scalar.is_none() && candidates.len() > 1)
        {
            candidates
        } else {
            selected.into_iter().collect::<Vec<_>>()
        };
        let mut fork = false;
        if p.scalar.is_none() && candidates.len() > 1 {
            let template_text = out.definitions.iter().any(|definition| {
                out.template_kinds
                    .contains(&definition.kind.to_ascii_lowercase())
                    && definition.range.start() <= p.range.start()
                    && p.range.end() <= definition.range.end()
            });
            let checked = crate::block_checking::select_overloads::<std::convert::Infallible>(
                ir,
                props,
                children,
                index,
                schema,
                &state,
                &candidates,
                &FactsRef(facts, &out.symbol_facts_dependency),
                &out.unknown_ranges,
                template_text,
                &mut || Ok(()),
            )
            .expect("infallible checkpoint");
            out.analysis_coverage.merge(&checked.coverage);
            candidates = checked.value.fields.clone();
            fork = checked.value.validation != crate::analysis::Validation::Valid;
            if out.retain_validation_facts {
                out.overload_facts.push(checked.value);
            }
        }
        // Scope alternatives remain in FieldFact for diagnostics. Symbol
        // collection must still respect each alternative's value domain: a
        // religion accepted by one overload is not an unresolved country tag
        // merely because another overload uses that namespace.
        let scalar_symbol_fields = p.scalar.as_ref().map(|scalar| {
            let matching = candidates
                .iter()
                .copied()
                .filter(|id| match ir.field(*id).value {
                    rules::ir::FieldValue::Scalar(matcher) => {
                        reference_matches(ir, matcher, &scalar.value, facts, out)
                            || definitely_non_reference(ir, matcher, &scalar.value)
                            || ir.scalar_matches(
                                matcher,
                                &scalar.value,
                                &FactsRef(facts, &out.symbol_facts_dependency),
                            )
                    }
                    _ => false,
                })
                .collect::<Vec<_>>();
            if matching.is_empty() {
                candidates.clone()
            } else {
                matching
            }
        });
        if candidates.is_empty()
            && shape == Shape::Block
            && !crate::parameters::delimited_parameters(&p.key, p.key_range, '$').is_empty()
            && out.definitions.iter().any(|definition| {
                out.template_kinds
                    .contains(&definition.kind.to_ascii_lowercase())
                    && definition.range.start() <= p.range.start()
                    && p.range.end() <= definition.range.end()
            })
        {
            // A replacement can supply a scope key. Retain symbols reachable through the
            // declared link body with unknown current scope; this is not a concrete field
            // selection, so defer structural/scope facts until invocation bindings exist.
            let schemas = ir
                .schema(schema)
                .patterns
                .iter()
                .filter_map(|field| {
                    matches!(ir.matcher(ir.field(*field).key), Matcher::Link)
                        .then(|| ir.child(*field, schema))
                        .flatten()
                })
                .collect::<std::collections::BTreeSet<_>>();
            for child_schema in schemas {
                let schema_start = out.schema_facts.len();
                let field_start = out.field_facts.len();
                let scope_start = out.scope_facts.len();
                let mut child_state = state.clone();
                child_state.current = vec![ScopeValue::Unknown];
                descend(
                    ir,
                    path,
                    props,
                    bare_values,
                    children,
                    children[index].clone(),
                    child_schema,
                    subtypes.clone(),
                    child_state,
                    facts,
                    source,
                    out,
                    seen,
                    p.value_range.unwrap_or(p.range),
                    None,
                );
                out.schema_facts.truncate(schema_start);
                out.field_facts.truncate(field_start);
                out.scope_facts.truncate(scope_start);
            }
        }
        if out.retain_validation_facts {
            out.field_facts.push(FieldFact {
                range: p.key_range,
                schema,
                fields: candidates.clone(),
                subtypes: subtypes.clone(),
            });
        }
        if out.retain_validation_facts {
            out.scope_facts.push(ScopeFact {
                range: p.key_range,
                context: ir.strings().resolve(ir.schema(schema).name).to_owned(),
                parent_path: Vec::new(),
                state: state.clone(),
                transition: candidates.first().map(|id| {
                    transition_state(ir, state.clone(), ir.field(*id).scope.as_ref(), &p.key)
                }),
            });
        }
        if fork {
            let base = out.clone();
            let mut branches = Vec::new();
            for id in candidates {
                let mut branch = base.clone();
                let mut branch_seen = seen.clone();
                lower_field_candidate(
                    ir,
                    path,
                    props,
                    bare_values,
                    children,
                    index,
                    schema,
                    &subtypes,
                    &state,
                    facts,
                    source,
                    &mut branch,
                    &mut branch_seen,
                    branch_self_schema,
                    scalar_symbol_fields.as_ref(),
                    id,
                );
                branches.push(branch);
            }
            if let Some(first) = branches.first() {
                macro_rules! common {
                    ($field:ident) => {
                        out.$field = first
                            .$field
                            .iter()
                            .filter(|item| {
                                branches.iter().all(|branch| branch.$field.contains(item))
                            })
                            .cloned()
                            .collect();
                    };
                }
                common!(field_facts);
                common!(scope_facts);
                common!(definitions);
                common!(references);
                common!(binding_references);
                common!(definition_attributes);
                common!(runtime_parameter_guards);
                common!(overload_facts);
                out.schema_facts = branches
                    .iter()
                    .flat_map(|branch| branch.schema_facts.iter().cloned())
                    .collect();
                out.schema_facts
                    .sort_by_key(|fact| (fact.range, fact.schema.index()));
                out.schema_facts.dedup();
                for branch in &branches {
                    out.analysis_coverage.merge(&branch.analysis_coverage);
                }
                out.symbol_facts_dependency.set(
                    branches
                        .iter()
                        .any(|branch| branch.symbol_facts_dependency.get()),
                );
            }
        } else {
            for id in candidates {
                lower_field_candidate(
                    ir,
                    path,
                    props,
                    bare_values,
                    children,
                    index,
                    schema,
                    &subtypes,
                    &state,
                    facts,
                    source,
                    out,
                    seen,
                    branch_self_schema,
                    scalar_symbol_fields.as_ref(),
                    id,
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn lower_field_candidate(
    ir: &RulesIr,
    path: &LogicalPath,
    props: &[HirProperty],
    bare_values: &[crate::HirScalar],
    children: &[Vec<usize>],
    index: usize,
    schema: SchemaId,
    subtypes: &SubtypeSet,
    state: &ScopeState,
    facts: Option<&dyn SymbolFacts>,
    source: &ParsedFile,
    out: &mut IrFacts,
    seen: &mut std::collections::BTreeSet<(u32, u32)>,
    branch_self_schema: Option<SchemaId>,
    scalar_symbol_fields: Option<&Vec<FieldId>>,
    id: FieldId,
) {
    let p = &props[index];
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
    let mut candidate_subtypes = SubtypeSet::default();
    if !key_def_is_field_def && let Matcher::Def { type_id, subtype } = ir.matcher(field.key) {
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
        add_bindings(ir, *type_id, &name, p.key_range, out);
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
        add_bindings(ir, *type_id, &scalar.value, scalar.range, out);
    }
    if let Some(def) = &field.def {
        if let Some(subtype) = def.subtype {
            candidate_subtypes.insert(def.type_id, subtype);
        }
        if let Some(name) = instance_name(ir, def, Some(p), path, props, &children[index]) {
            let selection =
                definition_selection(ir, def, Some(p), props, &children[index], p.key_range);
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
            add_bindings(ir, def.type_id, &name, selection, out);
        }
    }
    if let rules::ir::FieldValue::Scalar(matcher) = field.value
        && let Some(s) = &p.scalar
    {
        // Pure scalar definitions already have the property's range,
        // attributes and bindings above. Collecting them again with
        // the token range would create a second definition.
        if !matches!(ir.matcher(matcher), Matcher::Def { .. })
            && scalar_symbol_fields
                .as_ref()
                .is_none_or(|fields| fields.contains(&id))
        {
            collect_matcher_refs(ir, matcher, &s.value, s.range, facts, out);
        }
    }
    if p.scalar.is_none()
        && let Some(kind) = crate::template::template_kind(ir, field.key)
    {
        // The first pass has no workspace facts yet. Missing template
        // templates must still schedule a replay once definitions exist.
        out.symbol_facts_dependency.set(true);
        if let Some(facts) = facts {
            lower_template_arguments(
                ir,
                source,
                path,
                props,
                &children[index],
                &kind,
                &p.key,
                state,
                facts,
                out,
                seen,
            );
        }
    }
    if let Some(child_schema) = if matches!(field.value, rules::ir::FieldValue::SelfBlock) {
        Some(branch_self_schema.unwrap_or(schema))
    } else {
        ir.child(id, schema)
    } {
        if p.scalar.as_ref().is_some_and(|scalar| scalar.quoted) {
            return;
        }
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
            SubtypeSet::default()
        } else {
            subtypes.clone()
        };
        if let Some((type_id, Some(subtype))) = instance_def {
            child_subtypes.insert(type_id, subtype);
        }
        let child_state = transition_state(ir, state.clone(), field.scope.as_ref(), &p.key);
        if let Some(range) = p.value_range
            && out.retain_validation_facts
        {
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

#[allow(clippy::too_many_arguments)]
fn lower_template_arguments(
    ir: &RulesIr,
    source: &ParsedFile,
    _path: &LogicalPath,
    props: &[HirProperty],
    children: &[usize],
    kind: &str,
    name: &str,
    state: &ScopeState,
    facts: &dyn SymbolFacts,
    out: &mut IrFacts,
    _seen: &mut std::collections::BTreeSet<(u32, u32)>,
) {
    // Unknown definition-side bindings remain holes. Concrete siblings still
    // contribute independently provable references.
    let symbolic_context = out.definitions.iter().any(|definition| {
        out.template_kinds
            .contains(&definition.kind.to_ascii_lowercase())
            && children.iter().any(|index| {
                definition.range.start() <= props[*index].range.start()
                    && props[*index].range.end() <= definition.range.end()
            })
    });
    let Some(ty) = ir.type_by_name(kind) else {
        return;
    };
    let Some(template) = facts.template(ty, name) else {
        out.analysis_coverage
            .limits
            .insert(crate::analysis::AnalysisLimit::UnavailableTemplate);
        return;
    };
    let Some(schema) = crate::template::template_body(ir, ty) else {
        return;
    };
    let mut values = std::collections::BTreeMap::new();
    let mut raw = std::collections::BTreeMap::new();
    let mut present = std::collections::BTreeSet::new();
    for index in children {
        let property = &props[*index];
        let parameter = property.key.to_ascii_lowercase();
        present.insert(parameter.clone());
        if let Some(scalar) = &property.scalar {
            if symbolic_context && scalar.value.contains('$') {
                continue;
            }
            values.insert(
                parameter.clone(),
                crate::template::binding_value(source, scalar),
            );
            if let Some(text) = source.text(scalar.range) {
                raw.insert(parameter, text.to_owned());
            }
        }
    }
    let body = crate::template_instance::instantiate::<std::convert::Infallible>(
        ir,
        facts,
        template,
        &values,
        &raw,
        &present,
        schema,
        state.clone(),
        &[],
        crate::template_instance::InstanceGoal::Facts,
        &mut || Ok(()),
    )
    .expect("infallible checkpoint");
    let mut coverage = body.coverage.clone();
    if symbolic_context {
        // Definition-side parameters are deferred until an invocation supplies
        // them. Their expected holes do not make the declaration's lexical
        // references incomplete. Actual calls and resource/source frontiers
        // retain their coverage below.
        coverage
            .residuals
            .remove(&crate::analysis::ResidualReason::Binding);
    }
    out.analysis_coverage.merge(&coverage);
    let project = |range| {
        crate::template_instance::project_source_range(
            &body.rendered,
            range,
            source,
            children.iter().map(|index| &props[*index]),
        )
    };
    for call in &body.rendered.calls {
        let Some((parameter, relative)) = &call.source else {
            out.analysis_coverage
                .limits
                .insert(crate::analysis::AnalysisLimit::Output);
            continue;
        };
        let Some(range) = crate::template_instance::project_binding_range(
            parameter,
            *relative,
            source,
            children.iter().map(|index| &props[*index]),
        ) else {
            out.analysis_coverage
                .limits
                .insert(crate::analysis::AnalysisLimit::Output);
            continue;
        };
        out.references.push(HirReference {
            kind: call.kind.clone().into(),
            name: call.name.clone(),
            range,
            origin: crate::HirReferenceOrigin::DynamicDefinition,
            subtype: None,
        });
    }
    let definition_start = out.definitions.len();
    let reference_start = out.references.len();
    let binding_start = out.binding_references.len();
    let mut origins = std::collections::BTreeMap::new();
    for definition in body.hir.definitions() {
        if body
            .rendered
            .dependencies(definition.selection_range)
            .is_empty()
        {
            continue;
        }
        if body.rendered.has_hole(definition.selection_range)
            || body
                .rendered
                .has_unresolved_structure(definition.selection_range)
        {
            continue;
        }
        let selection = match project(definition.selection_range) {
            Some(range) => range,
            None => {
                // Keep a generated identity for fact discovery, but an empty
                // selection cannot become a rename edit of an entire argument.
                out.analysis_coverage
                    .limits
                    .insert(crate::analysis::AnalysisLimit::Output);
                let dependencies = body.rendered.dependencies(definition.selection_range);
                let Some(scalar) = children
                    .iter()
                    .filter_map(|index| {
                        let arg = &props[*index];
                        dependencies
                            .contains(&arg.key.to_ascii_lowercase())
                            .then_some(arg.scalar.as_ref())
                            .flatten()
                    })
                    .next()
                else {
                    continue;
                };
                TextRange::empty(scalar.range.start())
            }
        };
        let range = project(definition.range).unwrap_or(selection);
        origins.insert(definition.range, range);
        let mut projected = definition.clone();
        projected.range = range;
        projected.selection_range = selection;
        out.definitions.push(projected);
    }
    for attributes in body.hir.definition_attributes() {
        if let Some(range) = origins.get(&attributes.definition_range) {
            let mut attrs = attributes.clone();
            attrs.definition_range = *range;
            out.definition_attributes.push(attrs);
        }
    }
    for (refs, destination) in [
        (body.hir.references(), &mut out.references),
        (
            body.hir.binding_references_for_hover(),
            &mut out.binding_references,
        ),
    ] {
        for reference in refs {
            if body.rendered.dependencies(reference.range).is_empty() {
                continue;
            }
            let Some(range) = project(reference.range) else {
                out.analysis_coverage
                    .limits
                    .insert(crate::analysis::AnalysisLimit::Output);
                continue;
            };
            let mut reference = reference.clone();
            reference.range = range;
            destination.push(reference);
        }
    }
    let mut keys = std::collections::BTreeSet::new();
    let mut i = 0;
    out.definitions.retain(|definition| {
        let old = i < definition_start;
        i += 1;
        old || keys.insert((
            definition.kind.clone(),
            definition.name.clone(),
            definition.range,
            definition.selection_range,
        ))
    });
    dedup_template_references(&mut out.references, reference_start);
    dedup_template_references(&mut out.binding_references, binding_start);
}

fn dedup_template_references(references: &mut Vec<HirReference>, previous: usize) {
    let mut keys = std::collections::BTreeSet::new();
    let mut i = 0;
    references.retain(|reference| {
        let old = i < previous;
        i += 1;
        old || keys.insert((
            reference.kind.clone(),
            reference.name.clone(),
            reference.range,
        ))
    });
}

pub(super) fn transition_state(
    ir: &RulesIr,
    mut state: ScopeState,
    effect: Option<&rules::ir::ScopeEffect>,
    key: &str,
) -> ScopeState {
    if key.contains('.') && link_or_register_matches(ir, key) {
        for segment in key.split('.') {
            state = transition_state(ir, state, None, segment);
        }
    }
    if let Some((register, _)) = ir.scopes.register(ir.strings(), key) {
        let value = crate::ir_scope_register_value(ir, &state, key)
            .cloned()
            .unwrap_or(ScopeValue::Unknown);
        if register.role != rules::source::RegisterRole::Current {
            state.previous.insert(
                0,
                state
                    .current
                    .first()
                    .cloned()
                    .unwrap_or(ScopeValue::Unknown),
            );
            state.current.insert(0, value);
        }
    }
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
            if let Some((register, depth)) = ir.scopes.register(ir.strings(), name) {
                match register.role {
                    rules::source::RegisterRole::Root => state.root = value,
                    rules::source::RegisterRole::Current => {
                        set_register(&mut state.current, 0, value)
                    }
                    rules::source::RegisterRole::Previous => {
                        set_register(&mut state.previous, depth - 1, value)
                    }
                    rules::source::RegisterRole::From => {
                        set_register(&mut state.from, depth - 1, value)
                    }
                }
            }
        }
    }
    state
}

pub(super) fn scope_value_allows(
    ir: &RulesIr,
    current: &ScopeValue,
    expected: &[rules::ir::Symbol],
) -> bool {
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
        Matcher::Enum { id } => ir
            .enum_info(*id)
            .rows
            .iter()
            .any(|row| ir.strings().resolve(row.name).eq_ignore_ascii_case(key)),
        Matcher::Int { min, max } => key.parse::<i64>().is_ok_and(|value| {
            min.is_none_or(|min| value >= min) && max.is_none_or(|max| value <= max)
        }),
        Matcher::Date => {
            key.split('.').count() == 3 && key.split('.').all(|part| part.parse::<u32>().is_ok())
        }
        Matcher::Pattern(parts) => {
            let mut no_cancel = || false;
            let mut budget = rules::pattern::SearchBudget::new(Default::default(), &mut no_cancel);
            let result =
                rules::pattern::search(ir, parts, key, &mut budget, &mut |matcher, text| {
                    Some(matcher_accepts_key(ir, matcher, text, facts, out))
                });
            if budget.limit.is_some() {
                out.pattern_search_incomplete.set(true);
            }
            result.matched != Some(false)
        }
        Matcher::Union(items) => items
            .iter()
            .any(|item| matcher_accepts_key(ir, *item, key, facts, out)),
        Matcher::Ref(RefTarget::Type {
            type_id,
            subtype,
            strip_prefix,
        }) => {
            let Some(key) = reference_symbol_name(ir, key, *strip_prefix) else {
                return false;
            };
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
            out.symbol_facts_dependency.set(true);
            facts.is_some_and(|facts| match subtype {
                Some(subtype) => facts.type_subtype_member(*type_id, *subtype, &key),
                None => facts.type_member(*type_id, &key),
            })
        }

        Matcher::Scope(scope) => ir.scope_matches(*scope, key),
        Matcher::Link => link_or_register_matches(ir, key),
        Matcher::Opaque
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
fn template_key_matches(ir: &RulesIr, parts: &[rules::ir::PatternPart], key: &str) -> bool {
    if let [rules::ir::PatternPart::Text(text)] = parts {
        return key.eq_ignore_ascii_case(ir.strings().resolve(*text));
    }
    let mut cursor = 0;
    for (index, part) in parts.iter().enumerate() {
        match part {
            rules::ir::PatternPart::Text(text) => {
                let literal = ir.strings().resolve(*text);
                let offset = if index == 0 {
                    if !key[cursor..]
                        .get(..literal.len())
                        .is_some_and(|head| head.eq_ignore_ascii_case(literal))
                    {
                        return false;
                    }
                    0
                } else {
                    let Some(offset) = find_ascii_case_insensitive(&key[cursor..], literal) else {
                        return false;
                    };
                    offset
                };
                cursor += offset + literal.len();
            }
            rules::ir::PatternPart::Hole(_) => {
                if index + 1 == parts.len() {
                    cursor = key.len();
                } else if let rules::ir::PatternPart::Text(next) = parts[index + 1] {
                    let needle = ir.strings().resolve(next);
                    let Some(offset) = find_ascii_case_insensitive(&key[cursor..], needle) else {
                        return false;
                    };
                    cursor += offset;
                }
            }
        }
    }
    cursor == key.len()
}

fn find_ascii_case_insensitive(text: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    text.as_bytes()
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

pub(super) fn link_or_register_matches(ir: &RulesIr, key: &str) -> bool {
    if key.contains('.') {
        return key
            .split('.')
            .all(|segment| !segment.is_empty() && link_or_register_matches(ir, segment));
    }
    if ir.scopes.register(ir.strings(), key).is_some() {
        return true;
    }
    ir.scopes
        .links
        .iter()
        .any(|link| template_key_matches(ir, &link.pattern, key))
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
        DefName::Key => property.map_or(fallback, |property| {
            definition_name_selection(ir, def, &property.key, property.key_range)
        }),
        DefName::Field(field) => siblings
            .iter()
            .find_map(|index| {
                let property = &props[*index];
                property
                    .key
                    .eq_ignore_ascii_case(ir.strings().resolve(field))
                    .then(|| {
                        property.scalar.as_ref().map(|scalar| {
                            definition_name_selection(ir, def, &scalar.value, scalar.range)
                        })
                    })
                    .flatten()
            })
            .unwrap_or(fallback),
        DefName::File => TextRange::new(0, 0).unwrap_or(fallback),
    }
}

fn definition_name_selection(
    ir: &RulesIr,
    def: &rules::ir::DefSpec,
    value: &str,
    range: TextRange,
) -> TextRange {
    let folded = value.to_ascii_lowercase();
    let start = def
        .strip_prefix
        .map(|prefix| ir.strings().resolve(prefix))
        .filter(|prefix| folded.starts_with(prefix))
        .map_or(0, str::len);
    let end = def
        .strip_suffix
        .map(|suffix| ir.strings().resolve(suffix))
        .filter(|suffix| folded[start..].ends_with(suffix))
        .map_or(value.len(), |suffix| value.len() - suffix.len());
    subrange(range, value, start, end)
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
fn reference_symbol_name(
    ir: &RulesIr,
    value: &str,
    strip_prefix: Option<rules::ir::Symbol>,
) -> Option<String> {
    let Some(prefix) = strip_prefix else {
        return Some(value.to_owned());
    };
    let prefix = ir.strings().resolve(prefix);
    // The authored reference omits the definition's prefix. Restore it for
    // membership and navigation; an already-prefixed value violates this
    // matcher, just as it does in RulesIr::scalar_matches.
    if value
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
    {
        return None;
    }
    Some(format!("{prefix}{value}"))
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
            let Some(name) = reference_symbol_name(ir, value, *strip_prefix) else {
                return;
            };
            out.references.push(HirReference {
                kind: ir.strings().resolve(ir.type_info(*type_id).name).into(),
                name,
                range,
                origin: HirReferenceOrigin::SemanticTyped,
                subtype: subtype.map(|subtype| ir.strings().resolve(subtype).into()),
            });
        }
        // Enum rows are declared literals, not workspace symbol references.
        // Membership is checked by the field matcher; resolving a row against
        // an unrelated symbol namespace would report valid literals as missing.
        Matcher::Enum { .. } => {}
        Matcher::Loc if value.trim().is_empty() => {}
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
                // Enum fallbacks can overlap actual symbols (for example installed
                // DLC sprites). Retain navigation when a declared ref resolves.
                if definite
                    .iter()
                    .all(|id| matches!(ir.matcher(*id), Matcher::Enum { .. }))
                {
                    for item in &alternatives {
                        if matcher_has_reference(ir, *item)
                            && branch_reference_matches(ir, *item, value, facts, out)
                        {
                            collect_branch_references(ir, *item, value, range, facts, out);
                        }
                    }
                } else {
                    // A scope expression may itself contain typed template holes.
                    for item in definite {
                        if matches!(ir.matcher(item), Matcher::Scope(_) | Matcher::Link) {
                            collect_matcher_refs(ir, item, value, range, facts, out);
                        }
                    }
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
            let typed_matches = matches
                .iter()
                .copied()
                .filter(|id| matcher_has_typed_reference(ir, *id))
                .collect::<Vec<_>>();
            if !typed_matches.is_empty() {
                // Overlapping subtypes and namespaces can both resolve. Keep
                // their real targets; HirFile deduplicates identical sites.
                for selected in typed_matches {
                    collect_branch_references(ir, selected, value, range, facts, out);
                }
            } else if !matches.is_empty() {
                for selected in matches {
                    collect_branch_references(ir, selected, value, range, facts, out);
                }
            } else if refs.len() == 1 && !alternatives.iter().any(|id| scalar_fallback(ir, *id)) {
                collect_branch_references(ir, refs[0], value, range, facts, out);
            }
        }
        Matcher::Pattern(parts) => {
            if let Some(holes) = pattern_value_holes(ir, parts, value, facts, Some(out)) {
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
        Matcher::Link | Matcher::Scope(_) => {
            let mut offset = 0;
            for segment in value.split('.') {
                let segment_range = subrange(range, value, offset, offset + segment.len());
                for link in &ir.scopes.links {
                    if let Some(holes) =
                        pattern_value_holes(ir, &link.pattern, segment, facts, Some(out))
                    {
                        for (matcher, start, end) in holes {
                            collect_matcher_refs(
                                ir,
                                matcher,
                                &segment[start..end],
                                subrange(segment_range, segment, start, end),
                                facts,
                                out,
                            );
                        }
                    }
                }
                offset += segment.len() + 1;
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
        Matcher::Enum { id } => ir.enum_contains(*id, value),
        Matcher::Scalar | Matcher::Loc => false,
        Matcher::Scope(expected)=>ir.scope_matches(*expected,value)||link_or_register_matches(ir,value),
        Matcher::Link=>link_or_register_matches(ir,value),
        Matcher::Opaque|Matcher::Path(_)|Matcher::Def{..}=>true,
        Matcher::Pattern(parts)=>pattern_value_holes(ir,parts,value,None,None).is_some()&&!parts.iter().any(|part|matches!(part,rules::ir::PatternPart::Hole(m) if matcher_has_reference(ir,*m))),
        Matcher::Union(items)=>items.iter().any(|item|definitely_non_reference(ir,*item,value)),
        Matcher::Ref(_)=>false,
    }
}
fn matcher_has_reference(ir: &RulesIr, id: MatcherId) -> bool {
    match ir.matcher(id) {
        Matcher::Ref(_) | Matcher::Loc => true,
        Matcher::Union(items) => items.iter().any(|item| matcher_has_reference(ir, *item)),
        Matcher::Pattern(parts) => parts.iter().any(
            |part| matches!(part,rules::ir::PatternPart::Hole(m) if matcher_has_reference(ir,*m)),
        ),
        _ => false,
    }
}
fn matcher_has_pattern(ir: &RulesIr, id: MatcherId) -> bool {
    match ir.matcher(id) {
        Matcher::Pattern(_) => true,
        Matcher::Union(items) => items.iter().any(|id| matcher_has_pattern(ir, *id)),
        _ => false,
    }
}
fn scalar_fallback(ir: &RulesIr, id: MatcherId) -> bool {
    match ir.matcher(id) {
        Matcher::Scalar => true,
        Matcher::Union(items) => items.iter().any(|item| scalar_fallback(ir, *item)),
        _ => false,
    }
}
fn matcher_has_typed_reference(ir: &RulesIr, id: MatcherId) -> bool {
    match ir.matcher(id) {
        Matcher::Ref(_) => true,
        Matcher::Union(items) => items.iter().any(|item| matcher_has_typed_reference(ir, *item)),
        Matcher::Pattern(parts) => parts.iter().any(
            |part| matches!(part, rules::ir::PatternPart::Hole(m) if matcher_has_typed_reference(ir, *m)),
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
        Matcher::Pattern(parts) => pattern_value_holes(ir, parts, value, facts, Some(out))
            .is_some_and(|holes| {
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
        Matcher::Pattern(parts) => {
            if let Some(holes) = pattern_value_holes(ir, parts, value, facts, Some(out)) {
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
            let Some(value) = reference_symbol_name(ir, value, *strip_prefix) else {
                return false;
            };
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
            out.symbol_facts_dependency.set(true);
            facts.is_some_and(|facts| match subtype {
                Some(subtype) => facts.type_subtype_member(*type_id, *subtype, &value),
                None => facts.type_member(*type_id, &value),
            })
        }
    }
}
fn pattern_value_holes(
    ir: &RulesIr,
    parts: &[rules::ir::PatternPart],
    value: &str,
    facts: Option<&dyn SymbolFacts>,
    out: Option<&IrFacts>,
) -> Option<Vec<(MatcherId, usize, usize)>> {
    let mut no_cancel = || false;
    let mut budget = rules::pattern::SearchBudget::new(Default::default(), &mut no_cancel);
    let multiple = parts
        .iter()
        .filter(|part| matches!(part, rules::ir::PatternPart::Hole(_)))
        .count()
        > 1;
    let mut primitive = |matcher, text: &str| {
        out.map_or(Some(true), |out| {
            Some(matcher_accepts_key(ir, matcher, text, facts, out))
        })
    };
    let result = if multiple {
        rules::pattern::unique_search(ir, parts, value, &mut budget, &mut primitive)
    } else {
        rules::pattern::search(ir, parts, value, &mut budget, &mut |_, _| Some(true))
    };
    if result.matched.is_none()
        && budget.limit.is_none()
        && let Some(out) = out
    {
        out.pattern_ambiguous.set(true);
    }
    if budget.limit.is_some()
        && let Some(out) = out
    {
        out.pattern_search_incomplete.set(true);
    }
    (result.matched == Some(true)).then_some(result.holes)
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
fn add_bindings(ir: &RulesIr, ty: TypeId, name: &str, range: TextRange, out: &mut IrFacts) {
    let info = ir.type_info(ty);
    for implementation in &info.trait_impls {
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

#[cfg(test)]
mod template_key_tests {
    use super::*;
    use rules::ir::PatternPart;

    fn reference_match(ir: &RulesIr, parts: &[PatternPart], key: &str) -> bool {
        let folded = key.to_ascii_lowercase();
        let mut cursor = 0;
        for (index, part) in parts.iter().enumerate() {
            match part {
                PatternPart::Text(text) => {
                    let literal = ir.strings().resolve(*text).to_ascii_lowercase();
                    let Some(offset) = folded[cursor..].find(&literal) else {
                        return false;
                    };
                    if index == 0 && offset != 0 {
                        return false;
                    }
                    cursor += offset + literal.len();
                }
                PatternPart::Hole(_) => {
                    if index + 1 == parts.len() {
                        cursor = key.len();
                    } else if let PatternPart::Text(next) = parts[index + 1] {
                        let literal = ir.strings().resolve(next).to_ascii_lowercase();
                        let Some(offset) = folded[cursor..].find(&literal) else {
                            return false;
                        };
                        cursor += offset;
                    }
                }
            }
        }
        cursor == key.len()
    }

    #[test]
    fn borrowed_template_matching_preserves_greedy_ascii_folded_search() {
        let mut ir = (*game::eu4::first_party_ir().unwrap()).clone();
        let hole = ir
            .scopes
            .links
            .iter()
            .flat_map(|link| link.pattern.iter())
            .find_map(|part| {
                if let PatternPart::Hole(matcher) = part {
                    Some(*matcher)
                } else {
                    None
                }
            })
            .expect("scope template hole");
        let [empty, prefix, suffix, accented, foo, bar] =
            ["", "pre_", "_post", "é", "foo", "bar"].map(|text| ir.strings.intern_verbatim(text));
        let mut patterns = ir
            .scopes
            .links
            .iter()
            .map(|link| link.pattern.to_vec())
            .collect::<Vec<_>>();
        patterns.extend([
            vec![PatternPart::Text(foo)],
            vec![
                PatternPart::Text(prefix),
                PatternPart::Hole(hole),
                PatternPart::Text(suffix),
            ],
            vec![PatternPart::Hole(hole), PatternPart::Text(accented)],
            vec![
                PatternPart::Text(empty),
                PatternPart::Hole(hole),
                PatternPart::Text(empty),
            ],
            vec![PatternPart::Text(foo), PatternPart::Text(bar)],
            vec![
                PatternPart::Hole(hole),
                PatternPart::Text(suffix),
                PatternPart::Hole(hole),
                PatternPart::Text(accented),
            ],
        ]);
        let keys = [
            "",
            "ROOT",
            "root",
            "owner",
            "FRA",
            "foo",
            "FoO",
            "foobar",
            "foo_gap_bar",
            "pre_x_post",
            "PRE_国_POST",
            "pre_é_post_post",
            "xpre_x_post",
            "pre_x_post_tail",
            "é",
            "É",
            "国é",
            "x_posté",
            "x_post_posté",
            "event_target:test",
            "event_target:国",
            "event_target:",
            "var:foo",
            "invalid:name",
        ];
        for parts in &patterns {
            for key in keys {
                assert_eq!(
                    template_key_matches(&ir, parts, key),
                    reference_match(&ir, parts, key),
                    "{parts:?} {key:?}"
                );
            }
        }
    }
}
