//! Binding-aware Template queries over the shared compiled program.
//! Each usage is an intersection; overloads at one usage are alternatives.
use std::collections::{BTreeMap, BTreeSet};

use engine::AnalysisSnapshot;
use hir::analysis::Analysis;
use hir::{ScopeState, TemplateFragment, TemplateToken};
use rules::ir::{FieldId, Matcher, MatcherId};

use crate::ir_semantic::{self, WorkspaceFacts};
use crate::types::{CancellationToken, Cancelled};

pub(crate) use hir::template::Domain;

pub(crate) struct BodyAnalysis {
    pub(crate) rendered: hir::template_text::RenderedTemplate,
    pub(crate) hir: hir::HirFile,
    pub(crate) evidence: Vec<hir::checking::ConstraintEvidence>,
    pub(crate) coverage: hir::analysis::AnalysisCoverage,
}

fn has_structural_reads(template: &hir::Template, snapshot: &AnalysisSnapshot) -> bool {
    use rules::replacement::{TemplateFragment, TemplateInstruction, TemplateOperand};
    let ir = snapshot.ir();
    let schema = ir
        .type_by_name(&template.kind)
        .and_then(|kind| hir::template::template_body(ir, kind));
    template.program.blocks.iter().any(|block|block.iter().any(|node|match node {
        TemplateInstruction::Recover(_)|TemplateInstruction::Consume(_)|TemplateInstruction::When{..}=>true,
        TemplateInstruction::Dispatch(property)=>{
            if matches!(property.value,TemplateOperand::Block(_)) || property.key.fragments.iter().any(|part|matches!(part,TemplateFragment::Parameter{..}))
                || matches!(&property.value,TemplateOperand::Scalar(token) if token.quoted && token.fragments.iter().any(|part|matches!(part,TemplateFragment::Parameter{..}))) {return true;}
            let key=property.key.fragments.iter().map(|part|match part {TemplateFragment::Literal(text)=>Some(text.as_str()),_=>None}).collect::<Option<String>>();
            schema.zip(key).is_some_and(|(schema,key)|hir::checking::field_candidates(ir,schema,&key,rules::ir::Shape::Scalar,&WorkspaceFacts{snapshot})
                .iter().any(|id|hir::template::template_kind(ir,ir.field(*id).key).is_some()))
        }
    }))
}

pub(crate) fn needs_body_analysis(
    snapshot: &AnalysisSnapshot,
    summary: &engine::DynamicDefinitionSummary,
) -> bool {
    summary
        .template
        .as_ref()
        .is_some_and(|template| has_structural_reads(template, snapshot))
}

/// Shared structural specialization for candidate filtering, diagnostics and explanations.
pub(crate) fn analyse_body(
    snapshot: &AnalysisSnapshot,
    source: &hir::HirFile,
    invocation: &hir::HirProperty,
    override_value: Option<(&str, &str)>,
    cancellation: &CancellationToken,
) -> Result<Option<BodyAnalysis>, Cancelled> {
    let overrides = override_value
        .into_iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value.to_owned()))
        .collect();
    analyse_body_with_bindings(snapshot, source, invocation, &overrides, cancellation)
}

fn analyse_body_with_bindings(
    snapshot: &AnalysisSnapshot,
    source: &hir::HirFile,
    invocation: &hir::HirProperty,
    overrides: &BTreeMap<String, String>,
    cancellation: &CancellationToken,
) -> Result<Option<BodyAnalysis>, Cancelled> {
    let ir = snapshot.ir();
    let Some(field) = source.field_fact_at(invocation.key_range) else {
        return Ok(None);
    };
    let Some(kind) = field
        .fields
        .iter()
        .find_map(|id| template_kind(ir, ir.field(*id).key))
    else {
        return Ok(None);
    };
    let facts = WorkspaceFacts { snapshot };
    let Some(type_id) = ir.type_by_name(&kind) else {
        return Ok(None);
    };
    let Some(template) =
        rules::ir::SymbolFacts::replacement_template(&facts, type_id, &invocation.key)
    else {
        return Ok(None);
    };
    let Some(schema) = hir::template::template_body(ir, type_id) else {
        return Ok(None);
    };
    let mut bindings = BTreeMap::new();
    let mut raw = BTreeMap::new();
    let mut present = BTreeSet::new();
    for argument in source.properties().iter().filter(|argument| {
        argument.path.len() == invocation.path.len() + 1
            && argument.path.starts_with(&invocation.path)
            && invocation.range.start() <= argument.range.start()
            && argument.range.end() <= invocation.range.end()
    }) {
        let name = argument.key.to_ascii_lowercase();
        present.insert(name.clone());
        if let Some(scalar) = &argument.scalar {
            bindings.insert(
                name.clone(),
                hir::template::binding_value(source.syntax(), scalar),
            );
            if let Some(value) = source.syntax().text(scalar.range) {
                raw.insert(name, value.to_owned());
            }
        }
    }
    for (parameter, value) in overrides {
        let name = parameter.to_ascii_lowercase();
        let quoted = raw.get(&name).is_some_and(|raw| raw.starts_with('"'));
        raw.insert(
            name.clone(),
            if quoted {
                format!("\"{}\"", parser::encode_quoted_script_text(value))
            } else {
                value.to_owned()
            },
        );
        bindings.insert(name.clone(), value.to_owned());
        present.insert(name);
    }
    let rendered = hir::template_text::render_expanded(
        ir,
        &facts,
        template,
        &bindings,
        &raw,
        &present,
        schema,
        invocation_state(source, invocation),
        1024 * 1024,
        &mut || cancellation.checkpoint(),
    )?;
    cancellation.checkpoint()?;
    let parsed = std::sync::Arc::new(parser::parse(parser::FileFormat::Script, &rendered.text));
    let hir = hir::lower_ir_schema(
        parsed,
        ir,
        schema,
        Default::default(),
        invocation_state(source, invocation),
        &facts,
    );
    let checked = hir::checking::check_fragment(ir, &hir, &facts, &rendered, &mut || {
        cancellation.checkpoint()
    })?;
    let mut coverage = rendered.coverage.clone();
    coverage.merge(&checked.coverage);
    Ok(Some(BodyAnalysis {
        rendered,
        hir,
        evidence: checked.value,
        coverage,
    }))
}

pub(crate) fn evidence_dependencies(
    body: &BodyAnalysis,
    evidence: &hir::checking::ConstraintEvidence,
) -> BTreeSet<String> {
    let mut dependencies = body.rendered.dependencies(evidence.range);
    for parent in body.hir.properties().iter().filter(|parent| {
        parent.range.start() <= evidence.range.start() && evidence.range.end() <= parent.range.end()
    }) {
        dependencies.extend(body.rendered.dependencies(parent.key_range));
    }
    if evidence.kind == hir::checking::IssueKind::Cardinality {
        for sibling in body.hir.properties().iter().filter(|property| {
            evidence.container.start() <= property.key_range.start()
                && property.key_range.end() <= evidence.container.end()
        }) {
            dependencies.extend(body.rendered.dependencies(sibling.key_range));
        }
    }
    dependencies
}

pub(crate) fn project_evidence_range(
    body: &BodyAnalysis,
    evidence: &hir::checking::ConstraintEvidence,
    source: &hir::HirFile,
    invocation: &hir::HirProperty,
) -> text::TextRange {
    for piece in &body.rendered.pieces {
        if piece.range.start() > evidence.range.start() || evidence.range.end() > piece.range.end()
        {
            continue;
        }
        let Some(mapping) = &piece.binding_source else {
            continue;
        };
        let Some(argument) = source.properties().iter().find(|argument| {
            argument.path.len() == invocation.path.len() + 1
                && argument.path.starts_with(&invocation.path)
                && argument.key.eq_ignore_ascii_case(&mapping.parameter)
        }) else {
            continue;
        };
        let Some(scalar) = &argument.scalar else {
            continue;
        };
        let (Some(start), Some(end)) = (
            mapping
                .offsets
                .get((evidence.range.start() - piece.range.start()) as usize),
            mapping
                .offsets
                .get((evidence.range.end() - piece.range.start()) as usize),
        ) else {
            continue;
        };
        let Some(relative) = text::TextRange::new(*start, *end) else {
            continue;
        };
        let mapped = if scalar.quoted {
            source
                .syntax()
                .text(scalar.range)
                .and_then(parser::parse_quoted_script)
                .and_then(|script| script.source_map().decoded_range(relative))
        } else {
            Some(relative)
        };
        if let Some(mapped) = mapped {
            return text::TextRange::new(
                scalar.range.start() + mapped.start(),
                scalar.range.start() + mapped.end(),
            )
            .unwrap_or(scalar.range);
        }
    }
    let dependencies = evidence_dependencies(body, evidence);
    let argument = source.properties().iter().find(|argument| {
        argument.path.len() == invocation.path.len() + 1
            && argument.path.starts_with(&invocation.path)
            && dependencies.contains(&argument.key.to_ascii_lowercase())
    });
    let Some(argument) = argument else {
        return invocation.key_range;
    };
    if let Some(scalar) = &argument.scalar {
        let root = argument.key.to_ascii_lowercase();
        if let Some(piece) = body.rendered.pieces.iter().find(|piece| {
            piece.parameters.len() == 1
                && piece.parameters.contains(&root)
                && piece.range.start() <= evidence.range.start()
                && evidence.range.end() <= piece.range.end()
        }) {
            let rendered = body
                .rendered
                .text
                .get(piece.range.start() as usize..piece.range.end() as usize);
            if rendered == Some(scalar.value.as_str()) {
                let relative = text::TextRange::new(
                    evidence.range.start() - piece.range.start(),
                    evidence.range.end() - piece.range.start(),
                )
                .expect("piece-relative evidence");
                if scalar.quoted {
                    if let Some(raw) = source.syntax().text(scalar.range)
                        && let Some(decoded) = parser::parse_quoted_script(raw)
                        && let Some(mapped) = decoded.source_map().decoded_range(relative)
                    {
                        return text::TextRange::new(
                            scalar.range.start() + mapped.start(),
                            scalar.range.start() + mapped.end(),
                        )
                        .unwrap_or(scalar.range);
                    }
                } else {
                    return text::TextRange::new(
                        scalar.range.start() + relative.start(),
                        scalar.range.start() + relative.end(),
                    )
                    .unwrap_or(scalar.range);
                }
            }
        }
        scalar.range
    } else {
        argument.value_range.unwrap_or(argument.range)
    }
}

pub(crate) fn validate_candidate(
    snapshot: &AnalysisSnapshot,
    source: &hir::HirFile,
    invocation: &hir::HirProperty,
    summary: &engine::DynamicDefinitionSummary,
    parameter: &str,
    witness: &hir::template_relations::Witness,
    cancellation: &CancellationToken,
) -> Result<Analysis<hir::analysis::Validation>, Cancelled> {
    use hir::analysis::Validation;
    if !needs_body_analysis(snapshot, summary) {
        return Ok(Analysis {
            value: Validation::Valid,
            coverage: Default::default(),
        });
    }
    let Some(body) =
        analyse_body_with_bindings(snapshot, source, invocation, witness, cancellation)?
    else {
        return Ok(Analysis {
            value: Validation::Unknown,
            coverage: Default::default(),
        });
    };
    let rejected = body
        .evidence
        .iter()
        .filter(|evidence| {
            !matches!(
                evidence.kind,
                hir::checking::IssueKind::LogicalContainer
                    | hir::checking::IssueKind::ConstantCondition
                    | hir::checking::IssueKind::MissingLimit
                    | hir::checking::IssueKind::EmptyBlock
            )
        })
        .any(|evidence| {
            evidence_dependencies(&body, evidence).contains(&parameter.to_ascii_lowercase())
                || evidence.kind == hir::checking::IssueKind::Syntax
                    && body
                        .rendered
                        .dependencies(evidence.range)
                        .contains(&parameter.to_ascii_lowercase())
        });
    let focus = parameter.to_ascii_lowercase();
    let related_hole = body
        .rendered
        .pieces
        .iter()
        .filter(|piece| piece.hole)
        .any(|piece| {
            body.hir.properties().iter().any(|parent| {
                parent.range.start() <= piece.range.start()
                    && piece.range.end() <= parent.range.end()
                    && body.rendered.dependencies(parent.range).contains(&focus)
            })
        });
    Ok(Analysis {
        value: if rejected {
            Validation::Invalid
        } else if related_hole
            || !body.coverage.is_complete()
            || body
                .coverage
                .residuals
                .contains(&hir::analysis::ResidualReason::TextInterpretation)
        {
            Validation::Unknown
        } else {
            Validation::Valid
        },
        coverage: body.coverage,
    })
}

#[derive(Clone, Debug)]
pub(crate) struct ParameterSite {
    pub(crate) origin: (String, String),
    pub(crate) domain: Domain,
    pub(crate) token: TemplateToken,
    pub(crate) state: ScopeState,
}

impl ParameterSite {
    pub(crate) fn rendered_value(&self, parameter: &str, value: &str) -> Option<String> {
        render_candidate(&self.token, parameter, value)
    }
    pub(crate) fn accepts_candidate(
        &self,
        snapshot: &AnalysisSnapshot,
        parameter: &str,
        value: &str,
    ) -> bool {
        if !self.accepts(snapshot, parameter, value) {
            return false;
        }
        let Some(rendered) = render_candidate(&self.token, parameter, value) else {
            return true;
        };
        let Domain::Value(matchers) = &self.domain else {
            return true;
        };
        matchers
            .iter()
            .any(|matcher| candidate_matches(snapshot, *matcher, &rendered, &self.state))
    }
    pub(crate) fn accepts(
        &self,
        snapshot: &AnalysisSnapshot,
        parameter: &str,
        value: &str,
    ) -> bool {
        let Some(rendered) = render_candidate(&self.token, parameter, value) else {
            return true;
        };
        let ir = snapshot.ir();
        match &self.domain {
            Domain::Value(matchers) => matchers.iter().any(|matcher| {
                rendered_value_matches(
                    snapshot,
                    *matcher,
                    &rendered,
                    &self.state,
                    affixes(&self.token, parameter)
                        .is_some_and(|(head, tail)| !head.is_empty() || !tail.is_empty()),
                )
            }),
            Domain::Key { schema, shape } => ir.lookup(*schema, &rendered, *shape).any(|id| {
                field_allowed(snapshot, id, &self.state)
                    && ir_semantic::matcher_matches(
                        ir,
                        ir.field(id).key,
                        &rendered,
                        &WorkspaceFacts { snapshot },
                    )
            }),
            Domain::Payload { .. } | Domain::Unresolved => true,
        }
    }

    fn candidate_values(
        &self,
        snapshot: &AnalysisSnapshot,
        parameter: &str,
        prefix: &str,
    ) -> Vec<String> {
        let ir = snapshot.ir();
        let rendered_prefix = self
            .token
            .fragments
            .first()
            .and_then(|part| match part {
                TemplateFragment::Literal(text) => Some(format!("{text}{prefix}")),
                _ => None,
            })
            .unwrap_or_default();
        let rendered_prefix = if affixes(&self.token, parameter).is_some() {
            rendered_prefix.as_str()
        } else {
            ""
        };
        match &self.domain {
            Domain::Value(matchers) => matchers
                .iter()
                .flat_map(|matcher| {
                    ir_semantic::spellings_with_state(
                        ir,
                        *matcher,
                        snapshot,
                        rendered_prefix,
                        Some(&self.state),
                    )
                })
                .collect(),
            Domain::Key { schema, shape } => ir
                .fields(*schema)
                .into_iter()
                .filter(|id| {
                    ir.shape(*id) == Some(*shape) && field_allowed(snapshot, *id, &self.state)
                })
                .flat_map(|id| {
                    ir_semantic::spellings_with_state(
                        ir,
                        ir.field(id).key,
                        snapshot,
                        rendered_prefix,
                        Some(&self.state),
                    )
                })
                .collect(),
            Domain::Payload { .. } | Domain::Unresolved => Vec::new(),
        }
    }
}

/// Candidate assignments share one witness across every finite usage relation.
pub(crate) fn relational_candidates(
    snapshot: &AnalysisSnapshot,
    sites: &[ParameterSite],
    parameter: &str,
    prefix: &str,
    cancellation: &CancellationToken,
) -> Result<Analysis<Vec<hir::template_relations::Witness>>, Cancelled> {
    use hir::template_relations::{SearchBudget, Witness, inverse, join};
    let mut budget = SearchBudget::new(100_000);
    let mut rows = vec![Witness::new()];
    let mut constrained = false;
    for site in sites {
        cancellation.checkpoint()?;
        let values = site.candidate_values(snapshot, parameter, prefix);
        if values.is_empty() {
            continue;
        }
        let mut relation = BTreeSet::new();
        for value in values {
            for row in inverse(&site.token.fragments, &value, &mut budget, &mut || {
                cancellation.checkpoint()
            })? {
                if row
                    .get(&parameter.to_ascii_lowercase())
                    .is_none_or(|value| {
                        value
                            .to_ascii_lowercase()
                            .contains(&prefix.to_ascii_lowercase())
                    })
                {
                    relation.insert(row);
                }
            }
        }
        rows = join(
            &rows,
            &relation.into_iter().collect::<Vec<_>>(),
            &mut budget,
            &mut || cancellation.checkpoint(),
        )?;
        constrained = true;
        if rows.is_empty() || !budget.coverage.is_complete() {
            break;
        }
    }
    if !constrained {
        rows.clear();
    }
    rows.retain(|row| {
        sites
            .iter()
            .all(|site| site.accepts_witness(snapshot, parameter, row))
    });
    Ok(hir::template_relations::result(rows, budget))
}

impl ParameterSite {
    pub(crate) fn witness_validation(
        &self,
        snapshot: &AnalysisSnapshot,
        witness: &hir::template_relations::Witness,
    ) -> hir::analysis::Validation {
        use hir::analysis::Validation;
        let rendered = self
            .token
            .fragments
            .iter()
            .map(|part| match part {
                TemplateFragment::Literal(value) => Some(value.as_str()),
                TemplateFragment::Parameter { name, .. } => {
                    witness.get(&name.to_ascii_lowercase()).map(String::as_str)
                }
            })
            .collect::<Option<String>>();
        let Some(rendered) = rendered else {
            return Validation::Unknown;
        };
        match &self.domain {
            Domain::Value(matchers) => {
                let mut unknown = false;
                for matcher in matchers {
                    match hir::checking::scalar_validation(
                        snapshot.ir(),
                        *matcher,
                        &rendered,
                        &self.state,
                        &WorkspaceFacts { snapshot },
                    ) {
                        Validation::Valid => return Validation::Valid,
                        Validation::Unknown => unknown = true,
                        Validation::Invalid => {}
                    }
                }
                if unknown {
                    Validation::Unknown
                } else {
                    Validation::Invalid
                }
            }
            Domain::Key { schema, shape } => {
                let mut unknown = false;
                for id in snapshot
                    .ir()
                    .lookup(*schema, &rendered, *shape)
                    .filter(|id| field_allowed(snapshot, *id, &self.state))
                {
                    if !ir_semantic::matcher_matches(
                        snapshot.ir(),
                        snapshot.ir().field(id).key,
                        &rendered,
                        &WorkspaceFacts { snapshot },
                    ) {
                        continue;
                    }
                    let field = snapshot.ir().field(id);
                    if field
                        .scope
                        .as_ref()
                        .is_some_and(|scope| !scope.scopes_in.is_empty())
                        && self
                            .state
                            .current
                            .first()
                            .is_none_or(|scope| matches!(scope, hir::ScopeValue::Unknown))
                    {
                        unknown = true;
                    } else {
                        return Validation::Valid;
                    }
                }
                if unknown {
                    Validation::Unknown
                } else {
                    Validation::Invalid
                }
            }
            Domain::Payload { .. } | Domain::Unresolved => Validation::Unknown,
        }
    }

    fn accepts_witness(
        &self,
        snapshot: &AnalysisSnapshot,
        parameter: &str,
        witness: &hir::template_relations::Witness,
    ) -> bool {
        let Some(value) = witness.get(&parameter.to_ascii_lowercase()) else {
            return false;
        };
        let mut site = self.clone();
        for part in &mut site.token.fragments {
            if let TemplateFragment::Parameter { name, .. } = part
                && !name.eq_ignore_ascii_case(parameter)
                && let Some(value) = witness.get(&name.to_ascii_lowercase())
            {
                *part = TemplateFragment::Literal(value.clone());
            }
        }
        site.accepts_candidate(snapshot, parameter, value)
    }
}

fn candidate_matches(
    snapshot: &AnalysisSnapshot,
    matcher: MatcherId,
    value: &str,
    state: &ScopeState,
) -> bool {
    let ir = snapshot.ir();
    match ir.matcher(matcher) {
        Matcher::Loc => crate::semantic::workspace_member(snapshot, "localisation", value),
        Matcher::Union(items) => items
            .iter()
            .any(|item| candidate_matches(snapshot, *item, value, state)),
        _ => ir_semantic::matcher_matches_with_state(
            snapshot,
            ir,
            matcher,
            value,
            &WorkspaceFacts { snapshot },
            state,
        ),
    }
}

fn rendered_value_matches(
    snapshot: &AnalysisSnapshot,
    matcher: MatcherId,
    value: &str,
    state: &ScopeState,
    affixed: bool,
) -> bool {
    let ir = snapshot.ir();
    match ir.matcher(matcher) {
        Matcher::Ref(rules::ir::RefTarget::Type { type_id, .. }) if affixed => {
            let info = ir.type_info(*type_id);
            (info.builtin.is_empty()
                && !crate::semantic::workspace_kind_has_members(
                    snapshot,
                    ir.strings().resolve(info.name),
                ))
                || ir_semantic::matcher_matches_with_state(
                    snapshot,
                    ir,
                    matcher,
                    value,
                    &WorkspaceFacts { snapshot },
                    state,
                )
        }
        Matcher::Union(items) => items
            .iter()
            .any(|item| rendered_value_matches(snapshot, *item, value, state, affixed)),
        _ => ir_semantic::matcher_matches_with_state(
            snapshot,
            ir,
            matcher,
            value,
            &WorkspaceFacts { snapshot },
            state,
        ),
    }
}

fn field_allowed(snapshot: &AnalysisSnapshot, id: FieldId, state: &ScopeState) -> bool {
    let ir = snapshot.ir();
    ir.field(id).scope.as_ref().is_none_or(|effect| {
        effect.scopes_in.is_empty()
            || state
                .current
                .first()
                .is_none_or(|current| ir_semantic::scope_allows(ir, current, &effect.scopes_in))
    })
}

fn map_sites(sites: Analysis<Vec<hir::template::ParameterSite>>) -> Analysis<Vec<ParameterSite>> {
    let coverage = sites.coverage;
    let value = sites
        .value
        .into_iter()
        .map(|site| ParameterSite {
            origin: site.origin,
            domain: site.domain,
            token: site.token,
            state: site.state,
        })
        .collect();
    Analysis { value, coverage }
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn parameter_sites(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    name: &str,
    parameter: &str,
    bindings: &BTreeMap<String, String>,
    state: ScopeState,
    cancellation: &CancellationToken,
) -> Result<Analysis<Vec<ParameterSite>>, Cancelled> {
    hir::template::parameter_sites(
        snapshot.ir(),
        &WorkspaceFacts { snapshot },
        kind,
        name,
        parameter,
        bindings,
        state,
        &mut || cancellation.checkpoint(),
    )
    .map(map_sites)
}
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn parameter_sites_with_budget(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    name: &str,
    parameter: &str,
    bindings: &BTreeMap<String, String>,
    state: ScopeState,
    nodes: usize,
    cancellation: &CancellationToken,
) -> Result<Analysis<Vec<ParameterSite>>, Cancelled> {
    hir::template::parameter_sites_with_budget(
        snapshot.ir(),
        &WorkspaceFacts { snapshot },
        kind,
        name,
        parameter,
        bindings,
        state,
        nodes,
        &mut || cancellation.checkpoint(),
    )
    .map(map_sites)
}
pub(crate) fn definition_parameter_sites(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    name: &str,
    parameter: &str,
    cancellation: &CancellationToken,
) -> Result<Analysis<Vec<ParameterSite>>, Cancelled> {
    hir::template::definition_parameter_sites(
        snapshot.ir(),
        &WorkspaceFacts { snapshot },
        kind,
        name,
        parameter,
        &mut || cancellation.checkpoint(),
    )
    .map(map_sites)
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn parameter_symbol_sites(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    name: &str,
    parameter: &str,
    bindings: &BTreeMap<String, String>,
    state: ScopeState,
    cancellation: &CancellationToken,
) -> Result<Analysis<Vec<ParameterSite>>, Cancelled> {
    hir::template::parameter_symbol_sites(
        snapshot.ir(),
        &WorkspaceFacts { snapshot },
        kind,
        name,
        parameter,
        bindings,
        state,
        &mut || cancellation.checkpoint(),
    )
    .map(map_sites)
}
pub(crate) fn missing_parameters(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    name: &str,
    bindings: &BTreeMap<String, String>,
    state: ScopeState,
    cancellation: &CancellationToken,
) -> Result<Analysis<BTreeSet<String>>, Cancelled> {
    hir::template::missing_parameters(
        snapshot.ir(),
        &WorkspaceFacts { snapshot },
        kind,
        name,
        bindings,
        state,
        &mut || cancellation.checkpoint(),
    )
}
pub(crate) fn template_kind(ir: &rules::ir::RulesIr, matcher: MatcherId) -> Option<String> {
    hir::template::template_kind(ir, matcher)
}

fn render_candidate(token: &TemplateToken, parameter: &str, value: &str) -> Option<String> {
    token
        .fragments
        .iter()
        .map(|fragment| match fragment {
            TemplateFragment::Literal(text) => Some(text.as_str()),
            TemplateFragment::Parameter { name, .. } if name.eq_ignore_ascii_case(parameter) => {
                Some(value)
            }
            _ => None,
        })
        .collect::<Option<Vec<_>>>()
        .map(|parts| parts.concat())
}

fn affixes(token: &TemplateToken, parameter: &str) -> Option<(String, String)> {
    let mut head = String::new();
    let mut tail = String::new();
    let mut found = false;
    for fragment in &token.fragments {
        match fragment {
            TemplateFragment::Literal(text) => {
                if found {
                    tail.push_str(text);
                } else {
                    head.push_str(text);
                }
            }
            TemplateFragment::Parameter { name, .. }
                if !found && name.eq_ignore_ascii_case(parameter) =>
            {
                found = true
            }
            _ => return None,
        }
    }
    found.then_some((head, tail))
}

pub(crate) fn invocation_inputs(
    source: &hir::HirFile,
    invocation: &hir::HirProperty,
) -> hir::template::BindingInputs {
    let mut inputs = hir::template::BindingInputs::default();
    for argument in source
        .properties_in_range(invocation.range)
        .filter(|argument| {
            argument.path.len() == invocation.path.len() + 1
                && argument.path.starts_with(&invocation.path)
        })
    {
        let name = argument.key.to_ascii_lowercase();
        inputs.present.insert(name.clone());
        if let Some(scalar) = &argument.scalar {
            inputs
                .values
                .insert(name, hir::template::binding_value(source.syntax(), scalar));
        }
    }
    inputs
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn parameter_sites_for_inputs(
    snapshot: &AnalysisSnapshot,
    kind: &str,
    name: &str,
    parameter: &str,
    inputs: &hir::template::BindingInputs,
    state: ScopeState,
    cancellation: &CancellationToken,
) -> Result<Analysis<Vec<ParameterSite>>, Cancelled> {
    hir::template::parameter_sites_with_inputs(
        snapshot.ir(),
        &WorkspaceFacts { snapshot },
        kind,
        name,
        parameter,
        inputs,
        state,
        false,
        &mut || cancellation.checkpoint(),
    )
    .map(map_sites)
}

pub(crate) fn parameter_sites_at(
    snapshot: &AnalysisSnapshot,
    source: &hir::HirFile,
    invocation: &hir::HirProperty,
    kind: &str,
    name: &str,
    parameter: &str,
    cancellation: &CancellationToken,
) -> Result<Analysis<Vec<ParameterSite>>, Cancelled> {
    hir::template::parameter_sites_with_inputs(
        snapshot.ir(),
        &WorkspaceFacts { snapshot },
        kind,
        name,
        parameter,
        &invocation_inputs(source, invocation),
        invocation_state(source, invocation),
        false,
        &mut || cancellation.checkpoint(),
    )
    .map(map_sites)
}

pub(crate) fn invocation_bindings(
    hir: &hir::HirFile,
    invocation: &hir::HirProperty,
) -> BTreeMap<String, String> {
    invocation_inputs(hir, invocation).values
}

pub(crate) fn invocation_state(hir: &hir::HirFile, invocation: &hir::HirProperty) -> ScopeState {
    hir.field_fact_at(invocation.key_range)
        .and_then(|field| {
            hir.schema_facts()
                .iter()
                .filter(|fact| {
                    fact.schema == field.schema
                        && crate::support::contains(fact.range, invocation.key_range.start())
                })
                .min_by_key(|fact| fact.range.len())
        })
        .map(|fact| fact.state.clone())
        .unwrap_or(ScopeState {
            root: hir::ScopeValue::Unknown,
            current: vec![hir::ScopeValue::Unknown],
            from: Vec::new(),
            previous: Vec::new(),
        })
}
