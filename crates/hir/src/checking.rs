//! Shared rule interpretation for ordinary script and Template constraints.
//! Workspace capabilities are supplied through a read-only facts adapter.
use crate::{ScopeState, ScopeValue};
use rules::ir::{
    FieldValue, Matcher, MatcherId, PatternPart, RefTarget, RulesIr, Shape, SymbolFacts,
};
use rules::source::ControlKind;

struct Facts<'a>(&'a dyn SymbolFacts);
impl SymbolFacts for Facts<'_> {
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

/// Shape/key dispatch shared by Template goals and ordinary lowering.
pub fn field_candidates(
    ir: &RulesIr,
    schema: rules::ir::SchemaId,
    key: &str,
    shape: Shape,
    facts: &dyn SymbolFacts,
) -> Vec<rules::ir::FieldId> {
    let mut candidates = ir.lookup(schema, key, shape).collect::<Vec<_>>();
    let exact=candidates.iter().copied().filter(|id|matches!(ir.matcher(ir.field(*id).key),Matcher::Literal(name) if ir.strings().resolve(*name).eq_ignore_ascii_case(key))).collect::<Vec<_>>();
    if !exact.is_empty() {
        candidates = exact;
    }
    candidates.retain(|id| scalar_matches(ir, ir.field(*id).key, key, facts));
    candidates
}

/// Input-scope validation preserves unresolved ambient registers as Unknown.
pub fn field_scope_validation(
    ir: &RulesIr,
    id: rules::ir::FieldId,
    state: &ScopeState,
) -> crate::analysis::Validation {
    use crate::analysis::Validation;
    let Some(scope) = ir
        .field(id)
        .scope
        .as_ref()
        .filter(|scope| !scope.scopes_in.is_empty())
    else {
        return Validation::Valid;
    };
    match state.current.first() {
        Some(ScopeValue::Invalid) => Validation::Invalid,
        Some(ScopeValue::Known(names))
            if names.len() == 1 && !names[0].eq_ignore_ascii_case("any") =>
        {
            if crate::ir_lowering::scope_value_allows(
                ir,
                state.current.first().expect("scope"),
                &scope.scopes_in,
            ) {
                Validation::Valid
            } else {
                Validation::Invalid
            }
        }
        _ => Validation::Unknown,
    }
}

/// Interprets a scalar matcher using the same reference-prefix and pattern policy everywhere.
pub fn scalar_matches(
    ir: &RulesIr,
    matcher: MatcherId,
    value: &str,
    facts: &dyn SymbolFacts,
) -> bool {
    match ir.matcher(matcher) {
        Matcher::Link => crate::is_ir_scope_link(ir, value),
        Matcher::Def { .. } => !value.is_empty(),
        Matcher::Union(alternatives) => alternatives
            .iter()
            .any(|id| scalar_matches(ir, *id, value, facts)),
        Matcher::Ref(RefTarget::Type {
            type_id,
            subtype,
            strip_prefix,
        }) => {
            if strip_prefix.is_some_and(|prefix| {
                value
                    .get(..ir.strings().resolve(prefix).len())
                    .is_some_and(|head| head.eq_ignore_ascii_case(ir.strings().resolve(prefix)))
            }) {
                return false;
            }
            let member = |candidate: &str| {
                subtype.map_or_else(
                    || facts.type_member(*type_id, candidate),
                    |subtype| facts.type_subtype_member(*type_id, subtype, candidate),
                )
            };
            member(value)
                || strip_prefix.is_some_and(|prefix| {
                    member(&format!("{}{value}", ir.strings().resolve(prefix)))
                })
        }
        Matcher::Pattern(parts) => pattern_matches(ir, parts, value, facts),
        _ => ir.scalar_matches(matcher, value, &Facts(facts)),
    }
}

/// Matches a regular text pattern. Search states are explicit, not recursive native frames.
pub fn pattern_matches(
    ir: &RulesIr,
    parts: &[PatternPart],
    value: &str,
    facts: &dyn SymbolFacts,
) -> bool {
    let mut pending = vec![(0, 0)];
    let mut seen = std::collections::BTreeSet::new();
    while let Some((part, offset)) = pending.pop() {
        if !seen.insert((part, offset)) {
            continue;
        }
        let Some(item) = parts.get(part) else {
            if offset == value.len() {
                return true;
            }
            continue;
        };
        let Some(tail) = value.get(offset..) else {
            continue;
        };
        match item {
            PatternPart::Text(text) => {
                let text = ir.strings().resolve(*text);
                if tail
                    .get(..text.len())
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case(text))
                {
                    pending.push((part + 1, offset + text.len()));
                }
            }
            PatternPart::Hole(matcher) => {
                for end in tail
                    .char_indices()
                    .map(|(i, _)| i)
                    .skip(1)
                    .chain(std::iter::once(tail.len()))
                {
                    if scalar_matches(ir, *matcher, &tail[..end], facts) {
                        pending.push((part + 1, offset + end));
                    }
                }
            }
        }
    }
    false
}

/// Checks scope links and registers against actual input/output scope context.
pub fn scope_expression_allowed(
    ir: &RulesIr,
    name: &str,
    expected: Option<rules::ir::Symbol>,
    state: Option<&ScopeState>,
    facts: &dyn SymbolFacts,
) -> bool {
    if name.contains('.') {
        let Some(mut current) = state.cloned() else {
            return crate::is_ir_scope_link(ir, name);
        };
        let parts = name.split('.').collect::<Vec<_>>();
        for (index, part) in parts.iter().enumerate() {
            if !scope_expression_allowed(
                ir,
                part,
                if index + 1 == parts.len() {
                    expected
                } else {
                    None
                },
                Some(&current),
                facts,
            ) {
                return false;
            }
            current = crate::transition_ir_scope(ir, current, None, part);
        }
        return true;
    }
    let current = state.and_then(|state| state.current.first());
    if let Some(link) = ir
        .scopes
        .links
        .iter()
        .find(|link| pattern_matches(ir, &link.pattern, name, facts))
    {
        let target = expected.is_none_or(|expected| match link.to {
            rules::ir::ScopeRef::Any => true,
            rules::ir::ScopeRef::Type(actual) => ir.scopes_compatible(actual, expected),
        });
        return target
            && current.is_none_or(|current| {
                matches!(current, ScopeValue::Unknown)
                    || link.from.iter().any(|from| match (from, current) {
                        (rules::ir::ScopeRef::Any, _) => true,
                        (rules::ir::ScopeRef::Type(expected), ScopeValue::Known(names)) => {
                            names.iter().any(|name| {
                                ir.strings()
                                    .lookup_folded(name)
                                    .is_some_and(|actual| ir.scopes_compatible(actual, *expected))
                            })
                        }
                        _ => false,
                    })
            });
    }
    if let Some(expected) = expected {
        if ir
            .strings()
            .lookup_folded(name)
            .is_some_and(|actual| ir.scopes_compatible(actual, expected))
        {
            return true;
        }
        return ir.scopes.register(ir.strings(), name).is_some()
            && state.is_none_or(
                |state| match crate::ir_scope_register_value(ir, state, name) {
                    Some(ScopeValue::Known(names)) => names.iter().any(|name| {
                        ir.strings()
                            .lookup_folded(name)
                            .is_some_and(|actual| ir.scopes_compatible(actual, expected))
                    }),
                    Some(ScopeValue::Invalid) => false,
                    _ => true,
                },
            );
    }
    ir.scope_matches(None, name) || ir.scopes.register(ir.strings(), name).is_some()
}

/// Three-valued scope expression checking, including actual register and link inputs.
pub fn scope_validation(
    ir: &RulesIr,
    name: &str,
    expected: Option<rules::ir::Symbol>,
    state: &ScopeState,
    facts: &dyn SymbolFacts,
) -> crate::analysis::Validation {
    use crate::analysis::Validation;
    let parts = name.split('.').collect::<Vec<_>>();
    let mut current = state.clone();
    let mut unknown = false;
    for (index, part) in parts.iter().enumerate() {
        let expected = if index + 1 == parts.len() {
            expected
        } else {
            None
        };
        if !scope_expression_allowed(ir, part, expected, Some(&current), facts) {
            return Validation::Invalid;
        }
        if ir.scopes.register(ir.strings(),part).is_some() {
            match crate::ir_scope_register_value(ir,&current,part) {
                Some(ScopeValue::Invalid)=>return Validation::Invalid,
                Some(ScopeValue::Known(names)) if names.len()==1 && !names[0].eq_ignore_ascii_case("any")=>{},
                _=>unknown=true,
            }
        } else if let Some(link)=ir.scopes.links.iter().find(|link|pattern_matches(ir,&link.pattern,part,facts))
            && !link.from.iter().any(|scope|matches!(scope,rules::ir::ScopeRef::Any))
            && current.current.first().is_none_or(|scope|matches!(scope,ScopeValue::Unknown)|matches!(scope,ScopeValue::Known(names) if names.len()!=1 || names[0].eq_ignore_ascii_case("any"))) {
            unknown=true;
        }
        current = crate::transition_ir_scope(ir, current, None, part);
    }
    if unknown {
        Validation::Unknown
    } else {
        Validation::Valid
    }
}

/// Checks the workspace-dependent form without granting missing environment capabilities a pass.
pub fn scalar_validation(
    ir: &RulesIr,
    matcher: MatcherId,
    value: &str,
    state: &ScopeState,
    facts: &dyn SymbolFacts,
) -> crate::analysis::Validation {
    use crate::analysis::Validation;
    let known = match ir.matcher(matcher) {
        Matcher::Path(Some(category))
            if ir.strings().resolve(*category).eq_ignore_ascii_case("gfx") =>
        {
            facts.asset_member(ir.strings().resolve(*category), value)
        }
        Matcher::Scope(expected) => return scope_validation(ir, value, *expected, state, facts),
        Matcher::Link => return scope_validation(ir, value, None, state, facts),
        Matcher::Union(items) => {
            let mut unknown = false;
            for id in items.iter() {
                match scalar_validation(ir, *id, value, state, facts) {
                    Validation::Valid => return Validation::Valid,
                    Validation::Unknown => unknown = true,
                    Validation::Invalid => {}
                }
            }
            if unknown {
                return Validation::Unknown;
            }
            Some(false)
        }
        _ => Some(scalar_matches(ir, matcher, value, facts)),
    };
    match known {
        Some(true) => Validation::Valid,
        Some(false) => Validation::Invalid,
        None => Validation::Unknown,
    }
}

/// Protocol-neutral reason a rendered container failed ordinary rule checking.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum IssueKind {
    /// The specialized source is syntactically malformed.
    Syntax,
    /// No field interpretation exists.
    Key,
    /// A value or shape is rejected.
    Value,
    /// Input scope is rejected.
    Scope,
    /// Counts or a complete-container form is rejected.
    Cardinality,
    /// An unattached branch continuation.
    OrphanElse,
    /// A logical-container style or truth-table warning.
    LogicalContainer,
    /// A statically constant condition.
    ConstantCondition,
    /// A branch without its guard.
    MissingLimit,
    /// An empty control body.
    EmptyBlock,
}

/// Evidence relative to the rendered source, shared by all Template consumers.
#[derive(Clone, Debug)]
pub struct ConstraintEvidence {
    /// Rejection category.
    pub kind: IssueKind,
    /// Rule or control diagnostic severity.
    pub severity: rules::source::Severity,
    /// Range in rendered UTF-8 text.
    pub range: text::TextRange,
    /// Explanatory rule fact without protocol formatting.
    pub explanation: String,
    /// Context whose key/scope selected the failing statement.
    pub container: text::TextRange,
}

impl IssueKind {
    /// Default severity for ordinary control facts.
    pub fn severity(self) -> rules::source::Severity {
        match self {
            Self::LogicalContainer
            | Self::ConstantCondition
            | Self::MissingLimit
            | Self::EmptyBlock => rules::source::Severity::Warning,
            _ => rules::source::Severity::Error,
        }
    }
}
impl ConstraintEvidence {
    fn new(
        kind: IssueKind,
        severity: rules::source::Severity,
        range: text::TextRange,
        explanation: String,
    ) -> Self {
        Self {
            kind,
            severity,
            range,
            explanation,
            container: range,
        }
    }
}

/// Checks a complete rendered script container with ordinary HIR facts and rule semantics.
pub fn check_fragment<E>(
    ir: &RulesIr,
    hir: &crate::HirFile,
    facts: &dyn SymbolFacts,
    source: &crate::template_text::RenderedTemplate,
    checkpoint: &mut dyn FnMut() -> Result<(), E>,
) -> Result<crate::analysis::Analysis<Vec<ConstraintEvidence>>, E> {
    use crate::analysis::{Analysis, ResidualReason, Validation};
    let mut coverage = hir.analysis_coverage().clone();
    let mut result = Vec::new();
    for error in hir.syntax().errors() {
        if !source.has_hole(error.range) && !source.has_unresolved_structure(error.range) {
            result.push(ConstraintEvidence {
                severity: rules::source::Severity::Error,
                kind: IssueKind::Syntax,
                range: error.range,
                explanation: error.message.clone(),
                container: hir.syntax().root().range(),
            });
        }
    }
    let display = display_ranges(ir, hir);
    for overload in hir
        .overload_facts()
        .iter()
        .filter(|overload| overload.validation == Validation::Invalid)
    {
        result.push(ConstraintEvidence {
            severity: rules::source::Severity::Error,
            kind: IssueKind::Value,
            range: overload.range,
            explanation: "no rule overload accepts this complete block".into(),
            container: overload.container,
        });
    }
    for scalar in hir.bare_values() {
        if hir.properties().iter().any(|property| {
            property.scalar.as_ref().is_some_and(|value| {
                value.range.start() <= scalar.range.start()
                    && scalar.range.end() <= value.range.end()
            })
        }) {
            continue;
        }
        checkpoint()?;
        if source.has_hole(scalar.range) || source.has_unresolved_structure(scalar.range) {
            coverage.residuals.insert(ResidualReason::Binding);
            continue;
        }
        if source
            .unresolved_bindings
            .iter()
            .any(|range| range.start() <= scalar.range.start() && scalar.range.end() <= range.end())
        {
            continue;
        }
        let Some(context) = hir
            .schema_facts()
            .iter()
            .filter(|context| {
                context.range.start() <= scalar.range.start()
                    && scalar.range.end() <= context.range.end()
            })
            .min_by_key(|context| context.range.len())
        else {
            continue;
        };
        if hir.overload_facts().iter().any(|overload| {
            overload.validation != Validation::Valid
                && overload.container.start() <= scalar.range.start()
                && scalar.range.end() <= overload.container.end()
        }) {
            continue;
        }
        if display
            .iter()
            .any(|range| range.start() <= scalar.range.start() && scalar.range.end() <= range.end())
        {
            continue;
        }
        let checked = ir.schema(context.schema).items.map_or_else(
            || {
                if ir.schema(context.schema).open {
                    Validation::Valid
                } else {
                    Validation::Invalid
                }
            },
            |matcher| scalar_validation(ir, matcher, &scalar.value, &context.state, facts),
        );
        if checked == Validation::Invalid {
            result.push(ConstraintEvidence {
                severity: rules::source::Severity::Error,
                kind: IssueKind::Value,
                range: scalar.range,
                explanation: format!(
                    "unkeyed value `{}` does not satisfy its container rule",
                    scalar.value
                ),
                container: context.range,
            });
        } else if checked == Validation::Unknown {
            coverage.residuals.insert(ResidualReason::Binding);
        }
    }
    let mut counts = std::collections::BTreeMap::<(text::TextRange, usize), u32>::new();
    for property in hir.properties() {
        if source.unresolved_bindings.iter().any(|range| {
            range.start() <= property.key_range.start() && property.key_range.end() <= range.end()
        }) {
            continue;
        }
        if hir.overload_facts().iter().any(|overload| {
            overload.validation != Validation::Valid
                && overload.container.start() <= property.key_range.start()
                && property.key_range.end() <= overload.container.end()
        }) {
            continue;
        }
        checkpoint()?;
        if display.iter().any(|range| {
            range.start() < property.range.start() && property.range.end() <= range.end()
        }) {
            continue;
        }
        let Some(field) = hir.field_fact_at(property.key_range) else {
            continue;
        };
        let Some(parent) = hir
            .schema_facts()
            .iter()
            .filter(|parent| {
                parent.schema == field.schema
                    && parent.range.start() <= property.key_range.start()
                    && property.key_range.end() <= parent.range.end()
            })
            .min_by_key(|parent| parent.range.len())
        else {
            continue;
        };
        if source.has_hole(property.key_range)
            || source.has_unresolved_structure(property.key_range)
        {
            coverage.residuals.insert(ResidualReason::Binding);
            continue;
        }
        let candidates = field
            .fields
            .iter()
            .copied()
            .filter(|id| scalar_matches(ir, ir.field(*id).key, &property.key, facts))
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            let recognized = ir
                .fields(parent.schema)
                .into_iter()
                .any(|id| scalar_matches(ir, ir.field(id).key, &property.key, facts));
            result.push(ConstraintEvidence {
                severity: rules::source::Severity::Error,
                kind: if recognized {
                    IssueKind::Value
                } else {
                    IssueKind::Key
                },
                range: property.key_range,
                explanation: if recognized {
                    format!("`{}` has an incompatible value shape", property.key)
                } else {
                    format!("unknown key `{}`", property.key)
                },
                container: parent.range,
            });
            continue;
        }
        let scope_ok = |id| {
            ir.field(id).scope.as_ref().is_none_or(|effect| {
                effect.scopes_in.is_empty()
                    || parent.state.current.first().is_none_or(|current| {
                        crate::ir_lowering::scope_value_allows(ir, current, &effect.scopes_in)
                    })
            })
        };
        let scoped = candidates
            .iter()
            .copied()
            .filter(|id| scope_ok(*id))
            .collect::<Vec<_>>();
        if scoped.is_empty() {
            result.push(ConstraintEvidence {
                severity: rules::source::Severity::Error,
                kind: IssueKind::Scope,
                range: property.key_range,
                explanation: format!("`{}` is not valid in this scope", property.key),
                container: parent.range,
            });
            continue;
        }
        let mut accepted = false;
        let mut unknown = false;
        let mut selected = scoped[0];
        for id in &scoped {
            let outcome = match (ir.field(*id).value, property.scalar.as_ref()) {
                (FieldValue::Scalar(_), Some(scalar)) if source.has_hole(scalar.range) => {
                    Validation::Unknown
                }
                (FieldValue::Scalar(matcher), Some(scalar)) => {
                    scalar_validation(ir, matcher, &scalar.value, &parent.state, facts)
                }
                (FieldValue::Scalar(_), None) => Validation::Invalid,
                (FieldValue::Block(_) | FieldValue::SelfBlock, None) => Validation::Valid,
                _ => Validation::Invalid,
            };
            match outcome {
                Validation::Valid => {
                    accepted = true;
                    selected = *id;
                    break;
                }
                Validation::Unknown => unknown = true,
                Validation::Invalid => {}
            }
        }
        if !accepted && unknown {
            coverage.residuals.insert(ResidualReason::Binding);
        }
        if !accepted && !unknown {
            result.push(ConstraintEvidence {
                severity: rules::source::Severity::Error,
                kind: IssueKind::Value,
                range: property
                    .scalar
                    .as_ref()
                    .map_or(property.value_range.unwrap_or(property.range), |scalar| {
                        scalar.range
                    }),
                explanation: format!("value of `{}` does not satisfy its rule", property.key),
                container: parent.range,
            });
        }
        *counts.entry((parent.range, selected.index())).or_default() += 1;
    }
    for context in hir.schema_facts() {
        if source.unresolved_bindings.iter().any(|range| {
            range.start() <= context.range.start() && context.range.end() <= range.end()
        }) {
            continue;
        }
        if hir.overload_facts().iter().any(|overload| {
            overload.validation != Validation::Valid
                && overload.container.start() <= context.range.start()
                && context.range.end() <= overload.container.end()
        }) {
            continue;
        }
        checkpoint()?;
        if display.iter().any(|range| {
            range.start() < context.range.start() && context.range.end() <= range.end()
        }) {
            continue;
        }
        if source.has_hole(context.range) || source.has_unresolved_structure(context.range) {
            coverage.residuals.insert(ResidualReason::Binding);
        }
        let schema = ir.schema(context.schema);
        if !source.has_hole(context.range)
            && !source.has_unresolved_structure(context.range)
            && !schema.forms.is_empty()
            && !schema.forms.iter().any(|form| {
                form.counts.iter().all(|(ids, card)| {
                    let n = ids
                        .iter()
                        .map(|id| {
                            counts
                                .get(&(context.range, id.index()))
                                .copied()
                                .unwrap_or(0)
                        })
                        .sum::<u32>();
                    n >= card.min && card.max.is_none_or(|max| n <= max)
                })
            })
        {
            result.push(ConstraintEvidence {
                severity: rules::source::Severity::Error,
                kind: IssueKind::Cardinality,
                range: text::TextRange::empty(context.range.start()),
                explanation: "container does not satisfy any allowed field form".into(),
                container: context.range,
            });
        }
        let mut families = std::collections::BTreeMap::<usize, Vec<rules::ir::FieldId>>::new();
        for id in ir.fields(context.schema) {
            families
                .entry(ir.field(id).key.index())
                .or_default()
                .push(id);
        }
        for ids in families.values() {
            let minimum = ids
                .iter()
                .map(|id| ir.field(*id).card.min)
                .max()
                .unwrap_or(0);
            let maximum = if ids.iter().any(|id| ir.field(*id).card.max.is_none()) {
                None
            } else {
                ids.iter().filter_map(|id| ir.field(*id).card.max).max()
            };
            let count = ids
                .iter()
                .map(|id| {
                    counts
                        .get(&(context.range, id.index()))
                        .copied()
                        .unwrap_or(0)
                })
                .sum::<u32>();
            if (count < minimum
                && !source.has_hole(context.range)
                && !source.has_unresolved_structure(context.range))
                || maximum.is_some_and(|max| count > max)
            {
                result.push(ConstraintEvidence {
                    severity: rules::source::Severity::Error,
                    kind: IssueKind::Cardinality,
                    range: text::TextRange::empty(context.range.start()),
                    explanation: format!("container field count for `{}` violates its rule: found {count}, minimum {minimum}{}",
                        matcher_key_label(ir,ir.field(ids[0]).key),maximum.map_or(String::new(),|maximum|format!(", maximum {maximum}"))),
                    container: context.range,
                });
            }
        }
    }
    result.extend(control_lints(ir, hir, facts, Some(source), checkpoint)?);
    Ok(Analysis {
        value: result,
        coverage,
    })
}

fn display_ranges(ir: &RulesIr, hir: &crate::HirFile) -> Vec<text::TextRange> {
    hir.properties()
        .iter()
        .filter(|property| {
            hir.field_fact_at(property.key_range).is_some_and(|fact| {
                fact.fields.iter().any(|id| {
                    ir.field(*id)
                        .control
                        .as_ref()
                        .is_some_and(|control| control.kind == ControlKind::DisplayOnly)
                })
            })
        })
        .map(|property| property.range)
        .collect()
}
fn matcher_key_label(ir: &RulesIr, id: MatcherId) -> String {
    match ir.matcher(id) {
        Matcher::Literal(name) => ir.strings().resolve(*name).to_owned(),
        _ => "pattern".into(),
    }
}

fn constant_control_value(
    ir: &RulesIr,
    hir: &crate::HirFile,
    property: &crate::HirProperty,
) -> Option<bool> {
    fn evaluate(
        ir: &RulesIr,
        hir: &crate::HirFile,
        property: &crate::HirProperty,
        depth: usize,
    ) -> Option<bool> {
        if depth >= 64 {
            return None;
        }
        let control = hir
            .field_fact_at(property.key_range)?
            .fields
            .iter()
            .find_map(|id| ir.field(*id).control.as_ref())?;
        if control.kind == ControlKind::Constant {
            let value = &property.scalar.as_ref()?.value;
            return if value.eq_ignore_ascii_case("yes") {
                Some(true)
            } else if value.eq_ignore_ascii_case("no") {
                Some(false)
            } else {
                None
            };
        }
        if !matches!(control.kind, ControlKind::Logic | ControlKind::Guard) {
            return None;
        }
        let children = hir
            .properties()
            .iter()
            .filter(|child| {
                child.path.len() == property.path.len() + 1
                    && child.path.starts_with(&property.path)
                    && property.range.start() <= child.range.start()
                    && child.range.end() <= property.range.end()
            })
            .collect::<Vec<_>>();
        if children.is_empty() {
            return None;
        }
        let op = control
            .op
            .map(|op| ir.strings().resolve(op))
            .unwrap_or("and");
        let values = children
            .iter()
            .map(|child| evaluate(ir, hir, child, depth + 1))
            .collect::<Vec<_>>();
        let or = op.eq_ignore_ascii_case("or") || op.eq_ignore_ascii_case("nor");
        let value = if or && values.contains(&Some(true)) {
            true
        } else if !or && values.contains(&Some(false)) {
            false
        } else {
            let values = values.into_iter().collect::<Option<Vec<_>>>()?;
            if or {
                values.iter().any(|value| *value)
            } else {
                values.iter().all(|value| *value)
            }
        };
        Some(
            if op.eq_ignore_ascii_case("not") || op.eq_ignore_ascii_case("nor") {
                !value
            } else {
                value
            },
        )
    }
    evaluate(ir, hir, property, 0)
}

pub fn control_lints<E>(
    ir: &RulesIr,
    hir: &crate::HirFile,
    facts: &dyn SymbolFacts,
    source: Option<&crate::template_text::RenderedTemplate>,
    checkpoint: &mut dyn FnMut() -> Result<(), E>,
) -> Result<Vec<ConstraintEvidence>, E> {
    let mut diagnostics = Vec::new();
    let display = display_ranges(ir, hir);
    let binding_dependent = |range: text::TextRange| {
        hir.parameter_references().iter().any(|reference| {
            range.start() <= reference.range.start() && reference.range.end() <= range.end()
        })
    };
    for property in hir.properties() {
        if display.iter().any(|range| {
            range.start() < property.range.start() && property.range.end() <= range.end()
        }) {
            continue;
        }
        checkpoint()?;
        if source.is_some_and(|source| source.has_hole(property.range)) {
            continue;
        }
        let Some(fact) = hir.field_fact_at(property.key_range) else {
            continue;
        };
        let Some(field) = fact.fields.first().map(|field| ir.field(*field)) else {
            continue;
        };
        let Some(control) = &field.control else {
            continue;
        };
        match control.kind {
            ControlKind::Logic => {
                let children = hir
                    .properties()
                    .iter()
                    .filter(|child| {
                        child.path.len() == property.path.len() + 1
                            && child.path.starts_with(&property.path)
                            && property.range.start() <= child.range.start()
                            && child.range.end() <= property.range.end()
                    })
                    .collect::<Vec<_>>();
                let op = control.op.map(|op| ir.strings().resolve(op));
                let invalid_count = if op.is_some_and(|op| op.eq_ignore_ascii_case("not")) {
                    children.len() != 1
                } else {
                    children.is_empty()
                };
                if invalid_count {
                    diagnostics.push(ConstraintEvidence::new(
                        IssueKind::LogicalContainer,
                        IssueKind::LogicalContainer.severity(),
                        property.key_range,
                        if op.is_some_and(|op| op.eq_ignore_ascii_case("not")) {
                            "`NOT` with multiple conditions is true only when none of them hold (an AND of NOTs); it is not \"not all of them hold\". Write `AND = { NOT = { ... } ... }` to state the intended reading".into()
                        } else {
                            format!("empty `{}` container is always {}; drop the container or add conditions", property.key, if op.is_some_and(|op| op.eq_ignore_ascii_case("or")) { "false" } else { "true" })
                        },
                    ));
                }
                if children.len() == 1
                    && op.is_some_and(|op| {
                        op.eq_ignore_ascii_case("or") || op.eq_ignore_ascii_case("and")
                    })
                {
                    diagnostics.push(ConstraintEvidence::new(IssueKind::LogicalContainer, IssueKind::LogicalContainer.severity(), property.key_range,
                        format!("`{}` with a single condition is equivalent to the condition itself; the wrapper can be removed", property.key)));
                }
                let constants = children
                    .iter()
                    .filter_map(|child| {
                        if !hir.field_fact_at(child.key_range).is_some_and(|fact| {
                            fact.fields.iter().any(|id| {
                                ir.field(*id)
                                    .control
                                    .as_ref()
                                    .is_some_and(|control| control.kind == ControlKind::Constant)
                            })
                        }) {
                            return None;
                        }
                        child.scalar.as_ref().and_then(|scalar| {
                            let value = scalar.value.to_ascii_lowercase();
                            (value == "yes" || value == "no").then_some(value == "yes")
                        })
                    })
                    .collect::<Vec<_>>();
                let constant = if op.is_some_and(|op| op.eq_ignore_ascii_case("and"))
                    && constants.contains(&false)
                {
                    Some(false)
                } else if op.is_some_and(|op| op.eq_ignore_ascii_case("or"))
                    && constants.contains(&true)
                {
                    Some(true)
                } else if op.is_some_and(|op| op.eq_ignore_ascii_case("not"))
                    && constants.len() == children.len()
                    && !constants.is_empty()
                {
                    Some(constants.iter().all(|value| !value))
                } else {
                    None
                };
                if let Some(value) = constant {
                    diagnostics.push(ConstraintEvidence::new(
                        IssueKind::ConstantCondition,
                        IssueKind::ConstantCondition.severity(),
                        property.range,
                        format!(
                            "this logic container is always {}",
                            if value { "true" } else { "false" }
                        ),
                    ));
                }
            }
            ControlKind::Branch | ControlKind::BranchContinue => {
                if let Some(guard) = control.guard {
                    let guard_name = ir.strings().resolve(guard);
                    if let Some(guard) = hir.properties().iter().find(|child| {
                        child.path.len() == property.path.len() + 1
                            && child.path.starts_with(&property.path)
                            && property.range.start() <= child.range.start()
                            && child.range.end() <= property.range.end()
                            && child.key.eq_ignore_ascii_case(guard_name)
                    }) && let Some(value) = constant_control_value(ir, hir, guard)
                    {
                        diagnostics.push(ConstraintEvidence::new(IssueKind::ConstantCondition, IssueKind::ConstantCondition.severity(), property.key_range,
                            if value { format!("`{guard_name}` of this `{}` is always true; the branch wrapper is redundant", property.key) }
                            else { format!("`{guard_name}` of this `{}` is always false; the branch can never run", property.key) }));
                    }
                    if !hir.properties().iter().any(|child| {
                        child.path.len() == property.path.len() + 1
                            && child.path.starts_with(&property.path)
                            && property.range.start() <= child.range.start()
                            && child.range.end() <= property.range.end()
                            && child.key.eq_ignore_ascii_case(guard_name)
                    }) {
                        diagnostics.push(ConstraintEvidence::new(
                            IssueKind::MissingLimit,
                            IssueKind::MissingLimit.severity(),
                            property.key_range,
                            format!("`{}` without `{guard_name}` executes its body unconditionally; the condition belongs in a `{guard_name}` block", property.key),
                        ));
                    }
                    if !hir.properties().iter().any(|child| {
                        child.path.len() == property.path.len() + 1
                            && child.path.starts_with(&property.path)
                            && property.range.start() <= child.range.start()
                            && child.range.end() <= property.range.end()
                    }) {
                        diagnostics.push(ConstraintEvidence::new(
                            IssueKind::EmptyBlock,
                            IssueKind::EmptyBlock.severity(),
                            property.key_range,
                            format!("`{}` block has an empty body", property.key),
                        ));
                    }
                }
                if control.kind == ControlKind::BranchContinue {
                    let parent_path = property
                        .path
                        .get(..property.path.len().saturating_sub(1))
                        .unwrap_or(&[]);
                    let container = hir
                        .schema_at(property.key_range.start())
                        .map(|fact| fact.range);
                    let mut previous = hir
                        .properties()
                        .iter()
                        .filter(|sibling| {
                            sibling.path.len() == property.path.len()
                                && sibling.path.get(..sibling.path.len().saturating_sub(1))
                                    == Some(parent_path)
                                && sibling.range.end() <= property.range.start()
                                && container.is_none_or(|container| {
                                    container.start() <= sibling.range.start()
                                        && sibling.range.end() <= container.end()
                                })
                        })
                        .collect::<Vec<_>>();
                    previous.sort_by_key(|sibling| std::cmp::Reverse(sibling.range.start()));
                    let mut attached = false;
                    for sibling in previous {
                        let previous_control = hir
                            .field_fact_at(sibling.key_range)
                            .and_then(|fact| fact.fields.first())
                            .and_then(|id| ir.field(*id).control.as_ref());
                        match previous_control {
                            Some(previous)
                                if matches!(
                                    previous.kind,
                                    ControlKind::Branch | ControlKind::BranchContinue
                                ) =>
                            {
                                if !previous.chain.is_empty() {
                                    attached = previous.chain.iter().any(|key| {
                                        ir.strings()
                                            .resolve(*key)
                                            .eq_ignore_ascii_case(&property.key)
                                    });
                                    break;
                                }
                                if previous.kind == ControlKind::Branch {
                                    break;
                                }
                            }
                            _ => break,
                        }
                    }
                    if !attached {
                        attached = hir
                            .properties()
                            .iter()
                            .filter(|parent| {
                                parent.path.len() + 1 == property.path.len()
                                    && property.path.starts_with(&parent.path)
                                    && parent.range.start() <= property.range.start()
                                    && property.range.end() <= parent.range.end()
                            })
                            .any(|parent| {
                                hir.field_fact_at(parent.key_range).is_some_and(|fact| {
                                    fact.fields.iter().any(|id| {
                                        ir.field(*id).control.as_ref().is_some_and(|control| {
                                            control.kind == ControlKind::Branch
                                                && control.chain.iter().any(|name| {
                                                    ir.strings()
                                                        .resolve(*name)
                                                        .eq_ignore_ascii_case(&property.key)
                                                })
                                        })
                                    })
                                })
                            });
                    }
                    if !attached {
                        diagnostics.push(ConstraintEvidence::new(
                            IssueKind::OrphanElse,
                            IssueKind::OrphanElse.severity(),
                            property.key_range,
                            format!("orphan `{}`: it must directly follow an `if`/`else_if` block or be nested inside one", property.key),
                        ));
                    }
                }
            }
            ControlKind::Switch => {
                if let Some(on) = control.on {
                    let selector = hir
                        .properties()
                        .iter()
                        .find(|child| {
                            child.path.len() == property.path.len() + 1
                                && child.path.starts_with(&property.path)
                                && property.range.start() <= child.range.start()
                                && child.range.end() <= property.range.end()
                                && child.key.eq_ignore_ascii_case(ir.strings().resolve(on))
                        })
                        .and_then(|child| child.scalar.as_ref())
                        .filter(|scalar| !binding_dependent(scalar.range));
                    if let Some(selector) = selector
                        && let Some(trigger_schema) = control.selector_schema
                    {
                        let matchers = ir
                            .lookup(trigger_schema, &selector.value, Shape::Scalar)
                            .filter_map(|id| {
                                let field = ir.field(id);
                                if !scalar_matches(ir, field.key, &selector.value, facts) {
                                    return None;
                                }
                                match field.value {
                                    FieldValue::Scalar(matcher) => Some(matcher),
                                    _ => None,
                                }
                            })
                            .collect::<Vec<_>>();
                        if matchers.is_empty() {
                            diagnostics.push(ConstraintEvidence::new(
                                IssueKind::Value,
                                IssueKind::Value.severity(),
                                selector.range,
                                format!("unknown scalar trigger `{}`", selector.value),
                            ));
                        } else {
                            for child in hir.properties().iter().filter(|child| {
                                child.path.len() == property.path.len() + 1
                                    && child.path.starts_with(&property.path)
                                    && property.range.start() <= child.range.start()
                                    && child.range.end() <= property.range.end()
                                    && child.scalar.is_none()
                            }) {
                                if binding_dependent(child.key_range) {
                                    continue;
                                }
                                if !matchers
                                    .iter()
                                    .any(|matcher| scalar_matches(ir, *matcher, &child.key, facts))
                                {
                                    diagnostics.push(ConstraintEvidence::new(
                                        IssueKind::Value,
                                        IssueKind::Value.severity(),
                                        child.key_range,
                                        format!(
                                            "expected {} for `{}` branch",
                                            matchers
                                                .iter()
                                                .map(|matcher| matcher_label(ir, *matcher))
                                                .collect::<Vec<_>>()
                                                .join(" or "),
                                            selector.value
                                        ),
                                    ));
                                }
                            }
                        }
                    }
                }
            }
            ControlKind::Guard | ControlKind::DisplayOnly => {
                let has_children = hir.properties().iter().any(|child| {
                    child.path.len() == property.path.len() + 1
                        && child.path.starts_with(&property.path)
                        && property.range.start() <= child.range.start()
                        && child.range.end() <= property.range.end()
                });
                if property.scalar.is_none() && !has_children {
                    diagnostics.push(ConstraintEvidence::new(
                        IssueKind::EmptyBlock,
                        IssueKind::EmptyBlock.severity(),
                        property.range,
                        format!("{} has an empty body", property.key),
                    ));
                }
            }
            _ => {}
        }
    }
    Ok(diagnostics)
}

fn matcher_label(ir: &RulesIr, id: MatcherId) -> String {
    match ir.matcher(id) {
        Matcher::Int { .. } => "an integer".into(),
        Matcher::Float { .. } => "a number".into(),
        Matcher::Bool => "yes or no".into(),
        Matcher::Literal(name) => format!("`{}`", ir.strings().resolve(*name)),
        Matcher::Union(ids) => ids
            .iter()
            .map(|id| matcher_label(ir, *id))
            .collect::<Vec<_>>()
            .join(" or "),
        _ => "a valid scalar value".into(),
    }
}
